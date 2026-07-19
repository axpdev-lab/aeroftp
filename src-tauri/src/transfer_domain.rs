// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Shared transfer domain model for GUI batch transfers.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::transfer_dag::{
    FailureScope, RetryDirective, TransferBudget, TransferCapabilities, TransferError,
    TransferErrorKind,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransferDirection {
    Download,
    Upload,
}

/// Public/domain failure taxonomy for batch and multipart outcomes.
///
/// Extended in DAG-P1-04 so congestion kinds (429 / 503 / timeout / max
/// connections / connection-reset) and non-congestion policy kinds (auth /
/// quota) survive the redacted user-facing message. Controllers map from
/// these discriminants only — never from [`TransferFailure::message`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransferFailureKind {
    Timeout,
    ConnectionLost,
    /// Distinct from generic connection loss: AIMD D2 congestion trigger.
    #[serde(alias = "connection_reset")]
    ConnectionReset,
    RateLimited,
    /// HTTP 503 / service unavailable under load (AIMD D2).
    ServiceUnavailable,
    /// Server refused for too many connections, e.g. FTP 421 (AIMD D2).
    MaxConnections,
    NotFound,
    PermissionDenied,
    InvalidPath,
    LocalIo,
    RemoteIo,
    Cancelled,
    /// Authentication / credential failure (never congestion).
    Auth,
    /// Hard storage / quota limit (never congestion).
    QuotaExceeded,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferFailure {
    pub kind: TransferFailureKind,
    /// Redacted, user-facing presentation string. Controllers must not parse it.
    pub message: String,
    pub retryable: bool,
    /// Typed Retry-After from the provider adapter, in whole seconds.
    /// Never recovered by substring-matching [`Self::message`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
}

impl TransferFailure {
    /// Build a failure without a Retry-After hint.
    pub fn new(kind: TransferFailureKind, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            kind,
            message: message.into(),
            retryable,
            retry_after_secs: None,
        }
    }

    /// Attach a typed Retry-After (seconds).
    pub fn with_retry_after_secs(mut self, secs: u64) -> Self {
        self.retry_after_secs = Some(secs);
        self
    }

    /// Lossless machine-field bridge into the DAG executor's typed error.
    ///
    /// Preserves congestion kind, file scope, retryability, and Retry-After.
    /// Presentation stays the (already redacted) domain message.
    pub fn to_transfer_error(&self) -> TransferError {
        transfer_error_from_failure(self)
    }

    /// Build a domain failure from a typed DAG error, redacting the public
    /// message while keeping machine fields.
    pub fn from_transfer_error(error: &TransferError) -> Self {
        transfer_failure_from_transfer_error(error)
    }

    /// Classify a raw provider / transport string once at the adapter boundary.
    ///
    /// Uses [`TransferError::from_message`] so 503 / 421 / connection-reset /
    /// Retry-After markers are lifted into typed fields before the redacted
    /// presentation string replaces the raw text.
    pub fn from_raw_message(raw: &str) -> Self {
        let typed = TransferError::from_message(raw);
        Self::from_transfer_error(&typed)
    }
}

/// Central, lossless adapter: domain failure → DAG [`TransferError`].
///
/// Single source of truth for whole-file, session-acquire and multipart
/// terminal outcomes. Never re-classifies from the presentation message.
pub fn transfer_error_from_failure(failure: &TransferFailure) -> TransferError {
    let kind = transfer_error_kind_from_failure_kind(failure.kind);
    let mut err = TransferError::new(kind, failure.message.clone()).with_scope(FailureScope::File);

    // The domain flag is authoritative when it is stricter than the kind
    // default. Congestion classification and AIMD feedback still use `kind`,
    // independently from whether retrying this particular operation is safe.
    if !failure.retryable {
        err.retry = RetryDirective::Never;
    }

    if let Some(secs) = failure.retry_after_secs {
        err.retry_after = Some(Duration::from_secs(secs));
        if failure.retryable
            && matches!(
                kind,
                TransferErrorKind::RateLimited | TransferErrorKind::ServiceUnavailable
            )
        {
            err.retry = RetryDirective::AfterHint;
        }
    }

    err
}

/// Central adapter: typed DAG error → domain failure with redacted message.
pub fn transfer_failure_from_transfer_error(error: &TransferError) -> TransferFailure {
    let kind = transfer_failure_kind_from_error_kind(error.kind);
    let retryable = !matches!(error.retry, RetryDirective::Never)
        && !matches!(
            error.kind,
            TransferErrorKind::Cancelled
                | TransferErrorKind::Auth
                | TransferErrorKind::NotFound
                | TransferErrorKind::PermissionDenied
                | TransferErrorKind::QuotaExceeded
        );
    let mut failure =
        TransferFailure::new(kind, user_facing_transfer_failure_message(&kind), retryable);
    if let Some(d) = error.retry_after {
        // Round up sub-second hints so a 0-duration never defeats AIMD.
        let secs = d.as_secs().max(if d.subsec_nanos() > 0 { 1 } else { 0 });
        if secs > 0 || d.as_secs() > 0 {
            failure.retry_after_secs = Some(d.as_secs().max(1));
        }
    }
    failure
}

