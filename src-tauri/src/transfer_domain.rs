// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Shared transfer domain model for GUI batch transfers.

use serde::{Deserialize, Serialize};

use crate::transfer_dag::TransferBudget;

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
    /// `TransferBudget::file_slots`.
    pub fn transfer_budget(&self) -> TransferBudget {
        TransferBudget::from_file_slots(self.max_concurrent.min(u16::MAX as u32) as u16)
            .with_resolved_buffer_budget()
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
}
