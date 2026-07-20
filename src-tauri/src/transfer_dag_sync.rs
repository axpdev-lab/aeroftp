// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! DAG-ENGINE phase 2: the sync-session transfer path through `execute_dag`.
//!
//! [`crate::sync::sync_tree_core`] is the shared sync engine: the MCP
//! `sync_tree` tool and the `sync_core` re-export both run through it. This
//! module is the flag-gated bridge that routes that engine through the shared
//! graph engine instead of its hand-rolled interleaved decide/perform loop.
//!
//! ## Shape of the sync graph
//!
//! A sync session has one global discovery prefix
//! (`DiscoverLocal` + `DiscoverRemote` -> `Compare`) and one per-file transfer
//! chain below `Compare` for every upload/download the plan resolved. The
//! graph is static, so the scan and the compare must finish *before* the
//! graph can be built: the discover/compare nodes are structural anchors,
//! and the real scan happens in [`execute_sync_dag`]'s planning phase.
//!
//! ## What this actually buys
//!
//! A normal sync can own independent `clone_for_transfer` workers, bounded by
//! the provider's live session ceiling and the graph's file slots. A requested
//! rsync delta sync retains the primary provider and one `DeltaBatch` in an
//! exclusive serial lane. Routing both modes through the graph buys:
//!
//! - **Parallel discovery and normal-file transfer.** The local filesystem
//!   walk overlaps the remote scan, and clone-backed providers can execute
//!   bounded whole-file nodes concurrently.
//! - **Structural uniformity** with the single-file (phase 1) and batch
//!   (phase 2) graphs: one scheduler, one observability surface, and the
//!   foundation phase 3 needs to make capability-aware nodes (server-side
//!   copy, multipart, resume, AIMD) benefit sync for free.
//! - **A provably non-mutating dry-run.** Dry-run never reaches this module:
//!   the [`crate::sync::sync_tree_core`] guard excludes it, so the dry-run
//!   plan output cannot build or execute a graph by construction.
//!
//! ## Byte-identical contract
//!
//! The per-file decisions are computed by the same pure `decide_upload` /
//! `decide_download` the legacy core uses, from the same pre-transfer scan
//! snapshots, so the plan is decision-equivalent. The real per-file I/O is
//! the same `perform_upload` / `perform_download`, holding the same single
//! delta-sync batch session across files.
//!
//! The contract is counter-based, the same precedent the phase-2 batch path
//! set: identical [`SyncReport`] counters, the same set of `on_file_*` sink
//! events (one per file), the same delta-batch session metrics, the same
//! journal format. One deliberate, documented difference: skip events are
//! emitted in the planning order up front, then the transfers, then the
//! orphan deletes; the legacy core interleaves skips with transfers in scan
//! order. Same events, same counts, grouped by phase instead of interleaved.
//!
//! ## Dry-run gate
//!
//! A dry-run sync never routes through the DAG path: the plan output must
//! be provably non-mutating, and gating on the action up front (in
//! [`should_route_sync_to_dag`]) makes that a structural guarantee rather
//! than a convention buried in the executor.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;

use crate::delta_transport::DeltaBatch;
use crate::provider_transfer_executor::compose_runtime_transfer_capabilities;
use crate::providers::{ProviderTransferExecutorKind, StorageProvider};
use crate::sync::{
    apply_sync_tree_outcome, decide_download, decide_upload, ensure_remote_dir, perform_download,
    perform_local_delete, perform_remote_delete, perform_upload, scan_options_for_sync,
    DeltaPolicy, FileOutcome, SyncDirection, SyncError, SyncOptions, SyncPhase, SyncProgressSink,
    SyncReport, SyncTransferSpec, SyncTreeAction,
};
use crate::sync_core::scan::{
    scan_local_tree_checked, scan_remote_tree_checked, LocalEntry, RemoteEntry, ScanCompleteness,
};
use async_trait::async_trait;

use crate::transfer_dag::executor::{execute_dag, DagNodeRunner, NodeFuture, NodeOutcome};
use crate::transfer_dag::graph::{TransferNode, TransferNodeKind};
use crate::transfer_dag::{
    AimdConfig, AimdController, DagObserver, DiskLeaseRequest, FileAdmission, NoopDagObserver,
    StreamingConfig, TransferBudget, TransferCapabilities, TransferDagBuilder, TransferDirection,
    TransferPriority, TransferWorkItem, WorkSource,
};

/// Bounded capacity of the transfer-job channel. The DAG file-slot budget is
/// the actual normal-file cap; this channel is bounded dispatch headroom.
const JOB_CHANNEL_CAPACITY: usize = 32;

/// Whether a sync call should route through the graph engine.
///
/// Dry-run is the only exit: the plan output must be provably non-mutating,
/// so a dry-run sync never builds or executes a mutating graph. Every other
/// sync call routes through [`execute_sync_dag`].
pub fn should_route_sync_to_dag(dry_run: bool) -> bool {
    !dry_run
}

/// One resolved transfer in a sync plan, in plan order.
#[derive(Clone)]
struct PlannedTransfer {
    rel: String,
    /// `"upload"` or `"download"`: the operation label the legacy core
    /// passes to `apply_sync_tree_outcome`.
    op: &'static str,
    total: u64,
    decision_policy: DeltaPolicy,
    expected_sha256_hex: Option<String>,
}

fn remote_sha256_hex(entry: &RemoteEntry) -> Option<String> {
    match (entry.checksum_alg.as_deref(), entry.checksum_hex.as_deref()) {
        (Some(alg), Some(hex)) if alg.eq_ignore_ascii_case("sha256") => Some(hex.to_string()),
        _ => None,
    }
}

/// One resolved skip in a sync plan.
struct PlannedSkip {
    rel: String,
    /// `"upload"` or `"download"`: the operation label, identical to the
    /// legacy core's `apply_sync_tree_outcome` call for a skipped file.
    apply_op: &'static str,
    reason: String,
    decision_policy: DeltaPolicy,
}

/// One resolved orphan deletion in a sync plan.
struct PlannedDelete {
    rel: String,
    /// `"delete_remote"` or `"delete_local"`.
    op: &'static str,
}

/// The fully resolved plan of a sync session, computed before any transfer.
struct SyncDagPlan {
    transfers: Vec<PlannedTransfer>,
    skips: Vec<PlannedSkip>,
    deletes: Vec<PlannedDelete>,
}

/// Resolve a sync session into an ordered plan from the pre-transfer scan
/// snapshots.
///
/// This mirrors the two-pass structure of [`crate::sync::sync_tree_core`]
/// (upload pass over `locals`, download pass over `remotes`, then orphan
/// deletes) but collects the decisions instead of performing them inline.
/// The decision functions `decide_upload` / `decide_download` are pure and
/// read only the scan snapshots, so collecting the whole plan up front is
/// decision-equivalent to the legacy interleaved loop.
fn plan_sync_dag(
    locals: &[LocalEntry],
    remotes: &[RemoteEntry],
    opts: &SyncOptions,
) -> SyncDagPlan {
    use std::collections::{HashMap as Map, HashSet};

    let mut seen_local: HashSet<String> = HashSet::new();
    let mut seen_remote: HashSet<String> = HashSet::new();
    // In direction=Both, a download is suppressed only for paths the upload
    // pass actually resolved with a Copy; a plain upload Skip must not
    // suppress a later download decision.
    let mut upload_resolved_for_both: HashSet<String> = HashSet::new();
    let local_by_path: Map<&str, &LocalEntry> = locals
        .iter()
        .map(|entry| (entry.rel_path.as_str(), entry))
        .collect();
    let remote_by_path: Map<&str, &RemoteEntry> = remotes
        .iter()
        .map(|entry| (entry.rel_path.as_str(), entry))
        .collect();

    let mut transfers: Vec<PlannedTransfer> = Vec::new();
    let mut skips: Vec<PlannedSkip> = Vec::new();

    if matches!(opts.direction, SyncDirection::Upload | SyncDirection::Both) {
        for local_entry in locals {
            if !seen_local.insert(local_entry.rel_path.clone()) {
                continue;
            }
            let remote_entry = remote_by_path.get(local_entry.rel_path.as_str()).copied();
            let decision = decide_upload(
                local_entry,
                remote_entry,
                opts.delta_policy,
                opts.conflict_mode,
            );
            match decision.action {
                SyncTreeAction::Copy => {
                    if matches!(opts.direction, SyncDirection::Both) {
                        upload_resolved_for_both.insert(local_entry.rel_path.clone());
                    }
                    transfers.push(PlannedTransfer {
                        rel: local_entry.rel_path.clone(),
                        op: "upload",
                        total: local_entry.size,
                        decision_policy: decision.decision_policy,
                        expected_sha256_hex: None,
                    });
                }
                SyncTreeAction::Skip(reason) => {
                    skips.push(PlannedSkip {
                        rel: local_entry.rel_path.clone(),
                        apply_op: "upload",
                        reason,
                        decision_policy: decision.decision_policy,
                    });
                }
            }
        }
    }

    if matches!(
        opts.direction,
        SyncDirection::Download | SyncDirection::Both
    ) {
        for remote_entry in remotes {
            if !seen_remote.insert(remote_entry.rel_path.clone()) {
                continue;
            }
            let handled_by_upload = matches!(opts.direction, SyncDirection::Both)
                && upload_resolved_for_both.contains(&remote_entry.rel_path);
            let decision = decide_download(
                remote_entry,
                local_by_path.get(remote_entry.rel_path.as_str()).copied(),
                opts.delta_policy,
                opts.conflict_mode,
                handled_by_upload,
            );
            match decision.action {
                SyncTreeAction::Copy => {
                    transfers.push(PlannedTransfer {
                        rel: remote_entry.rel_path.clone(),
                        op: "download",
                        total: remote_entry.size,
                        decision_policy: decision.decision_policy,
                        expected_sha256_hex: remote_sha256_hex(remote_entry),
                    });
                }
                SyncTreeAction::Skip(reason) => {
                    skips.push(PlannedSkip {
                        rel: remote_entry.rel_path.clone(),
                        apply_op: "download",
                        reason,
                        decision_policy: decision.decision_policy,
                    });
                }
            }
        }
    }

    let mut deletes: Vec<PlannedDelete> = Vec::new();
    if opts.delete_orphans {
        match opts.direction {
            SyncDirection::Upload => {
                for remote_entry in remotes {
                    if !local_by_path.contains_key(remote_entry.rel_path.as_str()) {
                        deletes.push(PlannedDelete {
                            rel: remote_entry.rel_path.clone(),
                            op: "delete_remote",
                        });
                    }
                }
            }
            SyncDirection::Download => {
                for local_entry in locals {
                    if !remote_by_path.contains_key(local_entry.rel_path.as_str()) {
                        deletes.push(PlannedDelete {
                            rel: local_entry.rel_path.clone(),
                            op: "delete_local",
                        });
                    }
                }
            }
            SyncDirection::Both => {}
        }
    }

    SyncDagPlan {
        transfers,
        skips,
        deletes,
    }
}

