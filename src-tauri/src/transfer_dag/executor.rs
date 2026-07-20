// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Ready-frontier executor for the transfer node graph.
//!
//! This walks a [`TransferDag`](super::graph::TransferDag): it keeps a
//! completed set, a ready queue of nodes whose `depends_on` are all
//! satisfied, and dispatches them concurrently up to a bounded
//! [`DEFAULT_DISPATCH_WINDOW`] (or an explicit window). Each dispatched
//! node first acquires its `ResourceRequest` permits from the shared
//! [`TransferResourceManager`], so I/O concurrency is bounded by the
//! per-class semaphores. The dispatch window is a separate cap: it limits
//! how many tasks may be resident in the `JoinSet` at once, so a million-wide
//! ready frontier cannot spawn unbounded futures. The scheduler exposes a
//! single dispatch step, which is also the point a later slice throttles
//! adaptively (AIMD).
//!
//! ## Graph-scoped cancel and fail-fast (DAG-P0-05)
//!
//! Every run owns a graph [`CancellationToken`] (optionally a child of an
//! external parent). The first [`NodeOutcome::Failed`] cancels that token,
//! stops new dispatch, and terminates resident siblings within
//! [`FAIL_FAST_ABORT_GRACE`] — cooperative cancel first, then forced
//! `JoinSet::abort_all` for non-cooperative work. Resource and AIMD permits
//! are always released on task exit (including abort). Optional per-node
//! timeouts are typed as [`TransferErrorKind::Timeout`] and never confused
//! with external cancel ([`TransferErrorKind::Cancelled`]).
//!
//! ## File-local continuing failure (DAG-P1-04)
//!
//! [`NodeOutcome::FileFailedButGraphContinues`] is a non-fatal terminal: the
//! node counts as failed for summary/observer/AIMD, dependents are released
//! so structural tails can drain, and the graph cancel token is **not**
//! fired. Congestion feedback for this outcome is always scoped to
//! [`AdaptiveClass::File`] (explicit file-terminal contract — never inferred
//! from an empty `ResourceRequest`, which `CommitTemp` legitimately has).
//! Fatal [`NodeOutcome::Failed`] remains the fail-fast default.
//!
//! The executor is active in the single-file, batch, and sync DAG wrappers;
//! the segmented-range wrapper also uses it when its graph path is selected.
//! Those callers bind different amounts of real I/O, so using this scheduler
//! does not by itself imply identical wire-level parallelism across surfaces.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::task::{Id as TaskId, JoinSet};
use tokio_util::sync::CancellationToken;

use super::adaptive::{congestion_from_error, AdaptiveClass, AimdController};
use super::error::{TransferError, TransferErrorKind};
use super::graph::{TransferDag, TransferNode};
use super::metrics::TransferDagMetrics;
use super::observer::{DagObserver, ObservedOutcome};
#[cfg(test)]
use super::resources::TransferBudget;
use super::resources::{request_exceeds_budget, ResourceRequest, TransferResourceManager};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DagExecutionSummary {
    pub nodes_completed: u32,
    pub nodes_failed: u32,
    pub fallback_count: u32,
    /// Accumulated run metrics. Generic fallback completions populate
    /// `range_fallbacks`; copy-specific fallback completions populate
    /// `copy_fallbacks`. Operation runners can emit richer byte fields in
    /// their combined observer snapshot. No value is fabricated.
    pub metrics: TransferDagMetrics,
}

/// Outcome of running a single node action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeOutcome {
    /// Node finished on its primary path.
    Completed,
    /// Node finished, but via a degraded path (counted as completed, plus
    /// `fallback_count`). This is an honest, non-error completion.
    Fallback,
    /// A native server-side copy decision transitioned to the separately
    /// shaped observable download-upload graph. Stops the current graph at
    /// this node and is kept distinct from range fallback so metrics do not
    /// conflate two different policy decisions.
    CopyFallback,
    /// File-local terminal failure that must not abort the graph (DAG-P1-04).
    ///
    /// Semantics:
    /// - increments `nodes_failed` (not `nodes_completed`);
    /// - emits exactly one [`ObservedOutcome::Failed`] (or `Cancelled` when
    ///   the typed kind is cancel);
    /// - never calls healthy AIMD feedback;
    /// - applies at most one file-class congestion decrease when the typed
    ///   kind is in the D2 set (always [`AdaptiveClass::File`], even when the
    ///   terminal node has a zero resource request such as `CommitTemp`);
    /// - treats the node as dependency-satisfied and releases dependents;
    /// - does **not** populate the graph-fatal first-error slot, cancel the
    ///   graph token, or start the fail-fast grace timer;
    /// - the run still returns `Ok(DagExecutionSummary)` when the graph drains.
    FileFailedButGraphContinues(TransferError),
    /// Graph-fatal node failure. Scheduling stops, the graph cancel token
    /// fires, resident siblings are terminated within
    /// [`FAIL_FAST_ABORT_GRACE`], and the typed error is propagated (kind /
    /// retry / Retry-After are machine-safe; presentation lives in
    /// [`TransferError::message`]). Dependents are **not** released.
    Failed(TransferError),
}

/// Boxed node future. `Send + 'static` so node work runs on the runtime's
/// worker threads (real parallelism, not single-task concurrency).
pub type NodeFuture = Pin<Box<dyn Future<Output = NodeOutcome> + Send + 'static>>;

/// Per-node action. Invoked once per node, after its dependencies are complete
/// and after the node's `ResourceRequest` permits have been acquired and are
/// held for the duration of the returned future.
pub trait DagNodeRunner: Send + Sync + 'static {
    fn run(&self, node: TransferNode) -> NodeFuture;
}

impl<F> DagNodeRunner for F
where
    F: Fn(TransferNode) -> NodeFuture + Send + Sync + 'static,
{
    fn run(&self, node: TransferNode) -> NodeFuture {
        (self)(node)
    }
}

/// Why a graph execution stopped without completing every node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DagExecutionError {
    /// A node action returned [`NodeOutcome::Failed`]. Scheduling stopped, the
    /// graph cancel token fired, and resident siblings were terminated before
    /// returning.
    NodeFailed {
        node_id: usize,
        error: TransferError,
    },
    /// A node's `ResourceRequest` exceeds the manager budget for some class
    /// and could never be satisfied. Detected before dispatch to avoid a hang.
    Unschedulable { node_id: usize, message: String },
    /// No node is ready, none is in flight, yet not every node ran. This means
    /// an unsatisfiable dependency (a malformed or cyclic graph). Reported
    /// instead of hanging.
    Stuck { pending: usize },
    /// A dispatched task panicked. Surfaced rather than silently swallowed.
    TaskPanicked { node_id: usize, message: String },
}

impl fmt::Display for DagExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DagExecutionError::NodeFailed { node_id, error } => {
                write!(f, "node {node_id} failed: {error}")
            }
            DagExecutionError::Unschedulable { node_id, message } => {
                write!(f, "node {node_id} is unschedulable: {message}")
            }
            DagExecutionError::Stuck { pending } => {
                write!(f, "graph stuck with {pending} node(s) never ready")
            }
            DagExecutionError::TaskPanicked { node_id, message } => {
                write!(f, "node {node_id} task panicked: {message}")
            }
        }
    }
}

impl std::error::Error for DagExecutionError {}

/// Default maximum number of concurrently resident dispatched tasks.
///
/// Resource permits remain the binding limit for normal transfer graphs
/// (multipart fan-out is typically far smaller). This cap prevents a wide
/// ready frontier — e.g. a million independent batch nodes — from spawning
/// an unbounded `JoinSet` of futures. A node is marked started and observed
/// only when it enters this window, not when it merely becomes ready.
pub const DEFAULT_DISPATCH_WINDOW: usize = 256;

/// After the first node failure (or external cancel), resident siblings are
/// given this long to exit cooperatively via the graph cancel token. Any
/// asynchronously yielding task still alive is then forcibly aborted — no
/// simple unbounded drain. Gate: sibling abort observed under 2s.
pub const FAIL_FAST_ABORT_GRACE: Duration = Duration::from_secs(2);

/// Total ceiling after [`FAIL_FAST_ABORT_GRACE`] + `abort_all` while draining
/// JoinSet results. Open started nodes are still notified Cancelled when it
/// expires. Tokio cannot preempt synchronous blocking code inside an async
/// task; node runners must yield or move blocking work to `spawn_blocking`.
const FAIL_FAST_DRAIN_CEILING: Duration = Duration::from_millis(500);

/// Options for a single graph run. [`execute_dag`] uses
/// [`DagExecuteOptions::default`]; callers that already own a user cancel
/// token or need a tighter dispatch window pass an explicit value through
/// [`execute_dag_with_options`].
#[derive(Debug, Clone)]
pub struct DagExecuteOptions {
    /// Maximum concurrent resident tasks in the `JoinSet`. Clamped to ≥ 1.
    pub dispatch_window: usize,
    /// Optional external cancel token. When set, the graph token is a child
    /// of this parent so user Stop cancels the whole run without the executor
    /// owning the caller's token. When `None`, the graph creates a root token.
    pub parent_cancel: Option<CancellationToken>,
    /// Optional per-node wall-clock deadline, measured from dispatch and
    /// therefore including AIMD/resource waits. `None` (production default)
    /// means no node timeout: valid long transfers are never cut by an
    /// arbitrary engine limit. When `Some(d)`, each node that exceeds `d` is
    /// failed with [`TransferErrorKind::Timeout`] — never as Cancelled.
    pub node_timeout: Option<Duration>,
}

impl Default for DagExecuteOptions {
    fn default() -> Self {
        Self {
            dispatch_window: DEFAULT_DISPATCH_WINDOW,
            parent_cancel: None,
            node_timeout: None,
        }
    }
}

impl DagExecuteOptions {
    /// Convenience: default options with an explicit dispatch window.
    pub fn with_dispatch_window(dispatch_window: usize) -> Self {
        Self {
            dispatch_window,
            ..Self::default()
        }
    }
}

/// Run `dag` to completion on a ready-frontier schedule with
/// [`DEFAULT_DISPATCH_WINDOW`] and default cancel/timeout options.
///
/// Compatibility wrapper: production callers that need a parent cancel token
/// or node timeout should prefer [`execute_dag_with_options`].
pub async fn execute_dag(
    dag: &TransferDag,
    manager: &TransferResourceManager,
    runner: Arc<dyn DagNodeRunner>,
    observer: Arc<dyn DagObserver>,
    controller: Option<Arc<AimdController>>,
) -> Result<DagExecutionSummary, DagExecutionError> {
    execute_dag_with_options(
        dag,
        manager,
        runner,
        observer,
        controller,
        DagExecuteOptions::default(),
    )
    .await
}

/// Run `dag` with an explicit dispatch window (other options default).
///
/// See [`execute_dag_with_options`] for full semantics.
pub async fn execute_dag_with_dispatch_window(
    dag: &TransferDag,
    manager: &TransferResourceManager,
    runner: Arc<dyn DagNodeRunner>,
    observer: Arc<dyn DagObserver>,
    controller: Option<Arc<AimdController>>,
    dispatch_window: usize,
) -> Result<DagExecutionSummary, DagExecutionError> {
    execute_dag_with_options(
        dag,
        manager,
        runner,
        observer,
        controller,
        DagExecuteOptions::with_dispatch_window(dispatch_window),
    )
    .await
}

