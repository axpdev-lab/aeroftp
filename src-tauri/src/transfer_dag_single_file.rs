// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! DAG-ENGINE shaped-graph wiring: the single-file transfer path through
//! `execute_dag`, with native multipart upload fan-out when the provider
//! advertises the capability.
//!
//! [`crate::transfer_dag`] is the pure, provider-free graph engine. This
//! module is the thin provider-coupled bridge that binds a
//! [`ShapedFileDag`] to a real [`StorageProvider`]: it is the
//! [`DagNodeRunner`] for the single-file shape, including the fan-out variant
//! where the transfer core is N `UploadPart` nodes instead of one
//! `UploadFile`.
//!
//! ## Scope
//!
//! The shaped-graph builder picks between two transfer-core shapes for the
//! seven-node single-file graph:
//!
//! - **Single transfer core**: a `DownloadFile` (download direction) or
//!   `UploadFile` (upload direction). Identical to the original phase-1
//!   wiring; the call path through the runner is byte-identical with the
//!   legacy `provider.download` / `provider.upload`.
//! - **Multipart upload fan-out**: when `direction == Upload` and the
//!   provider's [`TransferCapabilities::multipart_upload`] is available, the
//!   builder emits N `UploadPart` nodes, one per chunk. The runner then
//!   orchestrates `begin_multipart_upload` (lazy, once per session),
//!   `upload_part` (one per `UploadPart` node, parallelizable through the
//!   shared chunk budget), and `complete_multipart_upload` (driven by the
//!   terminal `CommitTemp` node). On any failure the runner best-effort
//!   `abort_multipart_upload` so the provider does not accumulate orphan
//!   upload IDs.
//!
//! The shaped-graph path deliberately keeps the delta, intra-file segmented,
//! resume, and GitHub-commit paths on their legacy code. The GUI
//! ([`crate::provider_commands`]) and CLI (`aeroftp_cli`) entry points run
//! their delta / segmented / resume logic exactly as before and hand only the
//! plain leaf to [`execute_single_file_dag`]. For uploads on a multipart-
//! capable provider the leaf now opens a native multipart session: the
//! observable result is the same finalized object on the remote, with a
//! different on-the-wire control flow.
//!
//! ## Node bindings
//!
//! For the seven-node single-file graph the runner binds:
//!
//! - `DiscoverRemote` / `DiscoverLocal`: no-op. The transfer size is already
//!   resolved by the shared legacy pre-DAG code; re-resolving it here would
//!   add a wire round-trip and break the byte-identical-on-the-wire contract.
//! - `AcquireResource`: no-op (phase 3 hooks the resume-checkpoint fetch).
//! - `DownloadFile` / `UploadFile`: the one real I/O node when the transfer
//!   core is single. Carries the only scarce resource and runs
//!   `provider.download` / `provider.upload` with the caller's progress
//!   callback.
//! - `UploadPart`: one real I/O node per chunk when the transfer core fans
//!   out. The first to enter the runner opens the multipart session under a
//!   per-context mutex; each part reads its slice from disk at the matching
//!   offset and submits it through `provider.upload_part`. Receipts
//!   accumulate in shared state for the commit node.
//! - `ServerSideCopy`: structural anchor in the single-file graph (the
//!   shaped-copy graph is the place where the kind is actually emitted by
//!   the builder); included for forward-compatibility.
//! - `VerifyChecksum`: real node on the durable native-multipart path. It runs
//!   after every `UploadPart` receipt and before `CommitTemp`, so the remote
//!   object is not yet finalized: it makes no provider checksum claim and
//!   instead verifies, from durable facts, that all parts are present and the
//!   local source still matches the identity that produced them, then records
//!   the durable `Verified` fact. On the plain single-transfer-core path there
//!   is no journal, so it is an honest no-op that makes no claim.
//! - `PreserveMetadata`: restores the remote mtime on a downloaded file; a
//!   no-op on the upload direction.
//! - `CommitTemp`: the commit node for native multipart. It is fail-closed on
//!   the durable `Verified` fact, then takes the accumulated parts, sorts by
//!   part number, calls `provider.complete_multipart_upload` to finalize the
//!   object, and records the durable `Committed` fact before reporting success.
//!   For the single transfer core it makes no atomic-commit claim: any temp
//!   rename or in-place boundary is owned by the provider's own download/upload.
//! - `EmitProgress`: no-op terminal node; its completion is the signal a
//!   [`DagObserver`] maps onto the GUI "complete" event.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::providers::{MultipartHandle, ProviderError, StorageProvider, UploadedPart};
use crate::transfer_dag::executor::{
    execute_dag_with_options, DagExecuteOptions, DagNodeRunner, NodeFuture, NodeOutcome,
};
use crate::transfer_dag::graph::{TransferNode, TransferNodeKind};
use crate::transfer_dag::{
    AimdConfig, AimdController, CopyDag, DagObserver, DiskLeaseRequest, FailureScope,
    ObservedOutcome, ShapedFileDag, TransferBudget, TransferCapabilities, TransferDagBuilder,
    TransferDagMetrics, TransferDirection, TransferError, TransferErrorKind, TransferPriority,
};
use crate::transfer_multipart::{clone_multipart_worker, MultipartFileState, MultipartLayout};

/// A per-byte transfer progress callback, as accepted by
/// [`StorageProvider::download`] / [`StorageProvider::upload`].
pub type ProgressCallback = Box<dyn Fn(u64, u64) + Send>;

/// The connected-provider handle shared between the GUI command state and the
/// spawned DAG node tasks. `Option` because a session may be disconnected.
pub type SharedProvider = Arc<Mutex<Option<Box<dyn StorageProvider>>>>;

/// The small synchronous bridge between the durable journal and the async DAG
/// runner. It never schedules work or owns provider permits; it merely makes a
/// durable fact survive before the corresponding node becomes visible as done.
struct DurableMultipartCheckpoint {
    store: crate::transfer_dag::TransferCheckpointStore,
    record: crate::transfer_dag::MultipartCheckpoint,
}

impl DurableMultipartCheckpoint {
    fn restored_handle(&self) -> Option<MultipartHandle> {
        self.record
            .upload_id
            .as_ref()
            .map(|upload_id| MultipartHandle {
                upload_id: upload_id.clone(),
                remote_path: self.record.destination.remote_path.clone(),
            })
    }

    fn restored_receipts(&self) -> Vec<UploadedPart> {
        self.record
            .receipts
            .values()
            .map(|receipt| UploadedPart {
                part_number: receipt.part_number,
                etag: receipt.etag.clone(),
            })
            .collect()
    }

    fn has_receipt(&self, part_number: u32) -> bool {
        self.record.receipts.contains_key(&part_number)
    }

    fn begin(&mut self, handle: &MultipartHandle) -> Result<(), ProviderError> {
        self.store
            .begin(&mut self.record, handle.upload_id.clone())
            .map_err(ProviderError::TransferFailed)
    }

    fn record_receipt(&mut self, receipt: &UploadedPart) -> Result<(), ProviderError> {
        self.store
            .record_receipt(
                &mut self.record,
                crate::transfer_dag::CheckpointPartReceipt {
                    part_number: receipt.part_number,
                    etag: receipt.etag.clone(),
                },
            )
            .map_err(ProviderError::TransferFailed)
    }

    /// Truthful pre-commit verification: check the durable payload facts against
    /// a fresh observation of the local source, then record the durable Verified
    /// fact. Fail-closed: a failed check never advances the record, so the
    /// commit node (which requires Verified) cannot finalize the session.
    fn verify(
        &mut self,
        observed: &crate::transfer_dag::ObservedSource,
    ) -> Result<(), ProviderError> {
        self.record
            .verify_against_source(observed)
            .map_err(ProviderError::TransferFailed)?;
        self.store
            .mark_verified(&mut self.record)
            .map_err(ProviderError::TransferFailed)
    }

    /// Whether the durable verified gate has been passed. The commit node checks
    /// this before finalizing the provider session so an unverified payload can
    /// never be committed or reported as complete.
    fn is_verified(&self) -> bool {
        self.record.is_verified()
    }

    fn mark_committed(&mut self) -> Result<(), ProviderError> {
        self.store
            .mark_committed(&mut self.record)
            .map_err(ProviderError::TransferFailed)
    }

    fn mark_failed(&mut self) {
        let _ = self.store.mark_failed(&mut self.record);
    }
}

/// Observe the local source file for pre-commit verification. A stat failure is
/// reported as a non-existent source so verification fails closed rather than
/// silently skipping the check.
fn observe_source(path: &str) -> crate::transfer_dag::ObservedSource {
    match std::fs::metadata(path) {
        Ok(meta) => crate::transfer_dag::ObservedSource {
            exists: true,
            size: meta.len(),
            modified_unix_nanos: meta
                .modified()
                .ok()
                .and_then(|stamp| stamp.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|stamp| stamp.as_nanos()),
        },
        Err(_) => crate::transfer_dag::ObservedSource {
            exists: false,
            size: 0,
            modified_unix_nanos: None,
        },
    }
}

/// Provider ownership accepted by the production copy DAG.
///
/// GUI commands keep the connected provider in an optional slot while CLI
/// commands and the WebDAV bridge own a mandatory provider box. Both shapes
/// route through the same runner without moving or duplicating the provider.
#[derive(Clone)]
pub enum CopyProviderHandle {
    Optional(SharedProvider),
    Required(Arc<Mutex<Box<dyn StorageProvider>>>),
}

impl CopyProviderHandle {
    pub fn optional(provider: SharedProvider) -> Self {
        Self::Optional(provider)
    }

    pub fn required(provider: Arc<Mutex<Box<dyn StorageProvider>>>) -> Self {
        Self::Required(provider)
    }

    async fn transfer_capabilities(&self) -> Result<TransferCapabilities, ProviderError> {
        match self {
            Self::Optional(provider) => provider
                .lock()
                .await
                .as_ref()
                .map(|provider| provider.transfer_capabilities())
                .ok_or(ProviderError::NotConnected),
            Self::Required(provider) => Ok(provider.lock().await.transfer_capabilities()),
        }
    }

    async fn source_size(&self, path: &str) -> Option<u64> {
        match self {
            Self::Optional(provider) => {
                let mut guard = provider.lock().await;
                let provider = guard.as_mut()?;
                provider.size(path).await.ok()
            }
            Self::Required(provider) => provider.lock().await.size(path).await.ok(),
        }
    }

    async fn endpoint_identity(
        &self,
    ) -> Result<crate::transfer_dag::EndpointIdentity, ProviderError> {
        match self {
            Self::Optional(provider) => provider
                .lock()
                .await
                .as_ref()
                .map(|provider| provider.endpoint_identity())
                .ok_or(ProviderError::NotConnected),
            Self::Required(provider) => Ok(provider.lock().await.endpoint_identity()),
        }
    }

    async fn server_side_copy(&self, from: &str, to: &str) -> Result<(), ProviderError> {
        match self {
            Self::Optional(provider) => {
                let mut guard = provider.lock().await;
                let provider = guard.as_mut().ok_or(ProviderError::NotConnected)?;
                provider.server_side_copy(from, to).await
            }
            Self::Required(provider) => provider.lock().await.server_side_copy(from, to).await,
        }
    }

    async fn download(&self, remote: &str, local: &str) -> Result<(), ProviderError> {
        match self {
            Self::Optional(provider) => {
                let mut guard = provider.lock().await;
                let provider = guard.as_mut().ok_or(ProviderError::NotConnected)?;
                provider.download(remote, local, None).await
            }
            Self::Required(provider) => provider.lock().await.download(remote, local, None).await,
        }
    }

    async fn upload(&self, local: &str, remote: &str) -> Result<(), ProviderError> {
        match self {
            Self::Optional(provider) => {
                let mut guard = provider.lock().await;
                let provider = guard.as_mut().ok_or(ProviderError::NotConnected)?;
                provider.upload(local, remote, None).await
            }
            Self::Required(provider) => provider.lock().await.upload(local, remote, None).await,
        }
    }
}

/// Typed production decision for a copy operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyDecision {
    /// The provider moved the bytes without a local payload path.
    ServerSide,
    /// The shaped graph exposed both payload legs.
    DownloadUpload { trigger: CopyFallbackTrigger },
}

/// Why the production copy path selected the download-upload subgraph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyFallbackTrigger {
    /// Capability shaping selected the two-node core before dispatch.
    CapabilityUnavailable,
    /// A native copy node was dispatched, then rejected at a capability
    /// boundary classified by `should_attempt_copy_fallback`.
    ServerRejected { kind: TransferErrorKind },
}

/// Completed production copy with its observed logical and data-path totals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyDagOutcome {
    pub decision: CopyDecision,
    pub metrics: TransferDagMetrics,
}

/// Observer adapter for a logical copy that can execute a second shaped graph
/// after a recoverable native-copy rejection.
///
/// Node ids from the second graph are offset so lifecycle events remain unique.
/// Executor-level per-graph snapshots are suppressed; `execute_copy_dag`
/// emits one combined logical-vs-wire snapshot after the whole copy completes.
struct CopyDagObserver {
    inner: Arc<dyn DagObserver>,
    node_offset: usize,
}

impl DagObserver for CopyDagObserver {
    fn on_node_start(&self, node_id: usize, kind: TransferNodeKind) {
        self.inner.on_node_start(self.node_offset + node_id, kind);
    }

    fn on_node_complete(&self, node_id: usize, outcome: ObservedOutcome) {
        self.inner
            .on_node_complete(self.node_offset + node_id, outcome);
    }

    fn on_scan_progress(&self, scanned: usize, in_flight: usize) {
        self.inner.on_scan_progress(scanned, in_flight);
    }

    fn on_metrics(&self, _metrics: &TransferDagMetrics) {}
}