/// The session lane selected before a transfer node is dispatched.
///
/// A requested rsync delta run retains the primary provider and its one
/// `DeltaBatch` for the whole session. Normal sync runs use only independent
/// `clone_for_transfer` workers, never the primary session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncTransferLane {
    NormalPool,
    DeltaExclusive,
}

fn transfer_lane(requested_policy: DeltaPolicy) -> SyncTransferLane {
    if matches!(requested_policy, DeltaPolicy::Delta) {
        SyncTransferLane::DeltaExclusive
    } else {
        SyncTransferLane::NormalPool
    }
}

fn is_clone_backed_executor(kind: ProviderTransferExecutorKind) -> bool {
    matches!(
        kind,
        ProviderTransferExecutorKind::HttpClonePool
            | ProviderTransferExecutorKind::SftpConnectionPool
            | ProviderTransferExecutorKind::FtpConnectionPool
    )
}

/// Snapshot runtime capabilities and take ownership of normal-file workers.
///
/// The caller keeps the primary provider. Delta always uses that primary and
/// therefore deliberately receives no clone workers. For normal syncs we
/// create every worker through the existing `clone_for_transfer` contract;
/// a partial allocation is discarded and the call falls back to one primary
/// serial lane rather than overclaiming the provider's session ceiling.
async fn prepare_normal_file_workers(
    provider: &mut Box<dyn StorageProvider>,
    requested_policy: DeltaPolicy,
) -> (TransferCapabilities, Vec<Box<dyn StorageProvider>>) {
    let advertised = provider.transfer_capabilities();
    let executor_kind = provider.transfer_executor_kind();

    if matches!(requested_policy, DeltaPolicy::Delta) || !is_clone_backed_executor(executor_kind) {
        return (
            compose_runtime_transfer_capabilities(&advertised, executor_kind, false),
            Vec::new(),
        );
    }

    let ceiling = advertised
        .max_file_slots
        .unwrap_or_else(|| provider.transfer_executor_max_sessions())
        .min(provider.transfer_executor_max_sessions())
        .max(1) as usize;
    // A one-session "pool" cannot add parallelism. Keep the established
    // primary serial path in that case, avoiding a needless extra login.
    if ceiling < 2 {
        return (
            compose_runtime_transfer_capabilities(&advertised, executor_kind, false),
            Vec::new(),
        );
    }

    let mut workers = Vec::with_capacity(ceiling);
    for _ in 0..ceiling {
        match provider.clone_for_transfer() {
            Ok(worker) => workers.push(worker),
            Err(error) => {
                tracing::info!(
                    "sync normal file pool demoted to serial after clone allocation failed: {}",
                    error
                );
                for mut worker in workers {
                    let _ = worker.disconnect().await;
                }
                return (
                    compose_runtime_transfer_capabilities(&advertised, executor_kind, false),
                    Vec::new(),
                );
            }
        }
    }

    (
        compose_runtime_transfer_capabilities(&advertised, executor_kind, true),
        workers,
    )
}

/// A transfer node handing one planned file off to the ownership driver.
struct TransferJob {
    /// Index into [`SyncDagPlan::transfers`].
    transfer_index: usize,
    lane: SyncTransferLane,
    /// Resolved once the driver has performed the transfer.
    ack: oneshot::Sender<()>,
}

/// Build the node runner for a sync graph.
///
/// Every structural node (discover / compare / acquire / verify / preserve /
/// commit / emit) is a no-op anchor. Only the real `DownloadFile` /
/// `UploadFile` nodes carry work: each one looks up its plan index, hands a
/// [`TransferJob`] to the driver, and waits for the acknowledgement before
/// reporting completion, so the graph's `EmitProgress` terminal fires only
/// after the real transfer finished.
///
/// A node always reports [`NodeOutcome::Completed`]: a failed file is
/// recorded in the [`SyncReport`] by the driver, never on the graph, exactly
/// like the phase-2 batch path. The graph therefore never aborts mid-sync.
///
/// DAG-P2-04: production now streams per-file subgraphs through
/// [`run_one_sync_file`], which replicates this node -> job handoff for one
/// admitted transfer. This multi-file builder is retained as test scaffolding
/// that exercises the same dedup/handoff contract against a whole plan graph.
#[cfg(test)]
fn build_sync_runner(
    node_to_index: Arc<HashMap<usize, usize>>,
    transfer_dispatched: Arc<std::sync::Mutex<Vec<bool>>>,
    job_tx: mpsc::Sender<TransferJob>,
    lanes: Arc<Vec<SyncTransferLane>>,
) -> Arc<dyn DagNodeRunner> {
    Arc::new(move |node: TransferNode| -> NodeFuture {
        let node_to_index = Arc::clone(&node_to_index);
        let transfer_dispatched = Arc::clone(&transfer_dispatched);
        let job_tx = job_tx.clone();
        let lanes = Arc::clone(&lanes);
        Box::pin(async move {
            if !matches!(
                node.kind,
                TransferNodeKind::DownloadFile
                    | TransferNodeKind::UploadFile
                    | TransferNodeKind::UploadPart
            ) {
                return NodeOutcome::Completed;
            }
            let Some(&transfer_index) = node_to_index.get(&node.id) else {
                return NodeOutcome::Completed;
            };
            // Multiple `UploadPart` nodes can map to the same plan entry
            // under multipart fan-out. The sync driver's `perform_upload`
            // / `perform_download` is per-file: only the first part to win
            // the dispatch guard hands off a `TransferJob`; the others
            // become structural no-ops and let the graph drain.
            {
                let mut dispatched = transfer_dispatched.lock().expect("dispatch lock poisoned");
                if dispatched[transfer_index] {
                    return NodeOutcome::Completed;
                }
                dispatched[transfer_index] = true;
            }
            let (ack_tx, ack_rx) = oneshot::channel::<()>();
            if job_tx
                .send(TransferJob {
                    transfer_index,
                    lane: lanes[transfer_index],
                    ack: ack_tx,
                })
                .await
                .is_err()
            {
                // The driver is gone; nothing more this node can do.
                return NodeOutcome::Completed;
            }
            let _ = ack_rx.await;
            NodeOutcome::Completed
        })
    })
}

/// DAG-P2-04: state shared across every transfer of one streaming sync run.
/// Cloned (Arc) into each admitted transfer; only the per-file subgraph and its
/// dispatch guard are built per file in [`run_one_sync_file`].
struct SyncStreamContext {
    caps: TransferCapabilities,
    manager: crate::transfer_dag::TransferResourceManager,
    aimd: Arc<AimdController>,
    lane: SyncTransferLane,
}

/// A lazy [`WorkSource`] over the precomputed sync transfer plan. Yields one
/// work item per planned transfer on demand. The plan stays a plain vector —
/// allowed by the streaming contract as a plan iterator, not a full executable
/// node graph for N files.
struct SyncTransfersWorkSource {
    transfers: Arc<Vec<PlannedTransfer>>,
    next: usize,
}

#[async_trait]
impl WorkSource for SyncTransfersWorkSource {
    async fn next_item(&mut self) -> Option<TransferWorkItem> {
        let index = self.next;
        let transfer = self.transfers.get(index)?;
        self.next += 1;
        let direction = match transfer.op {
            "upload" => TransferDirection::Upload,
            _ => TransferDirection::Download,
        };
        Some(TransferWorkItem::new(
            transfer.rel.clone(),
            index,
            transfer.total,
            direction,
        ))
    }