/// Run `dag` to completion on a ready-frontier schedule.
///
/// Each node, once its dependencies are complete, enters a ready queue and is
/// dispatched only while fewer than `options.dispatch_window` tasks are
/// resident in the `JoinSet`. Dispatched nodes acquire their
/// `ResourceRequest` from `manager` before the action runs; the lease is held
/// until the action returns (or the task is aborted). I/O concurrency is
/// therefore still bounded by the manager's semaphores; the window is an
/// orthogonal residency cap.
///
/// On the first failed node the graph cancel token is cancelled, no further
/// nodes are dispatched, resident siblings are terminated within
/// [`FAIL_FAST_ABORT_GRACE`], and [`DagExecutionError::NodeFailed`] is
/// returned with the first typed error. Failed nodes do not release
/// dependents (so `CommitTemp` / join nodes never run after a part failure).
///
/// Every node that received `on_node_start` receives exactly one terminal
/// `on_node_complete` (including force-aborted siblings, reported as
/// [`ObservedOutcome::Cancelled`]).
///
/// `controller`, when `Some`, throttles the dispatch step with a prudent
/// AIMD loop: each dispatched node first acquires per-class dispatch permits
/// from the controller and holds them for its lifetime. Congestion is mapped
/// from the typed [`TransferError`] on the failure; the controller never
/// substring-matches presentation text.
///
/// After the dependency index is built, readiness dispatch visits every node
/// and edge once (O(V+E)). Building that index first normalizes duplicate
/// predecessor ids with `sort_unstable` + `dedup`, so total scheduler setup is
/// O(V + sum(d_i log d_i)), not strictly O(V+E).
pub async fn execute_dag_with_options(
    dag: &TransferDag,
    manager: &TransferResourceManager,
    runner: Arc<dyn DagNodeRunner>,
    observer: Arc<dyn DagObserver>,
    controller: Option<Arc<AimdController>>,
    options: DagExecuteOptions,
) -> Result<DagExecutionSummary, DagExecutionError> {
    let dispatch_window = options.dispatch_window.max(1);
    let node_timeout = options.node_timeout;
    // Graph-scoped token: child of an external parent when provided so user
    // Stop cancels the run; always under executor control for fail-fast.
    let graph_cancel = match options.parent_cancel {
        Some(parent) => parent.child_token(),
        None => CancellationToken::new(),
    };

    let nodes = dag.nodes();
    let mut summary = DagExecutionSummary::default();
    if nodes.is_empty() {
        observer.on_metrics(&summary.metrics);
        return Ok(summary);
    }

    // Pre-flight: a node demanding more of a class than the manager owns would
    // wait on the semaphore forever. Fail it honestly instead of hanging. This
    // is detected before any node runs, so no metrics are reported.
    let budget = manager.budget();
    for node in nodes {
        if let Some(reason) = request_exceeds_budget(&node.resources, &budget) {
            return Err(DagExecutionError::Unschedulable {
                node_id: node.id,
                message: reason,
            });
        }
    }

    // Indexed ready frontier: remaining unique-predecessor count + reverse
    // edges. Nodes enter `ready` only when remaining hits zero; they become
    // `started` only when they enter the dispatch window (spawned into the
    // JoinSet). Duplicate edges in `depends_on` count once so release matches
    // the old `all(|d| completed.contains(d))` semantics. Out-of-range
    // predecessors are kept in the remaining count and never released → Stuck.
    let n = nodes.len();
    let mut remaining_deps: Vec<usize> = Vec::with_capacity(n);
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];
    for node in nodes {
        let mut preds = node.depends_on.clone();
        preds.sort_unstable();
        preds.dedup();
        remaining_deps.push(preds.len());
        for &dep in &preds {
            if dep < n {
                dependents[dep].push(node.id);
            }
        }
    }
    let mut ready: VecDeque<usize> = VecDeque::new();
    for (id, &rem) in remaining_deps.iter().enumerate() {
        if rem == 0 {
            ready.push_back(id);
        }
    }

    let mut completed: HashSet<usize> = HashSet::new();
    let mut started: HashSet<usize> = HashSet::new();
    // Nodes that have already received `on_node_complete`.
    let mut terminal_notified: HashSet<usize> = HashSet::new();
    let mut requests: HashMap<usize, ResourceRequest> = HashMap::new();
    let mut failed: Option<(usize, TransferError)> = None;
    let mut terminal_error: Option<DagExecutionError> = None;
    // A copy fallback is a successful transition to a separately shaped
    // DownloadFile -> UploadFile graph. Its current graph must stop at the
    // decision node so structural tail nodes do not report premature copy
    // completion before the payload graph runs.
    let mut copy_transition = false;
    let mut join_set: JoinSet<(usize, NodeOutcome)> = JoinSet::new();
    // Preserve task→node identity even when Tokio returns a JoinError and the
    // task output `(node_id, outcome)` is unavailable.
    let mut task_nodes: HashMap<TaskId, usize> = HashMap::new();
    // When set, we are in fail-fast: no new dispatch; force-abort after grace.
    let mut fail_fast_deadline: Option<Instant> = None;
    // True after `abort_all` has been issued for this run.
    let mut force_aborted = false;
    // Absolute deadline: the post-abort ceiling applies to the whole drain,
    // not afresh to every joined task.
    let mut force_abort_drain_deadline: Option<Instant> = None;

    loop {
        // External cancel without a prior node failure: treat as graph cancel
        // with a typed Cancelled first-error so the run stops promptly.
        if failed.is_none() && graph_cancel.is_cancelled() {
            let err = TransferError::cancelled();
            failed = Some((usize::MAX, err));
            fail_fast_deadline = Some(Instant::now() + FAIL_FAST_ABORT_GRACE);
        }

        // Dispatch step: fill the window from the ready queue. Once a node
        // has failed (or external cancel) we stop launching new work.
        if failed.is_none() && !copy_transition {
            while join_set.len() < dispatch_window {
                let Some(id) = ready.pop_front() else {
                    break;
                };
                // Defensive: a node should only enqueue once.
                if started.contains(&id) {
                    continue;
                }
                let node = nodes[id].clone();
                started.insert(id);
                requests.insert(id, node.resources);
                observer.on_node_start(node.id, node.kind);
                let manager = manager.clone();
                let runner = Arc::clone(&runner);
                let controller = controller.clone();
                let graph_cancel = graph_cancel.clone();
                let abort_handle = join_set.spawn(async move {
                    run_dispatched_node(
                        node,
                        manager,
                        runner,
                        controller,
                        graph_cancel,
                        node_timeout,
                    )
                    .await
                });
                task_nodes.insert(abort_handle.id(), id);
            }
        }

        if join_set.is_empty() {
            if failed.is_some() || completed.len() == n || copy_transition {
                break;
            }
            // Nothing ready, nothing running, not every node completed: a
            // dependency can never be satisfied (malformed/cyclic graph), or
            // a prior failure left dependents unreleased (handled above).
            terminal_error = Some(DagExecutionError::Stuck {
                pending: n - completed.len(),
            });
            break;
        }

        // Fail-fast: after grace, force-abort any still-resident sibling so a
        // non-cooperative task cannot hang the graph indefinitely.
        let past_grace = fail_fast_deadline
            .map(|d| Instant::now() >= d)
            .unwrap_or(false);
        if past_grace && !force_aborted {
            join_set.abort_all();
            force_aborted = true;
            force_abort_drain_deadline = Some(Instant::now() + FAIL_FAST_DRAIN_CEILING);
        }

        let joined = if let Some(deadline) = fail_fast_deadline {
            if force_aborted {
                // After abort_all, drain within one absolute ceiling. This
                // bounds executor bookkeeping for abortable async tasks.
                let remaining = force_abort_drain_deadline
                    .expect("force-aborted drain has a deadline")
                    .saturating_duration_since(Instant::now());
                match tokio::time::timeout(remaining, join_set.join_next_with_id()).await {
                    Ok(Some(joined)) => joined,
                    Ok(None) => break,
                    Err(_) => {
                        // Drop the JoinSet (aborts any still-tracked handles
                        // on drop) and let the safety net below close observer
                        // lifecycles. Synchronous blocking code is outside
                        // Tokio's preemptive control.
                        drop(join_set);
                        break;
                    }
                }
            } else {
                let remaining = deadline.saturating_duration_since(Instant::now());
                tokio::select! {
                    joined = join_set.join_next_with_id() => {
                        joined.expect("join_set checked non-empty above")
                    }
                    _ = tokio::time::sleep(remaining) => {
                        join_set.abort_all();
                        force_aborted = true;
                        force_abort_drain_deadline =
                            Some(Instant::now() + FAIL_FAST_DRAIN_CEILING);
                        continue;
                    }
                }
            }
        } else {
            join_set
                .join_next_with_id()
                .await
                .expect("join_set checked non-empty above")
        };

        let (id, outcome) = match joined {
            Ok((task_id, pair)) => {
                task_nodes.remove(&task_id);
                pair
            }
            Err(join_err) => {
                let node_id = task_nodes.remove(&join_err.id()).unwrap_or(usize::MAX);
                if join_err.is_cancelled() {
                    // Forced abort (or task cancel): Tokio retains task
                    // identity in JoinError even though the task output is
                    // lost, so close the exact node lifecycle.
                    if node_id == usize::MAX || terminal_notified.contains(&node_id) {
                        continue;
                    }
                    requests.remove(&node_id);
                    summary.nodes_failed += 1;
                    terminal_notified.insert(node_id);
                    observer.on_node_complete(node_id, ObservedOutcome::Cancelled);
                    if failed.is_none() {
                        failed = Some((node_id, TransferError::cancelled()));
                    }
                    continue;
                }
                // Notify any open started nodes so observers stay consistent,
                // then surface the panic as the terminal error.
                for open_id in started.iter().copied() {
                    if !terminal_notified.contains(&open_id) {
                        terminal_notified.insert(open_id);
                        observer.on_node_complete(open_id, ObservedOutcome::Failed);
                        summary.nodes_failed += 1;
                    }
                }
                terminal_error = Some(DagExecutionError::TaskPanicked {
                    node_id,
                    message: join_err.to_string(),
                });
                join_set.abort_all();
                // Apply the same absolute drain ceiling to panic cleanup.
                let drain_deadline = Instant::now() + FAIL_FAST_DRAIN_CEILING;
                while !join_set.is_empty() {
                    let remaining = drain_deadline.saturating_duration_since(Instant::now());
                    if tokio::time::timeout(remaining, join_set.join_next_with_id())
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                break;
            }
        };

        // Drop the stored request once the task finishes so a million-node
        // run does not retain a full request map after drain.
        let node_request = requests.remove(&id);
        match outcome {
            NodeOutcome::Completed => {
                summary.nodes_completed += 1;
                completed.insert(id);
                terminal_notified.insert(id);
                observer.on_node_complete(id, ObservedOutcome::Completed);
                aimd_note_healthy(&controller, node_request.as_ref());
                if failed.is_none() {
                    release_dependents(id, &dependents, &mut remaining_deps, &mut ready);
                }
            }
            NodeOutcome::Fallback => {
                summary.nodes_completed += 1;
                summary.fallback_count += 1;
                // A node taking its degraded path is the one metric this
                // slice can populate truthfully.
                summary.metrics.range_fallbacks += 1;
                completed.insert(id);
                terminal_notified.insert(id);
                observer.on_node_complete(id, ObservedOutcome::Fallback);
                // A successful (if degraded) completion is still a healthy
                // signal for the concurrency controller.
                aimd_note_healthy(&controller, node_request.as_ref());
                if failed.is_none() {
                    release_dependents(id, &dependents, &mut remaining_deps, &mut ready);
                }
            }
            NodeOutcome::CopyFallback => {
                summary.nodes_completed += 1;
                summary.fallback_count += 1;
                summary.metrics.copy_fallbacks += 1;
                completed.insert(id);
                terminal_notified.insert(id);
                observer.on_node_complete(id, ObservedOutcome::Fallback);
                aimd_note_healthy(&controller, node_request.as_ref());
                // The copy orchestrator builds and executes the fallback
                // graph after this run returns. Do not release this shape's
                // structural dependents: they would otherwise emit a terminal
                // completion before the real payload legs have run.
                copy_transition = true;
            }
            NodeOutcome::FileFailedButGraphContinues(error) => {
                // DAG-P1-04: file-local terminal — visible, non-fatal, releases
                // dependents so the structural tail can drain. Never healthy.
                summary.nodes_failed += 1;
                completed.insert(id);
                if aimd_note_file_congestion(&controller, &error) {
                    summary.metrics.backpressure_events += 1;
                }
                let observed = if error.kind == TransferErrorKind::Cancelled {
                    ObservedOutcome::Cancelled
                } else {
                    ObservedOutcome::Failed
                };
                terminal_notified.insert(id);
                observer.on_node_complete(id, observed);
                // Intentionally do NOT set `failed` / cancel the graph.
                if failed.is_none() {
                    release_dependents(id, &dependents, &mut remaining_deps, &mut ready);
                }
            }
            NodeOutcome::Failed(error) => {
                summary.nodes_failed += 1;
                // Only the narrow congestion set throttles; other failures
                // (auth, not-found, cancel, ...) must not shrink concurrency.
                // Kind + retry_after are typed — no message substring matching.
                // Failed nodes intentionally do not release dependents.
                if let (Some(ctrl), Some(request)) = (&controller, node_request.as_ref()) {
                    if congestion_from_error(&error).is_some() {
                        summary.metrics.backpressure_events += 1;
                        let hint = error.retry_after;
                        for class in AimdController::classes_for(request) {
                            ctrl.on_congestion_with_hint(class, hint);
                        }
                    }
                }
                let observed = if error.kind == TransferErrorKind::Cancelled {
                    ObservedOutcome::Cancelled
                } else {
                    ObservedOutcome::Failed
                };
                terminal_notified.insert(id);
                observer.on_node_complete(id, observed);
                if failed.is_none() {
                    // First error wins (including typed Timeout vs Cancelled).
                    failed = Some((id, error));
                    // Fail-fast: cancel the graph and start the abort grace clock.
                    graph_cancel.cancel();
                    fail_fast_deadline = Some(Instant::now() + FAIL_FAST_ABORT_GRACE);
                }
            }
        }
    }

    // Every started node must have a terminal observer event (force-abort path
    // may have left JoinErrors already mapped; this is the safety net).
    for id in started.iter().copied() {
        if !terminal_notified.contains(&id) {
            terminal_notified.insert(id);
            summary.nodes_failed += 1;
            observer.on_node_complete(id, ObservedOutcome::Cancelled);
            if failed.is_none() {
                failed = Some((id, TransferError::cancelled()));
            }
        }
    }

    // Single finalize: report the accumulated metrics once for any run that
    // entered the scheduling loop, then surface the terminal state.
    observer.on_metrics(&summary.metrics);
    if let Some(error) = terminal_error {
        return Err(error);
    }
    if let Some((node_id, error)) = failed {
        // External-only cancel used node_id = MAX as a sentinel; surface a
        // coherent id when we can (first started node) for diagnostics.
        let node_id = if node_id == usize::MAX {
            started.iter().copied().next().unwrap_or(usize::MAX)
        } else {
            node_id
        };
        return Err(DagExecutionError::NodeFailed { node_id, error });
    }
    Ok(summary)
}

