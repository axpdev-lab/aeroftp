// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! DAG-ENGINE phase 2: the sync-session transfer path through `execute_dag`.
//!
//! [`crate::sync::sync_tree_core`] is the shared sync engine: the MCP
//! `sync_tree` tool and the `sync_core` re-export both run through it. This
//! module is the flag-gated bridge that routes that engine through the shared
//! graph engine instead of its hand-rolled interleaved decide/perform loop.
//!
//! ## Shape of the sync graph
//!
//! A sync session has one global discovery prefix
//! (`DiscoverLocal` + `DiscoverRemote` -> `Compare`) and one per-file transfer
//! chain below `Compare` for every upload/download the plan resolved. The
//! graph is static, so the scan and the compare must finish *before* the
//! graph can be built: the discover/compare nodes are structural anchors,
//! and the real scan happens in [`execute_sync_dag`]'s planning phase.
//!
//! ## What this actually buys
//!
//! A sync drives a single provider connection, so the per-file transfers are
//! serial by nature (`file_slots = 1`). Routing them through the graph does
//! not add transfer parallelism; it buys:
//!
//! - **Parallel discovery.** The local filesystem walk and the network-bound
//!   remote scan overlap (blocking thread + provider task), instead of
//!   running back to back. This is the one real throughput win.
//! - **Structural uniformity** with the single-file (phase 1) and batch
//!   (phase 2) graphs: one scheduler, one observability surface, and the
//!   foundation phase 3 needs to make capability-aware nodes (server-side
//!   copy, multipart, resume, AIMD) benefit sync for free.
//! - **A provably non-mutating dry-run.** Dry-run never reaches this module:
//!   the [`crate::sync::sync_tree_core`] guard excludes it, so the dry-run
//!   plan output cannot build or execute a graph by construction.
//!
//! ## Byte-identical contract
//!
//! The per-file decisions are computed by the same pure `decide_upload` /
//! `decide_download` the legacy core uses, from the same pre-transfer scan
//! snapshots, so the plan is decision-equivalent. The real per-file I/O is
//! the same `perform_upload` / `perform_download`, holding the same single
//! delta-sync batch session across files.
//!
//! The contract is counter-based, the same precedent the phase-2 batch path
//! set: identical [`SyncReport`] counters, the same set of `on_file_*` sink
//! events (one per file), the same delta-batch session metrics, the same
//! journal format. One deliberate, documented difference: skip events are
//! emitted in the planning order up front, then the transfers, then the
//! orphan deletes; the legacy core interleaves skips with transfers in scan
//! order. Same events, same counts, grouped by phase instead of interleaved.
//!
//! ## Feature flag
//!
//! Gated by [`dag_sync_enabled`], read from [`DAG_SYNC_ENV`], default OFF.
//! With the flag off, not one byte of this module executes and
//! `sync_tree_core` runs its legacy path verbatim. The flag is a runtime
//! toggle (not a recompile), the same pattern as the phase-1 single-file and
//! phase-2 batch flags.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{mpsc, oneshot};

use crate::delta_transport::DeltaBatch;
use crate::providers::StorageProvider;
use crate::sync::{
    apply_sync_tree_outcome, decide_download, decide_upload, ensure_remote_dir, perform_download,
    perform_local_delete, perform_remote_delete, perform_upload, DeltaPolicy, FileOutcome,
    SyncDirection, SyncError, SyncOptions, SyncPhase, SyncProgressSink, SyncReport,
    SyncTransferSpec, SyncTreeAction,
};
use crate::sync_core::scan::{scan_local_tree, scan_remote_tree, LocalEntry, RemoteEntry};
use crate::transfer_dag::executor::{execute_dag, DagNodeRunner, NodeFuture, NodeOutcome};
use crate::transfer_dag::graph::{TransferNode, TransferNodeKind};
use crate::transfer_dag::{
    AimdConfig, AimdController, DagObserver, NoopDagObserver, SyncDagAction, SyncDagItem,
    TransferBudget, TransferDagBuilder, TransferResourceManager,
};

/// Environment variable that gates the phase-2 sync DAG path.
///
/// Recognised truthy values (case-insensitive, trimmed): `1`, `true`, `on`,
/// `yes`. Anything else, including an unset variable, leaves the path OFF.
pub const DAG_SYNC_ENV: &str = "AEROFTP_TRANSFER_ENGINE_DAG_SYNC";

/// Bounded capacity of the transfer-job channel. With `file_slots = 1` only
/// one transfer node ever holds the slot, so at most one job is in flight;
/// the buffer is pure headroom and the sender never blocks.
const JOB_CHANNEL_CAPACITY: usize = 32;