    fn known_total(&self) -> Option<usize> {
        Some(self.transfers.len())
    }
}

/// Execute one admitted sync transfer: expand its shaped subgraph, hand the
/// transfer off to the ownership driver exactly once, and drop the subgraph
/// when done.
///
/// The transfer-core nodes dedup through a per-file guard, so a multipart-shaped
/// upload still hands off a single [`TransferJob`] and its remaining
/// `UploadPart` nodes drain as structural no-ops — the same per-file
/// `perform_upload` / `perform_download` contract as before. Node permits, AIMD,
/// and observers stay on real nodes via [`execute_dag`]; the real per-file I/O,
/// the bounded worker pool, and the exclusive delta lane stay in
/// [`drive_sync_transfers`].
async fn run_one_sync_file(
    item: TransferWorkItem,
    mut admission: FileAdmission,
    ctx: Arc<SyncStreamContext>,
    job_tx: mpsc::Sender<TransferJob>,
) {
    let shaped = TransferDagBuilder::shaped_file(item.direction, &ctx.caps, item.size);
    admission.materialize_nodes(shaped.dag.nodes().len());

    let transfer_index = item.index;
    let lane = ctx.lane;
    let dispatched = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let runner: Arc<dyn DagNodeRunner> = Arc::new(move |node: TransferNode| -> NodeFuture {
        let job_tx = job_tx.clone();
        let dispatched = Arc::clone(&dispatched);
        Box::pin(async move {
            if !matches!(
                node.kind,
                TransferNodeKind::DownloadFile
                    | TransferNodeKind::UploadFile
                    | TransferNodeKind::UploadPart
            ) {
                return NodeOutcome::Completed;
            }
            // Per-file dedup: only the first transfer-core node hands off a job;
            // multipart parts become structural no-ops so the graph drains.
            if dispatched.swap(true, std::sync::atomic::Ordering::SeqCst) {
                return NodeOutcome::Completed;
            }
            let (ack_tx, ack_rx) = oneshot::channel::<()>();
            if job_tx
                .send(TransferJob {
                    transfer_index,
                    lane,
                    ack: ack_tx,
                })
                .await
                .is_err()
            {
                // The driver is gone; nothing more this node can do.
                return NodeOutcome::Completed;
            }
            let _ = ack_rx.await;
            NodeOutcome::Completed
        })
    });

    let observer: Arc<dyn DagObserver> = Arc::new(NoopDagObserver);
    if let Err(error) = execute_dag(
        &shaped.dag,
        &ctx.manager,
        runner,
        observer,
        Some(Arc::clone(&ctx.aimd)),
    )
    .await
    {
        tracing::warn!("Sync transfer file graph stopped early: {}", error);
    }
}

/// Progress produced by an independent normal-file worker.
///
/// Workers never borrow the session's caller-owned sink. They record events
/// and return them to the ownership driver, which is the only code that calls
/// the real sink and aggregates the report. This keeps callback ordering
/// coherent when files complete out of plan order.
enum RecordedProgress {
    Start {
        rel: String,
        total: u64,
        op: &'static str,
        decision_policy: DeltaPolicy,
    },
    Progress {
        rel: String,
        sent: u64,
        total: u64,
    },
}

struct RecordingProgressSink {
    events: Vec<RecordedProgress>,
    last_progress: HashMap<String, Instant>,
}

impl RecordingProgressSink {
    fn replay(self, sink: &mut dyn SyncProgressSink) {
        for event in self.events {
            match event {
                RecordedProgress::Start {
                    rel,
                    total,
                    op,
                    decision_policy,
                } => sink.on_file_start(&rel, total, op, decision_policy),
                RecordedProgress::Progress { rel, sent, total } => {
                    sink.on_file_progress(&rel, sent, total)
                }
            }
        }
    }
}

impl SyncProgressSink for RecordingProgressSink {
    fn on_phase(&mut self, _phase: SyncPhase) {}

    fn on_file_start(
        &mut self,
        rel: &str,
        total: u64,
        op: &'static str,
        decision_policy: DeltaPolicy,
    ) {
        self.events.push(RecordedProgress::Start {
            rel: rel.to_string(),
            total,
            op,
            decision_policy,
        });
    }

    fn on_file_progress(&mut self, rel: &str, sent: u64, total: u64) {
        // Preserve the P0-08 upper bound even before the caller's own
        // throttling sink sees a worker's replayed events. A terminal update
        // is always retained so a completed file cannot look stalled.
        let now = Instant::now();
        let emit = sent >= total
            || self
                .last_progress
                .get(rel)
                .is_none_or(|last| now.duration_since(*last).as_millis() >= 100);
        if emit {
            self.last_progress.insert(rel.to_string(), now);
            self.events.push(RecordedProgress::Progress {
                rel: rel.to_string(),
                sent,
                total,
            });
        }
    }

    fn on_file_done(&mut self, _rel: &str, _outcome: &FileOutcome) {}
}

struct NormalWorkerCompletion {
    worker: Box<dyn StorageProvider>,
    job: TransferJob,
    outcome: FileOutcome,
    progress: RecordingProgressSink,
}

