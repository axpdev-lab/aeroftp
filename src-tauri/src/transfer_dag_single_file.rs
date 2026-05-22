// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! DAG-ENGINE phase 1: the single-file transfer path through `execute_dag`.
//!
//! [`crate::transfer_dag`] is the pure, provider-free graph engine. This
//! module is the thin provider-coupled bridge that binds the seven-node
//! single-file graph produced by [`TransferDagBuilder::single_file`] to a real
//! [`StorageProvider`]: it is the [`DagNodeRunner`] for the single-file shape.
//!
//! ## Scope
//!
//! Phase 1 routes the *plain classic single-file leaf* of a download or upload
//! through the graph engine. It deliberately does NOT replace the delta,
//! intra-file segmented, resume, or GitHub-commit paths: those keep their
//! legacy code. The GUI ([`crate::provider_commands`]) and CLI (`aeroftp_cli`)
//! entry points run their delta / segmented / resume logic exactly as before
//! and hand only the plain leaf to [`execute_single_file_dag`]. The result is
//! byte-identical: the same `provider.download` / `provider.upload` call, the
//! same per-byte progress callback, behind a graph schedule.
//!
//! ## Feature flag
//!
//! The whole path is gated by [`dag_single_file_enabled`], read from the
//! [`DAG_SINGLE_FILE_ENV`] environment variable, default OFF. With the flag
//! off, not one byte of this module executes and the legacy path is verbatim.
//! The flag is a runtime toggle (not a recompile) so the parity harness can
//! measure flag-OFF vs flag-ON in a single build.
//!
//! ## Node bindings
//!
//! For the linear seven-node single-file graph the runner binds:
//!
//! - `DiscoverRemote` / `DiscoverLocal`: no-op. The transfer size is already
//!   resolved by the shared legacy pre-DAG code; re-resolving it here would
//!   add a wire round-trip and break the byte-identical-on-the-wire contract.
//! - `AcquireResource`: no-op (phase 3 hooks the resume-checkpoint fetch).
//! - `DownloadFile` / `UploadFile`: the one real I/O node. It carries the only
//!   scarce resource and runs `provider.download` / `provider.upload` with the
//!   caller's progress callback.
//! - `VerifyChecksum`: no-op in phase 1 (the legacy single-file path does not
//!   verify).
//! - `PreserveMetadata`: restores the remote mtime on a downloaded file; a
//!   no-op on the upload direction.
//! - `CommitTemp`: no-op (the provider's own `.aerotmp` finalize is internal).
//! - `EmitProgress`: no-op terminal node; its completion is the signal a
//!   [`DagObserver`] maps onto the GUI "complete" event.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::Mutex;

use crate::providers::{ProviderError, StorageProvider};
use crate::transfer_dag::executor::{execute_dag, DagNodeRunner, NodeFuture, NodeOutcome};
use crate::transfer_dag::graph::{TransferNode, TransferNodeKind};
use crate::transfer_dag::{
    DagObserver, SingleFileDag, TransferBudget, TransferDirection, TransferResourceManager,
};

/// Environment variable that gates the phase-1 single-file DAG path.
///
/// Recognised truthy values (case-insensitive, trimmed): `1`, `true`, `on`,
/// `yes`. Anything else, including an unset variable, leaves the path OFF.
pub const DAG_SINGLE_FILE_ENV: &str = "AEROFTP_TRANSFER_ENGINE_DAG_SINGLE_FILE";

/// A per-byte transfer progress callback, as accepted by
/// [`StorageProvider::download`] / [`StorageProvider::upload`].
pub type ProgressCallback = Box<dyn Fn(u64, u64) + Send>;

/// The connected-provider handle shared between the GUI command state and the
/// spawned DAG node tasks. `Option` because a session may be disconnected.
pub type SharedProvider = Arc<Mutex<Option<Box<dyn StorageProvider>>>>;

/// Returns `true` when the single-file DAG path is enabled for this process.
///
/// Default OFF: a phased migration enters production behind a flag so a
/// rollback is a toggle, never a revert (DAG-ENGINE plan, principle 2).
pub fn dag_single_file_enabled() -> bool {
    std::env::var(DAG_SINGLE_FILE_ENV)
        .ok()
        .map(|raw| flag_value_is_on(&raw))
        .unwrap_or(false)
}

/// Parses one environment-variable value into the on/off flag state.
fn flag_value_is_on(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "on" | "yes"
    )
}