/// DAG-P2-07: merges runner-attested byte totals into the executor's final
/// metrics snapshot, so the executor's single finalize stays single. Byte
/// semantics are directional: an upload crosses the provider data path once
/// with no local payload materialization; a download additionally lands the
/// whole object on local disk. `bytes_total` is the logical attestation;
/// `wire_bytes_total` is what actually crossed the wire this run (equal to
/// logical for whole-file transfers, only the re-uploaded parts for a
/// resumed multipart upload).
struct SingleFileMetricsObserver {
    inner: Arc<dyn DagObserver>,
    bytes_total: Arc<AtomicU64>,
    wire_bytes_total: Arc<AtomicU64>,
    direction: TransferDirection,
}

impl DagObserver for SingleFileMetricsObserver {
    fn on_node_start(&self, node_id: usize, kind: TransferNodeKind) {
        self.inner.on_node_start(node_id, kind);
    }

    fn on_node_complete(&self, node_id: usize, outcome: ObservedOutcome) {
        self.inner.on_node_complete(node_id, outcome);
    }

    fn on_scan_progress(&self, scanned: usize, in_flight: usize) {
        self.inner.on_scan_progress(scanned, in_flight);
    }

    fn on_metrics(&self, metrics: &TransferDagMetrics) {
        let logical = self.bytes_total.load(Ordering::SeqCst);
        let wire = self.wire_bytes_total.load(Ordering::SeqCst);
        let mut merged = metrics.clone();
        merged.logical_bytes = merged.logical_bytes.saturating_add(logical);
        merged.wire_bytes = merged.wire_bytes.saturating_add(wire);
        if self.direction == TransferDirection::Download {
            merged.local_payload_bytes = merged.local_payload_bytes.saturating_add(logical);
        }
        merged.bytes_transferred = merged.bytes_transferred.saturating_add(logical);
        self.inner.on_metrics(&merged);
    }
}

/// Execute one production copy through [`TransferDagBuilder::shaped_copy`].
///
/// Capability-unavailable providers run the observable `DownloadFile` then
/// `UploadFile` shape immediately. A recoverable rejection from a dispatched
/// `ServerSideCopy` node is recorded as [`ObservedOutcome::Fallback`], then a
/// second shaped graph with server copy disabled performs the two payload
/// legs. Non-recoverable errors retain their typed file scope and never reach
/// the fallback graph.
pub async fn execute_copy_dag(
    provider: CopyProviderHandle,
    from: String,
    to: String,
    observer: Arc<dyn DagObserver>,
) -> Result<CopyDagOutcome, ProviderError> {
    let caps = provider.transfer_capabilities().await?;
    let initial = TransferDagBuilder::shaped_copy(&caps);
    let source_size = provider.source_size(&from).await.unwrap_or(0);

    if initial.server_side {
        let fallback_trigger: Arc<StdMutex<Option<CopyFallbackTrigger>>> =
            Arc::new(StdMutex::new(None));
        run_copy_shape(
            &initial,
            provider.clone(),
            Arc::from(from.as_str()),
            Arc::from(to.as_str()),
            None,
            Arc::clone(&fallback_trigger),
            Arc::new(CopyDagObserver {
                inner: Arc::clone(&observer),
                node_offset: 0,
            }),
        )
        .await?;

        let trigger = fallback_trigger
            .lock()
            .expect("copy fallback trigger poisoned")
            .clone();
        if let Some(trigger) = trigger {
            let fallback = TransferDagBuilder::shaped_copy(&TransferCapabilities::default());
            let temp = copy_temp_path()?;
            let fallback_result = run_copy_shape(
                &fallback,
                provider,
                Arc::from(from.as_str()),
                Arc::from(to.as_str()),
                Some(Arc::clone(&temp)),
                Arc::new(StdMutex::new(None)),
                Arc::new(CopyDagObserver {
                    inner: Arc::clone(&observer),
                    node_offset: initial.dag.nodes().len(),
                }),
            )
            .await;
            let local_bytes = std::fs::metadata(&*temp)
                .map(|meta| meta.len())
                .unwrap_or(0);
            let _ = std::fs::remove_file(&*temp);
            fallback_result?;
            let logical_bytes = source_size.max(local_bytes);
            let metrics = TransferDagMetrics {
                logical_bytes,
                wire_bytes: local_bytes.saturating_mul(2),
                local_payload_bytes: local_bytes,
                bytes_transferred: logical_bytes,
                copy_fallbacks: 1,
                ..TransferDagMetrics::default()
            };
            observer.on_metrics(&metrics);
            return Ok(CopyDagOutcome {
                decision: CopyDecision::DownloadUpload { trigger },
                metrics,
            });
        }

        let metrics = TransferDagMetrics {
            logical_bytes: source_size,
            wire_bytes: 0,
            local_payload_bytes: 0,
            bytes_transferred: source_size,
            ..TransferDagMetrics::default()
        };
        observer.on_metrics(&metrics);
        return Ok(CopyDagOutcome {
            decision: CopyDecision::ServerSide,
            metrics,
        });
    }

    let temp = copy_temp_path()?;
    let fallback_trigger = CopyFallbackTrigger::CapabilityUnavailable;
    let fallback_result = run_copy_shape(
        &initial,
        provider,
        Arc::from(from.as_str()),
        Arc::from(to.as_str()),
        Some(Arc::clone(&temp)),
        Arc::new(StdMutex::new(None)),
        Arc::new(CopyDagObserver {
            inner: Arc::clone(&observer),
            node_offset: 0,
        }),
    )
    .await;
    let local_bytes = std::fs::metadata(&*temp)
        .map(|meta| meta.len())
        .unwrap_or(0);
    let _ = std::fs::remove_file(&*temp);
    fallback_result?;
    let logical_bytes = source_size.max(local_bytes);
    let metrics = TransferDagMetrics {
        logical_bytes,
        wire_bytes: local_bytes.saturating_mul(2),
        local_payload_bytes: local_bytes,
        bytes_transferred: logical_bytes,
        copy_fallbacks: 1,
        ..TransferDagMetrics::default()
    };
    observer.on_metrics(&metrics);
    Ok(CopyDagOutcome {
        decision: CopyDecision::DownloadUpload {
            trigger: fallback_trigger,
        },
        metrics,
    })
}

fn copy_temp_path() -> Result<Arc<str>, ProviderError> {
    let temp = tempfile::Builder::new()
        .prefix("aeroftp-copy-dag-")
        .tempfile()
        .map_err(ProviderError::IoError)?;
    let (_, path) = temp
        .keep()
        .map_err(|error| ProviderError::IoError(error.error))?;
    Ok(Arc::from(path.to_string_lossy().as_ref()))
}

#[allow(clippy::too_many_arguments)]
async fn run_copy_shape(
    built: &CopyDag,
    provider: CopyProviderHandle,
    from: Arc<str>,
    to: Arc<str>,
    temp: Option<Arc<str>>,
    fallback_trigger: Arc<StdMutex<Option<CopyFallbackTrigger>>>,
    observer: Arc<dyn DagObserver>,
) -> Result<(), ProviderError> {
    let endpoint = provider.endpoint_identity().await?;
    let disk_requests = temp
        .as_ref()
        .map(|path| {
            vec![
                DiskLeaseRequest::write(path.as_ref()),
                DiskLeaseRequest::read(path.as_ref()),
            ]
        })
        .unwrap_or_default();
    // A user-initiated copy is foreground work. The lease covers the whole
    // graph, so native and download-upload fallback shapes cannot exceed the
    // endpoint or local-device caps between nodes.
    let _job_lease = crate::transfer_dag::governor::global()
        .acquire_job(endpoint, TransferPriority::Foreground, disk_requests)
        .await;
    let first_error: Arc<StdMutex<Option<ProviderError>>> = Arc::new(StdMutex::new(None));
    let runner: Arc<dyn DagNodeRunner> = {
        let first_error = Arc::clone(&first_error);
        Arc::new(move |node: TransferNode| -> NodeFuture {
            let provider = provider.clone();
            let from = Arc::clone(&from);
            let to = Arc::clone(&to);
            let temp = temp.clone();
            let first_error = Arc::clone(&first_error);
            let fallback_trigger = Arc::clone(&fallback_trigger);
            Box::pin(async move {
                match node.kind {
                    TransferNodeKind::ServerSideCopy => {
                        match provider.server_side_copy(&from, &to).await {
                            Ok(()) => NodeOutcome::Completed,
                            Err(error)
                                if crate::copy_fallback::should_attempt_copy_fallback(&error) =>
                            {
                                let kind = TransferError::from_provider(&error).kind;
                                *fallback_trigger
                                    .lock()
                                    .expect("copy fallback trigger poisoned") =
                                    Some(CopyFallbackTrigger::ServerRejected { kind });
                                NodeOutcome::CopyFallback
                            }
                            Err(error) => record_failure(&first_error, error, FailureScope::File),
                        }
                    }
                    TransferNodeKind::DownloadFile => {
                        let Some(temp) = temp.as_deref() else {
                            return record_failure(
                                &first_error,
                                ProviderError::TransferFailed(
                                    "DownloadFile copy node missing temp path".to_string(),
                                ),
                                FailureScope::File,
                            );
                        };
                        match provider.download(&from, temp).await {
                            Ok(()) => NodeOutcome::Completed,
                            Err(error) => record_failure(&first_error, error, FailureScope::File),
                        }
                    }
                    TransferNodeKind::UploadFile => {
                        let Some(temp) = temp.as_deref() else {
                            return record_failure(
                                &first_error,
                                ProviderError::TransferFailed(
                                    "UploadFile copy node missing temp path".to_string(),
                                ),
                                FailureScope::File,
                            );
                        };
                        match provider.upload(temp, &to).await {
                            Ok(()) => NodeOutcome::Completed,
                            Err(error) => record_failure(&first_error, error, FailureScope::File),
                        }
                    }
                    _ => NodeOutcome::Completed,
                }
            })
        })
    };

    // DAG-P2-01: shared process-global buffer-byte pool (see governor.rs).
    let manager =
        crate::transfer_dag::governor::global().child_manager(TransferBudget::from_file_slots(1));
    match execute_dag_with_options(
        &built.dag,
        &manager,
        runner,
        observer,
        None,
        DagExecuteOptions::default(),
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(error) => Err(first_error
            .lock()
            .expect("copy first-error slot poisoned")
            .take()
            .unwrap_or_else(|| ProviderError::TransferFailed(error.to_string()))),
    }
}

fn cancelled_transfer_error() -> ProviderError {
    ProviderError::TransferFailed("Transfer cancelled by user".to_string())
}

async fn race_cancel<T, F>(
    cancel_token: &Option<CancellationToken>,
    fut: F,
) -> Result<T, ProviderError>
where
    F: Future<Output = Result<T, ProviderError>>,
{
    match cancel_token {
        Some(tok) => {
            tokio::select! {
                biased;
                _ = tok.cancelled() => Err(cancelled_transfer_error()),
                result = fut => result,
            }
        }
        None => fut.await,
    }
}