#[allow(clippy::too_many_arguments)]
async fn perform_sync_transfer(
    provider: &mut Box<dyn StorageProvider>,
    delta_batch: &mut Option<Box<dyn DeltaBatch>>,
    transfer: &PlannedTransfer,
    local_root: &str,
    remote_root: &str,
    download_segments: u32,
    requested_policy: DeltaPolicy,
    sink: &mut dyn SyncProgressSink,
    error_correction: &crate::sync::SyncErrorCorrectionOptions,
) -> FileOutcome {
    let spec = SyncTransferSpec {
        rel: &transfer.rel,
        total: transfer.total,
        decision_policy: transfer.decision_policy,
        requested_policy,
    };
    match transfer.op {
        "upload" => {
            perform_upload(
                provider,
                local_root,
                remote_root,
                spec,
                false,
                sink,
                delta_batch,
                error_correction,
            )
            .await
        }
        _ => {
            perform_download(
                provider,
                local_root,
                remote_root,
                spec,
                download_segments,
                false,
                sink,
                delta_batch,
                transfer.expected_sha256_hex.as_deref(),
                error_correction,
            )
            .await
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_normal_worker(
    workers: &mut VecDeque<Box<dyn StorageProvider>>,
    jobs: &mut JoinSet<NormalWorkerCompletion>,
    job: TransferJob,
    transfer: PlannedTransfer,
    local_root: String,
    remote_root: String,
    download_segments: u32,
    requested_policy: DeltaPolicy,
    error_correction: crate::sync::SyncErrorCorrectionOptions,
) {
    let mut worker = workers
        .pop_front()
        .expect("normal worker dispatch must have a clone lease");
    jobs.spawn(async move {
        let mut progress = RecordingProgressSink {
            events: Vec::new(),
            last_progress: HashMap::new(),
        };
        let mut no_delta_batch = None;
        let outcome = perform_sync_transfer(
            &mut worker,
            &mut no_delta_batch,
            &transfer,
            &local_root,
            &remote_root,
            download_segments,
            requested_policy,
            &mut progress,
            &error_correction,
        )
        .await;
        NormalWorkerCompletion {
            worker,
            job,
            outcome,
            progress,
        }
    });
}

/// Drain transfer jobs while retaining explicit session ownership.
///
/// Normal files receive one independent clone worker each and are bounded by
/// both the graph's file slots and the worker vector. Delta files never leave
/// this driver: they retain the primary provider plus its one `DeltaBatch`, so
/// no two delta operations can overlap. Report aggregation and calls to the
/// caller-owned progress sink remain here, after each worker returns.
#[allow(clippy::too_many_arguments)]
async fn drive_sync_transfers(
    mut job_rx: mpsc::Receiver<TransferJob>,
    provider: &mut Box<dyn StorageProvider>,
    delta_batch: &mut Option<Box<dyn DeltaBatch>>,
    report: &mut SyncReport,
    sink: &mut dyn SyncProgressSink,
    transfers: &[PlannedTransfer],
    local_root: &str,
    remote_root: &str,
    download_segments: u32,
    requested_policy: DeltaPolicy,
    error_correction: &crate::sync::SyncErrorCorrectionOptions,
    normal_workers: Vec<Box<dyn StorageProvider>>,
) {
    let mut workers = VecDeque::from(normal_workers);
    let mut normal_jobs = JoinSet::new();
    let mut pending_normals: VecDeque<TransferJob> = VecDeque::new();
    let mut channel_closed = false;

    loop {
        while !workers.is_empty() {
            let Some(job) = pending_normals.pop_front() else {
                break;
            };
            let transfer = transfers[job.transfer_index].clone();
            spawn_normal_worker(
                &mut workers,
                &mut normal_jobs,
                job,
                transfer,
                local_root.to_string(),
                remote_root.to_string(),
                download_segments,
                requested_policy,
                error_correction.clone(),
            );
        }

        if channel_closed && normal_jobs.is_empty() && pending_normals.is_empty() {
            break;
        }

        tokio::select! {
            joined = normal_jobs.join_next(), if !normal_jobs.is_empty() => {
                match joined.expect("normal worker set is non-empty") {
                    Ok(completion) => {
                        workers.push_back(completion.worker);
                        completion.progress.replay(sink);
                        let transfer = &transfers[completion.job.transfer_index];
                        apply_sync_tree_outcome(
                            report,
                            &transfer.rel,
                            transfer.op,
                            completion.outcome,
                            transfer.decision_policy,
                            sink,
                        );
                        let _ = completion.job.ack.send(());
                    }
                    Err(error) => {
                        // A provider worker panic is an executor fault, just
                        // like a panic in the legacy serial driver. Log it
                        // rather than fabricating a successful completion.
                        tracing::error!("sync normal worker task stopped: {}", error);
                    }
                }
            }
            received = job_rx.recv(), if !channel_closed => {
                match received {
                    Some(job) if matches!(job.lane, SyncTransferLane::NormalPool) && !workers.is_empty() => {
                        let transfer = transfers[job.transfer_index].clone();
                        spawn_normal_worker(
                            &mut workers,
                            &mut normal_jobs,
                            job,
                            transfer,
                            local_root.to_string(),
                            remote_root.to_string(),
                            download_segments,
                            requested_policy,
                            error_correction.clone(),
                        );
                    }
                    Some(job) if matches!(job.lane, SyncTransferLane::NormalPool) && normal_jobs.is_empty() => {
                        // No honest clone pool was available. Reuse the
                        // primary session exactly as the former serial driver.
                        let transfer = &transfers[job.transfer_index];
                        let outcome = perform_sync_transfer(
                            provider,
                            delta_batch,
                            transfer,
                            local_root,
                            remote_root,
                            download_segments,
                            requested_policy,
                            sink,
                            error_correction,
                        ).await;
                        apply_sync_tree_outcome(report, &transfer.rel, transfer.op, outcome, transfer.decision_policy, sink);
                        let _ = job.ack.send(());
                    }
                    Some(job) if matches!(job.lane, SyncTransferLane::NormalPool) => {
                        pending_normals.push_back(job);
                    }
                    Some(job) => {
                        // Delta retains the primary provider and the one
                        // session-backed batch. This branch is deliberately
                        // awaited in the driver, making the lane serial even
                        // when normal clone tasks are in flight.
                        let transfer = &transfers[job.transfer_index];
                        let outcome = perform_sync_transfer(
                            provider,
                            delta_batch,
                            transfer,
                            local_root,
                            remote_root,
                            download_segments,
                            requested_policy,
                            sink,
                            error_correction,
                        ).await;
                        apply_sync_tree_outcome(report, &transfer.rel, transfer.op, outcome, transfer.decision_policy, sink);
                        let _ = job.ack.send(());
                    }
                    None => channel_closed = true,
                }
            }
        }
    }
}

/// Run a full sync session through the graph engine.
///
/// DAG-P2-06: the single construction point for the sync whole-file AIMD
/// controller. It binds learned tuning to `endpoint` under the
/// [`crate::transfer_dag::AdaptiveWorkload::BatchSyncFile`] shape, the same
/// workload key batch uses, so both whole-file surfaces to one endpoint share a
/// learned safe target. The registry is a parameter so the wire-level test can
/// inject a fresh instance and assert the key this site records under.
fn sync_aimd_controller(
    budget: &crate::transfer_dag::TransferBudget,
    provider_type: crate::providers::ProviderType,
    endpoint: crate::transfer_dag::EndpointIdentity,
    registry: Arc<crate::transfer_dag::AdaptiveProfileRegistry>,
    config: AimdConfig,
) -> Arc<AimdController> {
    use crate::transfer_dag::{AdaptiveProfileKey, AdaptiveWorkload};
    Arc::new(AimdController::from_budget_for_profile(
        budget,
        Some(provider_type),
        AdaptiveProfileKey::new(endpoint, AdaptiveWorkload::BatchSyncFile),
        registry,
        config,
    ))
}

/// A drop-in replacement for [`crate::sync::sync_tree_core`]: same arguments,
/// same [`SyncReport`]. Reached for every non-dry-run sync; only a dry-run
/// sync diverts to the legacy interleaved path (kept exclusively for the
/// plan-output guarantee, see [`should_route_sync_to_dag`]).
pub async fn execute_sync_dag(
    provider: &mut Box<dyn StorageProvider>,
    local_root: &str,
    remote_root: &str,
    opts: &SyncOptions,
    sink: &mut dyn SyncProgressSink,
) -> SyncReport {
    let start = Instant::now();

    // Phase 1: Scanning. The local filesystem walk runs on a blocking thread
    // while the network-bound remote scan runs on the provider, so the two
    // overlap instead of running back to back.
    sink.on_phase(SyncPhase::Scanning);
    let scan = scan_options_for_sync(opts);
    let local_handle = {
        let root = local_root.to_string();
        let scan_opts = scan.clone();
        tokio::task::spawn_blocking(move || scan_local_tree_checked(&root, &scan_opts))
    };
    let (remotes, remote_scan) = scan_remote_tree_checked(provider, remote_root, &scan).await;
    // `scan_local_tree_checked` itself returns a `Vec` (best-effort, mirroring
    // `scan_remote_tree`), so the only way the join returns `Err` is a panic
    // inside the blocking thread. `.unwrap_or_default()` would silently turn
    // that panic into an empty local listing, which then drives a "no
    // uploads needed" decision through the planner: the run would report
    // success while the local side was never scanned. Surface the panic as
    // a hard `SyncError` on the report so the caller sees the failure;
    // keep `locals` empty so the remote-only side of the run still proceeds.
    // A panicked scan is treated as maximally incomplete so the orphan-delete
    // guard below refuses (CLAUDE-AV-B3-01).
    let (locals, local_scan, local_scan_panic) = match local_handle.await {
        Ok((entries, completeness)) => (entries, completeness, None),
        Err(join_err) => {
            eprintln!("[execute_sync_dag] local scan task failed: {}", join_err);
            (
                Vec::new(),
                ScanCompleteness {
                    list_errors: 1,
                    truncated: false,
                },
                Some(join_err.to_string()),
            )
        }
    };

    // Phase 2: Planning. Decisions read only the pre-transfer scan
    // snapshots, so resolving the whole plan up front is decision-equivalent
    // to the legacy interleaved decide/perform loop.
    sink.on_phase(SyncPhase::Planning);
    let mut report = SyncReport {
        dry_run: opts.dry_run,
        direction: Some(opts.direction),
        delta_policy: Some(opts.delta_policy),
        ..SyncReport::default()
    };
    if let Some(msg) = local_scan_panic {
        report.errors.push(SyncError {
            rel_path: local_root.to_string(),
            operation: "scan",
            message: format!("local scan task failed: {}", msg),
            decision_policy: opts.delta_policy,
        });
    }

    // CLAUDE-AV-B3-01: same orphan-delete guard as the legacy core. When the
    // source listing is untrustworthy (incomplete, or empty while the other
    // side is not), plan the run WITHOUT orphan deletes and record why, so a
    // transient listing failure cannot mirror a destructive delete here either.
    let dag_opts;
    let plan_opts = if opts.delete_orphans {
        match crate::sync::orphan_delete_guard(
            opts.direction,
            &locals,
            &remotes,
            &local_scan,
            &remote_scan,
        ) {
            Ok(()) => opts,
            Err(reason) => {
                tracing::warn!("sync.delete_orphans refused (dag): {}", reason);
                report.errors.push(SyncError {
                    rel_path: String::new(),
                    operation: "delete_skipped",
                    message: reason,
                    decision_policy: opts.delta_policy,
                });
                dag_opts = SyncOptions {
                    delete_orphans: false,
                    ..opts.clone()
                };
                &dag_opts
            }
        }
    } else {
        opts
    };
    let plan = plan_sync_dag(&locals, &remotes, plan_opts);

    // Phase 3: Executing.
    sink.on_phase(SyncPhase::Executing);

    // Mirror the legacy gate: create the remote root only for an
    // upload-bearing sync that actually has local files to push. Running
    // this after the scans (rather than before the remote scan, as the
    // legacy core does) is decision-equivalent: `scan_remote_tree` already
    // tolerates a missing directory by returning an empty listing.
    if !locals.is_empty() && matches!(opts.direction, SyncDirection::Upload | SyncDirection::Both) {
        ensure_remote_dir(provider, remote_root).await;
    }

    // Skips carry no I/O: emit them up front in plan order, before the
    // transfer graph runs.
    for skip in &plan.skips {
        sink.on_file_start(&skip.rel, 0, "skip", skip.decision_policy);
        apply_sync_tree_outcome(
            &mut report,
            &skip.rel,
            skip.apply_op,
            FileOutcome::Skipped {
                reason: skip.reason.clone(),
            },
            skip.decision_policy,
            sink,
        );
    }

    // Open the delta-sync batch once for the whole session: a single SSH
    // session reused across every eligible file, exactly like the legacy
    // core. `None` when delta policy is off or the provider is not
    // delta-eligible.
    let mut delta_batch: Option<Box<dyn DeltaBatch>> =
        if matches!(opts.delta_policy, DeltaPolicy::Delta) {
            crate::delta_sync_rsync::open_delta_batch(provider.as_mut()).await
        } else {
            None
        };

    // Snapshot the provider exactly once for this sync execution. Normal
    // whole-file work receives all of the independently cloned sessions the
    // provider honestly advertises; a delta request retains the primary
    // session and intentionally demotes the normal pool to one serial lane.
    let (sync_caps, normal_workers) =
        prepare_normal_file_workers(provider, opts.delta_policy).await;

    if !plan.transfers.is_empty() {
        let endpoint = provider.endpoint_identity();
        // A tree sync is background work. It may upload and download in one
        // plan, so it holds one directional lease for each local-root side.
        // The device registry coalesces both requests when they resolve to the
        // same physical disk.
        let _job_lease = crate::transfer_dag::governor::global()
            .acquire_job(
                endpoint.clone(),
                TransferPriority::Background,
                [
                    DiskLeaseRequest::read(local_root),
                    DiskLeaseRequest::write(local_root),
                ],
            )
            .await;
        // DAG-P2-04: the transfer set is no longer materialized as one static
        // `from_sync_plan_shaped` graph of N chains. The precomputed plan (a
        // plan iterator, decision-equivalent as before) is streamed through a
        // bounded backlog; each transfer's shaped subgraph is expanded only when
        // it is admitted into the bounded active set (`run_one_sync_file`) and
        // dropped when it completes, so peak resident nodes stay
        // O(active_file_cap * nodes_per_file), not O(total_transfers). The real
        // per-file I/O, the bounded worker pool, the exclusive delta lane, and
        // out-of-order report aggregation all remain in `drive_sync_transfers`.
        let transfers = Arc::new(plan.transfers);

        // The manager is the existing bounded DAG resource model. Its file cap
        // equals the live clone ceiling, or one after an honest demotion. Delta
        // work is additionally serialized by the ownership driver. DAG-P2-01:
        // the buffer-byte pool is the process-global governor's, shared with
        // every concurrent job; the slot classes stay per-job.
        let manager = crate::transfer_dag::governor::global().child_manager(
            TransferBudget::from_file_slots(sync_caps.max_file_slots.unwrap_or(1))
                .clamped_for_capabilities(&sync_caps),
        );
        let provider_type = provider.provider_type();
        // AIMD backpressure (F3-T05) uses the same bounded file resource as the
        // normal clone lane. A serial demotion retains the conservative one-slot
        // behavior. DAG-P2-06: seed from / learn into this endpoint's whole-file
        // profile, shared with batch under the same workload key.
        let aimd = sync_aimd_controller(
            &manager.budget(),
            provider_type,
            endpoint,
            crate::transfer_dag::global_profile_registry(),
            AimdConfig::runtime(),
        );

        // All transfers share one lane decision (delta vs normal pool) because
        // they share the requested delta policy.
        let lane = transfer_lane(opts.delta_policy);
        // Same production policy as batch (`StreamingConfig::for_file_slots`),
        // with backlog from SyncOptions (CLI --max-backlog when the caller sets
        // it; default DEFAULT_ENGINE_MAX_BACKLOG).
        let file_slots = sync_caps.max_file_slots.unwrap_or(1).max(1) as usize;
        let streaming_config =
            StreamingConfig::for_file_slots(file_slots, opts.max_backlog);
        let ctx = Arc::new(SyncStreamContext {
            caps: sync_caps,
            manager,
            aimd,
            lane,
        });

        // Channel must not be narrower than the active set: otherwise admitted
        // files block on send while holding a materialized subgraph.
        let job_channel_cap = JOB_CHANNEL_CAPACITY.max(streaming_config.active_file_cap);
        let (job_tx, job_rx) = mpsc::channel::<TransferJob>(job_channel_cap);

        // The streaming frontier (which expands and schedules per-file
        // subgraphs) and the real I/O driver (borrowing the provider) run
        // concurrently on this task. Each admitted subgraph hands its transfer
        // to the driver over a `job_tx` clone; when the frontier finishes, every
        // clone is dropped, the channel closes, and the driver loop ends.
        let source = SyncTransfersWorkSource {
            transfers: Arc::clone(&transfers),
            next: 0,
        };
        let stream_tx = job_tx.clone();
        // Drop the original handle so only the frontier's clones keep the
        // channel open; the driver sees close once the frontier is done.
        drop(job_tx);
        let stream_future = async move {
            crate::transfer_dag::run_streaming(source, streaming_config, move |item, admission| {
                let ctx = Arc::clone(&ctx);
                let job_tx = stream_tx.clone();
                async move {
                    run_one_sync_file(item, admission, ctx, job_tx).await;
                }
            })
            .await
        };
        let driver_future = drive_sync_transfers(
            job_rx,
            provider,
            &mut delta_batch,
            &mut report,
            sink,
            transfers.as_slice(),
            local_root,
            remote_root,
            opts.download_segments,
            opts.delta_policy,
            &opts.error_correction,
            normal_workers,
        );
        let (_summary, ()) = tokio::join!(stream_future, driver_future);
    }

    // Orphan deletion runs after every transfer, while the delta batch is
    // still open, exactly like the legacy core.
    for delete in &plan.deletes {
        let outcome = match delete.op {
            "delete_remote" => {
                perform_remote_delete(
                    provider,
                    remote_root,
                    &delete.rel,
                    opts.delta_policy,
                    false,
                    sink,
                    &opts.error_correction,
                )
                .await
            }
            _ => perform_local_delete(local_root, &delete.rel, opts.delta_policy, false, sink),
        };
        apply_sync_tree_outcome(
            &mut report,
            &delete.rel,
            delete.op,
            outcome,
            opts.delta_policy,
            sink,
        );
    }

    // Finalize the delta batch and surface the session-reuse metrics,
    // identical to the legacy core.
    if let Some(batch) = delta_batch.take() {
        match batch.finalize().await {
            Ok(stats) => {
                tracing::info!(
                    "sync.delta: batch finalize ok: files_transferred={}, session_count={}, bytes_on_wire={}, partial={}",
                    stats.files_transferred,
                    stats.session_count,
                    stats.bytes_on_wire,
                    stats.partial,
                );
                report.delta_session_count = Some(stats.session_count);
                report.delta_bytes_on_wire = Some(stats.bytes_on_wire);
                report.delta_batch_files = Some(stats.files_transferred);
            }
            Err(e) => {
                tracing::warn!("sync.delta: batch finalize failed: {e}");
            }
        }
    }

    report.elapsed_secs = start.elapsed().as_secs_f64();
    sink.on_phase(SyncPhase::Done);
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{ProviderError, ProviderType, RemoteEntry as ProviderRemoteEntry};
    use crate::sync::{
        CompareDirection, JournalEntryStatus, RetryPolicy, SyncJournal, SyncJournalEntry,
        VerifyPolicy,
    };
    use crate::transfer_dag::observer::SyncJournalDagObserver;
    use crate::transfer_dag::TransferResourceManager;
    use crate::transfer_dag::{
        Capability, NoopDagObserver, OrderedDagObserver, SyncDagAction, SyncDagItem,
    };
    use std::any::Any;

    // DAG-P2-06 (wire-level): the sync construction point binds learned tuning
    // to its endpoint under the whole-file BatchSyncFile workload key (shared
    // with batch so both whole-file surfaces to one endpoint learn together).
    #[test]
    fn sync_aimd_controller_records_under_the_batch_sync_key() {
        use crate::transfer_dag::{
            AdaptiveClass, AdaptiveProfileConfig, AdaptiveProfileKey, AdaptiveProfileRegistry,
            AdaptiveWorkload, EndpointIdentity, TransferBudget,
        };
        let registry = Arc::new(AdaptiveProfileRegistry::new(
            AdaptiveProfileConfig::default(),
        ));
        let endpoint = EndpointIdentity::new("sftp", "wire-test-host", "wire-acct");
        let budget = TransferBudget::from_file_slots(8);
        let ctrl = sync_aimd_controller(
            &budget,
            ProviderType::S3,
            endpoint.clone(),
            registry.clone(),
            AimdConfig::default(),
        );
        assert_eq!(
            ctrl.profile_key().map(|k| k.workload),
            Some(AdaptiveWorkload::BatchSyncFile)
        );
        ctrl.on_congestion(AdaptiveClass::File);
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(
            snapshot[0].key,
            AdaptiveProfileKey::new(endpoint, AdaptiveWorkload::BatchSyncFile)
        );
        assert_eq!(snapshot[0].file, Some(4));
    }
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;
    use tokio::sync::Barrier;

    #[derive(Default)]
    struct TestSyncSink {
        starts: Vec<String>,
        done: Vec<String>,
    }

    impl SyncProgressSink for TestSyncSink {
        fn on_phase(&mut self, _phase: SyncPhase) {}

        fn on_file_start(
            &mut self,
            rel: &str,
            _total: u64,
            _op: &'static str,
            _decision_policy: DeltaPolicy,
        ) {
            self.starts.push(rel.to_string());
        }

        fn on_file_progress(&mut self, _rel: &str, _sent: u64, _total: u64) {}

        fn on_file_done(&mut self, rel: &str, _outcome: &FileOutcome) {
            self.done.push(rel.to_string());
        }
    }

    struct BarrierProviderState {
        barrier: Arc<Barrier>,
        active: AtomicUsize,
        peak: AtomicUsize,
        clone_allowed: bool,
    }

    struct BarrierProvider {
        state: Arc<BarrierProviderState>,
    }

    impl BarrierProvider {
        fn new(barrier_parties: usize, clone_allowed: bool) -> (Self, Arc<BarrierProviderState>) {
            let state = Arc::new(BarrierProviderState {
                barrier: Arc::new(Barrier::new(barrier_parties)),
                active: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
                clone_allowed,
            });
            (
                Self {
                    state: Arc::clone(&state),
                },
                state,
            )
        }

        async fn enter_transfer(&self) {
            let active = self.state.active.fetch_add(1, Ordering::SeqCst) + 1;
            let mut observed = self.state.peak.load(Ordering::SeqCst);
            while active > observed {
                match self.state.peak.compare_exchange_weak(
                    observed,
                    active,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => break,
                    Err(next) => observed = next,
                }
            }
            self.state.barrier.wait().await;
            self.state.active.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl StorageProvider for BarrierProvider {
        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }

        fn provider_type(&self) -> ProviderType {
            ProviderType::Ftp
        }

        fn display_name(&self) -> String {
            "barrier-sync".to_string()
        }

        async fn connect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn disconnect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        fn is_connected(&self) -> bool {
            true
        }
        async fn list(&mut self, _path: &str) -> Result<Vec<ProviderRemoteEntry>, ProviderError> {
            Ok(Vec::new())
        }
        async fn pwd(&mut self) -> Result<String, ProviderError> {
            Ok("/".to_string())
        }
        async fn cd(&mut self, _path: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn cd_up(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn download(
            &mut self,
            _remote: &str,
            _local: &str,
            _progress: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            self.enter_transfer().await;
            Ok(())
        }
        async fn download_to_bytes(&mut self, _remote: &str) -> Result<Vec<u8>, ProviderError> {
            Ok(Vec::new())
        }
        async fn upload(
            &mut self,
            _local: &str,
            _remote: &str,
            _progress: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            self.enter_transfer().await;
            Ok(())
        }
        async fn mkdir(&mut self, _path: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn delete(&mut self, _path: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rmdir(&mut self, _path: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rmdir_recursive(&mut self, _path: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rename(&mut self, _from: &str, _to: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn stat(&mut self, _path: &str) -> Result<ProviderRemoteEntry, ProviderError> {
            Err(ProviderError::NotSupported("stat".to_string()))
        }
        async fn size(&mut self, _path: &str) -> Result<u64, ProviderError> {
            Ok(1)
        }
        async fn exists(&mut self, _path: &str) -> Result<bool, ProviderError> {
            Ok(true)
        }
        async fn keep_alive(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn server_info(&mut self) -> Result<String, ProviderError> {
            Ok("barrier".to_string())
        }

        fn transfer_capabilities(&self) -> TransferCapabilities {
            TransferCapabilities {
                file_parallel: Capability::Supported,
                session_pool: Capability::Supported,
                max_file_slots: Some(2),
                ..TransferCapabilities::default()
            }
        }

        fn transfer_executor_kind(&self) -> ProviderTransferExecutorKind {
            ProviderTransferExecutorKind::FtpConnectionPool
        }

        fn transfer_executor_max_sessions(&self) -> u16 {
            2
        }

        fn clone_for_transfer(&self) -> Result<Box<dyn StorageProvider>, ProviderError> {
            if self.state.clone_allowed {
                Ok(Box::new(Self {
                    state: Arc::clone(&self.state),
                }))
            } else {
                Err(ProviderError::NotSupported("clone".to_string()))
            }
        }
    }

    // ---- dry-run gate ----------------------------------------------------

    /// The dry-run gate is the only thing that diverts a sync away from the
    /// DAG runner. Every other call must route through it.
    #[test]
    fn dry_run_is_the_only_exit_from_the_dag_runner() {
        assert!(should_route_sync_to_dag(false));
        assert!(
            !should_route_sync_to_dag(true),
            "dry-run must never route to the mutating graph"
        );
    }

    // ---- plan_sync_dag ----------------------------------------------------

    fn local(rel: &str, size: u64) -> LocalEntry {
        LocalEntry {
            rel_path: rel.to_string(),
            size,
            mtime: None,
            sha256: None,
        }
    }

    fn remote(rel: &str, size: u64) -> RemoteEntry {
        RemoteEntry {
            rel_path: rel.to_string(),
            size,
            mtime: None,
            checksum_alg: None,
            checksum_hex: None,
        }
    }

    fn opts(direction: SyncDirection) -> SyncOptions {
        SyncOptions {
            direction,
            delta_policy: DeltaPolicy::SizeOnly,
            dry_run: false,
            delete_orphans: false,
            conflict_mode: crate::sync::ConflictMode::Newer,
            scan: Default::default(),
            error_correction: Default::default(),
            download_segments: 1,
            max_backlog: crate::transfer_dag::DEFAULT_ENGINE_MAX_BACKLOG,
        }
    }

    fn planned_upload(rel: &str, policy: DeltaPolicy) -> PlannedTransfer {
        PlannedTransfer {
            rel: rel.to_string(),
            op: "upload",
            total: 1,
            decision_policy: policy,
            expected_sha256_hex: None,
        }
    }

    #[tokio::test]
    async fn normal_clone_workers_are_barrier_parallel_and_report_out_of_order_safely() {
        let (primary, state) = BarrierProvider::new(2, true);
        let mut primary: Box<dyn StorageProvider> = Box::new(primary);
        let (caps, workers) =
            prepare_normal_file_workers(&mut primary, DeltaPolicy::SizeOnly).await;
        assert_eq!(caps.max_file_slots, Some(2));
        assert_eq!(workers.len(), 2);

        let transfers = vec![
            planned_upload("first.bin", DeltaPolicy::SizeOnly),
            planned_upload("second.bin", DeltaPolicy::SizeOnly),
        ];
        let (tx, rx) = mpsc::channel(JOB_CHANNEL_CAPACITY);
        let (first_ack_tx, first_ack_rx) = oneshot::channel();
        let (second_ack_tx, second_ack_rx) = oneshot::channel();
        tx.send(TransferJob {
            transfer_index: 0,
            lane: SyncTransferLane::NormalPool,
            ack: first_ack_tx,
        })
        .await
        .unwrap();
        tx.send(TransferJob {
            transfer_index: 1,
            lane: SyncTransferLane::NormalPool,
            ack: second_ack_tx,
        })
        .await
        .unwrap();
        drop(tx);

        let mut delta_batch = None;
        let mut report = SyncReport::default();
        let mut sink = TestSyncSink::default();
        tokio::time::timeout(
            Duration::from_secs(1),
            drive_sync_transfers(
                rx,
                &mut primary,
                &mut delta_batch,
                &mut report,
                &mut sink,
                &transfers,
                "/local",
                "/remote",
                1,
                DeltaPolicy::SizeOnly,
                &Default::default(),
                workers,
            ),
        )
        .await
        .expect("two clone workers must meet the barrier");

        assert!(first_ack_rx.await.is_ok());
        assert!(second_ack_rx.await.is_ok());
        assert_eq!(state.peak.load(Ordering::SeqCst), 2);
        assert_eq!(report.uploaded, 2);
        assert_eq!(report.error_count(), 0);
        assert_eq!(sink.starts.len(), 2);
        assert_eq!(sink.done.len(), 2);
        assert!(sink.starts.iter().all(|rel| sink.done.contains(rel)));
    }

    #[tokio::test]
    async fn delta_lane_is_serial_and_clone_failure_demotes_to_primary() {
        let (primary, state) = BarrierProvider::new(1, false);
        let mut primary: Box<dyn StorageProvider> = Box::new(primary);
        let (caps, workers) =
            prepare_normal_file_workers(&mut primary, DeltaPolicy::SizeOnly).await;
        assert_eq!(caps.max_file_slots, Some(1));
        assert!(workers.is_empty());

        let transfers = vec![
            planned_upload("delta-one.bin", DeltaPolicy::Delta),
            planned_upload("delta-two.bin", DeltaPolicy::Delta),
        ];
        let (tx, rx) = mpsc::channel(JOB_CHANNEL_CAPACITY);
        let (first_ack_tx, first_ack_rx) = oneshot::channel();
        let (second_ack_tx, second_ack_rx) = oneshot::channel();
        tx.send(TransferJob {
            transfer_index: 0,
            lane: SyncTransferLane::DeltaExclusive,
            ack: first_ack_tx,
        })
        .await
        .unwrap();
        tx.send(TransferJob {
            transfer_index: 1,
            lane: SyncTransferLane::DeltaExclusive,
            ack: second_ack_tx,
        })
        .await
        .unwrap();
        drop(tx);

        let mut delta_batch = None;
        let mut report = SyncReport::default();
        let mut sink = TestSyncSink::default();
        drive_sync_transfers(
            rx,
            &mut primary,
            &mut delta_batch,
            &mut report,
            &mut sink,
            &transfers,
            "/local",
            "/remote",
            1,
            DeltaPolicy::Delta,
            &Default::default(),
            Vec::new(),
        )
        .await;

        assert!(first_ack_rx.await.is_ok());
        assert!(second_ack_rx.await.is_ok());
        assert_eq!(state.peak.load(Ordering::SeqCst), 1);
        assert_eq!(report.uploaded, 2);
        assert_eq!(report.error_count(), 0);
        assert_eq!(
            transfer_lane(DeltaPolicy::Delta),
            SyncTransferLane::DeltaExclusive
        );
        assert_eq!(
            transfer_lane(DeltaPolicy::Mtime),
            SyncTransferLane::NormalPool
        );
    }

    #[test]
    fn plan_uploads_new_file_and_skips_identical() {
        let locals = vec![local("new.txt", 10), local("same.txt", 20)];
        let remotes = vec![remote("same.txt", 20)];
        let plan = plan_sync_dag(&locals, &remotes, &opts(SyncDirection::Upload));

        assert_eq!(plan.transfers.len(), 1);
        assert_eq!(plan.transfers[0].rel, "new.txt");
        assert_eq!(plan.transfers[0].op, "upload");
        assert_eq!(plan.transfers[0].total, 10);
        assert_eq!(plan.skips.len(), 1);
        assert_eq!(plan.skips[0].rel, "same.txt");
        assert_eq!(plan.skips[0].apply_op, "upload");
        assert!(plan.deletes.is_empty());
    }

    #[test]
    fn plan_downloads_new_remote_file_only() {
        let locals = vec![local("here.txt", 5)];
        let remotes = vec![remote("here.txt", 5), remote("only-remote.txt", 99)];
        let plan = plan_sync_dag(&locals, &remotes, &opts(SyncDirection::Download));

        assert_eq!(plan.transfers.len(), 1);
        assert_eq!(plan.transfers[0].rel, "only-remote.txt");
        assert_eq!(plan.transfers[0].op, "download");
        // The upload pass never runs in download direction.
        assert!(plan.transfers.iter().all(|t| t.op == "download"));
    }

    #[test]
    fn plan_both_direction_resolves_conflict_via_upload_then_skips_download() {
        // A file on both sides with different sizes: the upload pass resolves
        // it (ConflictMode::Newer always copies), and the download pass must
        // then skip it as already handled.
        let locals = vec![local("conflict.txt", 100)];
        let remotes = vec![remote("conflict.txt", 50)];
        let plan = plan_sync_dag(&locals, &remotes, &opts(SyncDirection::Both));

        assert_eq!(plan.transfers.len(), 1);
        assert_eq!(plan.transfers[0].op, "upload");
        assert_eq!(plan.transfers[0].rel, "conflict.txt");
        assert_eq!(plan.skips.len(), 1);
        assert_eq!(plan.skips[0].apply_op, "download");
        assert_eq!(plan.skips[0].rel, "conflict.txt");
    }

    #[test]
    fn plan_keeps_uploads_before_downloads_in_plan_order() {
        let locals = vec![local("up-only.txt", 1)];
        let remotes = vec![remote("down-only.txt", 2)];
        let plan = plan_sync_dag(&locals, &remotes, &opts(SyncDirection::Both));

        assert_eq!(plan.transfers.len(), 2);
        assert_eq!(plan.transfers[0].op, "upload");
        assert_eq!(plan.transfers[1].op, "download");
    }

    #[test]
    fn plan_delete_orphans_marks_the_right_side_per_direction() {
        let locals = vec![local("keep.txt", 1), local("local-orphan.txt", 1)];
        let remotes = vec![remote("keep.txt", 1), remote("remote-orphan.txt", 1)];

        let mut upload = opts(SyncDirection::Upload);
        upload.delete_orphans = true;
        let plan = plan_sync_dag(&locals, &remotes, &upload);
        assert_eq!(plan.deletes.len(), 1);
        assert_eq!(plan.deletes[0].rel, "remote-orphan.txt");
        assert_eq!(plan.deletes[0].op, "delete_remote");

        let mut download = opts(SyncDirection::Download);
        download.delete_orphans = true;
        let plan = plan_sync_dag(&locals, &remotes, &download);
        assert_eq!(plan.deletes.len(), 1);
        assert_eq!(plan.deletes[0].rel, "local-orphan.txt");
        assert_eq!(plan.deletes[0].op, "delete_local");

        // Bidirectional sync resolves orphans as transfers, never deletes.
        let mut both = opts(SyncDirection::Both);
        both.delete_orphans = true;
        let plan = plan_sync_dag(&locals, &remotes, &both);
        assert!(plan.deletes.is_empty());
    }

    #[test]
    fn plan_deduplicates_repeated_scan_entries() {
        let locals = vec![local("dup.txt", 7), local("dup.txt", 7)];
        let plan = plan_sync_dag(&locals, &[], &opts(SyncDirection::Upload));
        assert_eq!(
            plan.transfers.len(),
            1,
            "a repeated scan path is planned once"
        );
    }

    // ---- runner + driver mechanics ---------------------------------------

    #[tokio::test]
    async fn sync_runner_drives_every_transfer_exactly_once() {
        let items = vec![
            SyncDagItem::new("a.txt", SyncDagAction::Upload),
            SyncDagItem::new("b.txt", SyncDagAction::Download),
            SyncDagItem::new("c.txt", SyncDagAction::Upload),
        ];
        let sync_dag = TransferDagBuilder::from_sync_plan(&items);
        let node_to_index: Arc<HashMap<usize, usize>> = Arc::new(
            sync_dag
                .files
                .iter()
                .enumerate()
                .map(|(index, file)| (file.transfer, index))
                .collect(),
        );

        let (job_tx, mut job_rx) = mpsc::channel::<TransferJob>(JOB_CHANNEL_CAPACITY);
        let transfer_dispatched = Arc::new(StdMutex::new(vec![false; sync_dag.files.len()]));
        let runner = build_sync_runner(
            node_to_index,
            transfer_dispatched,
            job_tx,
            Arc::new(vec![SyncTransferLane::NormalPool; sync_dag.files.len()]),
        );
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(1));
        let observer: Arc<dyn DagObserver> = Arc::new(NoopDagObserver);

        let seen: Arc<StdMutex<Vec<usize>>> = Arc::new(StdMutex::new(Vec::new()));
        let driver = {
            let seen = Arc::clone(&seen);
            async move {
                while let Some(job) = job_rx.recv().await {
                    seen.lock().unwrap().push(job.transfer_index);
                    let _ = job.ack.send(());
                }
            }
        };
        let dag_future = execute_dag(&sync_dag.dag, &manager, runner, observer, None);
        let (dag_result, ()) = tokio::join!(dag_future, driver);

        let summary = dag_result.expect("sync graph must schedule cleanly");
        // 3 prefix nodes + 3 files x 6 chain nodes.
        assert_eq!(summary.nodes_completed, 21);
        let mut seen = seen.lock().unwrap().clone();
        seen.sort_unstable();
        assert_eq!(seen, vec![0, 1, 2], "every transfer is driven exactly once");
    }

    #[tokio::test]
    async fn sync_runner_handles_an_empty_plan() {
        let sync_dag = TransferDagBuilder::from_sync_plan(&[]);
        let node_to_index: Arc<HashMap<usize, usize>> = Arc::new(HashMap::new());
        let (job_tx, mut job_rx) = mpsc::channel::<TransferJob>(JOB_CHANNEL_CAPACITY);
        let transfer_dispatched = Arc::new(StdMutex::new(Vec::new()));
        let runner = build_sync_runner(
            node_to_index,
            transfer_dispatched,
            job_tx,
            Arc::new(Vec::new()),
        );
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(1));
        let observer: Arc<dyn DagObserver> = Arc::new(NoopDagObserver);

        let driver = async move {
            let mut jobs = 0usize;
            while job_rx.recv().await.is_some() {
                jobs += 1;
            }
            jobs
        };
        let dag_future = execute_dag(&sync_dag.dag, &manager, runner, observer, None);
        let (dag_result, jobs) = tokio::join!(dag_future, driver);

        assert_eq!(jobs, 0, "an empty sync plan hands off no transfer jobs");
        // Only the 3 global discover/compare prefix nodes.
        assert_eq!(dag_result.unwrap().nodes_completed, 3);
    }

    // ---- journal observer interop (F2-T10) -------------------------------

    fn journal_with_pending(paths: &[(&str, &str)]) -> SyncJournal {
        let mut journal = SyncJournal::new(
            "/local".to_string(),
            "/remote".to_string(),
            CompareDirection::Bidirectional,
            RetryPolicy::default(),
            VerifyPolicy::SizeOnly,
        );
        for (rel, action) in paths {
            journal.entries.push(SyncJournalEntry {
                relative_path: rel.to_string(),
                action: action.to_string(),
                status: JournalEntryStatus::Pending,
                attempts: 1,
                last_error: None,
                verified: None,
                bytes_transferred: 0,
                ec_status: None,
            });
        }
        journal
    }

    #[tokio::test]
    async fn sync_journal_observer_completes_entries_over_the_sync_graph() {
        // The sync graph's per-file EmitProgress terminals must drive the
        // legacy journal to completion, in plan order, so a journal written
        // by the DAG path is interoperable with the legacy resume logic.
        let items = vec![
            SyncDagItem::new("up.txt", SyncDagAction::Upload),
            SyncDagItem::new("down.txt", SyncDagAction::Download),
        ];
        let sync_dag = TransferDagBuilder::from_sync_plan(&items);
        let terminals = sync_dag.journal_terminals_for_plan_order();
        assert_eq!(terminals.len(), 2);

        let journal = Arc::new(std::sync::Mutex::new(journal_with_pending(&[
            ("up.txt", "upload"),
            ("down.txt", "download"),
        ])));
        let persisted = Arc::new(std::sync::Mutex::new(0u32));
        let persist = {
            let persisted = Arc::clone(&persisted);
            Arc::new(move |_journal: &SyncJournal| {
                *persisted.lock().unwrap() += 1;
                Ok(())
            })
        };
        let journal_observer =
            SyncJournalDagObserver::with_persist(Arc::clone(&journal), terminals, persist);
        // "Journal durable first, visible completion second": the journal
        // observer is composed ahead of any visible observer.
        let observer: Arc<dyn DagObserver> = Arc::new(OrderedDagObserver::new(vec![
            Arc::new(journal_observer) as Arc<dyn DagObserver>,
            Arc::new(NoopDagObserver) as Arc<dyn DagObserver>,
        ]));

        let runner: Arc<dyn DagNodeRunner> = Arc::new(|_node: TransferNode| -> NodeFuture {
            Box::pin(async { NodeOutcome::Completed })
        });
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(1));
        execute_dag(&sync_dag.dag, &manager, runner, observer, None)
            .await
            .expect("sync graph must schedule cleanly");

        let journal = journal.lock().unwrap();
        assert!(
            journal
                .entries
                .iter()
                .all(|entry| entry.status == JournalEntryStatus::Completed),
            "every journal entry must be marked completed"
        );
        assert!(
            journal.completed,
            "a fully transferred journal is completed"
        );
        assert!(
            !journal.has_resumable_entries(),
            "a completed journal has nothing to resume"
        );
        assert!(*persisted.lock().unwrap() >= 1, "the journal was persisted");
    }

    #[test]
    fn partial_journal_stays_resumable_for_the_legacy_path() {
        // A journal interrupted mid-run (flag ON) keeps the standard
        // Pending/Completed split, so the legacy path (flag OFF) resumes it.
        let mut journal = journal_with_pending(&[("a.txt", "upload"), ("b.txt", "upload")]);
        journal.entries[0].status = JournalEntryStatus::Completed;
        assert!(
            journal.has_resumable_entries(),
            "a half-finished journal is resumable regardless of which engine wrote it"
        );
    }

    // ---- graph build cost (F2-T12) ---------------------------------------

    #[test]
    fn sync_graph_build_cost_is_well_under_50ms() {
        // The watch loop rebuilds the sync graph every cycle; the build must
        // stay far below the filesystem-watcher cooldown.
        let items: Vec<SyncDagItem> = (0..1000)
            .map(|i| SyncDagItem::new(format!("file-{i}.bin"), SyncDagAction::Upload))
            .collect();
        let start = Instant::now();
        let sync_dag = TransferDagBuilder::from_sync_plan(&items);
        let elapsed = start.elapsed();

        assert_eq!(sync_dag.files.len(), 1000);
        assert!(
            elapsed < Duration::from_millis(50),
            "1000-file sync graph build took {elapsed:?}, budget is 50ms"
        );
    }

    // ---- DAG-P2-04: streaming sync frontier ------------------------------

    fn sync_stream_ctx() -> Arc<SyncStreamContext> {
        use crate::transfer_dag::{
            AdaptiveProfileConfig, AdaptiveProfileRegistry, EndpointIdentity,
        };
        let registry = Arc::new(AdaptiveProfileRegistry::new(
            AdaptiveProfileConfig::default(),
        ));
        let budget = TransferBudget::from_file_slots(2);
        let manager = TransferResourceManager::new(budget);
        let aimd = sync_aimd_controller(
            &budget,
            ProviderType::Ftp,
            EndpointIdentity::new("ftp", "stream-host", "acct"),
            registry,
            AimdConfig::default(),
        );
        Arc::new(SyncStreamContext {
            caps: TransferCapabilities::default(),
            manager,
            aimd,
            lane: SyncTransferLane::NormalPool,
        })
    }

    /// The streaming sync frontier hands every planned transfer to the driver
    /// exactly once, without materializing a full static graph of N chains.
    #[tokio::test]
    async fn streaming_sync_drives_every_transfer_exactly_once() {
        let transfers = Arc::new(vec![
            planned_upload("a.txt", DeltaPolicy::SizeOnly),
            PlannedTransfer {
                rel: "b.txt".to_string(),
                op: "download",
                total: 1,
                decision_policy: DeltaPolicy::SizeOnly,
                expected_sha256_hex: None,
            },
            planned_upload("c.txt", DeltaPolicy::SizeOnly),
        ]);
        let ctx = sync_stream_ctx();

        let (job_tx, mut job_rx) = mpsc::channel::<TransferJob>(JOB_CHANNEL_CAPACITY);
        let seen: Arc<StdMutex<Vec<usize>>> = Arc::new(StdMutex::new(Vec::new()));
        let driver = {
            let seen = Arc::clone(&seen);
            async move {
                while let Some(job) = job_rx.recv().await {
                    seen.lock().unwrap().push(job.transfer_index);
                    let _ = job.ack.send(());
                }
            }
        };

        let source = SyncTransfersWorkSource {
            transfers: Arc::clone(&transfers),
            next: 0,
        };
        let stream_tx = job_tx.clone();
        drop(job_tx);
        let stream_future = async move {
            crate::transfer_dag::run_streaming(
                source,
                StreamingConfig {
                    backlog_cap: 8,
                    active_file_cap: 2,
                },
                move |item, admission| {
                    let ctx = Arc::clone(&ctx);
                    let job_tx = stream_tx.clone();
                    async move {
                        run_one_sync_file(item, admission, ctx, job_tx).await;
                    }
                },
            )
            .await
        };
        let (summary, ()) = tokio::join!(stream_future, driver);

        let mut seen = seen.lock().unwrap().clone();
        seen.sort_unstable();
        assert_eq!(seen, vec![0, 1, 2], "every transfer is driven exactly once");
        assert_eq!(summary.items_admitted, 3);
        // Streaming, not a full static graph: peak nodes stay O(active x template).
        assert!(
            summary.peak_active_nodes <= 2 * 7,
            "peak nodes {} exceeded the active-set bound",
            summary.peak_active_nodes
        );
    }

    /// A streaming sync of far more transfers than the active window still hands
    /// off every one, proving admission/retirement re-uses the bounded frontier.
    #[tokio::test]
    async fn streaming_sync_handles_more_transfers_than_the_active_window() {
        let transfers = Arc::new(
            (0..200)
                .map(|i| planned_upload(&format!("f{i}.txt"), DeltaPolicy::SizeOnly))
                .collect::<Vec<_>>(),
        );
        let ctx = sync_stream_ctx();

        let (job_tx, mut job_rx) = mpsc::channel::<TransferJob>(JOB_CHANNEL_CAPACITY);
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let driver = {
            let count = Arc::clone(&count);
            async move {
                while let Some(job) = job_rx.recv().await {
                    count.fetch_add(1, Ordering::SeqCst);
                    let _ = job.ack.send(());
                }
            }
        };

        let source = SyncTransfersWorkSource {
            transfers: Arc::clone(&transfers),
            next: 0,
        };
        let stream_tx = job_tx.clone();
        drop(job_tx);
        let stream_future = async move {
            crate::transfer_dag::run_streaming(
                source,
                StreamingConfig {
                    backlog_cap: 16,
                    active_file_cap: 8,
                },
                move |item, admission| {
                    let ctx = Arc::clone(&ctx);
                    let job_tx = stream_tx.clone();
                    async move {
                        run_one_sync_file(item, admission, ctx, job_tx).await;
                    }
                },
            )
            .await
        };
        let (summary, ()) = tokio::join!(stream_future, driver);

        assert_eq!(count.load(Ordering::SeqCst), 200);
        assert_eq!(summary.items_admitted, 200);
        assert!(summary.peak_active_nodes <= 8 * 7);
    }

    /// SyncOptions.max_backlog maps into the streaming frontier (residual
    /// close-out of DAG-P2-04: same knob as batch / CLI --max-backlog).
    #[test]
    fn sync_streaming_config_honors_max_backlog() {
        let mut o = opts(SyncDirection::Upload);
        o.max_backlog = 2_500;
        let cfg = StreamingConfig::for_file_slots(4, o.max_backlog);
        assert_eq!(cfg.backlog_cap, 2_500);
        assert_eq!(cfg.active_file_cap, 6);

        o.max_backlog = 50_000;
        let cfg = StreamingConfig::for_file_slots(1, o.max_backlog);
        assert_eq!(cfg.backlog_cap, 50_000);
        assert_eq!(cfg.active_file_cap, 3);
    }
}
