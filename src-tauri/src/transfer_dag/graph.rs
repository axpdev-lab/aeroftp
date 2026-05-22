// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use serde::{Deserialize, Serialize};

use super::resources::ResourceRequest;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferNodeKind {
    DiscoverLocal,
    DiscoverRemote,
    Compare,
    PlanTransfer,
    AcquireResource,
    DownloadFile,
    DownloadRange,
    UploadFile,
    UploadPart,
    ServerSideCopy,
    VerifyChecksum,
    PreserveMetadata,
    CommitTemp,
    CleanupTemp,
    EmitProgress,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferNode {
    pub id: usize,
    pub kind: TransferNodeKind,
    pub depends_on: Vec<usize>,
    pub resources: ResourceRequest,
}

#[derive(Debug, Clone, Default)]
pub struct TransferDag {
    nodes: Vec<TransferNode>,
}

impl TransferDag {
    pub fn add_node(
        &mut self,
        kind: TransferNodeKind,
        depends_on: Vec<usize>,
        resources: ResourceRequest,
    ) -> usize {
        let id = self.nodes.len();
        self.nodes.push(TransferNode {
            id,
            kind,
            depends_on,
            resources,
        });
        id
    }

    pub fn nodes(&self) -> &[TransferNode] {
        &self.nodes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dag_nodes_keep_dependencies_explicit() {
        let mut dag = TransferDag::default();
        let plan = dag.add_node(
            TransferNodeKind::PlanTransfer,
            vec![],
            ResourceRequest::default(),
        );
        let download = dag.add_node(
            TransferNodeKind::DownloadFile,
            vec![plan],
            ResourceRequest::file_transfer(),
        );

        assert_eq!(download, 1);
        assert_eq!(dag.nodes()[download].depends_on, vec![plan]);
    }
}
