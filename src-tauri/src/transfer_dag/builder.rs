// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! `TransferDagBuilder`: shared construction of production transfer graphs.
//!
//! DAG-ENGINE phase 1 converges the single-file download and upload paths,
//! GUI and CLI, onto one [`TransferDag`] shape so every surface schedules the
//! same node graph through [`execute_dag`](super::executor::execute_dag).
//!
//! The single-file graph is a linear chain. A single file transfer is
//! inherently sequential (discover before acquire before transfer before
//! verify ...), so routing it through the DAG buys not parallelism but
//! structural uniformity with the batch and sync graphs (phase 2) and a
//! single observability surface, instead of a hand-rolled orchestration per
//! call site.
//!
//! Node responsibilities, in phase 1:
//!
//! - `DiscoverRemote` / `DiscoverLocal`: resolve the transfer size (a remote
//!   `size()` call, or a local `metadata()` read).
//! - `AcquireResource`: structural anchor. A no-op in phase 1; phase 3 hooks
//!   the resume-checkpoint fetch here.
//! - `DownloadFile` / `UploadFile`: the real I/O. The only node that carries a
//!   scarce resource ([`ResourceRequest::file_transfer`]).
//! - `VerifyChecksum`: structural anchor. A no-op in phase 1 (the legacy
//!   single-file path does not verify); phase 3 makes it real behind the
//!   `server_checksum` capability.
//! - `PreserveMetadata`: restore the remote mtime on a downloaded file. A
//!   no-op on the upload direction.
//! - `CommitTemp`: structural anchor. A no-op in phase 1 because every
//!   provider's own `download` / `upload` already performs the atomic
//!   `.aerotmp` finalize internally (since v3.0.5); phase 3 may host an
//!   explicit resume-checkpoint commit here.
//! - `EmitProgress`: emit the terminal completion event.
//!
//! Temp-file cleanup on failure is intentionally NOT a graph node. The
//! ready-frontier executor aborts on the first failed node and drains the
//! in-flight set; it never dispatches a node whose dependency failed, so a
//! `CleanupTemp` node could not run on the failure path. Cleanup stays an
//! RAII concern of the transfer runner (the provider's own `.aerotmp` guard),
//! which is the honest place for it.

use super::graph::{TransferDag, TransferNodeKind};
use super::resources::ResourceRequest;

/// Transfer direction for [`TransferDagBuilder::single_file`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    Download,
    Upload,
}

/// A built single-file transfer graph plus the id of every node, so a runner
/// can bind each [`TransferNodeKind`] to its action without re-scanning the
/// graph. Every kind appears exactly once in a single-file graph, so a runner
/// may equally match on [`TransferNodeKind`] directly; the ids are kept for
/// callers and tests that prefer an explicit handle.
#[derive(Debug, Clone)]
pub struct SingleFileDag {
    /// The graph, ready for [`execute_dag`](super::executor::execute_dag).
    pub dag: TransferDag,
    /// The direction this graph was built for.
    pub direction: TransferDirection,
    /// `DiscoverRemote` (download) or `DiscoverLocal` (upload).
    pub discover: usize,
    /// `AcquireResource`.
    pub acquire: usize,
    /// `DownloadFile` (download) or `UploadFile` (upload): the I/O node.
    pub transfer: usize,
    /// `VerifyChecksum`.
    pub verify: usize,
    /// `PreserveMetadata`.
    pub preserve_metadata: usize,
    /// `CommitTemp`.
    pub commit: usize,
    /// `EmitProgress`: the terminal node.
    pub emit_progress: usize,
}

/// Stateless constructor of shared production transfer graphs.
pub struct TransferDagBuilder;