/// Run a plain single-file transfer through the graph engine.
///
/// `built` is the capability-shaped seven-node graph from
/// [`crate::transfer_dag::TransferDagBuilder::shaped_file`]; the caller builds
/// it once so it can also read `built.emit_progress` to construct an observer.
/// `file_size` is the byte length of the source object (the remote object on
/// the download direction, the local object on the upload direction): the
/// runner uses it to slice the local file into multipart chunks when the
/// shape calls for it; on the single-transfer-core shape it is observational
/// only.
///
/// `provider` is locked by the transfer nodes from their spawned tasks: the
/// caller MUST NOT hold a guard on the same mutex across this call or the
/// node would deadlock.
///
/// On the download direction the actual on-disk size after a successful
/// transfer is stored into `report_size` (the observer reports it in the
/// completion event); on the upload direction `report_size` is left at the
/// value the caller seeded it with. The typed [`ProviderError`] of a failed
/// transfer is recovered and returned, so the caller keeps its exact error
/// classification (CLI exit codes, GUI error strings) unchanged.
#[allow(clippy::too_many_arguments)]
pub async fn execute_single_file_dag(
    built: &ShapedFileDag,
    provider: SharedProvider,
    remote_path: String,
    local_path: String,
    modified: Option<String>,
    progress_cb: Option<ProgressCallback>,
    observer: Arc<dyn DagObserver>,
    report_size: Arc<AtomicU64>,
    file_size: u64,
    // FINDING-4 Part B: when present, the plain single-file transfer node races
    // its `download` / `upload` against this token so a user Stop
    // (`cancel_transfer` -> `ProviderState::request_cancel`) drops the in-flight
    // future promptly (russh is async, so dropping tears the SFTP stream down)
    // instead of running the current file to completion. `None` = no cancel
    // wrapping (the CLI path keeps its own Ctrl+C handling).
    cancel_token: Option<CancellationToken>,
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
    // DAG-P2-07: byte totals the transfer nodes attest at their success
    // points, folded into the metrics snapshot by SingleFileMetricsObserver
    // below. `bytes_total` is the logical (user-visible) total;
    // `wire_bytes_total` counts only payload that actually crossed the wire
    // this run (receipt-backed resumed parts contribute 0 to wire).
    let bytes_total = Arc::new(AtomicU64::new(0));
    let wire_bytes_total = Arc::new(AtomicU64::new(0));

    // Resolve the canonical endpoint once before the durable multipart state
    // is built. The checkpoint identity includes provider + endpoint + remote
    // destination, so receipts cannot bleed across accounts or destinations.
    let (aimd_provider_type, endpoint) = {
        let guard = provider.lock().await;
        let provider = guard.as_ref().ok_or(ProviderError::NotConnected)?;
        (Some(provider.provider_type()), provider.endpoint_identity())
    };

    // The multipart context is set up once, only when the shape actually fans
    // out into N `UploadPart` nodes. When the transfer core is a single
    // `UploadFile` or `DownloadFile`, the context stays `None` and every
    // `UploadPart`/`CommitTemp` branch short-circuits to the legacy no-op.
    let (multipart_state, durable_checkpoint): (
        Option<Arc<MultipartFileState>>,
        Option<Arc<StdMutex<DurableMultipartCheckpoint>>>,
    ) = if direction == TransferDirection::Upload && built.profile.upload_parts > 1 && file_size > 0
    {
        let layout = MultipartLayout::from_profile(
            file_size,
            built.profile.upload_parts,
            built.profile.preferred_chunk_size,
            &local_path,
        );
        let node_to_part: HashMap<usize, u32> = built
            .transfer
            .iter()
            .enumerate()
            .map(|(idx, node_id)| (*node_id, (idx + 1) as u32))
            .collect();
        let source_meta = std::fs::metadata(&local_path).map_err(ProviderError::IoError)?;
        let source = crate::transfer_dag::CheckpointSourceIdentity {
            local_path: local_path.clone(),
            size: source_meta.len(),
            modified_unix_nanos: source_meta
                .modified()
                .ok()
                .and_then(|stamp| stamp.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|stamp| stamp.as_nanos()),
        };
        let destination = crate::transfer_dag::CheckpointDestinationIdentity {
            provider: format!("{:?}", aimd_provider_type.expect("provider resolved")),
            protocol: endpoint.protocol.clone(),
            host: endpoint.host.clone(),
            account: endpoint.account.clone(),
            remote_path: remote_path.clone(),
        };
        let checkpoint_layout = crate::transfer_dag::CheckpointLayout {
            total_size: layout.total_size,
            total_parts: layout.total_parts,
            preferred_part_size: layout.preferred_part_size,
        };
        let store = crate::transfer_dag::TransferCheckpointStore::default_store()
            .map_err(ProviderError::TransferFailed)?;
        // Conservative orphan scavenging: only records past the durable TTL,
        // only for this exact connected endpoint/provider, and only after a
        // successful provider abort. Unknown, current, and failed-abort records
        // stay intact for a later resume or a safer operator decision.
        let stale = store
            .stale_nonterminal()
            .map_err(ProviderError::TransferFailed)?;
        for stale_record in stale.into_iter().filter(|record| {
            record.destination.provider
                == format!(
                    "{:?}",
                    aimd_provider_type.as_ref().expect("provider resolved")
                )
                && record.destination.protocol == endpoint.protocol
                && record.destination.host == endpoint.host
                && record.destination.account == endpoint.account
        }) {
            let Some(upload_id) = stale_record.upload_id.clone() else {
                store
                    .remove(&stale_record.transfer_key)
                    .map_err(ProviderError::TransferFailed)?;
                continue;
            };
            let handle = MultipartHandle {
                upload_id,
                remote_path: stale_record.destination.remote_path.clone(),
            };
            let aborted = {
                let mut guard = provider.lock().await;
                let provider = guard.as_mut().ok_or(ProviderError::NotConnected)?;
                provider.abort_multipart_upload(handle).await.is_ok()
            };
            if aborted {
                store
                    .remove(&stale_record.transfer_key)
                    .map_err(ProviderError::TransferFailed)?;
            }
        }
        let opened = store
            .open_or_create(source, destination, checkpoint_layout)
            .map_err(ProviderError::TransferFailed)?;
        let checkpoint = Arc::new(StdMutex::new(DurableMultipartCheckpoint {
            store,
            record: opened.checkpoint,
        }));
        let state = MultipartFileState::new(layout, node_to_part);
        if opened.resumed {
            let restored = {
                let checkpoint = checkpoint.lock().expect("checkpoint mutex poisoned");
                (checkpoint.restored_handle(), checkpoint.restored_receipts())
            };
            if let Some(handle) = restored.0 {
                state
                    .restore_session(handle, restored.1)
                    .await
                    .map_err(|failure| ProviderError::TransferFailed(failure.message))?;
            }
        }
        (Some(state), Some(checkpoint))
    } else {
        (None, None)
    };

    let runner: Arc<dyn DagNodeRunner> = {
        let provider = Arc::clone(&provider);
        let remote = Arc::clone(&remote);
        let local = Arc::clone(&local);
        let modified = modified.clone();
        let progress_slot = Arc::clone(&progress_slot);
        let first_error = Arc::clone(&first_error);
        let report_size = Arc::clone(&report_size);
        let multipart_state = multipart_state.clone();
        let durable_checkpoint = durable_checkpoint.clone();
        let cancel_token = cancel_token.clone();
        let bytes_total = Arc::clone(&bytes_total);
        let wire_bytes_total = Arc::clone(&wire_bytes_total);
        Arc::new(move |node: TransferNode| -> NodeFuture {
            let provider = Arc::clone(&provider);
            let remote = Arc::clone(&remote);
            let local = Arc::clone(&local);
            let modified = modified.clone();
            let progress_slot = Arc::clone(&progress_slot);
            let first_error = Arc::clone(&first_error);
            let report_size = Arc::clone(&report_size);
            let multipart_state = multipart_state.clone();
            let durable_checkpoint = durable_checkpoint.clone();
            let cancel_token = cancel_token.clone();
            let bytes_total = Arc::clone(&bytes_total);
            let wire_bytes_total = Arc::clone(&wire_bytes_total);
            Box::pin(async move {
                match node.kind {
                    TransferNodeKind::DownloadFile => {
                        let cb = progress_slot.lock().expect("progress slot poisoned").take();
                        let mut guard = provider.lock().await;
                        let Some(p) = guard.as_mut() else {
                            return record_failure(
                                &first_error,
                                ProviderError::NotConnected,
                                FailureScope::File,
                            );
                        };
                        let dl = async { p.download(&remote, &local, cb).await };
                        let res = match &cancel_token {
                            Some(tok) => tokio::select! {
                                biased;
                                _ = tok.cancelled() => Err(ProviderError::TransferFailed(
                                    "Transfer cancelled by user".to_string(),
                                )),
                                r = dl => r,
                            },
                            None => dl.await,
                        };
                        match res {
                            Ok(()) => {
                                // Report the real on-disk size; fall back to the
                                // caller-seeded value if the stat fails.
                                if let Ok(meta) = std::fs::metadata(&*local) {
                                    report_size.store(meta.len(), Ordering::SeqCst);
                                    bytes_total.store(meta.len(), Ordering::SeqCst);
                                    wire_bytes_total.store(meta.len(), Ordering::SeqCst);
                                }
                                NodeOutcome::Completed
                            }
                            Err(e) => record_failure(&first_error, e, FailureScope::File),
                        }
                    }
                    TransferNodeKind::UploadFile => {
                        let cb = progress_slot.lock().expect("progress slot poisoned").take();
                        let mut guard = provider.lock().await;
                        let Some(p) = guard.as_mut() else {
                            return record_failure(
                                &first_error,
                                ProviderError::NotConnected,
                                FailureScope::File,
                            );
                        };
                        let ul = async { p.upload(&local, &remote, cb).await };
                        let res = match &cancel_token {
                            Some(tok) => tokio::select! {
                                biased;
                                _ = tok.cancelled() => Err(ProviderError::TransferFailed(
                                    "Transfer cancelled by user".to_string(),
                                )),
                                r = ul => r,
                            },
                            None => ul.await,
                        };
                        match res {
                            Ok(()) => {
                                // Whole-file upload: attest the real on-disk
                                // source length (mirrors the download arm).
                                // If the stat fails, keep the caller-declared
                                // size rather than attesting nothing.
                                let attested = std::fs::metadata(&*local)
                                    .map(|meta| meta.len())
                                    .unwrap_or(file_size);
                                bytes_total.store(attested, Ordering::SeqCst);
                                wire_bytes_total.store(attested, Ordering::SeqCst);
                                NodeOutcome::Completed
                            }
                            Err(e) => record_failure(&first_error, e, FailureScope::File),
                        }
                    }
                    TransferNodeKind::UploadPart => {
                        let Some(state) = multipart_state.as_ref() else {
                            return record_failure(
                                &first_error,
                                ProviderError::TransferFailed(
                                    "UploadPart node without multipart context".to_string(),
                                ),
                                FailureScope::Part,
                            );
                        };
                        let Some(part_number) = state.part_number_for_node(node.id) else {
                            return record_failure(
                                &first_error,
                                ProviderError::TransferFailed(format!(
                                    "UploadPart node {} not mapped to a part number",
                                    node.id
                                )),
                                FailureScope::Part,
                            );
                        };
                        // A receipt is durable only after the checkpoint's
                        // atomic write succeeded. On restart, receipt-backed
                        // nodes complete without another provider upload, so
                        // only the missing parts are dispatched to the wire.
                        if durable_checkpoint.as_ref().is_some_and(|checkpoint| {
                            checkpoint
                                .lock()
                                .expect("checkpoint mutex poisoned")
                                .has_receipt(part_number)
                        }) {
                            return NodeOutcome::Completed;
                        }
                        // 1. Lazy begin through the same once-guarded file state
                        //    used by the batch DAG runner.
                        let begin_result: Result<(), ProviderError> = state
                            .with_begin_gate(async {
                                if !state.needs_begin().await {
                                    return Ok(());
                                }
                                let mut guard = provider.lock().await;
                                let Some(p) = guard.as_mut() else {
                                    return Err(ProviderError::NotConnected);
                                };
                                let begin = async {
                                    p.begin_multipart_upload(
                                        &remote,
                                        state.layout().total_size,
                                        Some(&state.layout().content_type),
                                        Some(&local),
                                    )
                                    .await
                                };
                                match race_cancel(&cancel_token, begin).await {
                                    Ok(handle) => {
                                        if let Some(checkpoint) = durable_checkpoint.as_ref() {
                                            checkpoint
                                                .lock()
                                                .expect("checkpoint mutex poisoned")
                                                .begin(&handle)?;
                                        }
                                        state.install_handle(handle).await;
                                        Ok(())
                                    }
                                    Err(error) => Err(error),
                                }
                            })
                            .await;
                        if let Err(error) = begin_result {
                            return record_failure(&first_error, error, FailureScope::Part);
                        }

                        let handle = match state.clone_handle().await {
                            Some(handle) => handle,
                            None => {
                                return record_failure(
                                    &first_error,
                                    ProviderError::TransferFailed(
                                        "Multipart session was not begun".to_string(),
                                    ),
                                    FailureScope::Part,
                                );
                            }
                        };

                        // 2. Resolve this part's byte range from the shared
                        //    validated layout. DAG-P2-05: the slice is handed to
                        //    the provider as a `PartBody::DiskSlice`, not read
                        //    into an owned buffer here; streaming providers read a
                        //    bounded window, owning providers materialize it
                        //    inside `upload_part_body`. Both stay inside the held
                        //    `buffer_bytes` lease.
                        let (offset, len) = match state.layout().part_range(part_number) {
                            Ok(range) => range,
                            Err(failure) => {
                                return record_failure(
                                    &first_error,
                                    ProviderError::TransferFailed(failure.message),
                                    FailureScope::Part,
                                );
                            }
                        };

                        // 3. Upload the part using the resolved handle.
                        let cloned_worker = {
                            let mut guard = provider.lock().await;
                            let Some(p) = guard.as_mut() else {
                                return record_failure(
                                    &first_error,
                                    ProviderError::NotConnected,
                                    FailureScope::Part,
                                );
                            };
                            clone_multipart_worker(p.as_mut())
                        };
                        let upload_result = if let Some(mut worker) = cloned_worker {
                            race_cancel(&cancel_token, async {
                                worker
                                    .upload_part_body(
                                        &handle,
                                        part_number,
                                        crate::transfer_multipart::PartBody::disk_slice(
                                            local.to_string(),
                                            offset,
                                            len,
                                        ),
                                    )
                                    .await
                            })
                            .await
                        } else {
                            let mut guard = provider.lock().await;
                            let Some(p) = guard.as_mut() else {
                                return record_failure(
                                    &first_error,
                                    ProviderError::NotConnected,
                                    FailureScope::Part,
                                );
                            };
                            race_cancel(&cancel_token, async {
                                p.upload_part_body(
                                    &handle,
                                    part_number,
                                    crate::transfer_multipart::PartBody::disk_slice(
                                        local.to_string(),
                                        offset,
                                        len,
                                    ),
                                )
                                .await
                            })
                            .await
                        };
                        match upload_result {
                            Ok(receipt) => {
                                match state.store_receipt_for_part(part_number, receipt).await {
                                    Ok(()) => {
                                        let receipt = state
                                            .sorted_receipts_snapshot()
                                            .await
                                            .into_iter()
                                            .find(|receipt| receipt.part_number == part_number)
                                            .expect("stored receipt must be present");
                                        if let Some(checkpoint) = durable_checkpoint.as_ref() {
                                            if let Err(error) = checkpoint
                                                .lock()
                                                .expect("checkpoint mutex poisoned")
                                                .record_receipt(&receipt)
                                            {
                                                return record_failure(
                                                    &first_error,
                                                    error,
                                                    FailureScope::Part,
                                                );
                                            }
                                        }
                                        // Attest this part's size on the wire
                                        // only now that its upload succeeded.
                                        // Receipt-backed parts of a resumed
                                        // run return early above and
                                        // contribute 0 to the wire total.
                                        wire_bytes_total.fetch_add(len, Ordering::SeqCst);
                                        NodeOutcome::Completed
                                    }
                                    Err(failure) => record_failure(
                                        &first_error,
                                        ProviderError::TransferFailed(failure.message),
                                        FailureScope::Part,
                                    ),
                                }
                            }
                            Err(e) => record_failure(&first_error, e, FailureScope::Part),
                        }
                    }
                    TransferNodeKind::ServerSideCopy => {
                        // `shaped_file` never emits this kind. Production copy
                        // uses `execute_copy_dag`, whose node boundary records
                        // a typed fallback and then dispatches an observable
                        // DownloadFile -> UploadFile graph. Keep this defensive
                        // binding native-only so no hidden payload fallback can
                        // re-enter through the single-file runner.
                        let mut guard = provider.lock().await;
                        let Some(p) = guard.as_mut() else {
                            return record_failure(
                                &first_error,
                                ProviderError::NotConnected,
                                FailureScope::File,
                            );
                        };
                        match p.server_side_copy(&remote, &local).await {
                            Ok(()) => NodeOutcome::Completed,
                            Err(e) => record_failure(&first_error, e, FailureScope::File),
                        }
                    }
                    TransferNodeKind::CommitTemp => {
                        // For native multipart uploads this is the commit node:
                        // it finalizes the session by submitting the accumulated
                        // parts in part-number order. The handle is CLONED for
                        // the complete call and the slot is cleared only on
                        // success; on a commit-time failure the handle stays in
                        // place so the post-execute abort guard reliably aborts
                        // the orphaned session instead of leaking the upload id
                        // (audit ERR-01). The commit is fail-closed on the
                        // durable Verified fact (P2-03): VerifyChecksum runs
                        // before this node and writes Verified only after a
                        // truthful check, and this node refuses to finalize the
                        // provider session unless that fact is present. So an
                        // unverified payload can never be committed or reported
                        // complete. Ordering is explicit: provider complete,
                        // then the durable Committed fact, then visible success.
                        //
                        // For the single-transfer-core shape (a plain
                        // UploadFile/DownloadFile with no journal) this node
                        // makes NO atomic-commit claim: any temp-file rename or
                        // in-place write boundary is owned by the provider's own
                        // download/upload (see providers::atomic_write), so there
                        // is nothing to commit here and no false claim to make.
                        if let Some(state) = multipart_state.as_ref() {
                            let handle = state.clone_handle().await;
                            if let Some(handle) = handle {
                                if !state.has_all_receipts().await {
                                    return record_failure(
                                        &first_error,
                                        ProviderError::TransferFailed(format!(
                                            "multipart receipt count {} != expected {}",
                                            state.receipt_count().await,
                                            state.layout().total_parts
                                        )),
                                        FailureScope::File,
                                    );
                                }
                                // Fail-closed gate: never finalize the provider
                                // session unless the payload durably verified.
                                if durable_checkpoint.as_ref().is_some_and(|checkpoint| {
                                    !checkpoint
                                        .lock()
                                        .expect("checkpoint mutex poisoned")
                                        .is_verified()
                                }) {
                                    return record_failure(
                                        &first_error,
                                        ProviderError::TransferFailed(
                                            "multipart commit blocked: payload not durably verified"
                                                .to_string(),
                                        ),
                                        FailureScope::File,
                                    );
                                }
                                let parts = state.take_sorted_receipts().await;
                                let result = {
                                    let mut guard = provider.lock().await;
                                    let Some(p) = guard.as_mut() else {
                                        return record_failure(
                                            &first_error,
                                            ProviderError::NotConnected,
                                            FailureScope::File,
                                        );
                                    };
                                    race_cancel(&cancel_token, async {
                                        p.complete_multipart_upload(handle, parts).await
                                    })
                                    .await
                                };
                                match result {
                                    Ok(()) => {
                                        // The durable Committed fact must land
                                        // before this node reports Completed, and
                                        // mark_committed is itself fail-closed on
                                        // the Verified state. A crash after
                                        // provider completion but before this
                                        // write remains resumable: the reopened
                                        // record rewinds to Transferring, so the
                                        // resumed run re-verifies and retries the
                                        // provider complete before it can report
                                        // success.
                                        if let Some(checkpoint) = durable_checkpoint.as_ref() {
                                            if let Err(error) = checkpoint
                                                .lock()
                                                .expect("checkpoint mutex poisoned")
                                                .mark_committed()
                                            {
                                                return record_failure(
                                                    &first_error,
                                                    error,
                                                    FailureScope::File,
                                                );
                                            }
                                        }
                                        state.clear_handle_after_complete().await;
                                        // Multipart upload committed: the
                                        // layout total is the logical byte
                                        // truth for the whole object. The
                                        // wire total was attested per part as
                                        // each upload succeeded, so a resumed
                                        // run counts only the parts it
                                        // actually re-uploaded.
                                        bytes_total
                                            .store(state.layout().total_size, Ordering::SeqCst);
                                        NodeOutcome::Completed
                                    }
                                    Err(e) => record_failure(&first_error, e, FailureScope::File),
                                }
                            } else {
                                NodeOutcome::Completed
                            }
                        } else {
                            NodeOutcome::Completed
                        }
                    }
                    TransferNodeKind::VerifyChecksum => {
                        // On the durable native-multipart path this is a real
                        // node. It runs after every UploadPart receipt and
                        // BEFORE CommitTemp finalizes the provider session, so
                        // the remote object is not yet finalized and no provider
                        // checksum is claimed here. Instead it verifies, from
                        // durable facts, that every part is present and that the
                        // local source still matches the identity that produced
                        // the uploaded parts, then records the durable Verified
                        // fact. A failed check is terminal for this file: it
                        // never reaches commit or visible completion.
                        if let Some(checkpoint) = durable_checkpoint.as_ref() {
                            let observed = observe_source(&local);
                            if let Err(error) = checkpoint
                                .lock()
                                .expect("checkpoint mutex poisoned")
                                .verify(&observed)
                            {
                                return record_failure(&first_error, error, FailureScope::File);
                            }
                        }
                        // No durable journal (a single UploadFile/DownloadFile
                        // core): the legacy path performs no checksum
                        // verification and makes no claim.
                        NodeOutcome::Completed
                    }
                    TransferNodeKind::PreserveMetadata => {
                        if direction == TransferDirection::Download {
                            crate::preserve_remote_mtime(&local, modified.as_deref());
                        }
                        NodeOutcome::Completed
                    }
                    // DiscoverRemote / DiscoverLocal / AcquireResource /
                    // EmitProgress: structural anchors, no-ops in the current
                    // slice. A single-file graph never produces any other kind.
                    _ => NodeOutcome::Completed,
                }
            })
        })
    };

    // DAG-P2-01: shared process-global buffer-byte pool (see governor.rs).
    let manager = crate::transfer_dag::governor::global().child_manager(single_file_budget(built));
    let disk_request = match direction {
        TransferDirection::Upload => DiskLeaseRequest::read(local_path.clone()),
        TransferDirection::Download => DiskLeaseRequest::write(local_path.clone()),
    };
    // Single-file GUI/CLI actions are foreground work. Hold one endpoint and
    // directional local-device lease for the complete graph, including native
    // multipart fan-out and terminal cleanup.
    let _job_lease = crate::transfer_dag::governor::global()
        .acquire_job(
            endpoint.clone(),
            TransferPriority::Foreground,
            [disk_request],
        )
        .await;

    // AIMD backpressure only helps when a shaped graph has real chunk/http/api
    // concurrency to tune. Plain single-stream providers such as SFTP request
    // only the one file slot, whose ceiling cannot shrink usefully, so keep
    // their dispatch path free of no-op adaptive bookkeeping.
    let aimd = single_file_needs_aimd(built).then(|| {
        // DAG-P2-06: seed from / learn into this endpoint's shaped-file profile.
        single_file_aimd_controller(
            &manager.budget(),
            aimd_provider_type,
            endpoint,
            crate::transfer_dag::global_profile_registry(),
            AimdConfig::runtime(),
        )
    });

    // Graph-scoped cancel is a child of the caller's token (when present) so
    // user Stop and first-part fail-fast both terminate resident siblings
    // within FAIL_FAST_ABORT_GRACE. Production keeps node_timeout = None so
    // valid long transfers are never cut by an arbitrary engine limit.
    let observer: Arc<dyn DagObserver> = Arc::new(SingleFileMetricsObserver {
        inner: observer,
        bytes_total,
        wire_bytes_total,
        direction,
    });
    let outcome = execute_dag_with_options(
        &built.dag,
        &manager,
        runner,
        observer,
        aimd,
        DagExecuteOptions {
            parent_cancel: cancel_token,
            ..DagExecuteOptions::default()
        },
    )
    .await;

    // A durable multipart session is intentionally preserved after a failure:
    // its journaled receipts are the proof that a restart may submit only the
    // missing parts. Legacy in-memory multipart state keeps the old best-effort
    // abort behavior. Expired durable records are scavenged conservatively by
    // a later connected run, which can abort the matching provider session.
    if outcome.is_err() {
        if let Some(checkpoint) = durable_checkpoint.as_ref() {
            checkpoint
                .lock()
                .expect("checkpoint mutex poisoned")
                .mark_failed();
        } else if let Some(state) = multipart_state.as_ref() {
            if let Some(handle) = state.take_for_abort().await {
                let mut guard = provider.lock().await;
                if let Some(p) = guard.as_mut() {
                    let _ = p.abort_multipart_upload(handle).await;
                }
            }
        }
    }

    match outcome {
        Ok(_summary) => Ok(()),
        Err(dag_err) => Err(first_error
            .lock()
            .expect("first-error slot poisoned")
            .take()
            .unwrap_or_else(|| ProviderError::TransferFailed(dag_err.to_string()))),
    }
}

