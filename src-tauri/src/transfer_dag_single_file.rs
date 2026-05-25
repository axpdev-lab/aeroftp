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
//! - `VerifyChecksum`: no-op in this slice (the legacy single-file path does
//!   not verify).
//! - `PreserveMetadata`: restores the remote mtime on a downloaded file; a
//!   no-op on the upload direction.
//! - `CommitTemp`: no-op for the single transfer core. For multipart it
//!   takes the accumulated parts, sorts by part number, and calls
//!   `provider.complete_multipart_upload` to finalize the object.
//! - `EmitProgress`: no-op terminal node; its completion is the signal a
//!   [`DagObserver`] maps onto the GUI "complete" event.

use std::collections::HashMap;
use std::io::SeekFrom;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::Mutex;

use crate::providers::{
    B2Provider, MultipartHandle, ProviderError, S3Provider, StorageProvider, UploadedPart,
};
use crate::transfer_dag::executor::{execute_dag, DagNodeRunner, NodeFuture, NodeOutcome};
use crate::transfer_dag::graph::{TransferNode, TransferNodeKind};
use crate::transfer_dag::{
    AimdConfig, AimdController, DagObserver, ShapedFileDag, TransferBudget, TransferDirection,
    TransferResourceManager,
};

/// A per-byte transfer progress callback, as accepted by
/// [`StorageProvider::download`] / [`StorageProvider::upload`].
pub type ProgressCallback = Box<dyn Fn(u64, u64) + Send>;

/// The connected-provider handle shared between the GUI command state and the
/// spawned DAG node tasks. `Option` because a session may be disconnected.
pub type SharedProvider = Arc<Mutex<Option<Box<dyn StorageProvider>>>>;

/// Per-transfer multipart orchestration context, shared across every
/// `UploadPart` runner invocation and the terminal `CommitTemp` finalize /
/// abort path.
///
/// `handle` is lazy because parts run in topological order under the shared
/// chunk budget, and only one of them needs to open the session; the rest
/// reuse the handle. `parts` accumulates receipts (one per successful
/// `upload_part`); the commit node sorts by `part_number` ascending before
/// submitting to `complete_multipart_upload`, matching the S3 contract that
/// every multipart backend in our matrix happens to follow. `node_to_part`
/// maps the DAG node id of an `UploadPart` to the 1-based part number the
/// builder assigned to it (transfer[0] = part 1, transfer[1] = part 2, ...).
struct MultipartCtx {
    handle: Arc<Mutex<Option<MultipartHandle>>>,
    parts: Arc<Mutex<Vec<UploadedPart>>>,
    node_to_part: Arc<HashMap<usize, u32>>,
    part_size: u64,
    total_size: u64,
    content_type: String,
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

    // The multipart context is set up once, only when the shape actually fans
    // out into N `UploadPart` nodes. When the transfer core is a single
    // `UploadFile` or `DownloadFile`, the context stays `None` and every
    // `UploadPart`/`CommitTemp` branch short-circuits to the legacy no-op.
    let multipart_ctx: Option<Arc<MultipartCtx>> = if direction == TransferDirection::Upload
        && built.profile.upload_parts > 1
        && file_size > 0
    {
        let total_parts = built.profile.upload_parts as u64;
        // ceil(file_size / total_parts) keeps every part the same size as
        // the builder's preferred chunk modulo the last (slightly smaller)
        // tail: the builder picked total_parts = ceil(file_size / chunk)
        // exactly to honour that invariant.
        let part_size = file_size.div_ceil(total_parts);
        let node_to_part: HashMap<usize, u32> = built
            .transfer
            .iter()
            .enumerate()
            .map(|(idx, node_id)| (*node_id, (idx + 1) as u32))
            .collect();
        let content_type = mime_guess::from_path(&*local)
            .first_or_octet_stream()
            .to_string();
        Some(Arc::new(MultipartCtx {
            handle: Arc::new(Mutex::new(None)),
            parts: Arc::new(Mutex::new(Vec::with_capacity(total_parts as usize))),
            node_to_part: Arc::new(node_to_part),
            part_size,
            total_size: file_size,
            content_type,
        }))
    } else {
        None
    };