/// Body of one dispatched JoinSet task. The optional timeout covers the whole
/// dispatched lifetime (AIMD wait, resource wait, and runner); cancel is
/// biased ahead of timeout. Dropping the inner future releases any held lease
/// or AIMD permits on every exit path.
async fn run_dispatched_node(
    node: TransferNode,
    manager: TransferResourceManager,
    runner: Arc<dyn DagNodeRunner>,
    controller: Option<Arc<AimdController>>,
    graph_cancel: CancellationToken,
    node_timeout: Option<Duration>,
) -> (usize, NodeOutcome) {
    let id = node.id;
    let run_fut = run_node_cancel_aware(node, manager, runner, controller, &graph_cancel);
    let outcome = match node_timeout {
        Some(timeout) => {
            tokio::select! {
                biased;
                _ = graph_cancel.cancelled() => {
                    NodeOutcome::Failed(TransferError::cancelled())
                }
                _ = tokio::time::sleep(timeout) => {
                    NodeOutcome::Failed(TransferError::timeout(format!(
                        "node exceeded timeout of {timeout:?}"
                    )))
                }
                outcome = run_fut => outcome,
            }
        }
        None => {
            tokio::select! {
                biased;
                _ = graph_cancel.cancelled() => {
                    NodeOutcome::Failed(TransferError::cancelled())
                }
                outcome = run_fut => outcome,
            }
        }
    };
    (id, outcome)
}

async fn run_node_cancel_aware(
    node: TransferNode,
    manager: TransferResourceManager,
    runner: Arc<dyn DagNodeRunner>,
    controller: Option<Arc<AimdController>>,
    graph_cancel: &CancellationToken,
) -> NodeOutcome {
    let request = node.resources;
    // Adaptive dispatch gate (held for the node's lifetime): a shrink only
    // parks not-yet-started nodes here, never an already-running transfer.
    let _dispatch = match &controller {
        Some(ctrl) => {
            tokio::select! {
                biased;
                _ = graph_cancel.cancelled() => {
                    return NodeOutcome::Failed(TransferError::cancelled());
                }
                permit = ctrl.acquire(&request) => Some(permit),
            }
        }
        None => None,
    };

    let _lease = match tokio::select! {
        biased;
        _ = graph_cancel.cancelled() => {
            return NodeOutcome::Failed(TransferError::cancelled());
        }
        result = manager.acquire(request) => result,
    } {
        Ok(lease) => lease,
        Err(e) => {
            return NodeOutcome::Failed(TransferError::resource_acquire(format!(
                "resource acquire failed: {e}"
            )));
        }
    };

    tokio::select! {
        biased;
        _ = graph_cancel.cancelled() => {
            NodeOutcome::Failed(TransferError::cancelled())
        }
        outcome = runner.run(node) => outcome,
    }
}

/// After a successful completion, decrement remaining-deps for each child and
/// enqueue any that become fully satisfied.
fn release_dependents(
    id: usize,
    dependents: &[Vec<usize>],
    remaining_deps: &mut [usize],
    ready: &mut VecDeque<usize>,
) {
    if id >= dependents.len() {
        return;
    }
    for &child in &dependents[id] {
        if child >= remaining_deps.len() {
            continue;
        }
        // Saturating: a well-formed graph never double-releases, but a
        // malformed one must not underflow.
        let rem = &mut remaining_deps[child];
        if *rem == 0 {
            continue;
        }
        *rem -= 1;
        if *rem == 0 {
            ready.push_back(child);
        }
    }
}

/// Reward the AIMD controller for a healthy completion on every controlled
/// class the node used. No-op when no controller is wired.
fn aimd_note_healthy(controller: &Option<Arc<AimdController>>, request: Option<&ResourceRequest>) {
    if let (Some(ctrl), Some(request)) = (controller, request) {
        for class in AimdController::classes_for(request) {
            ctrl.note_healthy(class);
        }
    }
}