/// Run a plain single-file transfer through the graph engine.
///
/// `built` is the seven-node graph from [`TransferDagBuilder::single_file`];
/// the caller builds it once so it can also read `built.emit_progress` to
/// construct an observer. `provider` is locked by the `DownloadFile` /
/// `UploadFile` node from its spawned task: the caller MUST NOT hold a guard
/// on the same mutex across this call or the node would deadlock.
///
/// On the download direction the actual on-disk size after a successful
/// transfer is stored into `report_size` (the observer reports it in the
/// completion event); on the upload direction `report_size` is left at the
/// value the caller seeded it with. The typed [`ProviderError`] of a failed
/// transfer is recovered and returned, so the caller keeps its exact error
/// classification (CLI exit codes, GUI error strings) unchanged.
#[allow(clippy::too_many_arguments)]
pub async fn execute_single_file_dag(
    built: &SingleFileDag,
    provider: SharedProvider,
    remote_path: String,
    local_path: String,
    modified: Option<String>,
    progress_cb: Option<ProgressCallback>,
    observer: Arc<dyn DagObserver>,
    report_size: Arc<AtomicU64>,
) -> Result<(), ProviderError> {
    let direction = built.direction;
    let remote: Arc<str> = Arc::from(remote_path.as_str());
    let local: Arc<str> = Arc::from(local_path.as_str());
    let modified: Option<Arc<str>> = modified.as_deref().map(Arc::from);
    // The progress callback is consumed once, by the transfer node; the slot
    // makes the runner closure `Fn` (it can hand the callback out without the
    // closure itself being `FnOnce`).
    let progress_slot: Arc<StdMutex<Option<ProgressCallback>>> =
        Arc::new(StdMutex::new(progress_cb));
    // The executor only surfaces a stringified `DagExecutionError`; the typed
    // provider error is stashed here so the caller keeps exact error semantics.
    let first_error: Arc<StdMutex<Option<ProviderError>>> = Arc::new(StdMutex::new(None));

    let runner: Arc<dyn DagNodeRunner> = {
        let provider = Arc::clone(&provider);
        let remote = Arc::clone(&remote);
        let local = Arc::clone(&local);
        let modified = modified.clone();
        let progress_slot = Arc::clone(&progress_slot);
        let first_error = Arc::clone(&first_error);
        let report_size = Arc::clone(&report_size);
        Arc::new(move |node: TransferNode| -> NodeFuture {
            let provider = Arc::clone(&provider);
            let remote = Arc::clone(&remote);
            let local = Arc::clone(&local);
            let modified = modified.clone();
            let progress_slot = Arc::clone(&progress_slot);
            let first_error = Arc::clone(&first_error);
            let report_size = Arc::clone(&report_size);
            Box::pin(async move {
                match node.kind {
                    TransferNodeKind::DownloadFile => {
                        let cb = progress_slot.lock().expect("progress slot poisoned").take();
                        let mut guard = provider.lock().await;
                        let Some(p) = guard.as_mut() else {
                            return record_failure(
                                &first_error,
                                ProviderError::NotConnected,
                            );
                        };
                        match p.download(&remote, &local, cb).await {
                            Ok(()) => {
                                // Report the real on-disk size; fall back to the
                                // caller-seeded value if the stat fails.
                                if let Ok(meta) = std::fs::metadata(&*local) {
                                    report_size.store(meta.len(), Ordering::SeqCst);
                                }
                                NodeOutcome::Completed
                            }
                            Err(e) => record_failure(&first_error, e),
                        }
                    }
                    TransferNodeKind::UploadFile => {
                        let cb = progress_slot.lock().expect("progress slot poisoned").take();
                        let mut guard = provider.lock().await;
                        let Some(p) = guard.as_mut() else {
                            return record_failure(
                                &first_error,
                                ProviderError::NotConnected,
                            );
                        };
                        match p.upload(&local, &remote, cb).await {
                            Ok(()) => NodeOutcome::Completed,
                            Err(e) => record_failure(&first_error, e),
                        }
                    }
                    TransferNodeKind::PreserveMetadata => {
                        if direction == TransferDirection::Download {
                            crate::preserve_remote_mtime(&local, modified.as_deref());
                        }
                        NodeOutcome::Completed
                    }
                    // DiscoverRemote / DiscoverLocal / AcquireResource /
                    // VerifyChecksum / CommitTemp / EmitProgress: structural
                    // anchors, no-ops in phase 1. A single-file graph never
                    // produces any other kind.
                    _ => NodeOutcome::Completed,
                }
            })
        })
    };

    // A single-file transfer needs exactly one file slot; the other six nodes
    // request nothing, so the graph cannot deadlock against its own budget.
    let manager = TransferResourceManager::new(TransferBudget::from_file_slots(1));

    match execute_dag(&built.dag, &manager, runner, observer, None).await {
        Ok(_summary) => Ok(()),
        Err(dag_err) => Err(first_error
            .lock()
            .expect("first-error slot poisoned")
            .take()
            .unwrap_or_else(|| ProviderError::TransferFailed(dag_err.to_string()))),
    }
}

/// Stash the typed error for the caller and return the matching node failure.
fn record_failure(
    first_error: &Arc<StdMutex<Option<ProviderError>>>,
    error: ProviderError,
) -> NodeOutcome {
    let message = error.to_string();
    let mut slot = first_error.lock().expect("first-error slot poisoned");
    if slot.is_none() {
        *slot = Some(error);
    }
    NodeOutcome::Failed(message)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn flag_is_off_when_env_is_unset() {
        // The test runner does not set the variable; default must be OFF.
        std::env::remove_var(DAG_SINGLE_FILE_ENV);
        assert!(!dag_single_file_enabled());
    }
}