    let runner: Arc<dyn DagNodeRunner> = {
        let provider = Arc::clone(&provider);
        let remote = Arc::clone(&remote);
        let local = Arc::clone(&local);
        let modified = modified.clone();
        let progress_slot = Arc::clone(&progress_slot);
        let first_error = Arc::clone(&first_error);
        let report_size = Arc::clone(&report_size);
        let multipart_ctx = multipart_ctx.clone();
        Arc::new(move |node: TransferNode| -> NodeFuture {
            let provider = Arc::clone(&provider);
            let remote = Arc::clone(&remote);
            let local = Arc::clone(&local);
            let modified = modified.clone();
            let progress_slot = Arc::clone(&progress_slot);
            let first_error = Arc::clone(&first_error);
            let report_size = Arc::clone(&report_size);
            let multipart_ctx = multipart_ctx.clone();
            Box::pin(async move {
                match node.kind {
                    TransferNodeKind::DownloadFile => {
                        let cb = progress_slot.lock().expect("progress slot poisoned").take();
                        let mut guard = provider.lock().await;
                        let Some(p) = guard.as_mut() else {
                            return record_failure(&first_error, ProviderError::NotConnected);
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
                            return record_failure(&first_error, ProviderError::NotConnected);
                        };
                        match p.upload(&local, &remote, cb).await {
                            Ok(()) => NodeOutcome::Completed,
                            Err(e) => record_failure(&first_error, e),
                        }
                    }
                    TransferNodeKind::UploadPart => {
                        let Some(ctx) = multipart_ctx.as_ref() else {
                            return record_failure(
                                &first_error,
                                ProviderError::TransferFailed(
                                    "UploadPart node without multipart context".to_string(),
                                ),
                            );
                        };
                        let Some(part_number) = ctx.node_to_part.get(&node.id).copied() else {
                            return record_failure(
                                &first_error,
                                ProviderError::TransferFailed(format!(
                                    "UploadPart node {} not mapped to a part number",
                                    node.id
                                )),
                            );
                        };
                        // 1. Lazy `begin_multipart_upload`. The first runner
                        //    invocation that wins the handle mutex opens the
                        //    session; subsequent invocations observe an
                        //    initialized handle and skip the call. The
                        //    handle is cheap to clone (a few-byte `String`
                        //    plus the remote path), so we hold it by value
                        //    in every part call below.
                        {
                            let mut handle_guard = ctx.handle.lock().await;
                            if handle_guard.is_none() {
                                let mut guard = provider.lock().await;
                                let Some(p) = guard.as_mut() else {
                                    return record_failure(
                                        &first_error,
                                        ProviderError::NotConnected,
                                    );
                                };
                                match p
                                    .begin_multipart_upload(
                                        &remote,
                                        ctx.total_size,
                                        Some(&ctx.content_type),
                                        Some(&local),
                                    )
                                    .await
                                {
                                    Ok(handle) => {
                                        *handle_guard = Some(handle);
                                    }
                                    Err(e) => return record_failure(&first_error, e),
                                }
                            }
                        }

                        // 2. Read this part's slice from disk at the matching
                        //    offset. The last part may be smaller than
                        //    `part_size`; `saturating_sub` handles that
                        //    without panicking on an exact multiple.
                        let offset = (part_number as u64 - 1) * ctx.part_size;
                        let len = ctx.part_size.min(ctx.total_size.saturating_sub(offset));
                        let data = match read_chunk(&local, offset, len).await {
                            Ok(buf) => buf,
                            Err(e) => return record_failure(&first_error, e),
                        };

                        // 3. Upload the part using the resolved handle.
                        let handle = {
                            let handle_guard = ctx.handle.lock().await;
                            handle_guard
                                .as_ref()
                                .cloned()
                                .expect("multipart handle initialized in step 1")
                        };
                        let cloned_worker = {
                            let mut guard = provider.lock().await;
                            let Some(p) = guard.as_mut() else {
                                return record_failure(&first_error, ProviderError::NotConnected);
                            };
                            clone_multipart_worker(p.as_mut())
                        };
                        let upload_result = if let Some(mut worker) = cloned_worker {
                            worker.upload_part(&handle, part_number, data).await
                        } else {
                            let mut guard = provider.lock().await;
                            let Some(p) = guard.as_mut() else {
                                return record_failure(&first_error, ProviderError::NotConnected);
                            };
                            p.upload_part(&handle, part_number, data).await
                        };
                        match upload_result {
                            Ok(receipt) => {
                                ctx.parts.lock().await.push(receipt);
                                NodeOutcome::Completed
                            }
                            Err(e) => record_failure(&first_error, e),
                        }
                    }
                    TransferNodeKind::ServerSideCopy => {
                        // Forward-compat: the shaped-copy graph emits this
                        // kind, the shaped-file graph does not. Wired for
                        // SG-T12 when shaped-copy lands in the sync runner.
                        //
                        // Goes through `server_side_copy_with_fallback` so
                        // providers that advertise the capability but reject
                        // a specific operation (S3 cross-bucket without IAM,
                        // WebDAV 501, Nextcloud cross-share MOVE) degrade to
                        // streaming download → upload instead of failing the
                        // node outright. Hard errors (auth, missing source)
                        // still propagate via `record_failure`.
                        let mut guard = provider.lock().await;
                        let Some(p) = guard.as_mut() else {
                            return record_failure(&first_error, ProviderError::NotConnected);
                        };
                        match crate::copy_fallback::server_side_copy_with_fallback(
                            p.as_mut(),
                            &remote,
                            &local,
                        )
                        .await
                        {
                            Ok(_outcome) => NodeOutcome::Completed,
                            Err(e) => record_failure(&first_error, e),
                        }
                    }
                    TransferNodeKind::CommitTemp => {
                        // For multipart uploads, finalize the session by
                        // submitting the accumulated parts in part-number
                        // order. The handle is taken (the session is no
                        // longer valid after `complete_multipart_upload`,
                        // success or failure), so the failure-path abort
                        // below knows to skip when commit already consumed
                        // it. For the single-transfer-core shape this is a
                        // no-op.
                        if let Some(ctx) = multipart_ctx.as_ref() {
                            let handle = {
                                let mut handle_guard = ctx.handle.lock().await;
                                handle_guard.take()
                            };
                            if let Some(handle) = handle {
                                let mut parts = std::mem::take(&mut *ctx.parts.lock().await);
                                parts.sort_by_key(|p| p.part_number);
                                let mut guard = provider.lock().await;
                                let Some(p) = guard.as_mut() else {
                                    return record_failure(
                                        &first_error,
                                        ProviderError::NotConnected,
                                    );
                                };
                                match p.complete_multipart_upload(handle, parts).await {
                                    Ok(()) => NodeOutcome::Completed,
                                    Err(e) => record_failure(&first_error, e),
                                }
                            } else {
                                NodeOutcome::Completed
                            }
                        } else {
                            NodeOutcome::Completed
                        }
                    }
                    TransferNodeKind::PreserveMetadata => {
                        if direction == TransferDirection::Download {
                            crate::preserve_remote_mtime(&local, modified.as_deref());
                        }
                        NodeOutcome::Completed
                    }
                    // DiscoverRemote / DiscoverLocal / AcquireResource /
                    // VerifyChecksum / EmitProgress: structural anchors,
                    // no-ops in the current slice. A single-file graph
                    // never produces any other kind.
                    _ => NodeOutcome::Completed,
                }
            })
        })
    };