/// Stash the original [`ProviderError`] for the caller and return a typed
/// node failure so AIMD/cancel decisions never re-parse presentation text.
///
/// `scope` is set only when the node context makes it certain (File for the
/// single-stream transfer core / commit; Part for multipart part nodes).
fn record_failure(
    first_error: &Arc<StdMutex<Option<ProviderError>>>,
    error: ProviderError,
    scope: FailureScope,
) -> NodeOutcome {
    let typed = TransferError::from_provider(&error).with_scope(scope);
    let mut slot = first_error.lock().expect("first-error slot poisoned");
    if slot.is_none() {
        *slot = Some(error);
    }
    NodeOutcome::Failed(typed)
}

/// DAG-P2-06: the single construction point for the shaped single-file /
/// native-multipart AIMD controller. It binds learned tuning to `endpoint`
/// under the [`crate::transfer_dag::AdaptiveWorkload::ShapedFile`] shape, which
/// is distinct from the whole-file batch/sync key and the segmented-range key so
/// a chunk/http fan-out tuning cannot bleed across workload shapes. The registry
/// is a parameter so the wire-level test can inject a fresh instance and assert
/// the key this site records under.
fn single_file_aimd_controller(
    budget: &crate::transfer_dag::TransferBudget,
    provider_type: Option<crate::providers::ProviderType>,
    endpoint: crate::transfer_dag::EndpointIdentity,
    registry: Arc<crate::transfer_dag::AdaptiveProfileRegistry>,
    config: AimdConfig,
) -> Arc<AimdController> {
    use crate::transfer_dag::{AdaptiveProfileKey, AdaptiveWorkload};
    Arc::new(AimdController::from_budget_for_profile(
        budget,
        provider_type,
        AdaptiveProfileKey::new(endpoint, AdaptiveWorkload::ShapedFile),
        registry,
        config,
    ))
}

fn single_file_needs_aimd(built: &ShapedFileDag) -> bool {
    built.dag.nodes().iter().any(|node| {
        node.resources.chunk_slots > 0
            || node.resources.http_slots > 0
            || node.resources.api_slots > 0
    })
}