fn transfer_error_kind_from_failure_kind(kind: TransferFailureKind) -> TransferErrorKind {
    match kind {
        TransferFailureKind::Timeout => TransferErrorKind::Timeout,
        TransferFailureKind::ConnectionLost => TransferErrorKind::Network,
        TransferFailureKind::ConnectionReset => TransferErrorKind::ConnectionReset,
        TransferFailureKind::RateLimited => TransferErrorKind::RateLimited,
        TransferFailureKind::ServiceUnavailable => TransferErrorKind::ServiceUnavailable,
        TransferFailureKind::MaxConnections => TransferErrorKind::MaxConnections,
        TransferFailureKind::NotFound | TransferFailureKind::InvalidPath => {
            TransferErrorKind::NotFound
        }
        TransferFailureKind::PermissionDenied => TransferErrorKind::PermissionDenied,
        TransferFailureKind::LocalIo => TransferErrorKind::LocalIo,
        TransferFailureKind::RemoteIo => TransferErrorKind::RemoteIo,
        TransferFailureKind::Cancelled => TransferErrorKind::Cancelled,
        TransferFailureKind::Auth => TransferErrorKind::Auth,
        TransferFailureKind::QuotaExceeded => TransferErrorKind::QuotaExceeded,
        TransferFailureKind::Unknown => TransferErrorKind::Unknown,
    }
}

fn transfer_failure_kind_from_error_kind(kind: TransferErrorKind) -> TransferFailureKind {
    match kind {
        TransferErrorKind::Timeout => TransferFailureKind::Timeout,
        TransferErrorKind::ConnectionReset => TransferFailureKind::ConnectionReset,
        TransferErrorKind::Network => TransferFailureKind::ConnectionLost,
        TransferErrorKind::RateLimited => TransferFailureKind::RateLimited,
        TransferErrorKind::ServiceUnavailable => TransferFailureKind::ServiceUnavailable,
        TransferErrorKind::MaxConnections => TransferFailureKind::MaxConnections,
        TransferErrorKind::NotFound => TransferFailureKind::NotFound,
        TransferErrorKind::PermissionDenied => TransferFailureKind::PermissionDenied,
        TransferErrorKind::LocalIo => TransferFailureKind::LocalIo,
        TransferErrorKind::RemoteIo => TransferFailureKind::RemoteIo,
        TransferErrorKind::Cancelled => TransferFailureKind::Cancelled,
        TransferErrorKind::Auth => TransferFailureKind::Auth,
        TransferErrorKind::QuotaExceeded => TransferFailureKind::QuotaExceeded,
        TransferErrorKind::NotConnected => TransferFailureKind::ConnectionLost,
        TransferErrorKind::ResourceAcquire => TransferFailureKind::Unknown,
        TransferErrorKind::Unknown => TransferFailureKind::Unknown,
    }
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
        crate::sync::SyncErrorKind::Auth => TransferFailureKind::Auth,
        crate::sync::SyncErrorKind::QuotaExceeded => TransferFailureKind::QuotaExceeded,
        _ => TransferFailureKind::Unknown,
    }
}

