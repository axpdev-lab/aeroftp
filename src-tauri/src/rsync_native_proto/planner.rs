//! Planning logic for deciding whether a file should be skipped, copied fully,
//! or transferred as delta in the Strada C prototype.

use crate::rsync_native_proto::types::{
    FileEntry, NativeRsyncConfig, SessionRole, TransferStrategy,
};

#[derive(Debug, Clone)]
pub struct TransferCandidate {
    pub local: Option<FileEntry>,
    pub remote: Option<FileEntry>,
    pub role: SessionRole,
}

#[derive(Debug, Clone)]
pub struct PlannerDecision {
    pub strategy: TransferStrategy,
    pub reason: String,
    pub block_size: Option<usize>,
    pub requires_remote_signatures: bool,
}

#[derive(Debug, Clone)]
pub struct TransferPlanner {
    pub config: NativeRsyncConfig,
}

impl TransferPlanner {
    pub fn new(config: NativeRsyncConfig) -> Self {
        Self { config }
    }

    pub fn decide(&self, candidate: &TransferCandidate) -> PlannerDecision {
        match (&candidate.local, &candidate.remote) {
            (Some(local), Some(remote)) if local.is_dir || remote.is_dir => PlannerDecision {
                strategy: TransferStrategy::Skip,
                reason: "directory transfer not in first native subset".to_string(),
                block_size: None,
                requires_remote_signatures: false,
            },
            (Some(local), Some(remote)) if local.size < self.config.min_delta_file_size => {
                PlannerDecision {
                    strategy: TransferStrategy::FullCopy,
                    reason: format!(
                        "below delta threshold: {} < {}",
                        local.size, self.config.min_delta_file_size
                    ),
                    block_size: None,
                    requires_remote_signatures: false,
                }
            }
            (Some(local), Some(remote)) if local.size == remote.size => PlannerDecision {
                strategy: TransferStrategy::Delta,
                reason: "same-size candidate worth signature exchange".to_string(),
                block_size: Some(Self::recommended_block_size(local.size)),
                requires_remote_signatures: true,
            },
            (Some(_local), Some(_remote)) => PlannerDecision {
                strategy: TransferStrategy::FullCopy,
                reason: "metadata mismatch; start with conservative full copy".to_string(),
                block_size: None,
                requires_remote_signatures: false,
            },
            (Some(_local), None) => PlannerDecision {
                strategy: TransferStrategy::FullCopy,
                reason: "remote file missing".to_string(),
                block_size: None,
                requires_remote_signatures: false,
            },
            (None, Some(_remote)) => PlannerDecision {
                strategy: TransferStrategy::Skip,
                reason: "pull-side metadata unavailable in prototype planner".to_string(),
                block_size: None,
                requires_remote_signatures: false,
            },
            (None, None) => PlannerDecision {
                strategy: TransferStrategy::Skip,
                reason: "no metadata available".to_string(),
                block_size: None,
                requires_remote_signatures: false,
            },
        }
    }

    pub fn recommended_block_size(file_size: u64) -> usize {
        let candidate = (file_size as f64).sqrt() as usize;
        candidate.clamp(512, 8192)
    }
}
