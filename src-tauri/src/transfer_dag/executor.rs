// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Ready-frontier executor for the transfer node graph.
//!
//! This walks a [`TransferDag`](super::graph::TransferDag): it keeps a
//! completed set, repeatedly selects the nodes whose `depends_on` are all
//! satisfied (the ready frontier), and dispatches them concurrently. Each
//! dispatched node first acquires its `ResourceRequest` permits from the
//! shared [`TransferResourceManager`], so real concurrency is bounded by the
//! per-class semaphores, not by the scheduler. The scheduler exposes a single
//! dispatch step, which is the point a later slice throttles adaptively.
//!
//! It is additive: nothing dispatches a graph yet. Existing GUI/CLI transfer
//! paths are unchanged until one path is migrated onto this executor behind a
//! flag with a byte-identical guarantee.

use std::collections::HashSet;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;

use super::graph::{TransferDag, TransferNode};
use super::resources::{ResourceRequest, TransferBudget, TransferResourceManager};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DagExecutionSummary {
    pub nodes_completed: u32,
    pub nodes_failed: u32,
    pub fallback_count: u32,
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
    /// error is propagated.
    Failed(String),
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
    NodeFailed { node_id: usize, message: String },
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
            DagExecutionError::NodeFailed { node_id, message } => {
                write!(f, "node {node_id} failed: {message}")
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

/// Run `dag` to completion on a ready-frontier schedule.
///
/// Each node, once its dependencies are complete, is dispatched on the runtime
/// and acquires its `ResourceRequest` from `manager` before its action runs;
/// the lease is held until the action returns. Concurrency is bounded by the
/// manager's semaphores. On the first failed node, no further nodes are
/// dispatched, in-flight nodes are drained, and [`DagExecutionError::NodeFailed`]
/// is returned (matching the abort-on-error semantics of the converged
/// single-file path this executor will host).
pub async fn execute_dag(
    dag: &TransferDag,
    manager: &TransferResourceManager,
    runner: Arc<dyn DagNodeRunner>,
) -> Result<DagExecutionSummary, DagExecutionError> {
    let nodes = dag.nodes();
    let mut summary = DagExecutionSummary::default();
    if nodes.is_empty() {
        return Ok(summary);
    }

    // Pre-flight: a node demanding more of a class than the manager owns would
    // wait on the semaphore forever. Fail it honestly instead of hanging.
    let budget = manager.budget();
    for node in nodes {
        if let Some(reason) = request_exceeds_budget(&node.resources, &budget) {
            return Err(DagExecutionError::Unschedulable {
                node_id: node.id,
                message: reason,
            });
        }
    }

    let mut completed: HashSet<usize> = HashSet::new();
    let mut started: HashSet<usize> = HashSet::new();
    let mut failed: Option<(usize, String)> = None;
    let mut join_set: JoinSet<(usize, NodeOutcome)> = JoinSet::new();

    loop {
        // Dispatch step. Once a node has failed we stop launching new work and
        // only drain what is already running. This single step is the point a
        // later slice gates adaptively.
        if failed.is_none() {
            let ready: Vec<TransferNode> = nodes
                .iter()
                .filter(|n| {
                    !started.contains(&n.id) && n.depends_on.iter().all(|d| completed.contains(d))
                })
                .cloned()
                .collect();
            for node in ready {
                started.insert(node.id);
                let manager = manager.clone();
                let runner = Arc::clone(&runner);
                join_set.spawn(async move {
                    let id = node.id;
                    let request = node.resources;
                    match manager.acquire(request).await {
                        Ok(_lease) => {
                            let outcome = runner.run(node).await;
                            (id, outcome)
                        }
                        Err(e) => (
                            id,
                            NodeOutcome::Failed(format!("resource acquire failed: {e}")),
                        ),
                    }
                });
            }
        }

        if join_set.is_empty() {
            if failed.is_some() || started.len() == nodes.len() {
                break;
            }
            // Nothing ready, nothing running, not everything started: a
            // dependency can never be satisfied.
            return Err(DagExecutionError::Stuck {
                pending: nodes.len() - completed.len(),
            });
        }

        let joined = join_set
            .join_next()
            .await
            .expect("join_set checked non-empty above");
        let (id, outcome) = match joined {
            Ok(pair) => pair,
            Err(join_err) => {
                let node_id = guess_panicked_node(&started, &completed);
                return Err(DagExecutionError::TaskPanicked {
                    node_id,
                    message: join_err.to_string(),
                });
            }
        };
        match outcome {
            NodeOutcome::Completed => {
                summary.nodes_completed += 1;
                completed.insert(id);
            }
            NodeOutcome::Fallback => {
                summary.nodes_completed += 1;
                summary.fallback_count += 1;
                completed.insert(id);
            }
            NodeOutcome::Failed(message) => {
                summary.nodes_failed += 1;
                if failed.is_none() {
                    failed = Some((id, message));
                }
            }
        }
    }

    if let Some((node_id, message)) = failed {
        return Err(DagExecutionError::NodeFailed { node_id, message });
    }
    Ok(summary)
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

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
        let summary = execute_dag(&dag, &manager, runner_arc(Arc::clone(&probe)))
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
        let summary = execute_dag(&dag, &manager, runner_arc(Arc::clone(&probe)))
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
        let summary = execute_dag(&dag, &manager, runner_arc(Arc::clone(&probe)))
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
        let summary = execute_dag(&dag, &manager, runner_arc(Arc::clone(&probe)))
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
                    NodeOutcome::Failed("synthetic plan failure".into())
                } else {
                    NodeOutcome::Completed
                }
            })
        });
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(4));
        let err = execute_dag(&dag, &manager, runner).await.unwrap_err();

        match err {
            DagExecutionError::NodeFailed { node_id, message } => {
                assert_eq!(node_id, 0);
                assert!(message.contains("synthetic plan failure"));
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
        )
        .await
        .unwrap();

        assert_eq!(summary.nodes_completed, 1);
        assert_eq!(summary.fallback_count, 1);
        assert_eq!(summary.nodes_failed, 0);
    }
}
