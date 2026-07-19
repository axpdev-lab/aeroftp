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
//! The executor is active in the single-file, batch, and sync DAG wrappers;
//! the segmented-range wrapper also uses it when its graph path is selected.
//! Those callers bind different amounts of real I/O, so using this scheduler
//! does not by itself imply identical wire-level parallelism across surfaces.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;

use super::adaptive::{congestion_from_error, AimdController};
use super::error::TransferError;
use super::graph::{TransferDag, TransferNode};
use super::metrics::TransferDagMetrics;
use super::observer::{DagObserver, ObservedOutcome};
use super::resources::{ResourceRequest, TransferBudget, TransferResourceManager};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DagExecutionSummary {
    pub nodes_completed: u32,
    pub nodes_failed: u32,
    pub fallback_count: u32,
    /// Accumulated run metrics. In this slice only the truthfully known
    /// fields are populated (`range_fallbacks` from fallback completions);
    /// bytes/retries/backpressure stay zero until a real path is migrated
    /// and adaptive throttling lands. No value is fabricated.
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
    /// Node failed. Scheduling stops, in-flight nodes are drained, and the
    /// typed error is propagated (kind / retry / Retry-After are machine-safe;
    /// presentation lives in [`TransferError::message`]).
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
    /// A node action returned [`NodeOutcome::Failed`]. Scheduling stopped and
    /// in-flight nodes were drained before returning.
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

/// Returns a human-readable reason if `request` can never be satisfied by
/// `budget` (more permits of some class than the manager will ever own).
fn request_exceeds_budget(request: &ResourceRequest, budget: &TransferBudget) -> Option<String> {
    // Each semaphore is sized at `budget.X.max(1)` (see TransferResourceManager).
    let checks: [(&str, u16, u16); 8] = [
        ("file_slots", request.file_slots, budget.file_slots.max(1)),
        (
            "checker_slots",
            request.checker_slots,
            budget.checker_slots.max(1),
        ),
        (
            "chunk_slots",
            request.chunk_slots,
            budget.chunk_slots.max(1),
        ),
        ("http_slots", request.http_slots, budget.http_slots.max(1)),
        ("api_slots", request.api_slots, budget.api_slots.max(1)),
        (
            "disk_read_slots",
            request.disk_read_slots,
            budget.disk_read_slots.max(1),
        ),
        (
            "disk_write_slots",
            request.disk_write_slots,
            budget.disk_write_slots.max(1),
        ),
        ("hash_slots", request.hash_slots, budget.hash_slots.max(1)),
    ];
    for (name, want, have) in checks {
        if want > have {
            return Some(format!(
                "requests {want} {name} but the budget owns only {have}"
            ));
        }
    }
    None
}

/// Default maximum number of concurrently resident dispatched tasks.
///
/// Resource permits remain the binding limit for normal transfer graphs
/// (multipart fan-out is typically far smaller). This cap prevents a wide
/// ready frontier — e.g. a million independent batch nodes — from spawning
/// an unbounded `JoinSet` of futures. A node is marked started and observed
/// only when it enters this window, not when it merely becomes ready.
pub const DEFAULT_DISPATCH_WINDOW: usize = 256;

/// Run `dag` to completion on a ready-frontier schedule with
/// [`DEFAULT_DISPATCH_WINDOW`].
///
/// See [`execute_dag_with_dispatch_window`] for full semantics.
pub async fn execute_dag(
    dag: &TransferDag,
    manager: &TransferResourceManager,
    runner: Arc<dyn DagNodeRunner>,
    observer: Arc<dyn DagObserver>,
    controller: Option<Arc<AimdController>>,
) -> Result<DagExecutionSummary, DagExecutionError> {
    execute_dag_with_dispatch_window(
        dag,
        manager,
        runner,
        observer,
        controller,
        DEFAULT_DISPATCH_WINDOW,
    )
    .await
}

/// Run `dag` to completion on a ready-frontier schedule with an explicit
/// dispatch window.
///
/// Each node, once its dependencies are complete, enters a ready queue and is
/// dispatched only while fewer than `dispatch_window` tasks are resident in
/// the `JoinSet`. Dispatched nodes acquire their `ResourceRequest` from
/// `manager` before the action runs; the lease is held until the action
/// returns. I/O concurrency is therefore still bounded by the manager's
/// semaphores; the window is an orthogonal residency cap.
///
/// On the first failed node, no further nodes are dispatched, in-flight nodes
/// are drained, and [`DagExecutionError::NodeFailed`] is returned (matching
/// the abort-on-error semantics of the converged single-file path this
/// executor will host). Failed nodes do not release dependents.
///
/// `observer` receives node lifecycle and a final [`TransferDagMetrics`]
/// snapshot through an `AppHandle`-free abstraction. It defaults to a no-op
/// for non-GUI callers; the metrics it reports are the truthfully known ones
/// only (no fabricated throughput/retry counts before a real path is wired).
///
/// `controller`, when `Some`, throttles the dispatch step with a prudent
/// AIMD loop: each dispatched node first acquires per-class dispatch permits
/// from the controller and holds them for its lifetime, so shrinking the
/// concurrency target only withholds *future* dispatches and never aborts an
/// in-flight transfer. Congestion is mapped from the typed
/// [`TransferError`] on the failure (kind + optional `retry_after`); the
/// controller never substring-matches presentation text. `None` is the
/// previous behaviour exactly for congestion-free runs.
///
/// `dispatch_window` is clamped to at least 1. After the dependency index is
/// built, readiness dispatch visits every node and edge once (O(V+E)).
/// Building that index first normalizes duplicate predecessor ids with
/// `sort_unstable` + `dedup`, so total scheduler setup is
/// O(V + sum(d_i log d_i)), not strictly O(V+E). This avoids a per-node hash
/// allocation while keeping million-wide independent frontiers linear.
pub async fn execute_dag_with_dispatch_window(
    dag: &TransferDag,
    manager: &TransferResourceManager,
    runner: Arc<dyn DagNodeRunner>,
    observer: Arc<dyn DagObserver>,
    controller: Option<Arc<AimdController>>,
    dispatch_window: usize,
) -> Result<DagExecutionSummary, DagExecutionError> {
    let dispatch_window = dispatch_window.max(1);
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
    let mut requests: HashMap<usize, ResourceRequest> = HashMap::new();
    let mut failed: Option<(usize, TransferError)> = None;
    let mut terminal_error: Option<DagExecutionError> = None;
    let mut join_set: JoinSet<(usize, NodeOutcome)> = JoinSet::new();

    loop {
        // Dispatch step: fill the window from the ready queue. Once a node
        // has failed we stop launching new work and only drain what is
        // already running. This single step is the point AIMD gates adaptively.
        if failed.is_none() {
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
                join_set.spawn(async move {
                    let id = node.id;
                    let request = node.resources;
                    // Adaptive dispatch gate (held for the node's lifetime):
                    // a shrink only parks not-yet-started nodes here, never
                    // an in-flight transfer.
                    let _dispatch = match &controller {
                        Some(ctrl) => Some(ctrl.acquire(&request).await),
                        None => None,
                    };
                    match manager.acquire(request).await {
                        Ok(_lease) => {
                            let outcome = runner.run(node).await;
                            (id, outcome)
                        }
                        Err(e) => (
                            id,
                            NodeOutcome::Failed(TransferError::resource_acquire(format!(
                                "resource acquire failed: {e}"
                            ))),
                        ),
                    }
                });
            }
        }

        if join_set.is_empty() {
            if failed.is_some() || completed.len() == n {
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

        let joined = join_set
            .join_next()
            .await
            .expect("join_set checked non-empty above");
        let (id, outcome) = match joined {
            Ok(pair) => pair,
            Err(join_err) => {
                let node_id = guess_panicked_node(&started, &completed);
                terminal_error = Some(DagExecutionError::TaskPanicked {
                    node_id,
                    message: join_err.to_string(),
                });
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
                observer.on_node_complete(id, ObservedOutcome::Completed);
                aimd_note_healthy(&controller, node_request.as_ref());
                release_dependents(id, &dependents, &mut remaining_deps, &mut ready);
            }
            NodeOutcome::Fallback => {
                summary.nodes_completed += 1;
                summary.fallback_count += 1;
                // A node taking its degraded path is the one metric this
                // slice can populate truthfully.
                summary.metrics.range_fallbacks += 1;
                completed.insert(id);
                observer.on_node_complete(id, ObservedOutcome::Fallback);
                // A successful (if degraded) completion is still a healthy
                // signal for the concurrency controller.
                aimd_note_healthy(&controller, node_request.as_ref());
                release_dependents(id, &dependents, &mut remaining_deps, &mut ready);
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
                if failed.is_none() {
                    failed = Some((id, error));
                }
                observer.on_node_complete(id, ObservedOutcome::Failed);
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
        return Err(DagExecutionError::NodeFailed { node_id, error });
    }
    Ok(summary)
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

/// Best-effort node id for a panicking task: a started-but-not-completed node.
/// `JoinError` does not carry the task's payload, so this is informational.
fn guess_panicked_node(started: &HashSet<usize>, completed: &HashSet<usize>) -> usize {
    started
        .iter()
        .copied()
        .find(|id| !completed.contains(id))
        .unwrap_or(usize::MAX)
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
                ResourceRequest::file_transfer(),
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
            ResourceRequest::file_transfer(),
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
            ResourceRequest::file_transfer(),
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
            ResourceRequest::file_transfer(),
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
            ResourceRequest::file_transfer(),
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
            ResourceRequest::file_transfer(),
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
    /// In-flight siblings may still complete (drain semantics — fail-fast cancel
    /// is P0-05).
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
}
