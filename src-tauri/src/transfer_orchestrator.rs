// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Transfer batch orchestration skeleton.
//!
//! Phase 0 objective: establish the shared contract and bounded-concurrency
//! execution surface that later phases will wire to FTP and provider executors.
//!
//! DAG-P1-03 adds an optional multipart per-part wire contract. Defaults are
//! conservative so legacy and test executors never silently advertise runnable
//! per-part I/O.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use async_trait::async_trait;

use crate::providers::{MultipartHandle, ProviderType, UploadedPart};
use crate::transfer_dag::{
    EndpointIdentity, TransferCapabilities, TransferSessionLease, TransferSessionPoolHandle,
};
use crate::transfer_domain::{
    BatchProgressSnapshot, TransferBatchConfig, TransferBatchResult, TransferDirection,
    TransferEntry, TransferFailure, TransferOutcome,
};
use crate::transfer_event_sink::TransferEventSink;
use crate::transfer_multipart::{unsupported_multipart_failure, MultipartLayout};

pub type ProgressObserver = Arc<dyn Fn(BatchProgressSnapshot) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct TransferBatch {
    pub id: String,
    pub display_name: String,
    pub direction: TransferDirection,
    pub config: TransferBatchConfig,
    pub entries: Vec<TransferEntry>,
}

#[async_trait]
pub trait TransferExecutor {
    async fn execute(&self, entry: TransferEntry) -> TransferOutcome;

    fn provider_type(&self) -> Option<ProviderType> {
        None
    }

    /// Endpoint identity for the process-global governor. Production provider
    /// executors override this by querying their connected provider; legacy and
    /// test executors retain a conservative, provider-type-scoped fallback.
    async fn endpoint_identity(&self) -> EndpointIdentity {
        let protocol = self
            .provider_type()
            .map(|provider| provider.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        EndpointIdentity::new(protocol, "batch-executor", "")
    }

    /// Runtime transfer capability snapshot owned by this executor instance.
    ///
    /// Defaults to the conservative serial profile so legacy/test executors
    /// stay single-lease. Production provider executors must return the
    /// snapshot derived from the live provider and the session model that
    /// selected clone/pool feasibility (DAG-P1-01).
    fn transfer_capabilities(&self) -> TransferCapabilities {
        TransferCapabilities::default()
    }

    fn session_pool(&self, _max_concurrent: usize) -> TransferSessionPoolHandle {
        TransferSessionPoolHandle::legacy_single("legacy-provider")
    }

    async fn execute_with_session(
        &self,
        entry: TransferEntry,
        session_lease: TransferSessionLease,
    ) -> TransferOutcome {
        let outcome = self.execute(entry).await;
        drop(session_lease);
        outcome
    }

    /// Attempts metered for the most recently finished whole-file transfer of
    /// `entry_id`, first try included (so retries = attempts - 1). Consumed
    /// once: the value is removed on read so a long-lived executor does not
    /// accumulate per-file state. `None` means this executor does not meter
    /// attempts; callers record zero retries rather than guessing (DAG-P2-07).
    fn take_transfer_attempts(&self, _entry_id: &str) -> Option<u32> {
        None
    }

    /// Whether this executor implements real multipart begin/part/complete/abort.
    ///
    /// Default `false`: a shaped multipart graph against a conservative executor
    /// must fail closed at batch preflight (DAG-P1-03).
    fn supports_multipart_wire(&self) -> bool {
        false
    }

    /// Cooperative cancellation observed by multipart part nodes.
    fn is_transfer_cancelled(&self) -> bool {
        false
    }

    /// Begin one multipart session for `entry` (at most once per file state).
    async fn multipart_begin(
        &self,
        _entry: &TransferEntry,
        _layout: &MultipartLayout,
    ) -> Result<MultipartHandle, TransferFailure> {
        Err(unsupported_multipart_failure())
    }

    /// Upload one numbered part against an open multipart handle.
    async fn multipart_upload_part(
        &self,
        _entry: &TransferEntry,
        _handle: &MultipartHandle,
        _part_number: u32,
        _data: Vec<u8>,
    ) -> Result<UploadedPart, TransferFailure> {
        Err(unsupported_multipart_failure())
    }

    /// Upload one numbered part from a [`PartBody`] (DAG-P2-05).
    ///
    /// The default materializes the body and delegates to
    /// [`multipart_upload_part`](TransferExecutor::multipart_upload_part), so
    /// conservative and test executors keep their existing behaviour. The
    /// production provider executor overrides this to thread the `PartBody`
    /// through the provider's streaming `upload_part_body`.
    async fn multipart_upload_part_body(
        &self,
        entry: &TransferEntry,
        handle: &MultipartHandle,
        part_number: u32,
        body: crate::transfer_multipart::PartBody,
    ) -> Result<UploadedPart, TransferFailure> {
        let data = body.into_owned_bytes().await.map_err(|error| {
            crate::transfer_multipart::transfer_failure_from_provider(
                &error,
                Some(&entry.local_path),
            )
        })?;
        self.multipart_upload_part(entry, handle, part_number, data)
            .await
    }

    /// Complete a multipart session with receipts sorted by part number.
    async fn multipart_complete(
        &self,
        _entry: &TransferEntry,
        _handle: MultipartHandle,
        _parts: Vec<UploadedPart>,
    ) -> Result<(), TransferFailure> {
        Err(unsupported_multipart_failure())
    }

    /// Best-effort abort of a leftover multipart session (diagnostic errors).
    async fn multipart_abort(&self, _entry: &TransferEntry, _handle: MultipartHandle) {
        // Default: no-op. Production executors perform provider abort.
    }

    /// Emit once-per-file start for a multipart file (executor-owned events).
    fn multipart_emit_file_start(&self, _entry: &TransferEntry, _total_size: u64) {}

    /// Emit once-per-file terminal success for a multipart file.
    fn multipart_emit_file_complete(&self, _entry: &TransferEntry) {}

    /// Emit once-per-file terminal failure for a multipart file.
    fn multipart_emit_file_error(&self, _entry: &TransferEntry, _failure: &TransferFailure) {}
}

pub async fn execute_batch<E>(
    sink: Arc<dyn TransferEventSink>,
    batch: TransferBatch,
    executor: Arc<E>,
    cancel: Arc<AtomicBool>,
    progress_observer: Option<ProgressObserver>,
) -> TransferBatchResult
where
    E: TransferExecutor + Send + Sync + 'static,
{
    // DAG-ENGINE: every batch transfer schedules through the shared graph
    // engine. The hand-rolled `JoinSet` sliding window that lived here
    // before the convergence is gone; the resource manager / session pool
    // bookkeeping is now part of `execute_batch_dag` directly.
    crate::transfer_dag_batch::execute_batch_dag(sink, batch, executor, cancel, progress_observer)
        .await
}
