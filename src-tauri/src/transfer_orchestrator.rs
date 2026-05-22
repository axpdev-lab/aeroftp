// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Transfer batch orchestration skeleton.
//!
//! Phase 0 objective: establish the shared contract and bounded-concurrency
//! execution surface that later phases will wire to FTP and provider executors.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use tokio::sync::Mutex;
use tokio::task::JoinSet;

use crate::transfer_dag::{
    ResourceRequest, TransferResourceManager, TransferSessionLease, TransferSessionPoolHandle,
};
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
    // DAG-ENGINE phase 2 (F2-T07): with the batch flag on, schedule the whole
    // batch through the shared graph engine instead of this hand-rolled
    // sliding window. Default OFF, so the legacy path below is the default and
    // a rollback is a runtime toggle, never a revert.
    if crate::transfer_dag_batch::dag_batch_enabled() {
        return crate::transfer_dag_batch::execute_batch_dag(
            sink,
            batch,
            executor,
            cancel,
            progress_observer,
        )
        .await;
    }

    let started_at = Instant::now();
    let total = batch.entries.len() as u32;
    let progress = Arc::new(Mutex::new(BatchProgressSnapshot {
        total,
        bytes_total: batch.entries.iter().map(|entry| entry.size).sum(),
        ..BatchProgressSnapshot::default()
    }));
    let resource_manager = Arc::new(TransferResourceManager::new(batch.config.transfer_budget()));
    let max_concurrent = batch.config.max_concurrent.max(1) as usize;
    let session_pool = Arc::new(executor.session_pool(max_concurrent));
    let mut join_set = JoinSet::new();
    let mut entries = batch.entries.into_iter();

    sink.emit_batch_started(serde_json::json!({
        "batch_id": batch.id,
        "display_name": batch.display_name,
        "direction": batch.direction,
        "total": total,
    }));

    for _ in 0..max_concurrent {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Some(entry) = entries.next() else {
            break;
        };
        spawn_transfer_task(
            &mut join_set,
            sink.clone(),
            cancel.clone(),
            executor.clone(),
            progress.clone(),
            progress_observer.clone(),
            resource_manager.clone(),
            session_pool.clone(),
            entry,
        );
    }

    while let Some(result) = join_set.join_next().await {
        if let Err(error) = result {
            tracing::warn!("Transfer batch task failed: {}", error);
        }

        if cancel.load(Ordering::Relaxed) {
            continue;
        }

        if let Some(entry) = entries.next() {
            spawn_transfer_task(
                &mut join_set,
                sink.clone(),
                cancel.clone(),
                executor.clone(),
                progress.clone(),
                progress_observer.clone(),
                resource_manager.clone(),
                session_pool.clone(),
                entry,
            );
        }
    }

    let snapshot = progress.lock().await.clone();
    let batch_result = TransferBatchResult {
        completed: snapshot.completed,
        skipped: snapshot.skipped,
        failed: snapshot.failed,
        total: snapshot.total,
        cancelled: cancel.load(Ordering::Relaxed),
        duration_ms: started_at.elapsed().as_millis() as u64,
    };

    sink.emit_batch_completed(&batch_result);

    batch_result
}

#[allow(clippy::too_many_arguments)]
fn spawn_transfer_task<E>(
    join_set: &mut JoinSet<()>,
    sink: Arc<dyn TransferEventSink>,
    cancel: Arc<AtomicBool>,
    executor: Arc<E>,
    progress: Arc<Mutex<BatchProgressSnapshot>>,
    progress_observer: Option<ProgressObserver>,
    resource_manager: Arc<TransferResourceManager>,
    session_pool: Arc<TransferSessionPoolHandle>,
    entry: TransferEntry,
) where
    E: TransferExecutor + Send + Sync + 'static,
{
    join_set.spawn(async move {
        let _lease = match resource_manager
            .acquire(ResourceRequest::file_transfer())
            .await
        {
            Ok(lease) => lease,
            Err(error) => {
                tracing::warn!("Transfer resource acquisition failed: {}", error);
                return;
            }
        };
        let session_lease = match session_pool.acquire().await {
            Ok(lease) => lease,
            Err(error) => {
                tracing::warn!("Transfer session acquisition failed: {}", error);
                return;
            }
        };

        if cancel.load(Ordering::Relaxed) {
            return;
        }

        {
            let mut snapshot = progress.lock().await;
            snapshot.active += 1;
        }

        let outcome = executor
            .execute_with_session(entry.clone(), session_lease)
            .await;

        let mut snapshot = progress.lock().await;
        snapshot.active = snapshot.active.saturating_sub(1);
        match &outcome {
            TransferOutcome::Success => {
                snapshot.completed += 1;
                snapshot.bytes_transferred += entry.size;
            }
            TransferOutcome::Skipped { .. } => {
                snapshot.skipped += 1;
            }
            TransferOutcome::Failed(_) => {
                snapshot.failed += 1;
            }
        }

        let snapshot_clone = snapshot.clone();
        drop(snapshot);

        sink.emit_batch_progress(&snapshot_clone);
        if let Some(observer) = progress_observer {
            observer(snapshot_clone);
        }
    });
}