/// File-scoped congestion feedback for a continuing file-terminal failure.
///
/// Always targets [`AdaptiveClass::File`]: multipart commit nodes have a zero
/// resource request (session lease already held separately), and a second
/// synthetic `file_slots` lease would risk lock-order deadlocks. Returns true
/// when a D2 congestion decrease was applied.
fn aimd_note_file_congestion(
    controller: &Option<Arc<AimdController>>,
    error: &TransferError,
) -> bool {
    let Some(ctrl) = controller else {
        return false;
    };
    if congestion_from_error(error).is_none() {
        return false;
    }
    ctrl.on_congestion_with_hint(AdaptiveClass::File, error.retry_after);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer_dag::graph::TransferNodeKind;
    use crate::transfer_dag::observer::{CollectingDagObserver, NoopDagObserver};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    fn noop_observer() -> Arc<dyn DagObserver> {
        Arc::new(NoopDagObserver)
    }

    /// A runner that records completion order and tracks peak concurrency.
    #[derive(Default)]
    struct ProbeRunner {
        order: Mutex<Vec<usize>>,
        in_flight: AtomicUsize,
        peak: AtomicUsize,
    }

    impl ProbeRunner {
        fn order(&self) -> Vec<usize> {
            self.order.lock().unwrap().clone()
        }
        fn peak(&self) -> usize {
            self.peak.load(Ordering::SeqCst)
        }
    }

    fn probe_future(runner: Arc<ProbeRunner>, node: TransferNode) -> NodeFuture {
        Box::pin(async move {
            let now = runner.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            runner.peak.fetch_max(now, Ordering::SeqCst);
            // Hold the window open so overlapping nodes are observable.
            tokio::time::sleep(Duration::from_millis(15)).await;
            runner.order.lock().unwrap().push(node.id);
            runner.in_flight.fetch_sub(1, Ordering::SeqCst);
            NodeOutcome::Completed
        })
    }

    fn runner_arc(probe: Arc<ProbeRunner>) -> Arc<dyn DagNodeRunner> {
        Arc::new(move |node: TransferNode| probe_future(Arc::clone(&probe), node))
    }

    #[tokio::test]
    async fn linear_chain_runs_in_dependency_order() {
        let mut dag = TransferDag::default();
        let a = dag.add_node(
            TransferNodeKind::PlanTransfer,
            vec![],
            ResourceRequest::default(),
        );
        let b = dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![a],
            ResourceRequest::default(),
        );
        let _c = dag.add_node(
            TransferNodeKind::VerifyChecksum,
            vec![b],
            ResourceRequest::default(),
        );

        let probe = Arc::new(ProbeRunner::default());
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(4));
        let summary = execute_dag(
            &dag,
            &manager,
            runner_arc(Arc::clone(&probe)),
            noop_observer(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(summary.nodes_completed, 3);
        assert_eq!(summary.nodes_failed, 0);
        assert_eq!(probe.order(), vec![0, 1, 2]);
        assert_eq!(probe.peak(), 1);
    }

    #[tokio::test]
    async fn diamond_respects_join_dependency() {
        let mut dag = TransferDag::default();
        let a = dag.add_node(
            TransferNodeKind::PlanTransfer,
            vec![],
            ResourceRequest::default(),
        );
        let b = dag.add_node(
            TransferNodeKind::DownloadRange,
            vec![a],
            ResourceRequest::default(),
        );
        let c = dag.add_node(
            TransferNodeKind::DownloadRange,
            vec![a],
            ResourceRequest::default(),
        );
        let d = dag.add_node(
            TransferNodeKind::CommitTemp,
            vec![b, c],
            ResourceRequest::default(),
        );

        let probe = Arc::new(ProbeRunner::default());
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(4));
        let summary = execute_dag(
            &dag,
            &manager,
            runner_arc(Arc::clone(&probe)),
            noop_observer(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(summary.nodes_completed, 4);
        let order = probe.order();
        let pos = |id: usize| order.iter().position(|&x| x == id).unwrap();
        assert!(pos(a) < pos(b) && pos(a) < pos(c));
        assert!(pos(b) < pos(d) && pos(c) < pos(d));
    }

    #[tokio::test]
    async fn fan_out_children_overlap() {
        let mut dag = TransferDag::default();
        let root = dag.add_node(
            TransferNodeKind::PlanTransfer,
            vec![],
            ResourceRequest::default(),
        );
        for _ in 0..4 {
            dag.add_node(
                TransferNodeKind::EmitProgress,
                vec![root],
                ResourceRequest::default(),
            );
        }

        let probe = Arc::new(ProbeRunner::default());
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(8));
        let summary = execute_dag(
            &dag,
            &manager,
            runner_arc(Arc::clone(&probe)),
            noop_observer(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(summary.nodes_completed, 5);
        // The four children carry no scarce resource and must overlap.
        assert!(probe.peak() >= 2, "peak concurrency was {}", probe.peak());
    }

    #[tokio::test]
    async fn resource_starved_nodes_serialize() {
        let mut dag = TransferDag::default();
        for _ in 0..3 {
            dag.add_node(
                TransferNodeKind::DownloadFile,
                vec![],
                ResourceRequest::upload_file(),
            );
        }

        let probe = Arc::new(ProbeRunner::default());
        // Only one file slot: the three file transfers must serialize.
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(1));
        let summary = execute_dag(
            &dag,
            &manager,
            runner_arc(Arc::clone(&probe)),
            noop_observer(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(summary.nodes_completed, 3);
        assert_eq!(probe.peak(), 1, "file_slots=1 must force serialization");
    }

    #[tokio::test]
    async fn failed_node_stops_scheduling_and_propagates() {
        let mut dag = TransferDag::default();
        let a = dag.add_node(
            TransferNodeKind::PlanTransfer,
            vec![],
            ResourceRequest::default(),
        );
        let _b = dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![a],
            ResourceRequest::default(),
        );

        let runner: Arc<dyn DagNodeRunner> = Arc::new(|node: TransferNode| -> NodeFuture {
            Box::pin(async move {
                if node.id == 0 {
                    NodeOutcome::Failed(TransferError::from_message("synthetic plan failure"))
                } else {
                    NodeOutcome::Completed
                }
            })
        });
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(4));
        let err = execute_dag(&dag, &manager, runner, noop_observer(), None)
            .await
            .unwrap_err();

        match err {
            DagExecutionError::NodeFailed { node_id, error } => {
                assert_eq!(node_id, 0);
                assert!(error.message.contains("synthetic plan failure"));
            }
            other => panic!("expected NodeFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unschedulable_request_is_detected_before_dispatch() {
        let mut dag = TransferDag::default();
        dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![],
            ResourceRequest {
                file_slots: 5,
                ..ResourceRequest::default()
            },
        );

        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(1));
        let err = execute_dag(
            &dag,
            &manager,
            Arc::new(|_n: TransferNode| -> NodeFuture {
                Box::pin(async { NodeOutcome::Completed })
            }),
            noop_observer(),
            None,
        )
        .await
        .unwrap_err();

        assert!(matches!(
            err,
            DagExecutionError::Unschedulable { node_id: 0, .. }
        ));
    }

    #[tokio::test]
    async fn zero_buffer_budget_is_unschedulable_for_byte_requests() {
        let mut dag = TransferDag::default();
        dag.add_node(
            TransferNodeKind::UploadPart,
            vec![],
            ResourceRequest::upload_part(1024),
        );
        let manager = TransferResourceManager::new(TransferBudget {
            buffer_bytes: 0,
            chunk_slots: 2,
            disk_read_slots: 2,
            ..TransferBudget::from_file_slots(1)
        });
        let err = execute_dag(
            &dag,
            &manager,
            Arc::new(|_n: TransferNode| -> NodeFuture {
                Box::pin(async { NodeOutcome::Completed })
            }),
            noop_observer(),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, DagExecutionError::Unschedulable { .. }));
    }

    #[tokio::test]
    async fn byte_credits_bound_concurrent_part_nodes() {
        use crate::transfer_dag::resources::BUFFER_QUANTUM_BYTES;
        use std::sync::atomic::{AtomicU64, Ordering};

        let quantum = BUFFER_QUANTUM_BYTES;
        // Budget of 2 quanta: each part wants 2 quanta so only one can run.
        let budget_bytes = quantum * 2;
        let budget = TransferBudget {
            chunk_slots: 4,
            disk_read_slots: 4,
            buffer_bytes: budget_bytes,
            ..TransferBudget::from_file_slots(1)
        };
        let manager = TransferResourceManager::new(budget);

        let mut dag = TransferDag::default();
        for _ in 0..3 {
            dag.add_node(
                TransferNodeKind::UploadPart,
                vec![],
                ResourceRequest::upload_part(budget_bytes),
            );
        }

        let peak = Arc::new(AtomicU64::new(0));
        let live = Arc::new(AtomicU64::new(0));
        let peak_c = peak.clone();
        let live_c = live.clone();
        let runner: Arc<dyn DagNodeRunner> = Arc::new(move |node: TransferNode| -> NodeFuture {
            let peak = peak_c.clone();
            let live = live_c.clone();
            Box::pin(async move {
                // Lease is already held; sample peak concurrent buffer credits.
                let bytes = node.resources.buffer_bytes;
                let now = live.fetch_add(bytes, Ordering::SeqCst) + bytes;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                live.fetch_sub(bytes, Ordering::SeqCst);
                NodeOutcome::Completed
            })
        });

        let summary = execute_dag(&dag, &manager, runner, noop_observer(), None)
            .await
            .unwrap();
        assert_eq!(summary.nodes_completed, 3);
        assert!(
            peak.load(Ordering::SeqCst) <= budget_bytes,
            "peak credited bytes {} exceeded budget {}",
            peak.load(Ordering::SeqCst),
            budget_bytes
        );
    }

    #[tokio::test]
    async fn cancel_while_waiting_for_buffer_credits_releases_nothing_extra() {
        use crate::transfer_dag::resources::BUFFER_QUANTUM_BYTES;
        use tokio_util::sync::CancellationToken;

        let quantum = BUFFER_QUANTUM_BYTES;
        let budget = TransferBudget {
            chunk_slots: 2,
            disk_read_slots: 2,
            buffer_bytes: quantum,
            ..TransferBudget::from_file_slots(1)
        };
        let manager = TransferResourceManager::new(budget);

        // Hold the only quantum outside the graph.
        let held = manager
            .acquire(ResourceRequest {
                buffer_bytes: quantum,
                ..ResourceRequest::default()
            })
            .await
            .unwrap();

        let mut dag = TransferDag::default();
        dag.add_node(
            TransferNodeKind::UploadPart,
            vec![],
            ResourceRequest::upload_part(quantum),
        );

        let parent = CancellationToken::new();
        let parent2 = parent.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(40)).await;
            parent2.cancel();
        });

        let err = execute_dag_with_options(
            &dag,
            &manager,
            Arc::new(|_n: TransferNode| -> NodeFuture {
                Box::pin(async { NodeOutcome::Completed })
            }),
            noop_observer(),
            None,
            DagExecuteOptions {
                parent_cancel: Some(parent),
                ..DagExecuteOptions::default()
            },
        )
        .await
        .unwrap_err();

        match err {
            DagExecutionError::NodeFailed { error, .. } => {
                assert_eq!(error.kind, TransferErrorKind::Cancelled);
            }
            other => panic!("expected cancelled NodeFailed, got {other:?}"),
        }
        drop(held);
        assert_eq!(manager.available_buffer_quanta(), 1);
        assert_eq!(manager.available_oversize_permits(), 1);
    }

    #[tokio::test]
    async fn fallback_outcome_is_counted() {
        let mut dag = TransferDag::default();
        dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![],
            ResourceRequest::default(),
        );

        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(2));
        let summary = execute_dag(
            &dag,
            &manager,
            Arc::new(|_n: TransferNode| -> NodeFuture {
                Box::pin(async { NodeOutcome::Fallback })
            }),
            noop_observer(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(summary.nodes_completed, 1);
        assert_eq!(summary.fallback_count, 1);
        assert_eq!(summary.nodes_failed, 0);
        // The one metric this slice populates truthfully.
        assert_eq!(summary.metrics.range_fallbacks, 1);
    }

    #[tokio::test]
    async fn observer_receives_node_lifecycle_and_final_metrics() {
        let mut dag = TransferDag::default();
        let a = dag.add_node(
            TransferNodeKind::PlanTransfer,
            vec![],
            ResourceRequest::default(),
        );
        let _b = dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![a],
            ResourceRequest::default(),
        );

        // Node 0 completes normally, node 1 takes the degraded path.
        let runner: Arc<dyn DagNodeRunner> = Arc::new(|node: TransferNode| -> NodeFuture {
            Box::pin(async move {
                if node.id == 0 {
                    NodeOutcome::Completed
                } else {
                    NodeOutcome::Fallback
                }
            })
        });
        let observer = Arc::new(CollectingDagObserver::default());
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(4));
        let summary = execute_dag(
            &dag,
            &manager,
            runner,
            Arc::clone(&observer) as Arc<dyn DagObserver>,
            None,
        )
        .await
        .unwrap();

        assert_eq!(summary.nodes_completed, 2);
        assert_eq!(summary.metrics.range_fallbacks, 1);
        // on_metrics fired once at finalize with the same accumulated value.
        assert_eq!(observer.metrics().range_fallbacks, 1);
    }

    #[tokio::test]
    async fn aimd_controller_shrinks_file_target_on_congestion_failure() {
        use crate::transfer_dag::adaptive::{AdaptiveClass, AimdConfig, AimdController};

        // One file node that fails with a 429 (a congestion signal).
        let mut dag = TransferDag::default();
        dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![],
            ResourceRequest::upload_file(),
        );

        let runner: Arc<dyn DagNodeRunner> = Arc::new(|_n: TransferNode| -> NodeFuture {
            Box::pin(async {
                NodeOutcome::Failed(TransferError::from_message("HTTP 429 Too Many Requests"))
            })
        });
        let controller = Arc::new(AimdController::new(8, 1, 1, 1, AimdConfig::default()));
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(8));

        let err = execute_dag(
            &dag,
            &manager,
            runner,
            noop_observer(),
            Some(Arc::clone(&controller)),
        )
        .await
        .unwrap_err();

        assert!(matches!(err, DagExecutionError::NodeFailed { .. }));
        // The 429 must have halved the File-class target (8 -> 4) and never
        // grown it above the honest ceiling.
        assert_eq!(controller.target(AdaptiveClass::File), 4);
        assert_eq!(controller.live(AdaptiveClass::File), 4);
    }

    // DAG-P2-06: end-to-end through execute_dag, a profiled controller mirrors a
    // D2 congestion into its registry key but a cancellation records nothing:
    // the executor's existing typed gate (congestion_from_error) is what keeps
    // non-D2 outcomes from poisoning the learned profile.
    #[tokio::test]
    async fn profile_records_d2_congestion_but_not_cancellation_through_execute_dag() {
        use crate::transfer_dag::adaptive::{
            AdaptiveClass, AdaptiveProfileConfig, AdaptiveProfileKey, AdaptiveProfileRegistry,
            AdaptiveWorkload, AimdConfig, AimdController,
        };
        use crate::transfer_dag::EndpointIdentity;

        let registry = Arc::new(AdaptiveProfileRegistry::new(
            AdaptiveProfileConfig::default(),
        ));
        let endpoint = EndpointIdentity::new("s3", "exec-test-host", "exec-acct");

        // A cancelled node is not a D2 signal: it must not record anything.
        {
            let mut dag = TransferDag::default();
            dag.add_node(
                TransferNodeKind::DownloadFile,
                vec![],
                ResourceRequest::upload_file(),
            );
            let runner: Arc<dyn DagNodeRunner> = Arc::new(|_n: TransferNode| -> NodeFuture {
                Box::pin(async { NodeOutcome::Failed(TransferError::cancelled()) })
            });
            let key = AdaptiveProfileKey::new(endpoint.clone(), AdaptiveWorkload::BatchSyncFile);
            let controller = Arc::new(AimdController::from_budget_for_profile(
                &TransferBudget::from_file_slots(8),
                None,
                key,
                registry.clone(),
                AimdConfig::default(),
            ));
            let manager = TransferResourceManager::new(TransferBudget::from_file_slots(8));
            let _ = execute_dag(&dag, &manager, runner, noop_observer(), Some(controller)).await;
            assert!(
                registry.is_empty(),
                "a cancellation must not record a learned target"
            );
        }

        // A 429 on a distinct workload key records the halved safe target.
        {
            let mut dag = TransferDag::default();
            dag.add_node(
                TransferNodeKind::DownloadFile,
                vec![],
                ResourceRequest::upload_file(),
            );
            let runner: Arc<dyn DagNodeRunner> = Arc::new(|_n: TransferNode| -> NodeFuture {
                Box::pin(async {
                    NodeOutcome::Failed(TransferError::from_message("HTTP 429 Too Many Requests"))
                })
            });
            let key = AdaptiveProfileKey::new(endpoint.clone(), AdaptiveWorkload::ShapedFile);
            let controller = Arc::new(AimdController::from_budget_for_profile(
                &TransferBudget::from_file_slots(8),
                None,
                key.clone(),
                registry.clone(),
                AimdConfig::default(),
            ));
            let manager = TransferResourceManager::new(TransferBudget::from_file_slots(8));
            let err = execute_dag(
                &dag,
                &manager,
                runner,
                noop_observer(),
                Some(Arc::clone(&controller)),
            )
            .await
            .unwrap_err();
            assert!(matches!(err, DagExecutionError::NodeFailed { .. }));
            assert_eq!(controller.target(AdaptiveClass::File), 4);
            // Exactly one entry: the cancel run above recorded nothing.
            let snapshot = registry.snapshot();
            assert_eq!(snapshot.len(), 1, "only the D2 run recorded");
            assert_eq!(snapshot[0].key, key);
            assert_eq!(snapshot[0].file, Some(4));
        }
    }

    #[tokio::test]
    async fn aimd_honors_typed_retry_after_hint() {
        // DAG-P0-03: Retry-After is a typed field on TransferError. The
        // executor passes `error.retry_after` to on_congestion_with_hint so
        // the AIMD cooldown is armed to the server-provided value (clamped)
        // instead of the configured default — no marker re-parse in the
        // controller.
        use crate::transfer_dag::adaptive::{AdaptiveClass, AimdConfig, AimdController};
        use crate::transfer_dag::error::TransferErrorKind;
        use std::time::{Duration, Instant};

        let mut dag = TransferDag::default();
        dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![],
            ResourceRequest::upload_file(),
        );

        // The hint (45 s) is well above the lower clamp (1 s) and below the
        // upper clamp (10 × default cooldown = 50 s), so it must pass through
        // verbatim.
        let mut err =
            TransferError::new(TransferErrorKind::RateLimited, "HTTP 429 Too Many Requests");
        err.retry_after = Some(Duration::from_secs(45));
        let runner: Arc<dyn DagNodeRunner> = Arc::new(move |_n: TransferNode| -> NodeFuture {
            let e = err.clone();
            Box::pin(async move { NodeOutcome::Failed(e) })
        });
        let controller = Arc::new(AimdController::new(8, 1, 1, 1, AimdConfig::default()));
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(8));

        let before = Instant::now();
        let _ = execute_dag(
            &dag,
            &manager,
            runner,
            noop_observer(),
            Some(Arc::clone(&controller)),
        )
        .await;

        // Concurrency halved as before (8 -> 4).
        assert_eq!(controller.target(AdaptiveClass::File), 4);

        // The cooldown must be armed approximately to `before + 45s`, NOT
        // to the default 5 s cooldown. A 1 s tolerance around the lower
        // bound absorbs scheduling and lock-acquire overhead.
        let until = controller
            .cooldown_until(AdaptiveClass::File)
            .expect("cooldown must be armed after a congestion event");
        let armed = until.saturating_duration_since(before);
        assert!(
            armed >= Duration::from_secs(44) && armed <= Duration::from_secs(46),
            "expected ~45s (server-provided hint), got {:?}",
            armed
        );
    }

    #[tokio::test]
    async fn aimd_honors_retry_after_lifted_from_provider_marker() {
        // Providers still embed the marker in ProviderError presentation
        // strings; the adapter lifts it once into TransferError.retry_after.
        use crate::providers::ProviderError;
        use crate::transfer_dag::adaptive::{
            embed_retry_after_marker, AdaptiveClass, AimdConfig, AimdController,
        };
        use std::time::{Duration, Instant};

        let mut dag = TransferDag::default();
        dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![],
            ResourceRequest::upload_file(),
        );

        let marker = embed_retry_after_marker(45);
        let pe = ProviderError::TransferFailed(format!("HTTP 429 Too Many Requests{marker}"));
        let te = TransferError::from_provider(&pe);
        assert_eq!(te.retry_after, Some(Duration::from_secs(45)));

        let runner: Arc<dyn DagNodeRunner> = Arc::new(move |_n: TransferNode| -> NodeFuture {
            let e = te.clone();
            Box::pin(async move { NodeOutcome::Failed(e) })
        });
        let controller = Arc::new(AimdController::new(8, 1, 1, 1, AimdConfig::default()));
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(8));

        let before = Instant::now();
        let _ = execute_dag(
            &dag,
            &manager,
            runner,
            noop_observer(),
            Some(Arc::clone(&controller)),
        )
        .await;

        assert_eq!(controller.target(AdaptiveClass::File), 4);
        let until = controller
            .cooldown_until(AdaptiveClass::File)
            .expect("cooldown must be armed");
        let armed = until.saturating_duration_since(before);
        assert!(
            armed >= Duration::from_secs(44) && armed <= Duration::from_secs(46),
            "expected ~45s, got {:?}",
            armed
        );
    }

    #[tokio::test]
    async fn aimd_ignores_non_congestion_failures() {
        use crate::transfer_dag::adaptive::{AdaptiveClass, AimdConfig, AimdController};

        let mut dag = TransferDag::default();
        dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![],
            ResourceRequest::upload_file(),
        );

        let runner: Arc<dyn DagNodeRunner> = Arc::new(|_n: TransferNode| -> NodeFuture {
            Box::pin(async { NodeOutcome::Failed(TransferError::from_message("404 not found")) })
        });
        let controller = Arc::new(AimdController::new(8, 1, 1, 1, AimdConfig::default()));
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(8));

        let _ = execute_dag(
            &dag,
            &manager,
            runner,
            noop_observer(),
            Some(Arc::clone(&controller)),
        )
        .await;

        // A not-found failure is not congestion: the target stays put.
        assert_eq!(controller.target(AdaptiveClass::File), 8);
    }

    #[tokio::test]
    async fn aimd_ignores_cancel_failures() {
        use crate::transfer_dag::adaptive::{AdaptiveClass, AimdConfig, AimdController};

        let mut dag = TransferDag::default();
        dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![],
            ResourceRequest::upload_file(),
        );

        let runner: Arc<dyn DagNodeRunner> = Arc::new(|_n: TransferNode| -> NodeFuture {
            Box::pin(async { NodeOutcome::Failed(TransferError::cancelled()) })
        });
        let controller = Arc::new(AimdController::new(8, 1, 1, 1, AimdConfig::default()));
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(8));

        let err = execute_dag(
            &dag,
            &manager,
            runner,
            noop_observer(),
            Some(Arc::clone(&controller)),
        )
        .await
        .unwrap_err();

        match err {
            DagExecutionError::NodeFailed { error, .. } => {
                assert_eq!(
                    error.kind,
                    crate::transfer_dag::error::TransferErrorKind::Cancelled
                );
            }
            other => panic!("expected NodeFailed, got {other:?}"),
        }
        assert_eq!(controller.target(AdaptiveClass::File), 8);
        assert_eq!(controller.cooldown_until(AdaptiveClass::File), None);
    }

    #[tokio::test]
    async fn controller_at_ceiling_with_no_congestion_dispatches_like_none() {
        // F3-T05 non-regression: a controller seeded from the budget starts
        // every class at its ceiling, so a congestion-free run dispatches
        // exactly as the prior `None` did. This is the evidence that wiring
        // `Some(ctrl)` into the production segmented-download path is
        // behaviourally identical on the happy path.
        use crate::transfer_dag::adaptive::{AimdConfig, AimdController};

        let build = || {
            let mut dag = TransferDag::default();
            for _ in 0..6 {
                dag.add_node(
                    TransferNodeKind::DownloadRange,
                    vec![],
                    ResourceRequest::range_chunk(),
                );
            }
            dag
        };
        let budget = TransferBudget {
            chunk_slots: 6,
            http_slots: 6,
            disk_write_slots: 6,
            ..TransferBudget::from_file_slots(1)
        };

        let probe_none = Arc::new(ProbeRunner::default());
        let summary_none = execute_dag(
            &build(),
            &TransferResourceManager::new(budget),
            runner_arc(Arc::clone(&probe_none)),
            noop_observer(),
            None,
        )
        .await
        .unwrap();

        let probe_some = Arc::new(ProbeRunner::default());
        let controller = Arc::new(AimdController::from_budget(&budget, AimdConfig::default()));
        let summary_some = execute_dag(
            &build(),
            &TransferResourceManager::new(budget),
            runner_arc(Arc::clone(&probe_some)),
            noop_observer(),
            Some(controller),
        )
        .await
        .unwrap();

        assert_eq!(summary_none, summary_some);
        assert_eq!(probe_none.peak(), probe_some.peak());
        assert_eq!(
            probe_some.peak(),
            6,
            "an at-ceiling controller adds no throttle to a congestion-free run"
        );
    }

    // --- DAG-P0-04: bounded dispatch window --------------------------------

    /// Wide independent frontier: concurrent runner work never exceeds the
    /// dispatch window (resource budget is deliberately larger so the window
    /// is the binding cap).
    #[tokio::test]
    async fn dispatch_window_caps_resident_tasks_on_wide_frontier() {
        const N: usize = 64;
        const WINDOW: usize = 8;

        let mut dag = TransferDag::default();
        for _ in 0..N {
            dag.add_node(
                TransferNodeKind::EmitProgress,
                vec![],
                ResourceRequest::default(),
            );
        }

        let probe = Arc::new(ProbeRunner::default());
        // Budget >> window so resource permits cannot explain the serialization.
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(N as u16));
        let summary = execute_dag_with_dispatch_window(
            &dag,
            &manager,
            runner_arc(Arc::clone(&probe)),
            noop_observer(),
            None,
            WINDOW,
        )
        .await
        .unwrap();

        assert_eq!(summary.nodes_completed, N as u32);
        assert_eq!(summary.nodes_failed, 0);
        assert!(
            probe.peak() <= WINDOW,
            "peak resident tasks {} exceeded dispatch_window {}",
            probe.peak(),
            WINDOW
        );
        // With sleep overlap and WINDOW slots free of resource pressure, we
        // should actually use more than one slot (otherwise the cap is a no-op
        // and the test would not prove concurrency still happens).
        assert!(
            probe.peak() >= 2,
            "expected overlapping work under window={WINDOW}, peak={}",
            probe.peak()
        );
    }

    /// Window of 1 forces full serialization even with unlimited resources.
    #[tokio::test]
    async fn dispatch_window_one_serializes_independent_nodes() {
        let mut dag = TransferDag::default();
        for _ in 0..5 {
            dag.add_node(
                TransferNodeKind::EmitProgress,
                vec![],
                ResourceRequest::default(),
            );
        }
        let probe = Arc::new(ProbeRunner::default());
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(8));
        let summary = execute_dag_with_dispatch_window(
            &dag,
            &manager,
            runner_arc(Arc::clone(&probe)),
            noop_observer(),
            None,
            1,
        )
        .await
        .unwrap();
        assert_eq!(summary.nodes_completed, 5);
        assert_eq!(probe.peak(), 1);
    }

    /// Dependencies still hold under a tight window (diamond + chain).
    #[tokio::test]
    async fn dispatch_window_respects_dependencies() {
        let mut dag = TransferDag::default();
        let a = dag.add_node(
            TransferNodeKind::PlanTransfer,
            vec![],
            ResourceRequest::default(),
        );
        let b = dag.add_node(
            TransferNodeKind::DownloadRange,
            vec![a],
            ResourceRequest::default(),
        );
        let c = dag.add_node(
            TransferNodeKind::DownloadRange,
            vec![a],
            ResourceRequest::default(),
        );
        let d = dag.add_node(
            TransferNodeKind::CommitTemp,
            vec![b, c],
            ResourceRequest::default(),
        );

        let probe = Arc::new(ProbeRunner::default());
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(4));
        let summary = execute_dag_with_dispatch_window(
            &dag,
            &manager,
            runner_arc(Arc::clone(&probe)),
            noop_observer(),
            None,
            2, // window=2: b and c may overlap, d waits for both
        )
        .await
        .unwrap();

        assert_eq!(summary.nodes_completed, 4);
        let order = probe.order();
        let pos = |id: usize| order.iter().position(|&x| x == id).unwrap();
        assert!(pos(a) < pos(b) && pos(a) < pos(c));
        assert!(pos(b) < pos(d) && pos(c) < pos(d));
    }

    /// Failure stops further dispatch; dependents of the failed node never run.
    /// Fail-fast also cancels the graph token so in-flight siblings exit.
    #[tokio::test]
    async fn dispatch_window_failure_does_not_release_dependents() {
        let mut dag = TransferDag::default();
        let a = dag.add_node(
            TransferNodeKind::PlanTransfer,
            vec![],
            ResourceRequest::default(),
        );
        let _b = dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![a],
            ResourceRequest::default(),
        );
        let _c = dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![a],
            ResourceRequest::default(),
        );

        let ran: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
        let ran_c = Arc::clone(&ran);
        let runner: Arc<dyn DagNodeRunner> = Arc::new(move |node: TransferNode| -> NodeFuture {
            let ran = Arc::clone(&ran_c);
            Box::pin(async move {
                ran.lock().unwrap().push(node.id);
                if node.id == 0 {
                    NodeOutcome::Failed(TransferError::from_message("plan failed"))
                } else {
                    NodeOutcome::Completed
                }
            })
        });
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(4));
        let err =
            execute_dag_with_dispatch_window(&dag, &manager, runner, noop_observer(), None, 4)
                .await
                .unwrap_err();

        match err {
            DagExecutionError::NodeFailed { node_id, .. } => assert_eq!(node_id, 0),
            other => panic!("expected NodeFailed, got {other:?}"),
        }
        let ran = ran.lock().unwrap().clone();
        assert!(ran.contains(&0));
        assert!(
            !ran.contains(&1) && !ran.contains(&2),
            "dependents of a failed node must not run, ran={ran:?}"
        );
    }

    /// Duplicate `depends_on` edges must not leave a node stuck (unique count).
    #[tokio::test]
    async fn dispatch_window_tolerates_duplicate_depends_on() {
        let mut dag = TransferDag::default();
        let a = dag.add_node(
            TransferNodeKind::PlanTransfer,
            vec![],
            ResourceRequest::default(),
        );
        // Same predecessor listed twice — still a single logical edge.
        let _b = dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![a, a],
            ResourceRequest::default(),
        );

        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(2));
        let summary = execute_dag_with_dispatch_window(
            &dag,
            &manager,
            Arc::new(|_n: TransferNode| -> NodeFuture {
                Box::pin(async { NodeOutcome::Completed })
            }),
            noop_observer(),
            None,
            2,
        )
        .await
        .unwrap();
        assert_eq!(summary.nodes_completed, 2);
    }

    /// Zero window is clamped to 1 (never unlimited, never a hang).
    #[tokio::test]
    async fn dispatch_window_zero_clamps_to_one() {
        let mut dag = TransferDag::default();
        for _ in 0..3 {
            dag.add_node(
                TransferNodeKind::EmitProgress,
                vec![],
                ResourceRequest::default(),
            );
        }
        let probe = Arc::new(ProbeRunner::default());
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(8));
        let summary = execute_dag_with_dispatch_window(
            &dag,
            &manager,
            runner_arc(Arc::clone(&probe)),
            noop_observer(),
            None,
            0,
        )
        .await
        .unwrap();
        assert_eq!(summary.nodes_completed, 3);
        assert_eq!(probe.peak(), 1);
    }

    /// Gate: 1M independent synthetic nodes complete with resident tasks always
    /// ≤ dispatch_window. Instant runner (no sleep) so the test stays cheap;
    /// peak is observed via atomic in-flight around the node body.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn dispatch_window_one_million_independent_nodes() {
        const N: usize = 1_000_000;
        const WINDOW: usize = 64;

        let mut dag = TransferDag::default();
        for _ in 0..N {
            dag.add_node(
                TransferNodeKind::EmitProgress,
                vec![],
                ResourceRequest::default(),
            );
        }
        assert_eq!(dag.nodes().len(), N);

        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let in_flight_r = Arc::clone(&in_flight);
        let peak_r = Arc::clone(&peak);
        let runner: Arc<dyn DagNodeRunner> = Arc::new(move |_node: TransferNode| -> NodeFuture {
            let in_flight = Arc::clone(&in_flight_r);
            let peak = Arc::clone(&peak_r);
            Box::pin(async move {
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                // Yield once so the runtime can actually overlap tasks inside
                // the window; without a yield, single-threaded scheduling can
                // run bodies back-to-back and under-report peak.
                tokio::task::yield_now().await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
                NodeOutcome::Completed
            })
        });

        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(1));
        let summary =
            execute_dag_with_dispatch_window(&dag, &manager, runner, noop_observer(), None, WINDOW)
                .await
                .unwrap();

        assert_eq!(summary.nodes_completed, N as u32);
        assert_eq!(summary.nodes_failed, 0);
        let observed = peak.load(Ordering::SeqCst);
        assert!(
            observed <= WINDOW,
            "peak resident tasks {observed} exceeded dispatch_window {WINDOW}"
        );
        // Sanity: we must have used the window, not accidentally serialized to 1
        // on a multi-thread runtime with yields. (If this flakes under extreme
        // load, the hard gate remains `observed <= WINDOW`.)
        assert!(
            observed >= 2,
            "expected some concurrency under window={WINDOW}, peak={observed}"
        );
    }

    // --- DAG-P0-05: graph-scoped cancel, fail-fast, typed timeout ----------

    /// First part failure cancels siblings; they exit well under the 2s grace.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fail_fast_cancels_siblings_under_two_seconds() {
        use std::sync::atomic::AtomicBool;
        use std::time::Instant;

        let mut dag = TransferDag::default();
        // Three independent parts (wide frontier, window large enough).
        for _ in 0..3 {
            dag.add_node(
                TransferNodeKind::UploadPart,
                vec![],
                ResourceRequest::default(),
            );
        }

        let fail_started = Arc::new(AtomicBool::new(false));
        let sibling_saw_cancel = Arc::new(AtomicUsize::new(0));
        let fail_started_r = Arc::clone(&fail_started);
        let sibling_r = Arc::clone(&sibling_saw_cancel);

        let runner: Arc<dyn DagNodeRunner> = Arc::new(move |node: TransferNode| -> NodeFuture {
            let fail_started = Arc::clone(&fail_started_r);
            let sibling_saw_cancel = Arc::clone(&sibling_r);
            Box::pin(async move {
                if node.id == 0 {
                    fail_started.store(true, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    NodeOutcome::Failed(TransferError::from_message("part 0 exploded"))
                } else {
                    // Cooperative long work: yields so the graph cancel can win.
                    for _ in 0..200 {
                        if fail_started.load(Ordering::SeqCst) {
                            // After the primary fails, the executor cancels the
                            // graph token and the wrapper returns Cancelled —
                            // this body may or may not observe it; either way
                            // the task must not run for seconds.
                            sibling_saw_cancel.fetch_add(1, Ordering::SeqCst);
                        }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    NodeOutcome::Completed
                }
            })
        });

        let observer = Arc::new(CollectingDagObserver::default());
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(8));
        let started = Instant::now();
        let err = execute_dag_with_options(
            &dag,
            &manager,
            runner,
            Arc::clone(&observer) as Arc<dyn DagObserver>,
            None,
            DagExecuteOptions {
                dispatch_window: 8,
                ..DagExecuteOptions::default()
            },
        )
        .await
        .unwrap_err();
        let elapsed = started.elapsed();

        match err {
            DagExecutionError::NodeFailed { node_id, error } => {
                assert_eq!(node_id, 0);
                assert!(error.message.contains("part 0 exploded"));
            }
            other => panic!("expected NodeFailed, got {other:?}"),
        }
        assert!(
            elapsed < Duration::from_secs(2),
            "fail-fast must finish under 2s, took {elapsed:?}"
        );

        // Every started node got a terminal observer event.
        let started_ids: HashSet<usize> = observer
            .started_nodes()
            .into_iter()
            .map(|(i, _)| i)
            .collect();
        let completed_ids: HashSet<usize> = observer
            .completed_nodes()
            .into_iter()
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            started_ids, completed_ids,
            "every started node needs a terminal outcome; started={started_ids:?} completed={completed_ids:?}"
        );
        assert!(started_ids.contains(&0));
    }

    /// A sibling that never checks cancel itself (long sleep, no race_cancel)
    /// is still terminated by the executor's graph-token select — not by an
    /// unbounded drain. Wall time stays well under the 2s fail-fast gate.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fail_fast_terminates_sibling_that_never_checks_cancel() {
        use std::time::Instant;

        let mut dag = TransferDag::default();
        dag.add_node(
            TransferNodeKind::UploadPart,
            vec![],
            ResourceRequest::default(),
        );
        dag.add_node(
            TransferNodeKind::UploadPart,
            vec![],
            ResourceRequest::default(),
        );

        let sibling_finished = Arc::new(AtomicUsize::new(0));
        let sibling_finished_r = Arc::clone(&sibling_finished);
        let runner: Arc<dyn DagNodeRunner> = Arc::new(move |node: TransferNode| -> NodeFuture {
            let sibling_finished = Arc::clone(&sibling_finished_r);
            Box::pin(async move {
                if node.id == 0 {
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    NodeOutcome::Failed(TransferError::from_message("primary failed"))
                } else {
                    // Deliberately non-self-checking: a long sleep with no
                    // cancel poll. The executor races graph_cancel around this
                    // future and must drop it promptly on fail-fast.
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    sibling_finished.fetch_add(1, Ordering::SeqCst);
                    NodeOutcome::Completed
                }
            })
        });

        let observer = Arc::new(CollectingDagObserver::default());
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(4));
        let started = Instant::now();
        let err = execute_dag(
            &dag,
            &manager,
            runner,
            Arc::clone(&observer) as Arc<dyn DagObserver>,
            None,
        )
        .await
        .unwrap_err();
        let elapsed = started.elapsed();

        assert!(matches!(
            err,
            DagExecutionError::NodeFailed { node_id: 0, .. }
        ));
        assert!(
            elapsed < Duration::from_secs(2),
            "non-self-checking sibling must be terminated under 2s, took {elapsed:?}"
        );
        assert_eq!(
            sibling_finished.load(Ordering::SeqCst),
            0,
            "sibling body must not run to completion after fail-fast"
        );

        let started_ids: HashSet<usize> = observer
            .started_nodes()
            .into_iter()
            .map(|(i, _)| i)
            .collect();
        let completed = observer.completed_nodes();
        let completed_ids: HashSet<usize> = completed.iter().map(|(i, _)| *i).collect();
        assert_eq!(started_ids, completed_ids);
        assert!(
            completed.iter().any(|(id, o)| *id == 1
                && matches!(o, ObservedOutcome::Cancelled | ObservedOutcome::Failed)),
            "sibling terminal missing or unexpected: {completed:?}"
        );
    }

    /// Node timeout is typed Timeout, never Cancelled.
    #[tokio::test]
    async fn node_timeout_returns_typed_timeout_not_cancelled() {
        let mut dag = TransferDag::default();
        dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![],
            ResourceRequest::default(),
        );

        let runner: Arc<dyn DagNodeRunner> = Arc::new(|_n: TransferNode| -> NodeFuture {
            Box::pin(async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                NodeOutcome::Completed
            })
        });
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(2));
        let err = execute_dag_with_options(
            &dag,
            &manager,
            runner,
            noop_observer(),
            None,
            DagExecuteOptions {
                node_timeout: Some(Duration::from_millis(50)),
                ..DagExecuteOptions::default()
            },
        )
        .await
        .unwrap_err();

        match err {
            DagExecutionError::NodeFailed { error, .. } => {
                assert_eq!(error.kind, TransferErrorKind::Timeout);
                assert_ne!(error.kind, TransferErrorKind::Cancelled);
            }
            other => panic!("expected NodeFailed, got {other:?}"),
        }
    }

    /// The per-node wall clock starts at dispatch, not only after resource
    /// acquisition. A node parked behind a valid-but-busy permit still times
    /// out with the typed timeout and its runner is never entered.
    #[tokio::test]
    async fn node_timeout_includes_resource_wait() {
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(1));
        let held = manager
            .acquire(ResourceRequest::upload_file())
            .await
            .expect("test must hold the only file permit");

        let mut dag = TransferDag::default();
        dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![],
            ResourceRequest::upload_file(),
        );
        let runner_calls = Arc::new(AtomicUsize::new(0));
        let runner_calls_c = Arc::clone(&runner_calls);
        let runner: Arc<dyn DagNodeRunner> = Arc::new(move |_n: TransferNode| -> NodeFuture {
            runner_calls_c.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { NodeOutcome::Completed })
        });

        let err = execute_dag_with_options(
            &dag,
            &manager,
            runner,
            noop_observer(),
            None,
            DagExecuteOptions {
                node_timeout: Some(Duration::from_millis(30)),
                ..DagExecuteOptions::default()
            },
        )
        .await
        .unwrap_err();
        drop(held);

        match err {
            DagExecutionError::NodeFailed { node_id, error } => {
                assert_eq!(node_id, 0);
                assert_eq!(error.kind, TransferErrorKind::Timeout);
            }
            other => panic!("expected NodeFailed, got {other:?}"),
        }
        assert_eq!(
            runner_calls.load(Ordering::SeqCst),
            0,
            "runner must not start while its resource request is blocked"
        );
    }

    /// A JoinError carries Tokio's task id. Preserve its task→node mapping so
    /// panic diagnostics identify the actual node rather than an arbitrary
    /// open sibling.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn task_panic_reports_exact_node_id() {
        let mut dag = TransferDag::default();
        dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![],
            ResourceRequest::default(),
        );
        let panicking = dag.add_node(
            TransferNodeKind::UploadFile,
            vec![],
            ResourceRequest::default(),
        );

        let runner: Arc<dyn DagNodeRunner> = Arc::new(move |node: TransferNode| -> NodeFuture {
            Box::pin(async move {
                if node.id == panicking {
                    panic!("synthetic node panic");
                }
                std::future::pending::<NodeOutcome>().await
            })
        });
        let observer = Arc::new(CollectingDagObserver::default());
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(2));
        let err = execute_dag(
            &dag,
            &manager,
            runner,
            Arc::clone(&observer) as Arc<dyn DagObserver>,
            None,
        )
        .await
        .unwrap_err();

        assert!(matches!(
            err,
            DagExecutionError::TaskPanicked { node_id, .. } if node_id == panicking
        ));
        let completed = observer.completed_nodes();
        assert_eq!(completed.len(), 2);
        let completed_ids: HashSet<usize> = completed.iter().map(|(id, _)| *id).collect();
        assert_eq!(completed_ids, HashSet::from([0, panicking]));
    }

    /// External parent cancel surfaces Cancelled, not Timeout, even when a
    /// node timeout is configured.
    #[tokio::test]
    async fn external_cancel_returns_cancelled_not_timeout() {
        let mut dag = TransferDag::default();
        dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![],
            ResourceRequest::default(),
        );

        let parent = CancellationToken::new();
        let parent_c = parent.clone();
        let runner: Arc<dyn DagNodeRunner> = Arc::new(move |_n: TransferNode| -> NodeFuture {
            Box::pin(async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                NodeOutcome::Completed
            })
        });

        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(2));
        let exec = execute_dag_with_options(
            &dag,
            &manager,
            runner,
            noop_observer(),
            None,
            DagExecuteOptions {
                parent_cancel: Some(parent),
                // A long node timeout must not win over external cancel.
                node_timeout: Some(Duration::from_secs(60)),
                ..DagExecuteOptions::default()
            },
        );
        let cancel_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(40)).await;
            parent_c.cancel();
        });
        let err = exec.await.unwrap_err();
        let _ = cancel_task.await;

        match err {
            DagExecutionError::NodeFailed { error, .. } => {
                assert_eq!(
                    error.kind,
                    TransferErrorKind::Cancelled,
                    "external cancel must not be reported as timeout"
                );
            }
            other => panic!("expected NodeFailed, got {other:?}"),
        }
    }

    /// After a part failure, CommitTemp (dependent) never runs.
    #[tokio::test]
    async fn fail_fast_never_runs_commit_after_part_failure() {
        let mut dag = TransferDag::default();
        let p0 = dag.add_node(
            TransferNodeKind::UploadPart,
            vec![],
            ResourceRequest::default(),
        );
        let p1 = dag.add_node(
            TransferNodeKind::UploadPart,
            vec![],
            ResourceRequest::default(),
        );
        let commit = dag.add_node(
            TransferNodeKind::CommitTemp,
            vec![p0, p1],
            ResourceRequest::default(),
        );

        let ran: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
        let ran_c = Arc::clone(&ran);
        let runner: Arc<dyn DagNodeRunner> = Arc::new(move |node: TransferNode| -> NodeFuture {
            let ran = Arc::clone(&ran_c);
            Box::pin(async move {
                ran.lock().unwrap().push(node.id);
                if node.id == p0 {
                    NodeOutcome::Failed(TransferError::from_message("part failed"))
                } else {
                    // Sibling would complete if allowed to finish without cancel;
                    // fail-fast may cancel it mid-flight.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    NodeOutcome::Completed
                }
            })
        });

        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(4));
        let _ = execute_dag(&dag, &manager, runner, noop_observer(), None)
            .await
            .unwrap_err();

        let ran = ran.lock().unwrap().clone();
        assert!(
            !ran.contains(&commit),
            "CommitTemp must not run after part failure, ran={ran:?}"
        );
    }

    /// Resource permits are released after fail-fast cancel of a holding node
    /// so a subsequent acquire does not hang.
    #[tokio::test]
    async fn fail_fast_releases_resource_permits() {
        // Two independent file-transfer nodes, budget of 1 file slot.
        // Node 0 holds the slot and fails after a short delay; node 1 must
        // still be able to run (or at least the manager must free the slot).
        // With fail-fast, node 1 may be cancelled if already started, but a
        // *second* graph on the same manager must acquire immediately.
        let mut dag = TransferDag::default();
        dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![],
            ResourceRequest::upload_file(),
        );
        dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![],
            ResourceRequest::upload_file(),
        );

        let runner: Arc<dyn DagNodeRunner> = Arc::new(|node: TransferNode| -> NodeFuture {
            Box::pin(async move {
                if node.id == 0 {
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    NodeOutcome::Failed(TransferError::from_message("boom"))
                } else {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    NodeOutcome::Completed
                }
            })
        });

        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(1));
        let _ = execute_dag(&dag, &manager, runner, noop_observer(), None)
            .await
            .unwrap_err();

        // Same manager, single-node graph: acquire must not hang on a leaked permit.
        let mut dag2 = TransferDag::default();
        dag2.add_node(
            TransferNodeKind::DownloadFile,
            vec![],
            ResourceRequest::upload_file(),
        );
        let runner2: Arc<dyn DagNodeRunner> = Arc::new(|_n: TransferNode| -> NodeFuture {
            Box::pin(async { NodeOutcome::Completed })
        });
        let summary = tokio::time::timeout(
            Duration::from_secs(2),
            execute_dag(&dag2, &manager, runner2, noop_observer(), None),
        )
        .await
        .expect("permit leak: second graph hung on acquire")
        .expect("second graph should succeed");
        assert_eq!(summary.nodes_completed, 1);
    }

    /// DAG-P1-04: continuing file failure increments nodes_failed, reports
    /// ObservedOutcome::Failed, releases dependents, and returns Ok.
    #[tokio::test]
    async fn continuing_file_failure_releases_dependents_and_returns_ok() {
        let mut dag = TransferDag::default();
        let fail_id = dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![],
            ResourceRequest::upload_file(),
        );
        let tail_id = dag.add_node(
            TransferNodeKind::VerifyChecksum,
            vec![fail_id],
            ResourceRequest::default(),
        );

        let ran_tail = Arc::new(AtomicUsize::new(0));
        let ran_tail_c = Arc::clone(&ran_tail);
        let runner: Arc<dyn DagNodeRunner> = Arc::new(move |node: TransferNode| -> NodeFuture {
            let ran_tail = Arc::clone(&ran_tail_c);
            Box::pin(async move {
                if node.id == fail_id {
                    NodeOutcome::FileFailedButGraphContinues(TransferError::new(
                        TransferErrorKind::RateLimited,
                        "HTTP 429",
                    ))
                } else if node.id == tail_id {
                    ran_tail.fetch_add(1, Ordering::SeqCst);
                    NodeOutcome::Completed
                } else {
                    NodeOutcome::Completed
                }
            })
        });
        let observer = Arc::new(CollectingDagObserver::default());
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(4));
        let summary = execute_dag(
            &dag,
            &manager,
            runner,
            Arc::clone(&observer) as Arc<dyn DagObserver>,
            None,
        )
        .await
        .expect("continuing failure must return Ok");

        assert_eq!(summary.nodes_failed, 1);
        assert_eq!(summary.nodes_completed, 1);
        assert_eq!(
            ran_tail.load(Ordering::SeqCst),
            1,
            "structural tail must drain"
        );
        let completed = observer.completed_nodes();
        assert!(
            completed
                .iter()
                .any(|(id, o)| *id == fail_id && *o == ObservedOutcome::Failed),
            "observer must see Failed once for the file terminal"
        );
        assert!(
            completed
                .iter()
                .any(|(id, o)| *id == tail_id && *o == ObservedOutcome::Completed),
            "tail must complete"
        );
    }

    /// DAG-P1-04 negative: fatal Failed still cancels siblings and does not
    /// release dependents.
    #[tokio::test]
    async fn fatal_failed_still_cancels_siblings_without_releasing_dependents() {
        let mut dag = TransferDag::default();
        let fail_id = dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![],
            ResourceRequest::upload_file(),
        );
        let _sibling = dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![],
            ResourceRequest::upload_file(),
        );
        let dependent = dag.add_node(
            TransferNodeKind::VerifyChecksum,
            vec![fail_id],
            ResourceRequest::default(),
        );

        let dependent_ran = Arc::new(AtomicUsize::new(0));
        let dependent_ran_c = Arc::clone(&dependent_ran);
        let runner: Arc<dyn DagNodeRunner> = Arc::new(move |node: TransferNode| -> NodeFuture {
            let dependent_ran = Arc::clone(&dependent_ran_c);
            Box::pin(async move {
                if node.id == fail_id {
                    NodeOutcome::Failed(TransferError::from_message("plan failed"))
                } else if node.id == dependent {
                    dependent_ran.fetch_add(1, Ordering::SeqCst);
                    NodeOutcome::Completed
                } else {
                    // Sibling: park long enough to be cancelled by fail-fast.
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    NodeOutcome::Completed
                }
            })
        });
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(4));
        let err = execute_dag(&dag, &manager, runner, noop_observer(), None)
            .await
            .expect_err("fatal failure must return Err");
        assert!(matches!(err, DagExecutionError::NodeFailed { .. }));
        assert_eq!(
            dependent_ran.load(Ordering::SeqCst),
            0,
            "dependents of a fatal failure must not run"
        );
    }

    /// File-local 429 at target 8 halves File to 4; a later independent file
    /// still completes.
    #[tokio::test]
    async fn continuing_429_halves_file_target_and_independent_file_completes() {
        use crate::transfer_dag::adaptive::{AdaptiveClass, AimdConfig, AimdController};

        let mut dag = TransferDag::default();
        let a = dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![],
            ResourceRequest::upload_file(),
        );
        // Independent second file (no dependency on a).
        let b = dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![],
            ResourceRequest::upload_file(),
        );

        let b_completed = Arc::new(AtomicUsize::new(0));
        let b_completed_c = Arc::clone(&b_completed);
        let runner: Arc<dyn DagNodeRunner> = Arc::new(move |node: TransferNode| -> NodeFuture {
            let b_completed = Arc::clone(&b_completed_c);
            Box::pin(async move {
                if node.id == a {
                    // Brief yield so both can be ready; a fails with 429.
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    NodeOutcome::FileFailedButGraphContinues(TransferError::new(
                        TransferErrorKind::RateLimited,
                        "HTTP 429 Too Many Requests",
                    ))
                } else if node.id == b {
                    // Start after a's failure so the decrease is visible.
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    b_completed.fetch_add(1, Ordering::SeqCst);
                    NodeOutcome::Completed
                } else {
                    NodeOutcome::Completed
                }
            })
        });
        let controller = Arc::new(AimdController::new(8, 1, 1, 1, AimdConfig::default()));
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(8));
        let summary = execute_dag(
            &dag,
            &manager,
            runner,
            noop_observer(),
            Some(Arc::clone(&controller)),
        )
        .await
        .expect("batch-like continuing failure returns Ok");

        assert_eq!(summary.nodes_failed, 1);
        assert_eq!(summary.nodes_completed, 1);
        assert_eq!(b_completed.load(Ordering::SeqCst), 1);
        assert_eq!(controller.target(AdaptiveClass::File), 4);
        assert_eq!(summary.metrics.backpressure_events, 1);
    }

    /// A decrease must gate newly released file work, not merely update the
    /// observable target while permits continue to flow at the old ceiling.
    #[tokio::test]
    async fn continuing_429_blocks_new_file_dispatch_until_live_falls_to_target() {
        use crate::transfer_dag::adaptive::{AdaptiveClass, AimdConfig, AimdController};
        use tokio::sync::{Barrier, Notify};

        let mut dag = TransferDag::default();
        let congested = dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![],
            ResourceRequest::upload_file(),
        );
        let blockers: Vec<_> = (0..4)
            .map(|_| {
                dag.add_node(
                    TransferNodeKind::DownloadFile,
                    vec![],
                    ResourceRequest::upload_file(),
                )
            })
            .collect();
        let released_after_failure = dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![congested],
            ResourceRequest::upload_file(),
        );

        // The failing node and four blockers all acquire from the initial
        // ceiling of eight before the failure is allowed to return.
        let initial_started = Arc::new(Barrier::new(5));
        let unblock_one = Arc::new(Notify::new());
        let dependent_started = Arc::new(Notify::new());
        let runner: Arc<dyn DagNodeRunner> = {
            let initial_started = Arc::clone(&initial_started);
            let unblock_one = Arc::clone(&unblock_one);
            let dependent_started = Arc::clone(&dependent_started);
            Arc::new(move |node: TransferNode| -> NodeFuture {
                let initial_started = Arc::clone(&initial_started);
                let unblock_one = Arc::clone(&unblock_one);
                let dependent_started = Arc::clone(&dependent_started);
                let is_blocker = blockers.contains(&node.id);
                Box::pin(async move {
                    if node.id == congested {
                        initial_started.wait().await;
                        NodeOutcome::FileFailedButGraphContinues(TransferError::new(
                            TransferErrorKind::RateLimited,
                            "redacted congestion",
                        ))
                    } else if is_blocker {
                        initial_started.wait().await;
                        unblock_one.notified().await;
                        NodeOutcome::Completed
                    } else if node.id == released_after_failure {
                        dependent_started.notify_one();
                        NodeOutcome::Completed
                    } else {
                        NodeOutcome::Completed
                    }
                })
            })
        };

        let controller = Arc::new(AimdController::new(8, 1, 1, 1, AimdConfig::default()));
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(8));
        let execution = {
            let controller = Arc::clone(&controller);
            tokio::spawn(async move {
                execute_dag(&dag, &manager, runner, noop_observer(), Some(controller)).await
            })
        };

        tokio::time::timeout(Duration::from_secs(2), async {
            while controller.target(AdaptiveClass::File) != 4 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("congestion feedback did not halve the target");

        // Four blockers still own the new target's four live permits, so the
        // just-released dependent file must remain parked at the AIMD gate.
        assert!(
            tokio::time::timeout(Duration::from_millis(50), dependent_started.notified())
                .await
                .is_err(),
            "new file dispatched while four live permits already filled target=4"
        );

        unblock_one.notify_one();
        tokio::time::timeout(Duration::from_secs(2), dependent_started.notified())
            .await
            .expect("new file did not dispatch after one live permit was released");
        unblock_one.notify_waiters();

        let summary = execution
            .await
            .expect("executor task panicked")
            .expect("continuing failure graph should drain");
        assert_eq!(summary.nodes_failed, 1);
        assert_eq!(summary.nodes_completed, 5);
    }

    /// Typed 503 / timeout / max-connections / connection-reset each trigger
    /// one file-class decrease without message parsing.
    #[tokio::test]
    async fn continuing_d2_kinds_each_trigger_file_decrease() {
        use crate::transfer_dag::adaptive::{AdaptiveClass, AimdConfig, AimdController};

        let kinds = [
            TransferErrorKind::ServiceUnavailable,
            TransferErrorKind::Timeout,
            TransferErrorKind::MaxConnections,
            TransferErrorKind::ConnectionReset,
        ];
        for kind in kinds {
            let mut dag = TransferDag::default();
            dag.add_node(
                TransferNodeKind::DownloadFile,
                vec![],
                ResourceRequest::upload_file(),
            );
            // Presentation message deliberately lacks status codes.
            let err = TransferError::new(kind, "redacted user-facing failure");
            let runner: Arc<dyn DagNodeRunner> = Arc::new(move |_n: TransferNode| -> NodeFuture {
                let e = err.clone();
                Box::pin(async move { NodeOutcome::FileFailedButGraphContinues(e) })
            });
            let controller = Arc::new(AimdController::new(8, 1, 1, 1, AimdConfig::default()));
            let manager = TransferResourceManager::new(TransferBudget::from_file_slots(8));
            let summary = execute_dag(
                &dag,
                &manager,
                runner,
                noop_observer(),
                Some(Arc::clone(&controller)),
            )
            .await
            .expect("Ok");
            assert_eq!(summary.nodes_failed, 1, "{kind:?}");
            assert_eq!(
                controller.target(AdaptiveClass::File),
                4,
                "{kind:?} must halve 8→4 without message parsing"
            );
        }
    }

    /// auth / not-found / permission / quota / local-I/O / remote-I/O / unknown
    /// must not change the File target.
    #[tokio::test]
    async fn continuing_non_congestion_kinds_do_not_shrink() {
        use crate::transfer_dag::adaptive::{AdaptiveClass, AimdConfig, AimdController};

        let kinds = [
            TransferErrorKind::Auth,
            TransferErrorKind::NotFound,
            TransferErrorKind::PermissionDenied,
            TransferErrorKind::QuotaExceeded,
            TransferErrorKind::LocalIo,
            TransferErrorKind::RemoteIo,
            TransferErrorKind::Unknown,
        ];
        for kind in kinds {
            let mut dag = TransferDag::default();
            dag.add_node(
                TransferNodeKind::DownloadFile,
                vec![],
                ResourceRequest::upload_file(),
            );
            let err = TransferError::new(kind, "redacted");
            let runner: Arc<dyn DagNodeRunner> = Arc::new(move |_n: TransferNode| -> NodeFuture {
                let e = err.clone();
                Box::pin(async move { NodeOutcome::FileFailedButGraphContinues(e) })
            });
            let controller = Arc::new(AimdController::new(8, 1, 1, 1, AimdConfig::default()));
            let manager = TransferResourceManager::new(TransferBudget::from_file_slots(8));
            let _ = execute_dag(
                &dag,
                &manager,
                runner,
                noop_observer(),
                Some(Arc::clone(&controller)),
            )
            .await
            .expect("Ok");
            assert_eq!(
                controller.target(AdaptiveClass::File),
                8,
                "{kind:?} must not shrink"
            );
        }
    }

    /// Cancellation continuing outcome is observer-Cancelled and not congestion.
    #[tokio::test]
    async fn continuing_cancellation_is_not_congestion() {
        use crate::transfer_dag::adaptive::{AdaptiveClass, AimdConfig, AimdController};

        let mut dag = TransferDag::default();
        let id = dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![],
            ResourceRequest::upload_file(),
        );
        let runner: Arc<dyn DagNodeRunner> = Arc::new(|_n: TransferNode| -> NodeFuture {
            Box::pin(async { NodeOutcome::FileFailedButGraphContinues(TransferError::cancelled()) })
        });
        let observer = Arc::new(CollectingDagObserver::default());
        let controller = Arc::new(AimdController::new(8, 1, 1, 1, AimdConfig::default()));
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(8));
        let summary = execute_dag(
            &dag,
            &manager,
            runner,
            Arc::clone(&observer) as Arc<dyn DagObserver>,
            Some(Arc::clone(&controller)),
        )
        .await
        .expect("Ok");
        assert_eq!(summary.nodes_failed, 1);
        assert_eq!(controller.target(AdaptiveClass::File), 8);
        assert_eq!(controller.cooldown_until(AdaptiveClass::File), None);
        assert!(observer
            .completed_nodes()
            .iter()
            .any(|(nid, o)| *nid == id && *o == ObservedOutcome::Cancelled));
    }

    /// Retry-After on a continuing congestion failure arms the controller cooldown.
    #[tokio::test]
    async fn continuing_failure_honors_typed_retry_after() {
        use crate::transfer_dag::adaptive::{AdaptiveClass, AimdConfig, AimdController};
        use std::time::Instant;

        let mut dag = TransferDag::default();
        dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![],
            ResourceRequest::upload_file(),
        );
        let mut err = TransferError::new(
            TransferErrorKind::RateLimited,
            "Transfer rate limit reached",
        );
        err.retry_after = Some(Duration::from_secs(45));
        let runner: Arc<dyn DagNodeRunner> = Arc::new(move |_n: TransferNode| -> NodeFuture {
            let e = err.clone();
            Box::pin(async move { NodeOutcome::FileFailedButGraphContinues(e) })
        });
        let controller = Arc::new(AimdController::new(8, 1, 1, 1, AimdConfig::default()));
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(8));
        let before = Instant::now();
        let _ = execute_dag(
            &dag,
            &manager,
            runner,
            noop_observer(),
            Some(Arc::clone(&controller)),
        )
        .await
        .expect("Ok");
        assert_eq!(controller.target(AdaptiveClass::File), 4);
        let until = controller
            .cooldown_until(AdaptiveClass::File)
            .expect("cooldown armed");
        let armed = until.saturating_duration_since(before);
        assert!(
            armed >= Duration::from_secs(44) && armed <= Duration::from_secs(46),
            "expected ~45s, got {armed:?}"
        );
    }

    /// AIMD kill switch leaves the target at the honest ceiling on continuing
    /// congestion.
    #[tokio::test]
    async fn continuing_failure_aimd_disabled_is_noop() {
        use crate::transfer_dag::adaptive::{AdaptiveClass, AimdConfig, AimdController};

        let mut dag = TransferDag::default();
        dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![],
            ResourceRequest::upload_file(),
        );
        let runner: Arc<dyn DagNodeRunner> = Arc::new(|_n: TransferNode| -> NodeFuture {
            Box::pin(async {
                NodeOutcome::FileFailedButGraphContinues(TransferError::new(
                    TransferErrorKind::RateLimited,
                    "429",
                ))
            })
        });
        let cfg = AimdConfig {
            disabled: true,
            ..AimdConfig::default()
        };
        let controller = Arc::new(AimdController::new(8, 1, 1, 1, cfg));
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(8));
        let _ = execute_dag(
            &dag,
            &manager,
            runner,
            noop_observer(),
            Some(Arc::clone(&controller)),
        )
        .await
        .expect("Ok");
        assert_eq!(controller.target(AdaptiveClass::File), 8);
    }

    /// CommitTemp-style terminal (zero resource request) still feeds File-class
    /// AIMD once; no false healthy signal.
    #[tokio::test]
    async fn continuing_failure_on_empty_request_uses_file_class() {
        use crate::transfer_dag::adaptive::{AdaptiveClass, AimdConfig, AimdController};

        let mut dag = TransferDag::default();
        // CommitTemp has no transfer resource classes.
        dag.add_node(
            TransferNodeKind::CommitTemp,
            vec![],
            ResourceRequest::default(),
        );
        let runner: Arc<dyn DagNodeRunner> = Arc::new(|_n: TransferNode| -> NodeFuture {
            Box::pin(async {
                NodeOutcome::FileFailedButGraphContinues(TransferError::new(
                    TransferErrorKind::RateLimited,
                    "part failed at commit",
                ))
            })
        });
        let controller = Arc::new(AimdController::new(8, 1, 1, 1, AimdConfig::default()));
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(8));
        let summary = execute_dag(
            &dag,
            &manager,
            runner,
            noop_observer(),
            Some(Arc::clone(&controller)),
        )
        .await
        .expect("Ok");
        assert_eq!(summary.nodes_failed, 1);
        assert_eq!(summary.nodes_completed, 0);
        assert_eq!(controller.target(AdaptiveClass::File), 4);
        // Chunk class must be untouched (no false multi-class feedback).
        assert_eq!(controller.target(AdaptiveClass::Chunk), 1);
    }

    /// Happy path and fallback still complete under options API.
    #[tokio::test]
    async fn options_default_preserves_happy_path_and_fallback() {
        let mut dag = TransferDag::default();
        let a = dag.add_node(
            TransferNodeKind::PlanTransfer,
            vec![],
            ResourceRequest::default(),
        );
        let _b = dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![a],
            ResourceRequest::default(),
        );

        let runner: Arc<dyn DagNodeRunner> = Arc::new(|node: TransferNode| -> NodeFuture {
            Box::pin(async move {
                if node.id == 0 {
                    NodeOutcome::Completed
                } else {
                    NodeOutcome::Fallback
                }
            })
        });
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(4));
        let summary = execute_dag_with_options(
            &dag,
            &manager,
            runner,
            noop_observer(),
            None,
            DagExecuteOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(summary.nodes_completed, 2);
        assert_eq!(summary.fallback_count, 1);
        assert_eq!(summary.metrics.range_fallbacks, 1);
    }
}
