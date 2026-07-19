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
use std::future::Future;
use std::io::SeekFrom;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::providers::{MultipartHandle, ProviderError, StorageProvider, UploadedPart};
use crate::transfer_dag::executor::{
    execute_dag_with_options, DagExecuteOptions, DagNodeRunner, NodeFuture, NodeOutcome,
};
use crate::transfer_dag::graph::{TransferNode, TransferNodeKind};
use crate::transfer_dag::{
    AimdConfig, AimdController, DagObserver, FailureScope, ShapedFileDag, TransferBudget,
    TransferDirection, TransferError, TransferResourceManager,
};

/// A per-byte transfer progress callback, as accepted by
/// [`StorageProvider::download`] / [`StorageProvider::upload`].
pub type ProgressCallback = Box<dyn Fn(u64, u64) + Send>;

/// The connected-provider handle shared between the GUI command state and the
/// spawned DAG node tasks. `Option` because a session may be disconnected.
pub type SharedProvider = Arc<Mutex<Option<Box<dyn StorageProvider>>>>;

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

    // The multipart context is set up once, only when the shape actually fans
    // out into N `UploadPart` nodes. When the transfer core is a single
    // `UploadFile` or `DownloadFile`, the context stays `None` and every
    // `UploadPart`/`CommitTemp` branch short-circuits to the legacy no-op.
    let multipart_ctx: Option<Arc<MultipartCtx>> = if direction == TransferDirection::Upload
        && built.profile.upload_parts > 1
        && file_size > 0
    {
        let total_parts = built.profile.upload_parts as u64;
        // Prefer the provider's exact advertised chunk size when supplied
        // so chunks honour per-provider alignment contracts (Drive: 256 KiB
        // multiple; OneDrive: 320 KiB multiple; B2 / S3: any size ≥ 5 MiB).
        // The last part takes whatever remains; the `ctx.part_size.min(...)`
        // slice below handles that automatically. When `preferred_chunk_size`
        // is `0` (no provider hint) fall back to the legacy `div_ceil`
        // equalisation that keeps every part the same size.
        let part_size = if built.profile.preferred_chunk_size > 0 {
            built.profile.preferred_chunk_size
        } else {
            file_size.div_ceil(total_parts)
        };
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
        let cancel_token = cancel_token.clone();
        Arc::new(move |node: TransferNode| -> NodeFuture {
            let provider = Arc::clone(&provider);
            let remote = Arc::clone(&remote);
            let local = Arc::clone(&local);
            let modified = modified.clone();
            let progress_slot = Arc::clone(&progress_slot);
            let first_error = Arc::clone(&first_error);
            let report_size = Arc::clone(&report_size);
            let multipart_ctx = multipart_ctx.clone();
            let cancel_token = cancel_token.clone();
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
                            Ok(()) => NodeOutcome::Completed,
                            Err(e) => record_failure(&first_error, e, FailureScope::File),
                        }
                    }
                    TransferNodeKind::UploadPart => {
                        let Some(ctx) = multipart_ctx.as_ref() else {
                            return record_failure(
                                &first_error,
                                ProviderError::TransferFailed(
                                    "UploadPart node without multipart context".to_string(),
                                ),
                                FailureScope::Part,
                            );
                        };
                        let Some(part_number) = ctx.node_to_part.get(&node.id).copied() else {
                            return record_failure(
                                &first_error,
                                ProviderError::TransferFailed(format!(
                                    "UploadPart node {} not mapped to a part number",
                                    node.id
                                )),
                                FailureScope::Part,
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
                                        FailureScope::Part,
                                    );
                                };
                                let begin = async {
                                    p.begin_multipart_upload(
                                        &remote,
                                        ctx.total_size,
                                        Some(&ctx.content_type),
                                        Some(&local),
                                    )
                                    .await
                                };
                                match race_cancel(&cancel_token, begin).await {
                                    Ok(handle) => {
                                        *handle_guard = Some(handle);
                                    }
                                    Err(e) => {
                                        return record_failure(&first_error, e, FailureScope::Part);
                                    }
                                }
                            }
                        }

                        // 2. Read this part's slice from disk at the matching
                        //    offset. The last part may be smaller than
                        //    `part_size`; `saturating_sub` handles that
                        //    without panicking on an exact multiple.
                        let offset = (part_number as u64 - 1) * ctx.part_size;
                        let len = ctx.part_size.min(ctx.total_size.saturating_sub(offset));
                        let data = match race_cancel(&cancel_token, read_chunk(&local, offset, len))
                            .await
                        {
                            Ok(buf) => buf,
                            Err(e) => {
                                return record_failure(&first_error, e, FailureScope::Part);
                            }
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
                                worker.upload_part(&handle, part_number, data).await
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
                                p.upload_part(&handle, part_number, data).await
                            })
                            .await
                        };
                        match upload_result {
                            Ok(receipt) => {
                                ctx.parts.lock().await.push(receipt);
                                NodeOutcome::Completed
                            }
                            Err(e) => record_failure(&first_error, e, FailureScope::Part),
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
                            return record_failure(
                                &first_error,
                                ProviderError::NotConnected,
                                FailureScope::File,
                            );
                        };
                        match crate::copy_fallback::server_side_copy_with_fallback(
                            p.as_mut(),
                            &remote,
                            &local,
                        )
                        .await
                        {
                            Ok(_outcome) => NodeOutcome::Completed,
                            Err(e) => record_failure(&first_error, e, FailureScope::File),
                        }
                    }
                    TransferNodeKind::CommitTemp => {
                        // For multipart uploads, finalize the session by
                        // submitting the accumulated parts in part-number
                        // order. The handle is CLONED for the complete call
                        // and the slot is cleared only on success; on a
                        // commit-time failure the handle stays in place so the
                        // post-execute abort guard reliably aborts the
                        // orphaned session instead of leaking the upload id
                        // (audit ERR-01). For the single-transfer-core shape
                        // this is a no-op.
                        if let Some(ctx) = multipart_ctx.as_ref() {
                            let handle = {
                                let handle_guard = ctx.handle.lock().await;
                                handle_guard.as_ref().cloned()
                            };
                            if let Some(handle) = handle {
                                let mut parts = std::mem::take(&mut *ctx.parts.lock().await);
                                parts.sort_by_key(|p| p.part_number);
                                // Scope the provider guard so it is released
                                // before we touch the handle mutex, keeping the
                                // lock order (handle then provider) consistent
                                // with the lazy-begin path.
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
                                        // Session finalized: clear the handle so
                                        // the abort guard becomes a no-op.
                                        ctx.handle.lock().await.take();
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
    let aimd_provider_type = {
        let guard = provider.lock().await;
        guard.as_ref().map(|provider| provider.provider_type())
    };

    // AIMD backpressure only helps when a shaped graph has real chunk/http/api
    // concurrency to tune. Plain single-stream providers such as SFTP request
    // only the one file slot, whose ceiling cannot shrink usefully, so keep
    // their dispatch path free of no-op adaptive bookkeeping.
    let aimd = single_file_needs_aimd(built).then(|| {
        Arc::new(AimdController::from_budget_for_provider(
            &manager.budget(),
            aimd_provider_type,
            AimdConfig::runtime(),
        ))
    });

    // Graph-scoped cancel is a child of the caller's token (when present) so
    // user Stop and first-part fail-fast both terminate resident siblings
    // within FAIL_FAST_ABORT_GRACE. Production keeps node_timeout = None so
    // valid long transfers are never cut by an arbitrary engine limit.
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

    // On failure, best-effort abort an in-flight multipart session so the
    // provider does not accumulate orphan upload IDs. Idempotent because the
    // commit branch clears the handle ONLY on success, so this runs for both a
    // failure before commit AND a commit-time failure (audit ERR-01); a
    // successful commit leaves no handle and this is a no-op. `take()` ensures
    // abort is called at most once even when fail-fast cancelled many parts.
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

/// Mint an independent worker for a concurrent part upload, or `None` when the
/// provider must serialise its parts on the shared session mutex.
///
/// Gated on `clone_for_transfer()` (the actual capability the parallel part
/// path needs: an independent HTTP/session worker) rather than a hardcoded
/// `S3`/`B2` downcast or the `transfer_executor_kind()` flag. Gating on the
/// clone itself lets a provider parallelise its multipart chunks WITHOUT also
/// opting into clone-pool BATCH execution (which `transfer_executor_kind`
/// drives), so the blast radius stays exactly the per-part path. S3, B2, Azure
/// and Nextcloud WebDAV produce an independent worker here; providers that
/// cannot clone (the trait default) return `None` and keep the session-mutex
/// fallback at the call site (audit CHUNK-01). Ordering-sensitive providers
/// (Drive/OneDrive) stay correct regardless: the builder serialises their part
/// nodes when `max_chunk_slots <= 1`, so even a cloned worker runs them one at
/// a time in part-number order.
fn clone_multipart_worker(provider: &dyn StorageProvider) -> Option<Box<dyn StorageProvider>> {
    provider.clone_for_transfer().ok()
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
            }
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
            self.multipart_part_started.fetch_add(1, Ordering::SeqCst);
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
    async fn cancel_token_aborts_in_flight_multipart_upload() {
        let dir = tempfile::tempdir().expect("tempdir");
        let local = dir.path().join("source.bin");
        std::fs::write(&local, b"0123456789abcdefghij").expect("write source");

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
            *multipart_aborted.lock().unwrap(),
            "cancelled multipart upload must abort the provider session"
        );
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
