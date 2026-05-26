// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Transfer batch orchestration skeleton.
//!
//! Phase 0 objective: establish the shared contract and bounded-concurrency
//! execution surface that later phases will wire to FTP and provider executors.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use async_trait::async_trait;

use crate::providers::ProviderType;
use crate::transfer_dag::{TransferSessionLease, TransferSessionPoolHandle};
use crate::transfer_domain::{
    BatchProgressSnapshot, TransferBatchConfig, TransferBatchResult, TransferDirection,
    TransferEntry, TransferOutcome,
};
use crate::transfer_event_sink::TransferEventSink;

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