/// Returns `true` when the sync DAG path is enabled for this process.
///
/// Default OFF: a phased migration enters production behind a flag so a
/// rollback is a toggle, never a revert (DAG-ENGINE plan, principle 2).
pub fn dag_sync_enabled() -> bool {
    std::env::var(DAG_SYNC_ENV)
        .ok()
        .map(|raw| flag_value_is_on(&raw))
        .unwrap_or(false)
}

/// Whether a sync call should route through the graph engine.
///
/// `true` requires both the flag on and a non-dry-run sync. Dry-run always
/// returns `false`: the dry-run plan must never build or execute a mutating
/// graph, and gating it here makes that a structural guarantee rather than a
/// convention buried in the executor.
pub fn should_route_sync_to_dag(dry_run: bool) -> bool {
    !dry_run && dag_sync_enabled()
}

/// Parses one environment-variable value into the on/off flag state.
fn flag_value_is_on(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "on" | "yes"
    )
}

/// One resolved transfer in a sync plan, in plan order.
struct PlannedTransfer {
    rel: String,
    /// `"upload"` or `"download"`: the operation label the legacy core
    /// passes to `apply_sync_tree_outcome`.
    op: &'static str,
    total: u64,
    decision_policy: DeltaPolicy,
}

/// One resolved skip in a sync plan.
struct PlannedSkip {
    rel: String,
    /// `"upload"` or `"download"`: the operation label, identical to the
    /// legacy core's `apply_sync_tree_outcome` call for a skipped file.
    apply_op: &'static str,
    reason: String,
    decision_policy: DeltaPolicy,
}

/// One resolved orphan deletion in a sync plan.
struct PlannedDelete {
    rel: String,
    /// `"delete_remote"` or `"delete_local"`.
    op: &'static str,
}

/// The fully resolved plan of a sync session, computed before any transfer.
struct SyncDagPlan {
    transfers: Vec<PlannedTransfer>,
    skips: Vec<PlannedSkip>,
    deletes: Vec<PlannedDelete>,
}

/// Resolve a sync session into an ordered plan from the pre-transfer scan
/// snapshots.
///
/// This mirrors the two-pass structure of [`crate::sync::sync_tree_core`]
/// (upload pass over `locals`, download pass over `remotes`, then orphan
/// deletes) but collects the decisions instead of performing them inline.
/// The decision functions `decide_upload` / `decide_download` are pure and
/// read only the scan snapshots, so collecting the whole plan up front is
/// decision-equivalent to the legacy interleaved loop.
fn plan_sync_dag(
    locals: &[LocalEntry],
    remotes: &[RemoteEntry],
    opts: &SyncOptions,
) -> SyncDagPlan {
    use std::collections::{HashMap as Map, HashSet};

    let mut seen_local: HashSet<String> = HashSet::new();
    let mut seen_remote: HashSet<String> = HashSet::new();
    // In direction=Both, a download is suppressed only for paths the upload
    // pass actually resolved with a Copy; a plain upload Skip must not
    // suppress a later download decision.
    let mut upload_resolved_for_both: HashSet<String> = HashSet::new();
    let local_by_path: Map<&str, &LocalEntry> = locals
        .iter()
        .map(|entry| (entry.rel_path.as_str(), entry))
        .collect();
    let remote_by_path: Map<&str, &RemoteEntry> = remotes
        .iter()
        .map(|entry| (entry.rel_path.as_str(), entry))
        .collect();

    let mut transfers: Vec<PlannedTransfer> = Vec::new();
    let mut skips: Vec<PlannedSkip> = Vec::new();

    if matches!(opts.direction, SyncDirection::Upload | SyncDirection::Both) {
        for local_entry in locals {
            if !seen_local.insert(local_entry.rel_path.clone()) {
                continue;
            }
            let remote_entry = remote_by_path.get(local_entry.rel_path.as_str()).copied();
            let decision = decide_upload(
                local_entry,
                remote_entry,
                opts.delta_policy,
                opts.conflict_mode,
            );
            match decision.action {
                SyncTreeAction::Copy => {
                    if matches!(opts.direction, SyncDirection::Both) {
                        upload_resolved_for_both.insert(local_entry.rel_path.clone());
                    }
                    transfers.push(PlannedTransfer {
                        rel: local_entry.rel_path.clone(),
                        op: "upload",
                        total: local_entry.size,
                        decision_policy: decision.decision_policy,
                    });
                }
                SyncTreeAction::Skip(reason) => {
                    skips.push(PlannedSkip {
                        rel: local_entry.rel_path.clone(),
                        apply_op: "upload",
                        reason,
                        decision_policy: decision.decision_policy,
                    });
                }
            }
        }
    }

    if matches!(
        opts.direction,
        SyncDirection::Download | SyncDirection::Both
    ) {
        for remote_entry in remotes {
            if !seen_remote.insert(remote_entry.rel_path.clone()) {
                continue;
            }
            let handled_by_upload = matches!(opts.direction, SyncDirection::Both)
                && upload_resolved_for_both.contains(&remote_entry.rel_path);
            let decision = decide_download(
                remote_entry,
                local_by_path.get(remote_entry.rel_path.as_str()).copied(),
                opts.delta_policy,
                opts.conflict_mode,
                handled_by_upload,
            );
            match decision.action {
                SyncTreeAction::Copy => {
                    transfers.push(PlannedTransfer {
                        rel: remote_entry.rel_path.clone(),
                        op: "download",
                        total: remote_entry.size,
                        decision_policy: decision.decision_policy,
                    });
                }
                SyncTreeAction::Skip(reason) => {
                    skips.push(PlannedSkip {
                        rel: remote_entry.rel_path.clone(),
                        apply_op: "download",
                        reason,
                        decision_policy: decision.decision_policy,
                    });
                }
            }
        }
    }

    let mut deletes: Vec<PlannedDelete> = Vec::new();
    if opts.delete_orphans {
        match opts.direction {
            SyncDirection::Upload => {
                for remote_entry in remotes {
                    if !local_by_path.contains_key(remote_entry.rel_path.as_str()) {
                        deletes.push(PlannedDelete {
                            rel: remote_entry.rel_path.clone(),
                            op: "delete_remote",
                        });
                    }
                }
            }
            SyncDirection::Download => {
                for local_entry in locals {
                    if !remote_by_path.contains_key(local_entry.rel_path.as_str()) {
                        deletes.push(PlannedDelete {
                            rel: local_entry.rel_path.clone(),
                            op: "delete_local",
                        });
                    }
                }
            }
            SyncDirection::Both => {}
        }
    }

    SyncDagPlan {
        transfers,
        skips,
        deletes,
    }
}