impl TransferDagBuilder {
    /// Build the single-file transfer graph for `direction`.
    ///
    /// Seven nodes, a linear chain:
    /// `Discover{Remote|Local}` → `AcquireResource` → `{Download|Upload}File`
    /// → `VerifyChecksum` → `PreserveMetadata` → `CommitTemp` → `EmitProgress`.
    ///
    /// Only the transfer node carries a scarce resource
    /// ([`ResourceRequest::file_transfer`]); every other node is a metadata
    /// or structural step with no resource request, so they never contend on
    /// the shared semaphores and the graph cannot deadlock against its own
    /// budget.
    pub fn single_file(direction: TransferDirection) -> SingleFileDag {
        let mut dag = TransferDag::default();

        let (discover_kind, transfer_kind) = match direction {
            TransferDirection::Download => (
                TransferNodeKind::DiscoverRemote,
                TransferNodeKind::DownloadFile,
            ),
            TransferDirection::Upload => {
                (TransferNodeKind::DiscoverLocal, TransferNodeKind::UploadFile)
            }
        };

        let discover = dag.add_node(discover_kind, vec![], ResourceRequest::default());
        let acquire = dag.add_node(
            TransferNodeKind::AcquireResource,
            vec![discover],
            ResourceRequest::default(),
        );
        let transfer = dag.add_node(transfer_kind, vec![acquire], ResourceRequest::file_transfer());
        let verify = dag.add_node(
            TransferNodeKind::VerifyChecksum,
            vec![transfer],
            ResourceRequest::default(),
        );
        let preserve_metadata = dag.add_node(
            TransferNodeKind::PreserveMetadata,
            vec![verify],
            ResourceRequest::default(),
        );
        let commit = dag.add_node(
            TransferNodeKind::CommitTemp,
            vec![preserve_metadata],
            ResourceRequest::default(),
        );
        let emit_progress = dag.add_node(
            TransferNodeKind::EmitProgress,
            vec![commit],
            ResourceRequest::default(),
        );

        SingleFileDag {
            dag,
            direction,
            discover,
            acquire,
            transfer,
            verify,
            preserve_metadata,
            commit,
            emit_progress,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer_dag::executor::{execute_dag, DagNodeRunner, NodeFuture, NodeOutcome};
    use crate::transfer_dag::graph::TransferNode;
    use crate::transfer_dag::observer::NoopDagObserver;
    use crate::transfer_dag::resources::{TransferBudget, TransferResourceManager};
    use std::sync::Arc;

    #[test]
    fn download_graph_has_seven_nodes_in_order() {
        let built = TransferDagBuilder::single_file(TransferDirection::Download);
        let nodes = built.dag.nodes();

        assert_eq!(nodes.len(), 7);
        assert_eq!(nodes[0].kind, TransferNodeKind::DiscoverRemote);
        assert_eq!(nodes[1].kind, TransferNodeKind::AcquireResource);
        assert_eq!(nodes[2].kind, TransferNodeKind::DownloadFile);
        assert_eq!(nodes[3].kind, TransferNodeKind::VerifyChecksum);
        assert_eq!(nodes[4].kind, TransferNodeKind::PreserveMetadata);
        assert_eq!(nodes[5].kind, TransferNodeKind::CommitTemp);
        assert_eq!(nodes[6].kind, TransferNodeKind::EmitProgress);
    }

    #[test]
    fn upload_graph_swaps_only_the_discover_and_transfer_kinds() {
        let built = TransferDagBuilder::single_file(TransferDirection::Upload);
        let nodes = built.dag.nodes();

        assert_eq!(nodes.len(), 7);
        assert_eq!(nodes[0].kind, TransferNodeKind::DiscoverLocal);
        assert_eq!(nodes[1].kind, TransferNodeKind::AcquireResource);
        assert_eq!(nodes[2].kind, TransferNodeKind::UploadFile);
        assert_eq!(nodes[3].kind, TransferNodeKind::VerifyChecksum);
        assert_eq!(nodes[4].kind, TransferNodeKind::PreserveMetadata);
        assert_eq!(nodes[5].kind, TransferNodeKind::CommitTemp);
        assert_eq!(nodes[6].kind, TransferNodeKind::EmitProgress);
    }

    #[test]
    fn nodes_form_a_strict_linear_chain() {
        let built = TransferDagBuilder::single_file(TransferDirection::Download);
        let nodes = built.dag.nodes();

        assert_eq!(nodes[0].depends_on, Vec::<usize>::new());
        for i in 1..nodes.len() {
            assert_eq!(
                nodes[i].depends_on,
                vec![i - 1],
                "node {i} must depend solely on node {}",
                i - 1
            );
        }
    }

    #[test]
    fn named_ids_match_node_positions() {
        let built = TransferDagBuilder::single_file(TransferDirection::Download);

        assert_eq!(built.discover, 0);
        assert_eq!(built.acquire, 1);
        assert_eq!(built.transfer, 2);
        assert_eq!(built.verify, 3);
        assert_eq!(built.preserve_metadata, 4);
        assert_eq!(built.commit, 5);
        assert_eq!(built.emit_progress, 6);
    }

    #[test]
    fn only_the_transfer_node_carries_a_scarce_resource() {
        let built = TransferDagBuilder::single_file(TransferDirection::Upload);
        let nodes = built.dag.nodes();

        for node in nodes {
            if node.id == built.transfer {
                assert_eq!(node.resources, ResourceRequest::file_transfer());
            } else {
                assert_eq!(
                    node.resources,
                    ResourceRequest::default(),
                    "node {} ({:?}) must not reserve a resource",
                    node.id,
                    node.kind
                );
            }
        }
    }

    #[tokio::test]
    async fn built_graph_runs_to_completion_on_the_executor() {
        // The graph must be schedulable end-to-end: a noop runner over a
        // minimal budget completes all seven nodes in dependency order.
        let built = TransferDagBuilder::single_file(TransferDirection::Download);
        let runner: Arc<dyn DagNodeRunner> = Arc::new(|_node: TransferNode| -> NodeFuture {
            Box::pin(async { NodeOutcome::Completed })
        });
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(1));

        let summary = execute_dag(
            &built.dag,
            &manager,
            runner,
            Arc::new(NoopDagObserver),
            None,
        )
        .await
        .expect("single-file graph must schedule cleanly");

        assert_eq!(summary.nodes_completed, 7);
        assert_eq!(summary.nodes_failed, 0);
    }
}