    let manager = TransferResourceManager::new(single_file_budget(built));

    // AIMD backpressure only helps when a shaped graph has real chunk/http/api
    // concurrency to tune. Plain single-stream providers such as SFTP request
    // only the one file slot, whose ceiling cannot shrink usefully, so keep
    // their dispatch path free of no-op adaptive bookkeeping.
    let aimd = single_file_needs_aimd(built).then(|| {
        Arc::new(AimdController::from_budget(
            &manager.budget(),
            AimdConfig::default(),
        ))
    });

    let outcome = execute_dag(&built.dag, &manager, runner, observer, aimd).await;

    // On failure, best-effort abort an in-flight multipart session so the
    // provider does not accumulate orphan upload IDs. Idempotent because the
    // commit branch already `take()`s the handle on success, so this only
    // runs when the failure happened before commit ran.
    if outcome.is_err() {
        if let Some(ctx) = multipart_ctx.as_ref() {
            let leftover_handle = {
                let mut handle_guard = ctx.handle.lock().await;
                handle_guard.take()
            };
            if let Some(handle) = leftover_handle {
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

/// Read `len` bytes from `path` starting at `offset` into a fresh `Vec`.
async fn read_chunk(path: &str, offset: u64, len: u64) -> Result<Vec<u8>, ProviderError> {
    let mut file = tokio::fs::File::open(path).await?;
    file.seek(SeekFrom::Start(offset)).await?;
    let mut data = vec![0u8; len as usize];
    file.read_exact(&mut data).await?;
    Ok(data)
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

fn single_file_needs_aimd(built: &ShapedFileDag) -> bool {
    built.dag.nodes().iter().any(|node| {
        node.resources.chunk_slots > 0
            || node.resources.http_slots > 0
            || node.resources.api_slots > 0
    })
}

fn single_file_budget(built: &ShapedFileDag) -> TransferBudget {
    let mut budget = TransferBudget::from_file_slots(1);
    if built.direction == TransferDirection::Upload && built.profile.upload_parts > 1 {
        let chunk_slots = built
            .profile
            .max_chunk_slots
            .max(1)
            .min(built.profile.upload_parts as u16);
        budget.chunk_slots = chunk_slots;
        budget.disk_read_slots = budget.disk_read_slots.max(chunk_slots);
    }
    budget
}

fn clone_multipart_worker(provider: &mut dyn StorageProvider) -> Option<Box<dyn StorageProvider>> {
    if let Some(s3) = provider.as_any_mut().downcast_mut::<S3Provider>() {
        return Some(Box::new(s3.clone()));
    }
    if let Some(b2) = provider.as_any_mut().downcast_mut::<B2Provider>() {
        return Some(Box::new(b2.clone()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer_dag::{Capability, TransferCapabilities, TransferDagBuilder};

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
            ..TransferCapabilities::default()
        };
        let built =
            TransferDagBuilder::shaped_file(TransferDirection::Upload, &caps, 40 * 1024 * 1024);

        let budget = single_file_budget(&built);
        assert_eq!(budget.file_slots, 1);
        assert_eq!(budget.chunk_slots, 4);
        assert_eq!(budget.disk_read_slots, 4);
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

        let inner = read_chunk(path.to_str().unwrap(), 5, 8)
            .await
            .expect("read inner");
        assert_eq!(inner, content[5..13]);

        let tail = read_chunk(path.to_str().unwrap(), 16, 4)
            .await
            .expect("read tail");
        assert_eq!(tail, content[16..20]);
    }
}
