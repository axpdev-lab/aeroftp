// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

//! Transfer batch orchestration skeleton.
//!
//! Phase 0 objective: establish the shared contract and bounded-concurrency
//! execution surface that later phases will wire to FTP and provider executors.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;

use crate::transfer_domain::{
    BatchProgressSnapshot, TransferBatchConfig, TransferBatchResult, TransferDirection,
    TransferEntry, TransferOutcome,
};

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
}

pub async fn execute_batch<E>(
    app: &AppHandle,
    batch: TransferBatch,
    executor: Arc<E>,
    cancel: Arc<AtomicBool>,
    progress_observer: Option<ProgressObserver>,
) -> TransferBatchResult
where
    E: TransferExecutor + Send + Sync + 'static,
{
    let started_at = Instant::now();
    let total = batch.entries.len() as u32;
    let progress = Arc::new(Mutex::new(BatchProgressSnapshot {
        total,
        bytes_total: batch.entries.iter().map(|entry| entry.size).sum(),
        ..BatchProgressSnapshot::default()
    }));
    let semaphore = Arc::new(Semaphore::new(batch.config.max_concurrent.max(1) as usize));
    let mut join_set = JoinSet::new();
    let max_concurrent = batch.config.max_concurrent.max(1) as usize;
    let mut entries = batch.entries.into_iter();

    let _ = app.emit(
        "transfer_batch_started",
        serde_json::json!({
            "batch_id": batch.id,
            "display_name": batch.display_name,
            "direction": batch.direction,
            "total": total,
        }),
    );

    for _ in 0..max_concurrent {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Some(entry) = entries.next() else {
            break;
        };
        spawn_transfer_task(
            &mut join_set,
            app.clone(),
            cancel.clone(),
            executor.clone(),
            progress.clone(),
            progress_observer.clone(),
            semaphore.clone(),
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
                app.clone(),
                cancel.clone(),
                executor.clone(),
                progress.clone(),
                progress_observer.clone(),
                semaphore.clone(),
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

    let _ = app.emit("transfer_batch_completed", &batch_result);

    batch_result
}

#[allow(clippy::too_many_arguments)]
fn spawn_transfer_task<E>(
    join_set: &mut JoinSet<()>,
    app: AppHandle,
    cancel: Arc<AtomicBool>,
    executor: Arc<E>,
    progress: Arc<Mutex<BatchProgressSnapshot>>,
    progress_observer: Option<ProgressObserver>,
    semaphore: Arc<Semaphore>,
    entry: TransferEntry,
) where
    E: TransferExecutor + Send + Sync + 'static,
{
    join_set.spawn(async move {
        let _permit = semaphore.acquire_owned().await.ok();

        if cancel.load(Ordering::Relaxed) {
            return;
        }

        {
            let mut snapshot = progress.lock().await;
            snapshot.active += 1;
        }

        let outcome = executor.execute(entry.clone()).await;

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

        let _ = app.emit("transfer_batch_progress", snapshot_clone.clone());
        if let Some(observer) = progress_observer {
            observer(snapshot_clone);
        }
    });
}