/// A transfer node handing one planned file off to the serial driver.
struct TransferJob {
    /// Index into [`SyncDagPlan::transfers`].
    transfer_index: usize,
    /// Resolved once the driver has performed the transfer.
    ack: oneshot::Sender<()>,
}

/// Build the node runner for a sync graph.
///
/// Every structural node (discover / compare / acquire / verify / preserve /
/// commit / emit) is a no-op anchor. Only the real `DownloadFile` /
/// `UploadFile` nodes carry work: each one looks up its plan index, hands a
/// [`TransferJob`] to the driver, and waits for the acknowledgement before
/// reporting completion, so the graph's `EmitProgress` terminal fires only
/// after the real transfer finished.
///
/// A node always reports [`NodeOutcome::Completed`]: a failed file is
/// recorded in the [`SyncReport`] by the driver, never on the graph, exactly
/// like the phase-2 batch path. The graph therefore never aborts mid-sync.
fn build_sync_runner(
    node_to_index: Arc<HashMap<usize, usize>>,
    job_tx: mpsc::Sender<TransferJob>,
) -> Arc<dyn DagNodeRunner> {
    Arc::new(move |node: TransferNode| -> NodeFuture {
        let node_to_index = Arc::clone(&node_to_index);
        let job_tx = job_tx.clone();
        Box::pin(async move {
            if !matches!(
                node.kind,
                TransferNodeKind::DownloadFile | TransferNodeKind::UploadFile
            ) {
                return NodeOutcome::Completed;
            }
            let Some(&transfer_index) = node_to_index.get(&node.id) else {
                return NodeOutcome::Completed;
            };
            let (ack_tx, ack_rx) = oneshot::channel::<()>();
            if job_tx
                .send(TransferJob {
                    transfer_index,
                    ack: ack_tx,
                })
                .await
                .is_err()
            {
                // The driver is gone; nothing more this node can do.
                return NodeOutcome::Completed;
            }
            let _ = ack_rx.await;
            NodeOutcome::Completed
        })
    })
}

