// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Shared transfer domain model for GUI batch transfers.

use serde::{Deserialize, Serialize};

use crate::transfer_dag::{TransferBudget, TransferCapabilities};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransferDirection {
    Download,
    Upload,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransferFailureKind {
    Timeout,
    ConnectionLost,
    RateLimited,
    NotFound,
    PermissionDenied,
    InvalidPath,
    LocalIo,
    RemoteIo,
    Cancelled,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferFailure {
    pub kind: TransferFailureKind,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TransferOutcome {
    Success,
    Skipped { reason: String },
    Failed(TransferFailure),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferBatchConfig {
    pub max_concurrent: u32,
    pub max_retries: u32,
    pub timeout_ms: u64,
}

impl Default for TransferBatchConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 1,
            max_retries: 0,
            timeout_ms: 30_000,
        }
    }
}

impl TransferBatchConfig {
    /// Build the DAG budget used by the batch scheduler.
    ///
    /// GUI `Settings.maxConcurrentTransfers` arrives at the backend as
    /// `max_concurrent`; this method is the explicit handoff into
    /// `TransferBudget::file_slots`. Chunk budget stays at the conservative
    /// default (1) until [`Self::transfer_budget_for_capabilities`] raises it
    /// from a truthful runtime capability snapshot (DAG-P1-03).
    pub fn transfer_budget(&self) -> TransferBudget {
        TransferBudget::from_file_slots(self.max_concurrent.min(u16::MAX as u32) as u16)
            .with_resolved_buffer_budget()
    }

    /// File-slot budget plus capability-derived chunk/disk-read ceilings.
    ///
    /// - `file_slots` ← `max_concurrent` (file/session dimension). Clamped to
    ///   `max_file_slots` only when the executor advertises realizable
    ///   `file_parallel` (pool-backed). Conservative serial defaults keep
    ///   `max_file_slots = Some(1)` as advertisement, but production settings
    ///   already resolve effective concurrency before the batch is built; the
    ///   session pool remains the second bound.
    /// - `chunk_slots` ← provider `max_chunk_slots` when multipart is available
    /// - `disk_read_slots` raised to cover concurrent part buffers
    /// - never reinterprets `max_concurrent` as an unbounded clone count
    pub fn transfer_budget_for_capabilities(&self, caps: &TransferCapabilities) -> TransferBudget {
        let mut budget = self.transfer_budget();
        if caps.multipart_upload.is_available() {
            let chunk = caps.max_chunk_slots.unwrap_or(1).max(1);
            budget.chunk_slots = chunk;
            budget.disk_read_slots = budget.disk_read_slots.max(chunk);
            if let Some(max) = caps.max_chunk_slots {
                budget.chunk_slots = budget.chunk_slots.min(max.max(1));
            }
        }
        if caps.file_parallel.is_available() {
            if let Some(max) = caps.max_file_slots {
                budget.file_slots = budget.file_slots.min(max.max(1));
            }
        }
        budget.disk_read_slots = budget.disk_read_slots.max(1);
        budget.disk_write_slots = budget.disk_write_slots.max(1);
        budget
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferEntry {
    pub id: String,
    pub display_name: String,
    pub remote_path: String,
    pub local_path: String,
    pub size: u64,
    pub modified: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BatchProgressSnapshot {
    /// Identifies the batch this snapshot belongs to, so a frontend consuming
    /// the unkeyed `transfer_batch_progress` event can reject a snapshot that
    /// does not match the toast it is currently showing (concurrent batches).
    #[serde(default)]
    pub batch_id: String,
    pub completed: u32,
    pub skipped: u32,
    pub failed: u32,
    pub active: u32,
    pub total: u32,
    pub bytes_transferred: u64,
    pub bytes_total: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransferBatchResult {
    pub completed: u32,
    pub skipped: u32,
    pub failed: u32,
    pub total: u32,
    pub cancelled: bool,
    pub duration_ms: u64,
}

pub fn transfer_failure_kind_from_sync(kind: &crate::sync::SyncErrorKind) -> TransferFailureKind {
    match kind {
        crate::sync::SyncErrorKind::Timeout => TransferFailureKind::Timeout,
        crate::sync::SyncErrorKind::Network => TransferFailureKind::ConnectionLost,
        crate::sync::SyncErrorKind::RateLimit => TransferFailureKind::RateLimited,
        crate::sync::SyncErrorKind::PathNotFound => TransferFailureKind::NotFound,
        crate::sync::SyncErrorKind::PermissionDenied => TransferFailureKind::PermissionDenied,
        crate::sync::SyncErrorKind::DiskError => TransferFailureKind::LocalIo,
        _ => TransferFailureKind::Unknown,
    }
}

pub fn user_facing_transfer_failure_message(kind: &TransferFailureKind) -> &'static str {
    match kind {
        TransferFailureKind::Timeout => "Transfer timed out",
        TransferFailureKind::ConnectionLost => "Connection lost during transfer",
        TransferFailureKind::RateLimited => "Transfer rate limit reached",
        TransferFailureKind::NotFound => "Requested file or path was not found",
        TransferFailureKind::PermissionDenied => "Permission denied during transfer",
        TransferFailureKind::InvalidPath => "Invalid transfer path",
        TransferFailureKind::LocalIo => "Local file system error during transfer",
        TransferFailureKind::RemoteIo => "Remote storage error during transfer",
        TransferFailureKind::Cancelled => "Transfer cancelled by user",
        TransferFailureKind::Unknown => "Transfer failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_sync_timeout_to_transfer_timeout() {
        let kind = transfer_failure_kind_from_sync(&crate::sync::SyncErrorKind::Timeout);
        assert_eq!(kind, TransferFailureKind::Timeout);
    }

    #[test]
    fn maps_unhandled_sync_kind_to_unknown() {
        let kind = transfer_failure_kind_from_sync(&crate::sync::SyncErrorKind::Auth);
        assert_eq!(kind, TransferFailureKind::Unknown);
    }

    #[test]
    fn exposes_redacted_user_facing_message() {
        let message = user_facing_transfer_failure_message(&TransferFailureKind::PermissionDenied);
        assert_eq!(message, "Permission denied during transfer");
    }

    #[test]
    fn batch_config_maps_max_concurrent_to_dag_file_slots() {
        let config = TransferBatchConfig {
            max_concurrent: 6,
            max_retries: 0,
            timeout_ms: 30_000,
        };

        assert_eq!(config.transfer_budget().file_slots, 6);
    }

    #[test]
    fn batch_config_floors_dag_file_slots_to_one() {
        let config = TransferBatchConfig {
            max_concurrent: 0,
            max_retries: 0,
            timeout_ms: 30_000,
        };

        assert_eq!(config.transfer_budget().file_slots, 1);
    }

    #[test]
    fn batch_config_capability_budget_raises_chunk_and_disk_read() {
        use crate::transfer_dag::Capability;
        let config = TransferBatchConfig {
            max_concurrent: 4,
            max_retries: 0,
            timeout_ms: 30_000,
        };
        let caps = TransferCapabilities {
            multipart_upload: Capability::Supported,
            max_chunk_slots: Some(4),
            max_file_slots: Some(8),
            ..TransferCapabilities::default()
        };
        let budget = config.transfer_budget_for_capabilities(&caps);
        assert_eq!(budget.file_slots, 4);
        assert_eq!(budget.chunk_slots, 4);
        assert!(budget.disk_read_slots >= 4);
        // Unconditional path still keeps chunk=1.
        assert_eq!(config.transfer_budget().chunk_slots, 1);
    }

    #[test]
    fn batch_config_capability_budget_clamps_to_provider_ceiling() {
        use crate::transfer_dag::Capability;
        let config = TransferBatchConfig {
            max_concurrent: 16,
            max_retries: 0,
            timeout_ms: 30_000,
        };
        let caps = TransferCapabilities {
            multipart_upload: Capability::Supported,
            max_chunk_slots: Some(2),
            max_file_slots: Some(3),
            ..TransferCapabilities::default()
        };
        let budget = config.transfer_budget_for_capabilities(&caps);
        // file_parallel not set on this fixture → file_slots stay at config.
        assert_eq!(budget.file_slots, 16);
        assert_eq!(budget.chunk_slots, 2);

        let caps_parallel = TransferCapabilities {
            file_parallel: Capability::Supported,
            multipart_upload: Capability::Supported,
            max_chunk_slots: Some(2),
            max_file_slots: Some(3),
            ..TransferCapabilities::default()
        };
        let budget_p = config.transfer_budget_for_capabilities(&caps_parallel);
        assert_eq!(budget_p.file_slots, 3);
        assert_eq!(budget_p.chunk_slots, 2);
    }
}