fn single_file_budget(built: &ShapedFileDag) -> TransferBudget {
    // Per-manager buffer budget (env / MemAvailable / fallback). Not process-global.
    let mut budget = TransferBudget::from_file_slots(1).with_resolved_buffer_budget();
    if built.direction == TransferDirection::Upload && built.profile.upload_parts > 1 {
        let chunk_slots = built
            .profile
            .max_chunk_slots
            .max(1)
            .min(built.profile.upload_parts as u16);
        budget.chunk_slots = chunk_slots;
        // Disk-read slots must cover concurrent parts that each hold a lease.
        budget.disk_read_slots = budget.disk_read_slots.max(chunk_slots);
        // Directional: multipart upload never needs disk-write permits.
        budget.disk_write_slots = 1;
    } else if built.direction == TransferDirection::Upload {
        budget.disk_write_slots = 1;
    } else {
        // Download path: no disk-read contention for the payload.
        budget.disk_read_slots = 1;
    }
    budget
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{MultipartHandle, ProviderType, RemoteEntry, UploadedPart};
    use crate::transfer_dag::observer::CollectingDagObserver;
    use crate::transfer_dag::{Capability, TransferCapabilities, TransferDagBuilder};

    // DAG-P2-06 (wire-level): the shaped single-file construction point binds
    // learned tuning to its endpoint under the ShapedFile workload key, which is
    // distinct from the whole-file and segmented-range keys.
    #[test]
    fn single_file_aimd_controller_records_under_the_shaped_file_key() {
        use crate::transfer_dag::{
            AdaptiveClass, AdaptiveProfileConfig, AdaptiveProfileKey, AdaptiveProfileRegistry,
            AdaptiveWorkload, EndpointIdentity, TransferBudget,
        };
        let registry = Arc::new(AdaptiveProfileRegistry::new(
            AdaptiveProfileConfig::default(),
        ));
        let endpoint = EndpointIdentity::new("s3", "wire-test-host", "wire-acct");
        let budget = TransferBudget {
            chunk_slots: 8,
            http_slots: 8,
            ..TransferBudget::from_file_slots(1)
        };
        let ctrl = single_file_aimd_controller(
            &budget,
            Some(ProviderType::S3),
            endpoint.clone(),
            registry.clone(),
            AimdConfig::default(),
        );
        assert_eq!(
            ctrl.profile_key().map(|k| k.workload),
            Some(AdaptiveWorkload::ShapedFile)
        );
        ctrl.on_congestion(AdaptiveClass::Chunk);
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(
            snapshot[0].key,
            AdaptiveProfileKey::new(endpoint, AdaptiveWorkload::ShapedFile)
        );
        assert_eq!(snapshot[0].chunk, Some(4));
    }

    #[derive(Default)]
    struct CopyMockState {
        server_copy_calls: AtomicU64,
        download_calls: AtomicU64,
        upload_calls: AtomicU64,
        files: StdMutex<HashMap<String, Vec<u8>>>,
    }

    struct DagCopyMockProvider {
        supports_copy: bool,
        server_copy_error: StdMutex<Option<ProviderError>>,
        reported_size: u64,
        state: Arc<CopyMockState>,
    }

    impl DagCopyMockProvider {
        fn new(
            supports_copy: bool,
            server_copy_error: Option<ProviderError>,
            reported_size: u64,
        ) -> (Self, Arc<CopyMockState>) {
            let state = Arc::new(CopyMockState::default());
            state
                .files
                .lock()
                .expect("copy mock files poisoned")
                .insert("/src.bin".to_string(), b"hello aeroftp".to_vec());
            (
                Self {
                    supports_copy,
                    server_copy_error: StdMutex::new(server_copy_error),
                    reported_size,
                    state: Arc::clone(&state),
                },
                state,
            )
        }
    }

    #[async_trait::async_trait]
    impl StorageProvider for DagCopyMockProvider {
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }

        fn provider_type(&self) -> ProviderType {
            ProviderType::S3
        }

        fn display_name(&self) -> String {
            "dag-copy-mock".to_string()
        }

        async fn connect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }

        async fn disconnect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }

        fn is_connected(&self) -> bool {
            true
        }

        async fn list(&mut self, _path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
            Ok(Vec::new())
        }

        async fn pwd(&mut self) -> Result<String, ProviderError> {
            Ok("/".to_string())
        }

        async fn cd(&mut self, _path: &str) -> Result<(), ProviderError> {
            Ok(())
        }

        async fn cd_up(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }

        async fn download(
            &mut self,
            remote_path: &str,
            local_path: &str,
            _progress: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            self.state.download_calls.fetch_add(1, Ordering::SeqCst);
            let data = self
                .state
                .files
                .lock()
                .expect("copy mock files poisoned")
                .get(remote_path)
                .cloned()
                .ok_or_else(|| ProviderError::NotFound(remote_path.to_string()))?;
            std::fs::write(local_path, data).map_err(ProviderError::IoError)
        }

        async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
            self.state
                .files
                .lock()
                .expect("copy mock files poisoned")
                .get(remote_path)
                .cloned()
                .ok_or_else(|| ProviderError::NotFound(remote_path.to_string()))
        }

        async fn upload(
            &mut self,
            local_path: &str,
            remote_path: &str,
            _progress: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            self.state.upload_calls.fetch_add(1, Ordering::SeqCst);
            let data = std::fs::read(local_path).map_err(ProviderError::IoError)?;
            self.state
                .files
                .lock()
                .expect("copy mock files poisoned")
                .insert(remote_path.to_string(), data);
            Ok(())
        }

        async fn mkdir(&mut self, _path: &str) -> Result<(), ProviderError> {
            Ok(())
        }

        async fn delete(&mut self, _path: &str) -> Result<(), ProviderError> {
            Ok(())
        }

        async fn rmdir(&mut self, _path: &str) -> Result<(), ProviderError> {
            Ok(())
        }

        async fn rmdir_recursive(&mut self, _path: &str) -> Result<(), ProviderError> {
            Ok(())
        }

        async fn rename(&mut self, _from: &str, _to: &str) -> Result<(), ProviderError> {
            Ok(())
        }

        async fn stat(&mut self, _path: &str) -> Result<RemoteEntry, ProviderError> {
            Err(ProviderError::NotSupported("stat".to_string()))
        }

        async fn size(&mut self, _path: &str) -> Result<u64, ProviderError> {
            Ok(self.reported_size)
        }

        async fn exists(&mut self, _path: &str) -> Result<bool, ProviderError> {
            Ok(true)
        }

        async fn keep_alive(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }

        async fn server_info(&mut self) -> Result<String, ProviderError> {
            Ok("dag-copy-mock".to_string())
        }

        fn supports_server_copy(&self) -> bool {
            self.supports_copy
        }

        async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
            self.state.server_copy_calls.fetch_add(1, Ordering::SeqCst);
            if let Some(error) = self
                .server_copy_error
                .lock()
                .expect("copy mock error poisoned")
                .take()
            {
                return Err(error);
            }
            let mut files = self.state.files.lock().expect("copy mock files poisoned");
            if let Some(data) = files.get(from).cloned() {
                files.insert(to.to_string(), data);
            }
            Ok(())
        }
    }

    fn copy_handle(provider: DagCopyMockProvider) -> CopyProviderHandle {
        CopyProviderHandle::optional(Arc::new(Mutex::new(Some(
            Box::new(provider) as Box<dyn StorageProvider>
        ))))
    }

    #[tokio::test]
    async fn production_copy_dispatches_one_native_node_with_zero_local_payload() {
        let (provider, state) = DagCopyMockProvider::new(true, None, 13);
        let observer = Arc::new(CollectingDagObserver::default());
        let outcome = execute_copy_dag(
            copy_handle(provider),
            "/src.bin".to_string(),
            "/dst.bin".to_string(),
            Arc::clone(&observer) as Arc<dyn DagObserver>,
        )
        .await
        .expect("native copy DAG");

        assert_eq!(outcome.decision, CopyDecision::ServerSide);
        assert_eq!(outcome.metrics.logical_bytes, 13);
        assert_eq!(outcome.metrics.wire_bytes, 0);
        assert_eq!(outcome.metrics.local_payload_bytes, 0);
        assert_eq!(state.server_copy_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.download_calls.load(Ordering::SeqCst), 0);
        assert_eq!(state.upload_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            observer
                .started_nodes()
                .iter()
                .filter(|(_, kind)| *kind == TransferNodeKind::ServerSideCopy)
                .count(),
            1
        );
        assert_eq!(observer.metrics(), outcome.metrics);
    }

    #[tokio::test]
    async fn production_copy_without_capability_dispatches_download_then_upload() {
        let (provider, state) = DagCopyMockProvider::new(false, None, 13);
        let observer = Arc::new(CollectingDagObserver::default());
        let outcome = execute_copy_dag(
            copy_handle(provider),
            "/src.bin".to_string(),
            "/dst.bin".to_string(),
            Arc::clone(&observer) as Arc<dyn DagObserver>,
        )
        .await
        .expect("fallback copy DAG");

        assert_eq!(
            outcome.decision,
            CopyDecision::DownloadUpload {
                trigger: CopyFallbackTrigger::CapabilityUnavailable
            }
        );
        assert_eq!(outcome.metrics.logical_bytes, 13);
        assert_eq!(outcome.metrics.wire_bytes, 26);
        assert_eq!(outcome.metrics.local_payload_bytes, 13);
        assert_eq!(outcome.metrics.copy_fallbacks, 1);
        assert_eq!(state.server_copy_calls.load(Ordering::SeqCst), 0);
        assert_eq!(state.download_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.upload_calls.load(Ordering::SeqCst), 1);
        let transfer_kinds: Vec<TransferNodeKind> = observer
            .started_nodes()
            .into_iter()
            .map(|(_, kind)| kind)
            .filter(|kind| {
                matches!(
                    kind,
                    TransferNodeKind::ServerSideCopy
                        | TransferNodeKind::DownloadFile
                        | TransferNodeKind::UploadFile
                )
            })
            .collect();
        assert_eq!(
            transfer_kinds,
            vec![TransferNodeKind::DownloadFile, TransferNodeKind::UploadFile]
        );
    }

    #[tokio::test]
    async fn recoverable_native_rejection_is_observed_before_payload_fallback() {
        let (provider, state) = DagCopyMockProvider::new(
            true,
            Some(ProviderError::NotSupported("cross-bucket".to_string())),
            13,
        );
        let observer = Arc::new(CollectingDagObserver::default());
        let outcome = execute_copy_dag(
            copy_handle(provider),
            "/src.bin".to_string(),
            "/dst.bin".to_string(),
            Arc::clone(&observer) as Arc<dyn DagObserver>,
        )
        .await
        .expect("rejected native copy falls back");

        assert_eq!(
            outcome.decision,
            CopyDecision::DownloadUpload {
                trigger: CopyFallbackTrigger::ServerRejected {
                    kind: TransferErrorKind::RemoteIo
                }
            }
        );
        assert_eq!(outcome.metrics.logical_bytes, 13);
        assert_eq!(outcome.metrics.wire_bytes, 26);
        assert_eq!(outcome.metrics.local_payload_bytes, 13);
        assert_eq!(state.server_copy_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.download_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.upload_calls.load(Ordering::SeqCst), 1);
        let started = observer.started_nodes();
        let native_shape_len = TransferDagBuilder::shaped_copy(&TransferCapabilities {
            server_side_copy: Capability::Supported,
            ..TransferCapabilities::default()
        })
        .dag
        .nodes()
        .len();
        let initial_kinds: Vec<TransferNodeKind> = started
            .iter()
            .filter(|(id, _)| *id < native_shape_len)
            .map(|(_, kind)| *kind)
            .collect();
        assert_eq!(
            initial_kinds,
            vec![
                TransferNodeKind::DiscoverRemote,
                TransferNodeKind::AcquireResource,
                TransferNodeKind::ServerSideCopy
            ],
            "the rejected native graph must stop before its structural tail"
        );
        let server_id = started
            .iter()
            .find(|(_, kind)| *kind == TransferNodeKind::ServerSideCopy)
            .map(|(id, _)| *id)
            .expect("server copy node observed");
        assert!(observer
            .completed_nodes()
            .contains(&(server_id, ObservedOutcome::Fallback)));
        assert!(started
            .iter()
            .any(|(_, kind)| *kind == TransferNodeKind::DownloadFile));
        assert!(started
            .iter()
            .any(|(_, kind)| *kind == TransferNodeKind::UploadFile));
    }

    #[tokio::test]
    async fn permission_and_not_found_fail_at_file_node_without_fallback() {
        for expected in [
            ProviderError::PermissionDenied("403".to_string()),
            ProviderError::NotFound("/src.bin".to_string()),
        ] {
            let (provider, state) = DagCopyMockProvider::new(true, Some(expected), 13);
            let observer = Arc::new(CollectingDagObserver::default());
            let error = execute_copy_dag(
                copy_handle(provider),
                "/src.bin".to_string(),
                "/dst.bin".to_string(),
                Arc::clone(&observer) as Arc<dyn DagObserver>,
            )
            .await
            .expect_err("hard copy error");

            assert!(matches!(
                error,
                ProviderError::PermissionDenied(_) | ProviderError::NotFound(_)
            ));
            assert_eq!(state.server_copy_calls.load(Ordering::SeqCst), 1);
            assert_eq!(state.download_calls.load(Ordering::SeqCst), 0);
            assert_eq!(state.upload_calls.load(Ordering::SeqCst), 0);
            assert!(!observer.started_nodes().iter().any(|(_, kind)| matches!(
                kind,
                TransferNodeKind::DownloadFile | TransferNodeKind::UploadFile
            )));
            assert!(observer
                .completed_nodes()
                .iter()
                .any(|(_, outcome)| *outcome == ObservedOutcome::Failed));
        }
    }

    #[tokio::test]
    async fn object_larger_than_five_gib_stays_on_native_provider_copy() {
        let large_size = 5 * 1024 * 1024 * 1024 + 1;
        let (provider, state) = DagCopyMockProvider::new(true, None, large_size);
        let outcome = execute_copy_dag(
            copy_handle(provider),
            "/src.bin".to_string(),
            "/dst.bin".to_string(),
            Arc::new(crate::transfer_dag::NoopDagObserver),
        )
        .await
        .expect("large native copy");

        assert_eq!(outcome.decision, CopyDecision::ServerSide);
        assert_eq!(outcome.metrics.logical_bytes, large_size);
        assert_eq!(outcome.metrics.wire_bytes, 0);
        assert_eq!(state.server_copy_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.download_calls.load(Ordering::SeqCst), 0);
        assert_eq!(state.upload_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn shaped_upload_without_multipart_keeps_single_transfer_core() {
        // A provider with no multipart capability degrades the shaped-file
        // graph back to the seven-node linear chain: exactly one transfer
        // node (`UploadFile`), no fan-out.
        let caps = TransferCapabilities::default();
        let built = TransferDagBuilder::shaped_file(TransferDirection::Upload, &caps, 100 * 1024);
        assert_eq!(built.transfer.len(), 1);
        assert_eq!(built.profile.upload_parts, 1);
    }

    #[test]
    fn shaped_upload_with_multipart_fans_out_into_parts() {
        // A 24 MiB upload over an 8 MiB chunk profile fans out into 3
        // `UploadPart` nodes. The node_to_part map collected from the
        // built graph respects creation order (transfer[0] = part 1, ...).
        let caps = TransferCapabilities {
            multipart_upload: Capability::Supported,
            preferred_chunk_size: Some(8 * 1024 * 1024),
            multipart_threshold: 0, // unset: fan out at chunk size
            ..TransferCapabilities::default()
        };
        let file_size: u64 = 24 * 1024 * 1024;
        let built = TransferDagBuilder::shaped_file(TransferDirection::Upload, &caps, file_size);
        assert_eq!(built.transfer.len(), 3);
        assert_eq!(built.profile.upload_parts, 3);

        let node_to_part: HashMap<usize, u32> = built
            .transfer
            .iter()
            .enumerate()
            .map(|(idx, node_id)| (*node_id, (idx + 1) as u32))
            .collect();
        let part_size = file_size.div_ceil(built.profile.upload_parts as u64);
        assert_eq!(part_size, 8 * 1024 * 1024);
        // The mapping is dense and 1-based.
        let mut parts: Vec<u32> = node_to_part.values().copied().collect();
        parts.sort_unstable();
        assert_eq!(parts, vec![1, 2, 3]);
    }

    #[test]
    fn single_stream_shape_skips_noop_aimd() {
        let caps = TransferCapabilities::default();
        let built =
            TransferDagBuilder::shaped_file(TransferDirection::Upload, &caps, 100 * 1024 * 1024);

        assert!(
            !single_file_needs_aimd(&built),
            "plain SFTP-like single-stream graphs should not acquire AIMD permits"
        );
    }

    #[test]
    fn multipart_shape_keeps_aimd() {
        let caps = TransferCapabilities {
            multipart_upload: Capability::Supported,
            preferred_chunk_size: Some(8 * 1024 * 1024),
            max_chunk_slots: Some(4),
            multipart_threshold: 0, // unset: fan out at chunk size
            ..TransferCapabilities::default()
        };
        let built =
            TransferDagBuilder::shaped_file(TransferDirection::Upload, &caps, 24 * 1024 * 1024);

        assert!(
            single_file_needs_aimd(&built),
            "multipart upload graphs still need adaptive chunk dispatch"
        );
    }

    #[test]
    fn multipart_shape_gets_chunk_parallel_budget() {
        let caps = TransferCapabilities {
            multipart_upload: Capability::Supported,
            preferred_chunk_size: Some(8 * 1024 * 1024),
            max_chunk_slots: Some(4),
            multipart_threshold: 0, // unset: fan out at chunk size
            ..TransferCapabilities::default()
        };
        let built =
            TransferDagBuilder::shaped_file(TransferDirection::Upload, &caps, 40 * 1024 * 1024);

        let budget = single_file_budget(&built);
        assert_eq!(budget.file_slots, 1);
        assert_eq!(budget.chunk_slots, 4);
        assert_eq!(budget.disk_read_slots, 4);
        assert!(
            budget.buffer_bytes >= crate::transfer_dag::MIN_BUFFER_BUDGET_BYTES
                || budget.buffer_bytes == crate::transfer_dag::DEFAULT_BUFFER_BUDGET_BYTES,
            "multipart budget must expose a real buffer pool, got {}",
            budget.buffer_bytes
        );
        // Directional: upload manager need not stockpile write permits.
        assert_eq!(budget.disk_write_slots, 1);
    }

    #[test]
    fn single_stream_shape_keeps_one_chunk_slot() {
        let caps = TransferCapabilities::default();
        let built =
            TransferDagBuilder::shaped_file(TransferDirection::Upload, &caps, 40 * 1024 * 1024);

        let budget = single_file_budget(&built);
        assert_eq!(budget.file_slots, 1);
        assert_eq!(budget.chunk_slots, 1);
        assert_eq!(budget.disk_read_slots, 1);
        assert!(budget.buffer_bytes > 0);
    }

    #[test]
    fn multipart_part_nodes_request_exact_buffer_bytes() {
        use crate::transfer_dag::multipart_part_byte_len;
        let chunk = 8 * 1024 * 1024u64;
        let file_size = 25 * 1024 * 1024u64;
        let caps = TransferCapabilities {
            multipart_upload: Capability::Supported,
            preferred_chunk_size: Some(chunk),
            max_chunk_slots: Some(4),
            multipart_threshold: 0,
            ..TransferCapabilities::default()
        };
        let built = TransferDagBuilder::shaped_file(TransferDirection::Upload, &caps, file_size);
        let parts = built.profile.upload_parts;
        assert!(parts > 1);
        for (idx, &node_id) in built.transfer.iter().enumerate() {
            let req = &built.dag.nodes()[node_id].resources;
            assert_eq!(
                req.buffer_bytes,
                multipart_part_byte_len(file_size, idx, parts, built.profile.preferred_chunk_size)
            );
            assert_eq!(req.disk_write_slots, 0);
            assert_eq!(req.disk_read_slots, 1);
        }
    }

    #[test]
    fn shaped_download_never_fans_out() {
        // Multipart is an upload-only concept. The download direction always
        // produces a single transfer node, even on a multipart-capable
        // capability set.
        let caps = TransferCapabilities {
            multipart_upload: Capability::Supported,
            preferred_chunk_size: Some(4 * 1024 * 1024),
            ..TransferCapabilities::default()
        };
        let built =
            TransferDagBuilder::shaped_file(TransferDirection::Download, &caps, 16 * 1024 * 1024);
        assert_eq!(built.transfer.len(), 1);
        assert_eq!(built.profile.upload_parts, 1);
    }

    /// FINDING-4 Part B: a provider whose `download` / `upload` runs as a slow
    /// chunked loop, so a mid-flight `CancellationToken` can win the
    /// `execute_single_file_dag` race. `*_completed` flips only if the transfer
    /// runs to the end, letting the tests assert the cancel actually aborted it.
    struct SlowMockProvider {
        download_completed: Arc<StdMutex<bool>>,
        upload_completed: Arc<StdMutex<bool>>,
        multipart_completed: Arc<StdMutex<bool>>,
        multipart_aborted: Arc<StdMutex<bool>>,
        multipart_part_started: Arc<AtomicU64>,
        /// Part numbers whose upload fails (a scriptable first-run failure so
        /// resume tests are deterministic, not timing-based).
        multipart_failing_parts: Arc<StdMutex<std::collections::HashSet<u32>>>,
        /// Cancelled from inside `upload_part`, after the part is counted and
        /// before it returns, so "the cancel lands while a part is in flight"
        /// is a fact of the call order rather than a wall-clock race.
        cancel_during_part: Option<CancellationToken>,
    }

    impl SlowMockProvider {
        fn new(
            download_completed: Arc<StdMutex<bool>>,
            upload_completed: Arc<StdMutex<bool>>,
        ) -> Self {
            Self {
                download_completed,
                upload_completed,
                multipart_completed: Arc::new(StdMutex::new(false)),
                multipart_aborted: Arc::new(StdMutex::new(false)),
                multipart_part_started: Arc::new(AtomicU64::new(0)),
                multipart_failing_parts: Arc::new(StdMutex::new(std::collections::HashSet::new())),
                cancel_during_part: None,
            }
        }

        /// Cancel `token` from inside the first `upload_part`, so a cancellation
        /// test never has to guess when a part is on the wire.
        fn cancelling_during_part(mut self, token: CancellationToken) -> Self {
            self.cancel_during_part = Some(token);
            self
        }

        fn with_multipart_state(
            mut self,
            completed: Arc<StdMutex<bool>>,
            aborted: Arc<StdMutex<bool>>,
            part_started: Arc<AtomicU64>,
        ) -> Self {
            self.multipart_completed = completed;
            self.multipart_aborted = aborted;
            self.multipart_part_started = part_started;
            self
        }

        fn with_failing_parts(
            mut self,
            failing: Arc<StdMutex<std::collections::HashSet<u32>>>,
        ) -> Self {
            self.multipart_failing_parts = failing;
            self
        }
    }

    #[async_trait::async_trait]
    impl crate::providers::StorageProvider for SlowMockProvider {
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn provider_type(&self) -> crate::providers::ProviderType {
            crate::providers::ProviderType::Ftp
        }
        fn display_name(&self) -> String {
            "slow-mock".into()
        }
        async fn connect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn disconnect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        fn is_connected(&self) -> bool {
            true
        }
        async fn list(
            &mut self,
            _p: &str,
        ) -> Result<Vec<crate::providers::RemoteEntry>, ProviderError> {
            Ok(vec![])
        }
        async fn pwd(&mut self) -> Result<String, ProviderError> {
            Ok("/".into())
        }
        async fn cd(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn cd_up(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn download(
            &mut self,
            _remote: &str,
            local: &str,
            cb: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            for i in 0..30u64 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                if let Some(cb) = &cb {
                    cb(i, 30);
                }
            }
            std::fs::write(local, b"complete").map_err(ProviderError::IoError)?;
            *self.download_completed.lock().unwrap() = true;
            Ok(())
        }
        async fn download_to_bytes(&mut self, _remote: &str) -> Result<Vec<u8>, ProviderError> {
            Ok(vec![])
        }
        async fn upload(
            &mut self,
            _local: &str,
            _remote: &str,
            cb: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            for i in 0..30u64 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                if let Some(cb) = &cb {
                    cb(i, 30);
                }
            }
            *self.upload_completed.lock().unwrap() = true;
            Ok(())
        }
        async fn begin_multipart_upload(
            &mut self,
            remote_path: &str,
            _total_size: u64,
            _content_type: Option<&str>,
            _local_source_path: Option<&str>,
        ) -> Result<MultipartHandle, ProviderError> {
            Ok(MultipartHandle {
                upload_id: "mock-upload".to_string(),
                remote_path: remote_path.to_string(),
            })
        }
        async fn upload_part(
            &mut self,
            _handle: &MultipartHandle,
            part_number: u32,
            _data: Vec<u8>,
        ) -> Result<UploadedPart, ProviderError> {
            if self
                .multipart_failing_parts
                .lock()
                .unwrap()
                .contains(&part_number)
            {
                return Err(ProviderError::TransferFailed(format!(
                    "mock part {part_number} failure"
                )));
            }
            self.multipart_part_started.fetch_add(1, Ordering::SeqCst);
            if let Some(token) = self.cancel_during_part.as_ref() {
                token.cancel();
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            Ok(UploadedPart {
                part_number,
                etag: format!("etag-{part_number}"),
            })
        }
        async fn complete_multipart_upload(
            &mut self,
            _handle: MultipartHandle,
            _parts: Vec<UploadedPart>,
        ) -> Result<(), ProviderError> {
            *self.multipart_completed.lock().unwrap() = true;
            Ok(())
        }
        async fn abort_multipart_upload(
            &mut self,
            _handle: MultipartHandle,
        ) -> Result<(), ProviderError> {
            *self.multipart_aborted.lock().unwrap() = true;
            Ok(())
        }
        async fn mkdir(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn delete(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rmdir(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rmdir_recursive(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rename(&mut self, _f: &str, _t: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn stat(&mut self, _p: &str) -> Result<crate::providers::RemoteEntry, ProviderError> {
            Err(ProviderError::NotSupported("stat".into()))
        }
        async fn size(&mut self, _p: &str) -> Result<u64, ProviderError> {
            Ok(30)
        }
        async fn exists(&mut self, _p: &str) -> Result<bool, ProviderError> {
            Ok(true)
        }
        async fn keep_alive(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn server_info(&mut self) -> Result<String, ProviderError> {
            Ok("slow-mock".into())
        }
    }

    /// A pressed Stop mid-download drops the in-flight future and surfaces a
    /// cancellation error, and the transfer does NOT run to completion.
    #[tokio::test]
    async fn cancel_token_aborts_in_flight_download() {
        let dir = tempfile::tempdir().expect("tempdir");
        let local = dir.path().join("out.bin");
        let completed = Arc::new(StdMutex::new(false));
        let mock = SlowMockProvider::new(Arc::clone(&completed), Arc::new(StdMutex::new(false)));
        let arc: SharedProvider = Arc::new(Mutex::new(Some(
            Box::new(mock) as Box<dyn crate::providers::StorageProvider>
        )));
        let caps = TransferCapabilities::default();
        let built = TransferDagBuilder::shaped_file(TransferDirection::Download, &caps, 30);
        let report = Arc::new(AtomicU64::new(30));
        let observer: Arc<dyn DagObserver> = Arc::new(crate::transfer_dag::NoopDagObserver);

        let token = CancellationToken::new();
        let canceller = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            canceller.cancel();
        });

        let res = execute_single_file_dag(
            &built,
            arc,
            "/remote.bin".to_string(),
            local.to_string_lossy().to_string(),
            None,
            None,
            observer,
            report,
            30,
            Some(token),
        )
        .await;

        assert!(res.is_err(), "a cancelled transfer must return an error");
        let msg = res.unwrap_err().to_string().to_lowercase();
        assert!(
            msg.contains("cancel"),
            "error should mention cancellation, got: {msg}"
        );
        assert!(
            !*completed.lock().unwrap(),
            "the download must not run to completion after a mid-flight cancel"
        );
        assert!(
            !local.exists(),
            "no finalized output file should exist after a cancelled download"
        );
    }

    /// The cancel wrapper is transparent on the happy path: an uncancelled
    /// token lets the transfer complete exactly as before.
    #[tokio::test]
    async fn uncancelled_token_completes_download() {
        let dir = tempfile::tempdir().expect("tempdir");
        let local = dir.path().join("out.bin");
        let completed = Arc::new(StdMutex::new(false));
        let mock = SlowMockProvider::new(Arc::clone(&completed), Arc::new(StdMutex::new(false)));
        let arc: SharedProvider = Arc::new(Mutex::new(Some(
            Box::new(mock) as Box<dyn crate::providers::StorageProvider>
        )));
        let caps = TransferCapabilities::default();
        let built = TransferDagBuilder::shaped_file(TransferDirection::Download, &caps, 30);
        let report = Arc::new(AtomicU64::new(30));
        let observer: Arc<dyn DagObserver> = Arc::new(crate::transfer_dag::NoopDagObserver);

        let res = execute_single_file_dag(
            &built,
            arc,
            "/remote.bin".to_string(),
            local.to_string_lossy().to_string(),
            None,
            None,
            observer,
            report,
            30,
            Some(CancellationToken::new()),
        )
        .await;

        assert!(res.is_ok(), "an uncancelled transfer must succeed: {res:?}");
        assert!(
            *completed.lock().unwrap(),
            "the download should run to completion"
        );
        assert!(local.exists(), "the output file should be finalized");
    }

    #[tokio::test]
    async fn cancel_token_keeps_durable_multipart_session_for_resume() {
        let dir = tempfile::tempdir().expect("tempdir");
        let local = dir.path().join("source.bin");
        std::fs::write(&local, b"0123456789abcdefghij").expect("write source");

        let multipart_completed = Arc::new(StdMutex::new(false));
        let multipart_aborted = Arc::new(StdMutex::new(false));
        let part_started = Arc::new(AtomicU64::new(0));
        let token = CancellationToken::new();
        let mock = SlowMockProvider::new(
            Arc::new(StdMutex::new(false)),
            Arc::new(StdMutex::new(false)),
        )
        .with_multipart_state(
            Arc::clone(&multipart_completed),
            Arc::clone(&multipart_aborted),
            Arc::clone(&part_started),
        )
        // Cancel from inside the first `upload_part`, after it has been counted
        // and before it returns. The previous shape polled `part_started` from a
        // spawned task and cancelled when it saw the count rise, which reads as
        // "cancel on the observable precondition" but is still a race: the poll
        // has a 5ms granularity against a 200ms window, so on a loaded runner the
        // wake could land after BOTH parts had gone by, the DAG had passed its
        // checkpoint, and the session was aborted. That is the failure this test
        // exists to catch, so it was reporting a scheduling accident as the very
        // regression it guards. Cancelling from the call itself makes "a part is
        // in flight" a fact of the call order, with no timing assumption left.
        .cancelling_during_part(token.clone());
        let arc: SharedProvider = Arc::new(Mutex::new(Some(
            Box::new(mock) as Box<dyn crate::providers::StorageProvider>
        )));
        let caps = TransferCapabilities {
            multipart_upload: Capability::Supported,
            preferred_chunk_size: Some(10),
            multipart_threshold: 0,
            max_chunk_slots: Some(1),
            ..TransferCapabilities::default()
        };
        let built = TransferDagBuilder::shaped_file(TransferDirection::Upload, &caps, 20);
        assert_eq!(built.profile.upload_parts, 2);
        let report = Arc::new(AtomicU64::new(20));
        let observer: Arc<dyn DagObserver> = Arc::new(crate::transfer_dag::NoopDagObserver);

        let res = execute_single_file_dag(
            &built,
            arc,
            "/remote.bin".to_string(),
            local.to_string_lossy().to_string(),
            None,
            None,
            observer,
            report,
            20,
            Some(token),
        )
        .await;

        assert!(res.is_err(), "a cancelled multipart upload must fail");
        let msg = res.unwrap_err().to_string().to_lowercase();
        assert!(
            msg.contains("cancel"),
            "error should mention cancellation, got: {msg}"
        );
        assert!(
            part_started.load(Ordering::SeqCst) >= 1,
            "at least one upload part should have been in flight"
        );
        assert!(
            !*multipart_completed.lock().unwrap(),
            "cancelled multipart upload must not be completed"
        );
        assert!(
            !*multipart_aborted.lock().unwrap(),
            "a durable checkpoint keeps the provider session for missing-part resume"
        );
    }

    /// A consistent native-multipart upload passes the real VerifyChecksum node
    /// and then commits: the provider session is finalized only after a durable
    /// verified fact, and the happy path still completes end to end.
    #[tokio::test]
    async fn multipart_verifies_then_commits_on_a_consistent_source() {
        let dir = tempfile::tempdir().expect("tempdir");
        let local = dir.path().join("source.bin");
        std::fs::write(&local, b"0123456789abcdefghij").expect("write source"); // 20 bytes

        let multipart_completed = Arc::new(StdMutex::new(false));
        let multipart_aborted = Arc::new(StdMutex::new(false));
        let part_started = Arc::new(AtomicU64::new(0));
        let mock = SlowMockProvider::new(
            Arc::new(StdMutex::new(false)),
            Arc::new(StdMutex::new(false)),
        )
        .with_multipart_state(
            Arc::clone(&multipart_completed),
            Arc::clone(&multipart_aborted),
            Arc::clone(&part_started),
        );
        let arc: SharedProvider = Arc::new(Mutex::new(Some(
            Box::new(mock) as Box<dyn crate::providers::StorageProvider>
        )));
        let caps = TransferCapabilities {
            multipart_upload: Capability::Supported,
            preferred_chunk_size: Some(10),
            multipart_threshold: 0,
            max_chunk_slots: Some(1),
            ..TransferCapabilities::default()
        };
        let built = TransferDagBuilder::shaped_file(TransferDirection::Upload, &caps, 20);
        assert_eq!(built.profile.upload_parts, 2);
        let report = Arc::new(AtomicU64::new(20));
        let observer: Arc<dyn DagObserver> = Arc::new(crate::transfer_dag::NoopDagObserver);

        let res = execute_single_file_dag(
            &built,
            arc,
            "/remote.bin".to_string(),
            local.to_string_lossy().to_string(),
            None,
            None,
            observer,
            report,
            20,
            Some(CancellationToken::new()),
        )
        .await;

        assert!(
            res.is_ok(),
            "a consistent multipart upload verifies and commits: {res:?}"
        );
        assert!(
            *multipart_completed.lock().unwrap(),
            "the provider session is finalized only after the durable verified fact"
        );
        assert!(!*multipart_aborted.lock().unwrap());
    }

    /// A native-multipart payload that fails the real VerifyChecksum node never
    /// reaches CommitTemp: the provider session is not finalized and the
    /// transfer surfaces a verification failure rather than a false success.
    #[tokio::test]
    async fn verify_failure_blocks_multipart_commit_and_visible_completion() {
        let dir = tempfile::tempdir().expect("tempdir");
        let local = dir.path().join("source.bin");
        // 25 bytes on disk, but the caller-declared transfer size is 20: the
        // multipart layout covers only 20 bytes, so 5 source bytes would be lost.
        // VerifyChecksum catches this durable inconsistency and blocks commit.
        std::fs::write(&local, b"0123456789abcdefghijKLMNO").expect("write source"); // 25 bytes

        let multipart_completed = Arc::new(StdMutex::new(false));
        let multipart_aborted = Arc::new(StdMutex::new(false));
        let part_started = Arc::new(AtomicU64::new(0));
        let mock = SlowMockProvider::new(
            Arc::new(StdMutex::new(false)),
            Arc::new(StdMutex::new(false)),
        )
        .with_multipart_state(
            Arc::clone(&multipart_completed),
            Arc::clone(&multipart_aborted),
            Arc::clone(&part_started),
        );
        let arc: SharedProvider = Arc::new(Mutex::new(Some(
            Box::new(mock) as Box<dyn crate::providers::StorageProvider>
        )));
        let caps = TransferCapabilities {
            multipart_upload: Capability::Supported,
            preferred_chunk_size: Some(10),
            multipart_threshold: 0,
            max_chunk_slots: Some(1),
            ..TransferCapabilities::default()
        };
        let built = TransferDagBuilder::shaped_file(TransferDirection::Upload, &caps, 20);
        assert_eq!(built.profile.upload_parts, 2);
        let report = Arc::new(AtomicU64::new(20));
        let observer: Arc<dyn DagObserver> = Arc::new(crate::transfer_dag::NoopDagObserver);

        let res = execute_single_file_dag(
            &built,
            arc,
            "/remote.bin".to_string(),
            local.to_string_lossy().to_string(),
            None,
            None,
            observer,
            report,
            20,
            Some(CancellationToken::new()),
        )
        .await;

        assert!(
            res.is_err(),
            "verification must fail closed on a size-inconsistent payload"
        );
        let msg = res.unwrap_err().to_string().to_lowercase();
        assert!(
            msg.contains("inconsistent") || msg.contains("layout"),
            "error should be a verification failure, got: {msg}"
        );
        assert!(
            !*multipart_completed.lock().unwrap(),
            "a payload that fails verification is never committed or reported complete"
        );
    }

    /// A plain single-transfer-core upload (no multipart, no durable journal)
    /// makes no atomic-commit claim: CommitTemp finalizes nothing, so the
    /// provider's own `upload` owns the write and no multipart commit is called.
    #[tokio::test]
    async fn plain_single_file_upload_makes_no_commit_claim() {
        let dir = tempfile::tempdir().expect("tempdir");
        let local = dir.path().join("source.bin");
        std::fs::write(&local, b"hello aeroftp").expect("write source");

        let upload_completed = Arc::new(StdMutex::new(false));
        let multipart_completed = Arc::new(StdMutex::new(false));
        let multipart_aborted = Arc::new(StdMutex::new(false));
        let part_started = Arc::new(AtomicU64::new(0));
        let mock = SlowMockProvider::new(
            Arc::new(StdMutex::new(false)),
            Arc::clone(&upload_completed),
        )
        .with_multipart_state(
            Arc::clone(&multipart_completed),
            Arc::clone(&multipart_aborted),
            Arc::clone(&part_started),
        );
        let arc: SharedProvider = Arc::new(Mutex::new(Some(
            Box::new(mock) as Box<dyn crate::providers::StorageProvider>
        )));
        // Default capabilities: no multipart, so the shape is a single
        // UploadFile core with a no-op CommitTemp.
        let caps = TransferCapabilities::default();
        let built = TransferDagBuilder::shaped_file(TransferDirection::Upload, &caps, 13);
        assert_eq!(built.profile.upload_parts, 1);
        let report = Arc::new(AtomicU64::new(13));
        let observer: Arc<dyn DagObserver> = Arc::new(crate::transfer_dag::NoopDagObserver);

        let res = execute_single_file_dag(
            &built,
            arc,
            "/remote.bin".to_string(),
            local.to_string_lossy().to_string(),
            None,
            None,
            observer,
            report,
            13,
            Some(CancellationToken::new()),
        )
        .await;

        assert!(res.is_ok(), "a plain upload completes: {res:?}");
        assert!(
            *upload_completed.lock().unwrap(),
            "the provider's own upload owns the write"
        );
        assert!(
            !*multipart_completed.lock().unwrap(),
            "a plain single-file path never claims a multipart commit"
        );
        assert_eq!(
            part_started.load(Ordering::SeqCst),
            0,
            "no multipart part is ever uploaded on the single-transfer-core path"
        );
    }

    /// DAG-P2-07: a successful whole-file download attests its on-disk byte
    /// total in the final metrics snapshot (logical == wire == local payload).
    #[tokio::test]
    async fn single_file_download_reports_bytes_in_final_metrics() {
        let dir = tempfile::tempdir().expect("tempdir");
        let local = dir.path().join("out.bin");
        let mock = SlowMockProvider::new(
            Arc::new(StdMutex::new(false)),
            Arc::new(StdMutex::new(false)),
        );
        let arc: SharedProvider = Arc::new(Mutex::new(Some(
            Box::new(mock) as Box<dyn crate::providers::StorageProvider>
        )));
        let caps = TransferCapabilities::default();
        let built = TransferDagBuilder::shaped_file(TransferDirection::Download, &caps, 30);
        let report = Arc::new(AtomicU64::new(30));
        let observer = Arc::new(CollectingDagObserver::default());

        let res = execute_single_file_dag(
            &built,
            arc,
            "/remote.bin".to_string(),
            local.to_string_lossy().to_string(),
            None,
            None,
            Arc::clone(&observer) as Arc<dyn DagObserver>,
            report,
            30,
            Some(CancellationToken::new()),
        )
        .await;
        assert!(res.is_ok(), "download completes: {res:?}");

        // The mock writes b"complete" (8 bytes).
        let metrics = observer.metrics();
        assert_eq!(metrics.bytes_transferred, 8);
        assert_eq!(metrics.logical_bytes, 8);
        assert_eq!(metrics.wire_bytes, 8);
        assert_eq!(metrics.local_payload_bytes, 8);
        assert!(metrics.run_nanos_total > 0, "the transfer node ran");
        assert!(metrics.slot_peak >= 1);
    }

    /// DAG-P2-07: a successful whole-file upload attests the real on-disk
    /// source size (not the caller-declared value) with no local payload
    /// materialization (local stays 0).
    #[tokio::test]
    async fn single_file_upload_reports_bytes_without_local_payload() {
        let dir = tempfile::tempdir().expect("tempdir");
        let local = dir.path().join("source.bin");
        std::fs::write(&local, b"hello aeroftp").expect("write source"); // 13 bytes
        let mock = SlowMockProvider::new(
            Arc::new(StdMutex::new(false)),
            Arc::new(StdMutex::new(false)),
        );
        let arc: SharedProvider = Arc::new(Mutex::new(Some(
            Box::new(mock) as Box<dyn crate::providers::StorageProvider>
        )));
        let caps = TransferCapabilities::default();
        // The declared size (5) deliberately disagrees with the on-disk
        // length (13): attestation must follow the real file, not the caller.
        let built = TransferDagBuilder::shaped_file(TransferDirection::Upload, &caps, 5);
        let report = Arc::new(AtomicU64::new(5));
        let observer = Arc::new(CollectingDagObserver::default());

        let res = execute_single_file_dag(
            &built,
            arc,
            "/remote.bin".to_string(),
            local.to_string_lossy().to_string(),
            None,
            None,
            Arc::clone(&observer) as Arc<dyn DagObserver>,
            report,
            5,
            Some(CancellationToken::new()),
        )
        .await;
        assert!(res.is_ok(), "upload completes: {res:?}");

        let metrics = observer.metrics();
        assert_eq!(metrics.bytes_transferred, 13);
        assert_eq!(metrics.logical_bytes, 13);
        assert_eq!(metrics.wire_bytes, 13);
        assert_eq!(metrics.local_payload_bytes, 0);
    }

    /// DAG-P2-07 fix wave (10a): a cancelled single-file download attests no
    /// bytes in the final metrics snapshot: the transfer node never reached
    /// its success point, so every byte counter stays zero.
    #[tokio::test]
    async fn cancelled_single_file_download_reports_zero_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let local = dir.path().join("out.bin");
        let completed = Arc::new(StdMutex::new(false));
        let mock = SlowMockProvider::new(Arc::clone(&completed), Arc::new(StdMutex::new(false)));
        let arc: SharedProvider = Arc::new(Mutex::new(Some(
            Box::new(mock) as Box<dyn crate::providers::StorageProvider>
        )));
        let caps = TransferCapabilities::default();
        let built = TransferDagBuilder::shaped_file(TransferDirection::Download, &caps, 30);
        let report = Arc::new(AtomicU64::new(30));
        let observer = Arc::new(CollectingDagObserver::default());

        let token = CancellationToken::new();
        let canceller = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            canceller.cancel();
        });

        let res = execute_single_file_dag(
            &built,
            arc,
            "/remote.bin".to_string(),
            local.to_string_lossy().to_string(),
            None,
            None,
            Arc::clone(&observer) as Arc<dyn DagObserver>,
            report,
            30,
            Some(token),
        )
        .await;

        assert!(res.is_err(), "a cancelled transfer must return an error");
        assert!(!*completed.lock().unwrap());
        let metrics = observer.metrics();
        assert_eq!(metrics.bytes_transferred, 0);
        assert_eq!(metrics.logical_bytes, 0);
        assert_eq!(metrics.wire_bytes, 0);
        assert_eq!(metrics.local_payload_bytes, 0);
    }

    /// DAG-P2-07 fix wave (M4): a resumed multipart upload keeps the layout
    /// total as its logical bytes but attests on the wire only the parts it
    /// actually re-uploaded this run (receipt-backed parts contribute 0).
    #[tokio::test]
    async fn resumed_multipart_upload_attests_only_reuploaded_parts_on_the_wire() {
        let dir = tempfile::tempdir().expect("tempdir");
        let local = dir.path().join("source.bin");
        std::fs::write(&local, b"0123456789abcdefghij").expect("write source"); // 20 bytes

        let multipart_completed = Arc::new(StdMutex::new(false));
        let multipart_aborted = Arc::new(StdMutex::new(false));
        let part_started = Arc::new(AtomicU64::new(0));
        // First run: part 2 fails deterministically after part 1's receipt is
        // durable, leaving a resumable checkpoint with one of two parts.
        let failing_parts: Arc<StdMutex<std::collections::HashSet<u32>>> =
            Arc::new(StdMutex::new(std::collections::HashSet::from([2])));
        let mock = SlowMockProvider::new(
            Arc::new(StdMutex::new(false)),
            Arc::new(StdMutex::new(false)),
        )
        .with_multipart_state(
            Arc::clone(&multipart_completed),
            Arc::clone(&multipart_aborted),
            Arc::clone(&part_started),
        )
        .with_failing_parts(Arc::clone(&failing_parts));
        let arc: SharedProvider = Arc::new(Mutex::new(Some(
            Box::new(mock) as Box<dyn crate::providers::StorageProvider>
        )));
        let caps = TransferCapabilities {
            multipart_upload: Capability::Supported,
            preferred_chunk_size: Some(10),
            multipart_threshold: 0,
            max_chunk_slots: Some(1),
            ..TransferCapabilities::default()
        };
        let built = TransferDagBuilder::shaped_file(TransferDirection::Upload, &caps, 20);
        assert_eq!(built.profile.upload_parts, 2);

        let first = execute_single_file_dag(
            &built,
            Arc::clone(&arc),
            "/remote.bin".to_string(),
            local.to_string_lossy().to_string(),
            None,
            None,
            Arc::new(crate::transfer_dag::NoopDagObserver) as Arc<dyn DagObserver>,
            Arc::new(AtomicU64::new(20)),
            20,
            Some(CancellationToken::new()),
        )
        .await;
        assert!(
            first.is_err(),
            "the scripted part-2 failure must stop the first run"
        );
        assert!(
            !*multipart_aborted.lock().unwrap(),
            "a durable checkpoint keeps the provider session for resume"
        );

        // Second run: no scripted failure. Part 1 is receipt-backed and skips
        // its upload; only part 2 crosses the wire.
        failing_parts.lock().unwrap().clear();
        let observer = Arc::new(CollectingDagObserver::default());
        let second = execute_single_file_dag(
            &built,
            arc,
            "/remote.bin".to_string(),
            local.to_string_lossy().to_string(),
            None,
            None,
            Arc::clone(&observer) as Arc<dyn DagObserver>,
            Arc::new(AtomicU64::new(20)),
            20,
            Some(CancellationToken::new()),
        )
        .await;
        assert!(second.is_ok(), "the resumed run commits: {second:?}");

        let metrics = observer.metrics();
        assert_eq!(metrics.bytes_transferred, 20);
        assert_eq!(metrics.logical_bytes, 20);
        assert_eq!(
            metrics.wire_bytes, 10,
            "only the re-uploaded part crosses the wire on resume"
        );
        assert!(metrics.wire_bytes < metrics.logical_bytes);
        assert_eq!(metrics.local_payload_bytes, 0);
    }

    /// DAG-P2-07: a committed native-multipart upload attests the layout
    /// total at the commit node, not per part (bytes are counted once).
    #[tokio::test]
    async fn multipart_upload_reports_layout_bytes_once_at_commit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let local = dir.path().join("source.bin");
        std::fs::write(&local, b"0123456789abcdefghij").expect("write source"); // 20 bytes

        let mock = SlowMockProvider::new(
            Arc::new(StdMutex::new(false)),
            Arc::new(StdMutex::new(false)),
        );
        let arc: SharedProvider = Arc::new(Mutex::new(Some(
            Box::new(mock) as Box<dyn crate::providers::StorageProvider>
        )));
        let caps = TransferCapabilities {
            multipart_upload: Capability::Supported,
            preferred_chunk_size: Some(10),
            multipart_threshold: 0,
            max_chunk_slots: Some(1),
            ..TransferCapabilities::default()
        };
        let built = TransferDagBuilder::shaped_file(TransferDirection::Upload, &caps, 20);
        assert_eq!(built.profile.upload_parts, 2);
        let report = Arc::new(AtomicU64::new(20));
        let observer = Arc::new(CollectingDagObserver::default());

        let res = execute_single_file_dag(
            &built,
            arc,
            "/remote.bin".to_string(),
            local.to_string_lossy().to_string(),
            None,
            None,
            Arc::clone(&observer) as Arc<dyn DagObserver>,
            report,
            20,
            Some(CancellationToken::new()),
        )
        .await;
        assert!(res.is_ok(), "multipart upload commits: {res:?}");

        let metrics = observer.metrics();
        assert_eq!(metrics.bytes_transferred, 20);
        assert_eq!(metrics.logical_bytes, 20);
        assert_eq!(metrics.wire_bytes, 20);
        assert_eq!(metrics.local_payload_bytes, 0);
    }

    #[tokio::test]
    async fn read_chunk_returns_exact_slice() {
        // The slicing helper underpins the multipart per-part disk read;
        // verify it returns the right bytes for an inner-and-tail slice.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("source.bin");
        let mut content = Vec::with_capacity(20);
        for i in 0u8..20 {
            content.push(i);
        }
        std::fs::write(&path, &content).expect("write fixture");

        let inner = crate::transfer_multipart::read_chunk(path.to_str().unwrap(), 5, 8)
            .await
            .expect("read inner");
        assert_eq!(inner, content[5..13]);

        let tail = crate::transfer_multipart::read_chunk(path.to_str().unwrap(), 16, 4)
            .await
            .expect("read tail");
        assert_eq!(tail, content[16..20]);
    }

    /// DAG-P2-07 (block C): a provider whose data path goes through the
    /// shared `send_with_retry` choke point. The single-file runner never
    /// measures anything itself; the choke point publishes the honest
    /// first-byte sample and the executor folds it at the single finalize.
    struct HttpFixtureMockProvider {
        addr: std::net::SocketAddr,
    }

    impl HttpFixtureMockProvider {
        async fn get_body(&self, url: &str) -> Result<Vec<u8>, ProviderError> {
            let client = reqwest::Client::new();
            let request = client
                .get(url)
                .build()
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            let response = crate::providers::send_with_retry(
                &client,
                request,
                &crate::providers::HttpRetryConfig::default(),
            )
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            response
                .bytes()
                .await
                .map(|b| b.to_vec())
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))
        }
    }

    #[async_trait::async_trait]
    impl crate::providers::StorageProvider for HttpFixtureMockProvider {
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn provider_type(&self) -> crate::providers::ProviderType {
            crate::providers::ProviderType::WebDav
        }
        fn display_name(&self) -> String {
            "http-fixture-mock".into()
        }
        async fn connect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn disconnect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        fn is_connected(&self) -> bool {
            true
        }
        async fn list(
            &mut self,
            _p: &str,
        ) -> Result<Vec<crate::providers::RemoteEntry>, ProviderError> {
            Ok(vec![])
        }
        async fn pwd(&mut self) -> Result<String, ProviderError> {
            Ok("/".into())
        }
        async fn cd(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn cd_up(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn download(
            &mut self,
            _remote: &str,
            local: &str,
            _cb: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            let body = self
                .get_body(&format!("http://{}/object", self.addr))
                .await?;
            std::fs::write(local, body).map_err(ProviderError::IoError)
        }
        async fn download_to_bytes(&mut self, _remote: &str) -> Result<Vec<u8>, ProviderError> {
            self.get_body(&format!("http://{}/object", self.addr)).await
        }
        async fn upload(
            &mut self,
            _local: &str,
            _remote: &str,
            _cb: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn begin_multipart_upload(
            &mut self,
            remote_path: &str,
            _total_size: u64,
            _content_type: Option<&str>,
            _local_source_path: Option<&str>,
        ) -> Result<MultipartHandle, ProviderError> {
            Ok(MultipartHandle {
                upload_id: "fixture-upload".to_string(),
                remote_path: remote_path.to_string(),
            })
        }
        async fn upload_part(
            &mut self,
            _handle: &MultipartHandle,
            part_number: u32,
            data: Vec<u8>,
        ) -> Result<UploadedPart, ProviderError> {
            let client = reqwest::Client::new();
            let request = client
                .put(format!("http://{}/part-{part_number}", self.addr))
                .body(data)
                .build()
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            let response = crate::providers::send_with_retry(
                &client,
                request,
                &crate::providers::HttpRetryConfig::default(),
            )
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            if !response.status().is_success() {
                return Err(ProviderError::TransferFailed(format!(
                    "part {part_number} rejected: {}",
                    response.status()
                )));
            }
            Ok(UploadedPart {
                part_number,
                etag: format!("etag-{part_number}"),
            })
        }
        async fn complete_multipart_upload(
            &mut self,
            _handle: MultipartHandle,
            _parts: Vec<UploadedPart>,
        ) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn abort_multipart_upload(
            &mut self,
            _handle: MultipartHandle,
        ) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn mkdir(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn delete(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rmdir(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rmdir_recursive(&mut self, _p: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn rename(&mut self, _f: &str, _t: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn stat(&mut self, _p: &str) -> Result<crate::providers::RemoteEntry, ProviderError> {
            Err(ProviderError::NotSupported("stat".into()))
        }
        async fn size(&mut self, _p: &str) -> Result<u64, ProviderError> {
            Ok(30)
        }
        async fn exists(&mut self, _p: &str) -> Result<bool, ProviderError> {
            Ok(true)
        }
        async fn keep_alive(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn server_info(&mut self) -> Result<String, ProviderError> {
            Ok("http-fixture-mock".into())
        }
    }

    /// DAG-P2-07 (block C): the whole-file single GET runner is honestly
    /// measurable through the shared choke point: one download request
    /// reaching headers folds exactly one first-byte sample into the run
    /// metrics the observer receives at the single finalize.
    #[tokio::test]
    async fn single_file_download_folds_one_ttfb_sample_from_send_with_retry() {
        use crate::transfer_dag::ttfb::test_fixture::{
            spawn_delayed_http_fixture, FixtureResponse,
        };

        let addr = spawn_delayed_http_fixture(std::time::Duration::from_millis(50), 1, |_head| {
            FixtureResponse::ok("payload")
        })
        .await;

        let dir = tempfile::tempdir().expect("tempdir");
        let local = dir.path().join("out.bin");
        let arc: SharedProvider =
            Arc::new(Mutex::new(Some(Box::new(HttpFixtureMockProvider { addr })
                as Box<dyn crate::providers::StorageProvider>)));
        let caps = TransferCapabilities::default();
        let built = TransferDagBuilder::shaped_file(TransferDirection::Download, &caps, 7);
        let report = Arc::new(AtomicU64::new(7));
        let observer = Arc::new(CollectingDagObserver::default());

        let res = execute_single_file_dag(
            &built,
            arc,
            "/remote.bin".to_string(),
            local.to_string_lossy().to_string(),
            None,
            None,
            Arc::clone(&observer) as Arc<dyn DagObserver>,
            report,
            7,
            Some(CancellationToken::new()),
        )
        .await;
        assert!(res.is_ok(), "fixture download completes: {res:?}");

        let metrics = observer.metrics();
        assert_eq!(
            metrics.ttfb_samples, 1,
            "one download request reaching headers is exactly one sample"
        );
        assert!(
            metrics.ttfb_nanos_total >= std::time::Duration::from_millis(40).as_nanos() as u64,
            "sampled TTFB {}ns below the 50ms fixture first-byte delay",
            metrics.ttfb_nanos_total
        );
    }

    /// DAG-P2-07 (block C): the multipart part path is honestly measurable
    /// through the same choke point: each `UploadPart` node PUT that reaches
    /// headers folds its own first-byte sample, so a 2-part fan-out reports
    /// exactly 2 samples.
    #[tokio::test]
    async fn multipart_upload_folds_one_ttfb_sample_per_part() {
        use crate::transfer_dag::ttfb::test_fixture::{
            spawn_delayed_http_fixture, FixtureResponse,
        };

        let addr = spawn_delayed_http_fixture(std::time::Duration::from_millis(20), 2, |_head| {
            FixtureResponse::ok("ack")
        })
        .await;

        let dir = tempfile::tempdir().expect("tempdir");
        let local = dir.path().join("source.bin");
        std::fs::write(&local, b"0123456789abcdefghij").expect("write source"); // 20 bytes

        let arc: SharedProvider =
            Arc::new(Mutex::new(Some(Box::new(HttpFixtureMockProvider { addr })
                as Box<dyn crate::providers::StorageProvider>)));
        let caps = TransferCapabilities {
            multipart_upload: Capability::Supported,
            preferred_chunk_size: Some(10),
            multipart_threshold: 0,
            max_chunk_slots: Some(1),
            ..TransferCapabilities::default()
        };
        let built = TransferDagBuilder::shaped_file(TransferDirection::Upload, &caps, 20);
        assert_eq!(built.profile.upload_parts, 2);
        let report = Arc::new(AtomicU64::new(20));
        let observer = Arc::new(CollectingDagObserver::default());

        let res = execute_single_file_dag(
            &built,
            arc,
            "/remote.bin".to_string(),
            local.to_string_lossy().to_string(),
            None,
            None,
            Arc::clone(&observer) as Arc<dyn DagObserver>,
            report,
            20,
            Some(CancellationToken::new()),
        )
        .await;
        assert!(res.is_ok(), "multipart fixture upload commits: {res:?}");

        let metrics = observer.metrics();
        assert_eq!(
            metrics.ttfb_samples, 2,
            "two part PUTs reaching headers are exactly two samples"
        );
        assert!(
            metrics.ttfb_nanos_total >= std::time::Duration::from_millis(30).as_nanos() as u64,
            "two 20ms-delayed parts must total >= 30ms, got {}ns",
            metrics.ttfb_nanos_total
        );
    }
}