/// Drain the transfer-job channel, performing each file serially.
///
/// Runs on the `execute_sync_dag` task itself (not spawned), so it can hold
/// the borrowed provider, delta batch, report and sink that the graph's
/// spawned nodes cannot. The loop ends when every sender drops: that happens
/// when `execute_dag` finishes its body and releases the runner, which is
/// the only thing the channel needs to terminate cleanly.
#[allow(clippy::too_many_arguments)]
async fn drive_sync_transfers(
    mut job_rx: mpsc::Receiver<TransferJob>,
    provider: &mut Box<dyn StorageProvider>,
    delta_batch: &mut Option<Box<dyn DeltaBatch>>,
    report: &mut SyncReport,
    sink: &mut dyn SyncProgressSink,
    transfers: &[PlannedTransfer],
    local_root: &str,
    remote_root: &str,
    download_segments: u32,
    requested_policy: DeltaPolicy,
) {
    while let Some(job) = job_rx.recv().await {
        let transfer = &transfers[job.transfer_index];
        let spec = SyncTransferSpec {
            rel: &transfer.rel,
            total: transfer.total,
            decision_policy: transfer.decision_policy,
            requested_policy,
        };
        let outcome = match transfer.op {
            "upload" => {
                perform_upload(
                    provider,
                    local_root,
                    remote_root,
                    spec,
                    false,
                    sink,
                    delta_batch,
                )
                .await
            }
            _ => {
                perform_download(
                    provider,
                    local_root,
                    remote_root,
                    spec,
                    download_segments,
                    false,
                    sink,
                    delta_batch,
                )
                .await
            }
        };
        apply_sync_tree_outcome(
            report,
            &transfer.rel,
            transfer.op,
            outcome,
            transfer.decision_policy,
            sink,
        );
        let _ = job.ack.send(());
    }
}