pub fn user_facing_transfer_failure_message(kind: &TransferFailureKind) -> &'static str {
    match kind {
        TransferFailureKind::Timeout => "Transfer timed out",
        TransferFailureKind::ConnectionLost => "Connection lost during transfer",
        TransferFailureKind::ConnectionReset => "Connection reset during transfer",
        TransferFailureKind::RateLimited => "Transfer rate limit reached",
        TransferFailureKind::ServiceUnavailable => "Remote service temporarily unavailable",
        TransferFailureKind::MaxConnections => "Too many connections to remote service",
        TransferFailureKind::NotFound => "Requested file or path was not found",
        TransferFailureKind::PermissionDenied => "Permission denied during transfer",
        TransferFailureKind::InvalidPath => "Invalid transfer path",
        TransferFailureKind::LocalIo => "Local file system error during transfer",
        TransferFailureKind::RemoteIo => "Remote storage error during transfer",
        TransferFailureKind::Cancelled => "Transfer cancelled by user",
        TransferFailureKind::Auth => "Authentication failed during transfer",
        TransferFailureKind::QuotaExceeded => "Storage quota exceeded during transfer",
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
    fn maps_sync_auth_to_transfer_auth() {
        let kind = transfer_failure_kind_from_sync(&crate::sync::SyncErrorKind::Auth);
        assert_eq!(kind, TransferFailureKind::Auth);
    }

    #[test]
    fn maps_unhandled_sync_kind_to_unknown() {
        let kind = transfer_failure_kind_from_sync(&crate::sync::SyncErrorKind::FileLocked);
        assert_eq!(kind, TransferFailureKind::Unknown);
    }

    #[test]
    fn exposes_redacted_user_facing_message() {
        let message = user_facing_transfer_failure_message(&TransferFailureKind::PermissionDenied);
        assert_eq!(message, "Permission denied during transfer");
    }

    #[test]
    fn from_raw_message_preserves_congestion_kinds_not_presentation() {
        let f429 =
            TransferFailure::from_raw_message("HTTP 429 Too Many Requests [retry-after-secs=30]");
        assert_eq!(f429.kind, TransferFailureKind::RateLimited);
        assert_eq!(f429.retry_after_secs, Some(30));
        assert_eq!(f429.message, "Transfer rate limit reached");
        assert!(!f429.message.contains("429"));

        let f503 = TransferFailure::from_raw_message("503 Service Unavailable");
        assert_eq!(f503.kind, TransferFailureKind::ServiceUnavailable);
        assert_eq!(f503.message, "Remote service temporarily unavailable");

        let f421 = TransferFailure::from_raw_message("421 too many connections from your IP");
        assert_eq!(f421.kind, TransferFailureKind::MaxConnections);

        let reset = TransferFailure::from_raw_message("connection reset by peer");
        assert_eq!(reset.kind, TransferFailureKind::ConnectionReset);
    }

    #[test]
    fn adapter_round_trip_preserves_d2_and_retry_after() {
        let failure = TransferFailure::new(
            TransferFailureKind::RateLimited,
            "Transfer rate limit reached",
            true,
        )
        .with_retry_after_secs(45);
        let te = failure.to_transfer_error();
        assert_eq!(te.kind, TransferErrorKind::RateLimited);
        assert_eq!(te.scope, FailureScope::File);
        assert_eq!(te.retry_after, Some(Duration::from_secs(45)));
        assert_eq!(te.retry, RetryDirective::AfterHint);
        assert!(te.is_congestion());

        // Reverse: typed error → redacted domain failure.
        let mut typed = TransferError::new(
            TransferErrorKind::ServiceUnavailable,
            "raw 503 secret token=abc",
        );
        typed.retry_after = Some(Duration::from_secs(12));
        let back = TransferFailure::from_transfer_error(&typed);
        assert_eq!(back.kind, TransferFailureKind::ServiceUnavailable);
        assert_eq!(back.retry_after_secs, Some(12));
        assert!(!back.message.contains("token"));
        assert!(!back.message.contains("503"));
    }

    #[test]
    fn adapter_preserves_non_retryable_congestion_with_retry_after() {
        let failure = TransferFailure::new(
            TransferFailureKind::RateLimited,
            "Transfer rate limit reached",
            false,
        )
        .with_retry_after_secs(45);
        let typed = failure.to_transfer_error();

        assert_eq!(typed.kind, TransferErrorKind::RateLimited);
        assert_eq!(typed.retry, RetryDirective::Never);
        assert_eq!(typed.retry_after, Some(Duration::from_secs(45)));
        assert!(typed.is_congestion());

        let back = TransferFailure::from_transfer_error(&typed);
        assert!(!back.retryable);
        assert_eq!(back.retry_after_secs, Some(45));
    }

    #[test]
    fn non_congestion_kinds_are_not_congestion_on_dag_error() {
        for kind in [
            TransferFailureKind::Auth,
            TransferFailureKind::NotFound,
            TransferFailureKind::PermissionDenied,
            TransferFailureKind::QuotaExceeded,
            TransferFailureKind::LocalIo,
            TransferFailureKind::RemoteIo,
            TransferFailureKind::Cancelled,
            TransferFailureKind::Unknown,
        ] {
            let te = TransferFailure::new(kind, user_facing_transfer_failure_message(&kind), false)
                .to_transfer_error();
            assert!(!te.is_congestion(), "{kind:?} must not be congestion");
        }
    }

    #[test]
    fn serde_defaults_accept_legacy_payload_without_retry_after() {
        let json = r#"{"kind":"timeout","message":"Transfer timed out","retryable":true}"#;
        let failure: TransferFailure = serde_json::from_str(json).expect("legacy payload");
        assert_eq!(failure.kind, TransferFailureKind::Timeout);
        assert_eq!(failure.retry_after_secs, None);
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