/// Run a full sync session through the graph engine.
///
/// A drop-in replacement for [`crate::sync::sync_tree_core`]: same arguments,
/// same [`SyncReport`]. Reached only with [`should_route_sync_to_dag`] true,
/// i.e. the [`DAG_SYNC_ENV`] flag on and a non-dry-run sync; the legacy
/// interleaved path stays the default.
pub async fn execute_sync_dag(
    provider: &mut Box<dyn StorageProvider>,
    local_root: &str,
    remote_root: &str,
    opts: &SyncOptions,
    sink: &mut dyn SyncProgressSink,
) -> SyncReport {
    let start = Instant::now();

    // Phase 1: Scanning. The local filesystem walk runs on a blocking thread
    // while the network-bound remote scan runs on the provider, so the two
    // overlap instead of running back to back.
    sink.on_phase(SyncPhase::Scanning);
    let local_handle = {
        let root = local_root.to_string();
        let scan_opts = opts.scan.clone();
        tokio::task::spawn_blocking(move || scan_local_tree(&root, &scan_opts))
    };
    let remotes = scan_remote_tree(provider, remote_root, &opts.scan).await;
    // `scan_local_tree` itself returns a `Vec` (best-effort, mirroring
    // `scan_remote_tree`), so the only way the join returns `Err` is a panic
    // inside the blocking thread. `.unwrap_or_default()` would silently turn
    // that panic into an empty local listing, which then drives a "no
    // uploads needed" decision through the planner: the run would report
    // success while the local side was never scanned. Surface the panic as
    // a hard `SyncError` on the report so the caller sees the failure;
    // keep `locals` empty so the remote-only side of the run still proceeds.
    let (locals, local_scan_panic) = match local_handle.await {
        Ok(entries) => (entries, None),
        Err(join_err) => {
            eprintln!(
                "[execute_sync_dag] local scan task failed: {}",
                join_err
            );
            (Vec::new(), Some(join_err.to_string()))
        }
    };

    // Phase 2: Planning. Decisions read only the pre-transfer scan
    // snapshots, so resolving the whole plan up front is decision-equivalent
    // to the legacy interleaved decide/perform loop.
    sink.on_phase(SyncPhase::Planning);
    let mut report = SyncReport {
        dry_run: opts.dry_run,
        direction: Some(opts.direction),
        delta_policy: Some(opts.delta_policy),
        ..SyncReport::default()
    };
    if let Some(msg) = local_scan_panic {
        report.errors.push(SyncError {
            rel_path: local_root.to_string(),
            operation: "scan",
            message: format!("local scan task failed: {}", msg),
            decision_policy: opts.delta_policy,
        });
    }
    let plan = plan_sync_dag(&locals, &remotes, opts);

    // Phase 3: Executing.
    sink.on_phase(SyncPhase::Executing);

    // Mirror the legacy gate: create the remote root only for an
    // upload-bearing sync that actually has local files to push. Running
    // this after the scans (rather than before the remote scan, as the
    // legacy core does) is decision-equivalent: `scan_remote_tree` already
    // tolerates a missing directory by returning an empty listing.
    if !locals.is_empty() && matches!(opts.direction, SyncDirection::Upload | SyncDirection::Both) {
        ensure_remote_dir(provider, remote_root).await;
    }

    // Skips carry no I/O: emit them up front in plan order, before the
    // transfer graph runs.
    for skip in &plan.skips {
        sink.on_file_start(&skip.rel, 0, "skip", skip.decision_policy);
        apply_sync_tree_outcome(
            &mut report,
            &skip.rel,
            skip.apply_op,
            FileOutcome::Skipped {
                reason: skip.reason.clone(),
            },
            skip.decision_policy,
            sink,
        );
    }

    // Open the delta-sync batch once for the whole session: a single SSH
    // session reused across every eligible file, exactly like the legacy
    // core. `None` when delta policy is off or the provider is not
    // delta-eligible.
    let mut delta_batch: Option<Box<dyn DeltaBatch>> =
        if matches!(opts.delta_policy, DeltaPolicy::Delta) {
            crate::delta_sync_rsync::open_delta_batch(provider.as_mut()).await
        } else {
            None
        };

    if !plan.transfers.is_empty() {
        // One sync graph: a global DiscoverLocal/DiscoverRemote -> Compare
        // prefix, then a per-file transfer chain for every upload/download.
        let sync_items: Vec<SyncDagItem> = plan
            .transfers
            .iter()
            .map(|transfer| {
                let action = match transfer.op {
                    "upload" => SyncDagAction::Upload,
                    _ => SyncDagAction::Download,
                };
                SyncDagItem::new(transfer.rel.clone(), action)
            })
            .collect();
        let sync_dag = TransferDagBuilder::from_sync_plan(&sync_items);

        // The builder preserves plan order, so `files[i]` is `transfers[i]`.
        let node_to_index: Arc<HashMap<usize, usize>> = Arc::new(
            sync_dag
                .files
                .iter()
                .enumerate()
                .map(|(index, file)| (file.transfer, index))
                .collect(),
        );

        let (job_tx, job_rx) = mpsc::channel::<TransferJob>(JOB_CHANNEL_CAPACITY);
        let runner = build_sync_runner(node_to_index, job_tx);
        // A sync drives one provider connection: file_slots = 1 keeps the
        // transfer nodes strictly serial, matching the single delta-batch
        // session and the legacy core.
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(1));
        let observer: Arc<dyn DagObserver> = Arc::new(NoopDagObserver);
        // AIMD backpressure (F3-T05). file_slots = 1 means the File class
        // cannot shrink, so this is structurally inert today; it is wired for
        // uniformity with the batch and single-file surfaces and so the Api
        // class becomes live once a sync connection pool lands (Fase 3).
        let aimd = Arc::new(AimdController::from_budget(
            &manager.budget(),
            AimdConfig::default(),
        ));

        // The graph scheduling (spawned) and the real I/O driver (borrowing
        // the provider) run concurrently on this task. The driver loop ends
        // when `execute_dag` finishes its body and drops the runner, which
        // closes the job channel.
        let dag_result = {
            let dag_future = execute_dag(&sync_dag.dag, &manager, runner, observer, Some(aimd));
            let driver_future = drive_sync_transfers(
                job_rx,
                provider,
                &mut delta_batch,
                &mut report,
                sink,
                &plan.transfers,
                local_root,
                remote_root,
                opts.download_segments,
                opts.delta_policy,
            );
            let (dag_result, ()) = tokio::join!(dag_future, driver_future);
            dag_result
        };
        if let Err(error) = dag_result {
            // The runner never reports Failed, so a graph error here is an
            // executor-level fault. Surface it; the report still reflects
            // whatever the driver recorded.
            tracing::warn!("Sync transfer graph stopped early: {}", error);
        }
    }

    // Orphan deletion runs after every transfer, while the delta batch is
    // still open, exactly like the legacy core.
    for delete in &plan.deletes {
        let outcome = match delete.op {
            "delete_remote" => {
                perform_remote_delete(
                    provider,
                    remote_root,
                    &delete.rel,
                    opts.delta_policy,
                    false,
                    sink,
                )
                .await
            }
            _ => perform_local_delete(local_root, &delete.rel, opts.delta_policy, false, sink),
        };
        apply_sync_tree_outcome(
            &mut report,
            &delete.rel,
            delete.op,
            outcome,
            opts.delta_policy,
            sink,
        );
    }

    // Finalize the delta batch and surface the session-reuse metrics,
    // identical to the legacy core.
    if let Some(batch) = delta_batch.take() {
        match batch.finalize().await {
            Ok(stats) => {
                tracing::info!(
                    "sync.delta: batch finalize ok: files_transferred={}, session_count={}, bytes_on_wire={}, partial={}",
                    stats.files_transferred,
                    stats.session_count,
                    stats.bytes_on_wire,
                    stats.partial,
                );
                report.delta_session_count = Some(stats.session_count);
                report.delta_bytes_on_wire = Some(stats.bytes_on_wire);
                report.delta_batch_files = Some(stats.files_transferred);
            }
            Err(e) => {
                tracing::warn!("sync.delta: batch finalize failed: {e}");
            }
        }
    }

    report.elapsed_secs = start.elapsed().as_secs_f64();
    sink.on_phase(SyncPhase::Done);
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::{
        CompareDirection, JournalEntryStatus, RetryPolicy, SyncJournal, SyncJournalEntry,
        VerifyPolicy,
    };
    use crate::transfer_dag::observer::SyncJournalDagObserver;
    use crate::transfer_dag::{NoopDagObserver, OrderedDagObserver};
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    // ---- flag parsing -----------------------------------------------------

    #[test]
    fn flag_recognises_truthy_values_case_insensitively() {
        for on in ["1", "true", "TRUE", "On", "yes", " yes ", "Yes"] {
            assert!(flag_value_is_on(on), "{on:?} must be on");
        }
    }

    #[test]
    fn flag_rejects_falsey_and_garbage_values() {
        for off in ["0", "false", "off", "no", "", "  ", "enabled", "2", "y"] {
            assert!(!flag_value_is_on(off), "{off:?} must be off");
        }
    }

    /// Single env-touching test: no other test in this module reads the env
    /// var, so this cannot race them.
    #[test]
    fn routing_respects_flag_and_dry_run() {
        std::env::remove_var(DAG_SYNC_ENV);
        assert!(!dag_sync_enabled());
        assert!(!should_route_sync_to_dag(false));
        assert!(!should_route_sync_to_dag(true));

        std::env::set_var(DAG_SYNC_ENV, "1");
        assert!(dag_sync_enabled());
        // A real sync routes; a dry-run never does, even with the flag on.
        assert!(should_route_sync_to_dag(false));
        assert!(
            !should_route_sync_to_dag(true),
            "dry-run must never route to the mutating graph"
        );

        std::env::remove_var(DAG_SYNC_ENV);
    }

    // ---- plan_sync_dag ----------------------------------------------------

    fn local(rel: &str, size: u64) -> LocalEntry {
        LocalEntry {
            rel_path: rel.to_string(),
            size,
            mtime: None,
            sha256: None,
        }
    }

    fn remote(rel: &str, size: u64) -> RemoteEntry {
        RemoteEntry {
            rel_path: rel.to_string(),
            size,
            mtime: None,
            checksum_alg: None,
            checksum_hex: None,
        }
    }

    fn opts(direction: SyncDirection) -> SyncOptions {
        SyncOptions {
            direction,
            delta_policy: DeltaPolicy::SizeOnly,
            dry_run: false,
            delete_orphans: false,
            conflict_mode: crate::sync::ConflictMode::Newer,
            scan: Default::default(),
            download_segments: 1,
        }
    }

    #[test]
    fn plan_uploads_new_file_and_skips_identical() {
        let locals = vec![local("new.txt", 10), local("same.txt", 20)];
        let remotes = vec![remote("same.txt", 20)];
        let plan = plan_sync_dag(&locals, &remotes, &opts(SyncDirection::Upload));

        assert_eq!(plan.transfers.len(), 1);
        assert_eq!(plan.transfers[0].rel, "new.txt");
        assert_eq!(plan.transfers[0].op, "upload");
        assert_eq!(plan.transfers[0].total, 10);
        assert_eq!(plan.skips.len(), 1);
        assert_eq!(plan.skips[0].rel, "same.txt");
        assert_eq!(plan.skips[0].apply_op, "upload");
        assert!(plan.deletes.is_empty());
    }

    #[test]
    fn plan_downloads_new_remote_file_only() {
        let locals = vec![local("here.txt", 5)];
        let remotes = vec![remote("here.txt", 5), remote("only-remote.txt", 99)];
        let plan = plan_sync_dag(&locals, &remotes, &opts(SyncDirection::Download));

        assert_eq!(plan.transfers.len(), 1);
        assert_eq!(plan.transfers[0].rel, "only-remote.txt");
        assert_eq!(plan.transfers[0].op, "download");
        // The upload pass never runs in download direction.
        assert!(plan.transfers.iter().all(|t| t.op == "download"));
    }

    #[test]
    fn plan_both_direction_resolves_conflict_via_upload_then_skips_download() {
        // A file on both sides with different sizes: the upload pass resolves
        // it (ConflictMode::Newer always copies), and the download pass must
        // then skip it as already handled.
        let locals = vec![local("conflict.txt", 100)];
        let remotes = vec![remote("conflict.txt", 50)];
        let plan = plan_sync_dag(&locals, &remotes, &opts(SyncDirection::Both));

        assert_eq!(plan.transfers.len(), 1);
        assert_eq!(plan.transfers[0].op, "upload");
        assert_eq!(plan.transfers[0].rel, "conflict.txt");
        assert_eq!(plan.skips.len(), 1);
        assert_eq!(plan.skips[0].apply_op, "download");
        assert_eq!(plan.skips[0].rel, "conflict.txt");
    }

    #[test]
    fn plan_keeps_uploads_before_downloads_in_plan_order() {
        let locals = vec![local("up-only.txt", 1)];
        let remotes = vec![remote("down-only.txt", 2)];
        let plan = plan_sync_dag(&locals, &remotes, &opts(SyncDirection::Both));

        assert_eq!(plan.transfers.len(), 2);
        assert_eq!(plan.transfers[0].op, "upload");
        assert_eq!(plan.transfers[1].op, "download");
    }

    #[test]
    fn plan_delete_orphans_marks_the_right_side_per_direction() {
        let locals = vec![local("keep.txt", 1), local("local-orphan.txt", 1)];
        let remotes = vec![remote("keep.txt", 1), remote("remote-orphan.txt", 1)];

        let mut upload = opts(SyncDirection::Upload);
        upload.delete_orphans = true;
        let plan = plan_sync_dag(&locals, &remotes, &upload);
        assert_eq!(plan.deletes.len(), 1);
        assert_eq!(plan.deletes[0].rel, "remote-orphan.txt");
        assert_eq!(plan.deletes[0].op, "delete_remote");

        let mut download = opts(SyncDirection::Download);
        download.delete_orphans = true;
        let plan = plan_sync_dag(&locals, &remotes, &download);
        assert_eq!(plan.deletes.len(), 1);
        assert_eq!(plan.deletes[0].rel, "local-orphan.txt");
        assert_eq!(plan.deletes[0].op, "delete_local");

        // Bidirectional sync resolves orphans as transfers, never deletes.
        let mut both = opts(SyncDirection::Both);
        both.delete_orphans = true;
        let plan = plan_sync_dag(&locals, &remotes, &both);
        assert!(plan.deletes.is_empty());
    }

    #[test]
    fn plan_deduplicates_repeated_scan_entries() {
        let locals = vec![local("dup.txt", 7), local("dup.txt", 7)];
        let plan = plan_sync_dag(&locals, &[], &opts(SyncDirection::Upload));
        assert_eq!(
            plan.transfers.len(),
            1,
            "a repeated scan path is planned once"
        );
    }

    // ---- runner + driver mechanics ---------------------------------------

    #[tokio::test]
    async fn sync_runner_drives_every_transfer_exactly_once() {
        let items = vec![
            SyncDagItem::new("a.txt", SyncDagAction::Upload),
            SyncDagItem::new("b.txt", SyncDagAction::Download),
            SyncDagItem::new("c.txt", SyncDagAction::Upload),
        ];
        let sync_dag = TransferDagBuilder::from_sync_plan(&items);
        let node_to_index: Arc<HashMap<usize, usize>> = Arc::new(
            sync_dag
                .files
                .iter()
                .enumerate()
                .map(|(index, file)| (file.transfer, index))
                .collect(),
        );

        let (job_tx, mut job_rx) = mpsc::channel::<TransferJob>(JOB_CHANNEL_CAPACITY);
        let runner = build_sync_runner(node_to_index, job_tx);
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(1));
        let observer: Arc<dyn DagObserver> = Arc::new(NoopDagObserver);

        let seen: Arc<StdMutex<Vec<usize>>> = Arc::new(StdMutex::new(Vec::new()));
        let driver = {
            let seen = Arc::clone(&seen);
            async move {
                while let Some(job) = job_rx.recv().await {
                    seen.lock().unwrap().push(job.transfer_index);
                    let _ = job.ack.send(());
                }
            }
        };
        let dag_future = execute_dag(&sync_dag.dag, &manager, runner, observer, None);
        let (dag_result, ()) = tokio::join!(dag_future, driver);

        let summary = dag_result.expect("sync graph must schedule cleanly");
        // 3 prefix nodes + 3 files x 6 chain nodes.
        assert_eq!(summary.nodes_completed, 21);
        let mut seen = seen.lock().unwrap().clone();
        seen.sort_unstable();
        assert_eq!(seen, vec![0, 1, 2], "every transfer is driven exactly once");
    }

    #[tokio::test]
    async fn sync_runner_handles_an_empty_plan() {
        let sync_dag = TransferDagBuilder::from_sync_plan(&[]);
        let node_to_index: Arc<HashMap<usize, usize>> = Arc::new(HashMap::new());
        let (job_tx, mut job_rx) = mpsc::channel::<TransferJob>(JOB_CHANNEL_CAPACITY);
        let runner = build_sync_runner(node_to_index, job_tx);
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(1));
        let observer: Arc<dyn DagObserver> = Arc::new(NoopDagObserver);

        let driver = async move {
            let mut jobs = 0usize;
            while job_rx.recv().await.is_some() {
                jobs += 1;
            }
            jobs
        };
        let dag_future = execute_dag(&sync_dag.dag, &manager, runner, observer, None);
        let (dag_result, jobs) = tokio::join!(dag_future, driver);

        assert_eq!(jobs, 0, "an empty sync plan hands off no transfer jobs");
        // Only the 3 global discover/compare prefix nodes.
        assert_eq!(dag_result.unwrap().nodes_completed, 3);
    }

    // ---- journal observer interop (F2-T10) -------------------------------

    fn journal_with_pending(paths: &[(&str, &str)]) -> SyncJournal {
        let mut journal = SyncJournal::new(
            "/local".to_string(),
            "/remote".to_string(),
            CompareDirection::Bidirectional,
            RetryPolicy::default(),
            VerifyPolicy::SizeOnly,
        );
        for (rel, action) in paths {
            journal.entries.push(SyncJournalEntry {
                relative_path: rel.to_string(),
                action: action.to_string(),
                status: JournalEntryStatus::Pending,
                attempts: 1,
                last_error: None,
                verified: None,
                bytes_transferred: 0,
            });
        }
        journal
    }

    #[tokio::test]
    async fn sync_journal_observer_completes_entries_over_the_sync_graph() {
        // The sync graph's per-file EmitProgress terminals must drive the
        // legacy journal to completion, in plan order, so a journal written
        // by the DAG path is interoperable with the legacy resume logic.
        let items = vec![
            SyncDagItem::new("up.txt", SyncDagAction::Upload),
            SyncDagItem::new("down.txt", SyncDagAction::Download),
        ];
        let sync_dag = TransferDagBuilder::from_sync_plan(&items);
        let terminals = sync_dag.journal_terminals_for_plan_order();
        assert_eq!(terminals.len(), 2);

        let journal = Arc::new(std::sync::Mutex::new(journal_with_pending(&[
            ("up.txt", "upload"),
            ("down.txt", "download"),
        ])));
        let persisted = Arc::new(std::sync::Mutex::new(0u32));
        let persist = {
            let persisted = Arc::clone(&persisted);
            Arc::new(move |_journal: &SyncJournal| {
                *persisted.lock().unwrap() += 1;
                Ok(())
            })
        };
        let journal_observer =
            SyncJournalDagObserver::with_persist(Arc::clone(&journal), terminals, persist);
        // "Journal durable first, visible completion second": the journal
        // observer is composed ahead of any visible observer.
        let observer: Arc<dyn DagObserver> = Arc::new(OrderedDagObserver::new(vec![
            Arc::new(journal_observer) as Arc<dyn DagObserver>,
            Arc::new(NoopDagObserver) as Arc<dyn DagObserver>,
        ]));

        let runner: Arc<dyn DagNodeRunner> = Arc::new(|_node: TransferNode| -> NodeFuture {
            Box::pin(async { NodeOutcome::Completed })
        });
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(1));
        execute_dag(&sync_dag.dag, &manager, runner, observer, None)
            .await
            .expect("sync graph must schedule cleanly");

        let journal = journal.lock().unwrap();
        assert!(
            journal
                .entries
                .iter()
                .all(|entry| entry.status == JournalEntryStatus::Completed),
            "every journal entry must be marked completed"
        );
        assert!(
            journal.completed,
            "a fully transferred journal is completed"
        );
        assert!(
            !journal.has_resumable_entries(),
            "a completed journal has nothing to resume"
        );
        assert!(*persisted.lock().unwrap() >= 1, "the journal was persisted");
    }

    #[test]
    fn partial_journal_stays_resumable_for_the_legacy_path() {
        // A journal interrupted mid-run (flag ON) keeps the standard
        // Pending/Completed split, so the legacy path (flag OFF) resumes it.
        let mut journal = journal_with_pending(&[("a.txt", "upload"), ("b.txt", "upload")]);
        journal.entries[0].status = JournalEntryStatus::Completed;
        assert!(
            journal.has_resumable_entries(),
            "a half-finished journal is resumable regardless of which engine wrote it"
        );
    }

    // ---- graph build cost (F2-T12) ---------------------------------------

    #[test]
    fn sync_graph_build_cost_is_well_under_50ms() {
        // The watch loop rebuilds the sync graph every cycle; the build must
        // stay far below the filesystem-watcher cooldown.
        let items: Vec<SyncDagItem> = (0..1000)
            .map(|i| SyncDagItem::new(format!("file-{i}.bin"), SyncDagAction::Upload))
            .collect();
        let start = Instant::now();
        let sync_dag = TransferDagBuilder::from_sync_plan(&items);
        let elapsed = start.elapsed();

        assert_eq!(sync_dag.files.len(), 1000);
        assert!(
            elapsed < Duration::from_millis(50),
            "1000-file sync graph build took {elapsed:?}, budget is 50ms"
        );
    }
}
