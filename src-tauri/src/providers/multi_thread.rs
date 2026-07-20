// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Shared concurrent HTTP Range download primitive (Core DAG, PD-HTTP-1).
//!
//! This module is the convergence target for multi-thread range download.
//! Until PD-HTTP-1 it lived inline in `s3.rs` and accepted both `200 OK` and
//! `206 Partial Content`. The shared helper here is deliberately **strict**:
//! a concurrent range download is only attempted when the server proves it
//! honours `Range` with `206 Partial Content` and a `Content-Range` header
//! whose window exactly matches the request. A `200 OK` means the server
//! ignored `Range` and would stream the whole object on a single connection,
//! so the helper reports an honest single-stream fallback instead of silently
//! corrupting the file by writing a full body at a chunk offset.
//!
//! `plan_multi_thread_ranges` is pure, fully unit-tested and wired by S3's
//! native `download_multi_thread`. As of **PD-HTTP-2** the strict download
//! helper is no longer scaffold: [`try_http_concurrent_range_download`] is
//! the live entry point WebDAV and Koofr call from their `download()` path.
//! It performs a real strict 206 probe (a one-byte `Range` request), only
//! commits to the concurrent path when the server proves `206` +
//! `Content-Range` honesty, and otherwise hands an honest single-stream
//! fallback (with the progress callback returned unconsumed) back to the
//! caller. S3 deliberately keeps its native path: it is already
//! live-validated byte-identical and converging it here would risk
//! regressing a proven path for no measured gain (tracked, not done in
//! PD-HTTP-2; rev 3 principle: no overclaim, no churn on a validated path).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures_util::StreamExt;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use reqwest::header::{HeaderName, HeaderValue, RANGE};

use super::http_retry::{send_with_retry, HttpRetryConfig};
use super::ProviderError;

/// A boxed progress callback shared across node-graph runner invocations.
/// The `Box` is `Send` but not `Sync`; the `Arc<Mutex<_>>` is what makes the
/// capturing runner closure `Send + Sync` for the executor.
type SharedProgress = Arc<std::sync::Mutex<Option<Box<dyn Fn(u64, u64) + Send>>>>;

/// Plan contiguous, gap-free, non-overlapping byte ranges covering the whole
/// object. End offsets are **inclusive**, matching HTTP `Range: bytes=s-e`.
///
/// `max_streams` is the provider-specific hard cap (e.g. S3's 16) so the
/// planner stays provider-agnostic. Returns an empty plan for a zero-size
/// object or zero streams. If the object is smaller than the requested stream
/// count the plan collapses to fewer ranges of `>= 1` byte rather than
/// emitting zero-length entries.
pub(crate) fn plan_multi_thread_ranges(
    total_size: u64,
    streams: usize,
    max_streams: usize,
) -> Vec<(u64, u64)> {
    if total_size == 0 || streams == 0 || max_streams == 0 {
        return Vec::new();
    }
    let streams = streams.clamp(1, max_streams) as u64;
    let effective = streams.min(total_size);
    let base = total_size / effective;
    let remainder = total_size % effective;

    let mut ranges = Vec::with_capacity(effective as usize);
    let mut offset = 0u64;
    for i in 0..effective {
        let len = base + if i < remainder { 1 } else { 0 };
        if len == 0 {
            continue;
        }
        ranges.push((offset, offset + len - 1));
        offset += len;
    }
    ranges
}

/// Plan fixed-size, contiguous, gap-free parts covering a whole local file
/// for a multipart / large-file upload. Returns `(part_number, offset, len)`
/// with `part_number` 1-based. Only the **last** part may be shorter than the
/// effective part size, exactly matching the S3/B2 multipart contract (every
/// non-final part must be >= the protocol minimum).
///
/// `part_size` is the caller's preferred part size; `max_parts` is the
/// protocol hard cap (S3 and B2 both cap a multipart upload at 10000 parts).
/// If the file is large enough that `ceil(total / part_size)` would exceed
/// `max_parts`, the effective part size is grown to the smallest value that
/// still fits within `max_parts`, so the plan never exceeds the protocol
/// limit. This is the adaptive-part-size behaviour every real multipart
/// client needs and is what lets a single conservative default part size
/// scale up to multi-terabyte files. Returns an empty plan for a zero-size
/// file, a zero part size, or a zero cap (the caller falls back to its
/// single-stream PUT).
pub(crate) fn plan_fixed_size_parts(
    total_size: u64,
    part_size: u64,
    max_parts: u32,
) -> Vec<(u32, u64, u64)> {
    if total_size == 0 || part_size == 0 || max_parts == 0 {
        return Vec::new();
    }
    let max_parts = max_parts as u64;
    let effective = if total_size.div_ceil(part_size) > max_parts {
        total_size.div_ceil(max_parts)
    } else {
        part_size
    };
    let count = total_size.div_ceil(effective);
    let mut parts = Vec::with_capacity(count as usize);
    let mut offset = 0u64;
    let mut part_number = 1u32;
    while offset < total_size {
        let len = effective.min(total_size - offset);
        parts.push((part_number, offset, len));
        offset += len;
        part_number += 1;
    }
    parts
}

/// Parse an HTTP `Content-Range: bytes start-end/total` header.
///
/// Returns `(start, end, total)` where `total` is `None` for the `*`
/// (unknown-length) form. Returns `None` on any deviation from the
/// `bytes <start>-<end>/<total|*>` grammar: the strict gate treats a
/// malformed header on a `206` as a hard failure, never a soft accept.
pub(crate) fn parse_content_range(header: &str) -> Option<(u64, u64, Option<u64>)> {
    let rest = header.trim().strip_prefix("bytes ")?;
    let (range_part, total_part) = rest.split_once('/')?;
    let (start_str, end_str) = range_part.split_once('-')?;
    let start: u64 = start_str.trim().parse().ok()?;
    let end: u64 = end_str.trim().parse().ok()?;
    if end < start {
        return None;
    }
    let total = match total_part.trim() {
        "*" => None,
        other => Some(other.parse::<u64>().ok()?),
    };
    Some((start, end, total))
}

/// True when a `206` response's `Content-Range` exactly covers the requested
/// inclusive `[want_start, want_end]` window. Any mismatch (wrong offset,
/// short window, missing/garbled header) fails the strict gate.
pub(crate) fn content_range_matches(
    content_range: Option<&str>,
    want_start: u64,
    want_end: u64,
) -> bool {
    match content_range.and_then(parse_content_range) {
        Some((start, end, _)) => start == want_start && end == want_end,
        None => false,
    }
}

/// Outcome of an attempted concurrent range download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConcurrentRangeOutcome {
    /// Every range came back as a strict `206` with a coherent
    /// `Content-Range`; the temp file is fully written.
    Completed,
    /// The server answered `200 OK` (ignored `Range`). No partial bytes were
    /// committed; the caller must fall back to a single-stream download.
    ServerIgnoredRange,
}

/// Configuration for [`download_via_concurrent_range`].
pub struct ConcurrentRangeConfig {
    /// Final destination path (used only to derive the `.aerotmp` sibling).
    pub final_path: PathBuf,
    pub provider_type: super::ProviderType,
    /// Canonical remote authority held for the entire segmented job. Keeping
    /// this at job scope avoids multiplying endpoint leases by every range.
    pub endpoint_identity: crate::transfer_dag::EndpointIdentity,
    pub total_size: u64,
    pub streams: usize,
    pub max_streams: usize,
    /// Max range requests in flight at once.
    pub max_parallel: usize,
}

/// RAII guard: removes the `.aerotmp` file on drop unless `commit()` is
/// called. Guarantees no partial temp survives an error, a fallback, or a
/// cancellation (the F-1 lesson: every new transfer path is cancellable and
/// leaves no debris).
struct TempFileGuard {
    path: PathBuf,
    committed: bool,
}

impl TempFileGuard {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            committed: false,
        }
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Compute the `.aerotmp` sibling of `final_path`, matching the convention
/// used by `AtomicFile::temp_path_for` so existing cleanup tooling and the
/// resume path stay consistent.
pub fn aerotmp_path_for(final_path: &Path) -> PathBuf {
    let mut p = final_path.as_os_str().to_owned();
    p.push(".aerotmp");
    PathBuf::from(p)
}

/// Transport-agnostic concurrent range download orchestrator (PD-SFTP-2).
///
/// This is the single shared engine for splitting a large file into N
/// gap-free windows downloaded in parallel into a pre-allocated `.aerotmp`.
/// Everything that is *not* transport-specific lives here exactly once: the
/// range plan, the temp pre-allocation, the [`TempFileGuard`] RAII cleanup
/// (the F-1 lesson), bounded concurrency, progress aggregation, cooperative
/// cancellation, and the commit/atomic-ready temp.
///
/// `write_one_range(start, end, temp_path, aggregate, cancel)` is the only
/// transport-specific part. It must write **exactly** the inclusive
/// `[start, end]` window at its absolute offset into `temp_path`, bump
/// `aggregate` by the bytes it wrote, honour `cancel`, and return:
/// - [`ConcurrentRangeOutcome::Completed`] on success;
/// - [`ConcurrentRangeOutcome::ServerIgnoredRange`] iff the transport proves
///   the server ignored `Range` (HTTP `200 OK`); SFTP never returns this
///   because a short read is a hard error, not a fallback signal;
/// - `Err` on any hard failure (the strict gate: HTTP = `206` +
///   `Content-Range`; SFTP = exactly `end - start + 1` bytes, no silent
///   short read).
pub async fn run_concurrent_range_download<W, WFut>(
    config: ConcurrentRangeConfig,
    write_one_range: W,
    cancel: CancellationToken,
    on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
) -> Result<ConcurrentRangeOutcome, ProviderError>
where
    W: Fn(u64, u64, PathBuf, Arc<AtomicU64>, CancellationToken) -> WFut + Send + Sync + 'static,
    WFut: std::future::Future<Output = Result<ConcurrentRangeOutcome, ProviderError>>
        + Send
        + 'static,
{
    let ranges = plan_multi_thread_ranges(config.total_size, config.streams, config.max_streams);
    if ranges.is_empty() {
        return Err(ProviderError::TransferFailed(
            "Concurrent range download: empty range plan".to_string(),
        ));
    }

    let temp_path = aerotmp_path_for(&config.final_path);
    // DAG-P2-01b: a segmented transfer remains one foreground job for the
    // global governor. The lease covers pre-allocation, every range and the
    // final atomic commit; range fan-out must not multiply endpoint permits.
    let _governor_lease = crate::transfer_dag::governor::global()
        .acquire_job(
            config.endpoint_identity.clone(),
            crate::transfer_dag::TransferPriority::Foreground,
            [crate::transfer_dag::DiskLeaseRequest::write(&temp_path)],
        )
        .await;
    if let Some(parent) = config.final_path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(ProviderError::IoError)?;
        }
    }

    // Pre-allocate so concurrent seek+writes never race on file extension.
    {
        let f = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp_path)
            .await
            .map_err(ProviderError::IoError)?;
        f.set_len(config.total_size)
            .await
            .map_err(ProviderError::IoError)?;
    }
    let mut guard = TempFileGuard::new(temp_path.clone());

    let write_one_range = Arc::new(write_one_range);
    let total_size = config.total_size;

    // DAG-P1-06: the shaped range graph is the single production scheduler.
    // The equivalence harness below keeps the former JoinSet implementation
    // test-only so byte, outcome, error, progress, cancellation and cap parity
    // remain regression-gated without retaining a production escape hatch.
    // The per-run DAG summary (DAG-P2-07 timing/byte telemetry) is available
    // to tests through `run_ranges_via_graph`; this transport-agnostic shell
    // has no metrics surface of its own, so it is dropped here.
    let (outcome, _dag_summary) = run_ranges_via_graph(
        &ranges,
        &temp_path,
        total_size,
        config.max_parallel,
        config.provider_type,
        config.endpoint_identity.clone(),
        write_one_range,
        cancel,
        on_progress,
    )
    .await?;

    match outcome {
        ConcurrentRangeOutcome::Completed => {
            guard.commit();
            Ok(ConcurrentRangeOutcome::Completed)
        }
        ConcurrentRangeOutcome::ServerIgnoredRange => {
            Ok(ConcurrentRangeOutcome::ServerIgnoredRange)
        }
    }
}

/// Test-only reference scheduler for the DAG-P1-06 equivalence matrix.
///
/// This is the former shipped JoinSet implementation, retained outside
/// production solely to regression-gate the promoted graph runner. The caller
/// owns the [`TempFileGuard`]; this returns the outcome only.
#[cfg(test)]
pub(crate) async fn run_ranges_via_joinset<W, WFut>(
    ranges: &[(u64, u64)],
    temp_path: &Path,
    total_size: u64,
    max_parallel: usize,
    write_one_range: Arc<W>,
    cancel: CancellationToken,
    on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
) -> Result<ConcurrentRangeOutcome, ProviderError>
where
    W: Fn(u64, u64, PathBuf, Arc<AtomicU64>, CancellationToken) -> WFut + Send + Sync + 'static,
    WFut: std::future::Future<Output = Result<ConcurrentRangeOutcome, ProviderError>>
        + Send
        + 'static,
{
    let aggregate = Arc::new(AtomicU64::new(0));
    let semaphore = Arc::new(Semaphore::new(max_parallel.max(1)));
    let mut join_set: JoinSet<Result<ConcurrentRangeOutcome, ProviderError>> = JoinSet::new();

    for &(start, end) in ranges {
        let semaphore = semaphore.clone();
        let write_one_range = write_one_range.clone();
        let aggregate = aggregate.clone();
        let temp_path = temp_path.to_path_buf();
        let cancel = cancel.clone();
        join_set.spawn(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .map_err(|_| ProviderError::TransferFailed("range scheduler closed".to_string()))?;
            if cancel.is_cancelled() {
                return Err(ProviderError::TransferFailed(
                    "Transfer cancelled by user".to_string(),
                ));
            }
            write_one_range(start, end, temp_path, aggregate, cancel).await
        });
    }

    while let Some(joined) = join_set.join_next().await {
        let task_result = match joined {
            Ok(r) => r,
            Err(e) => {
                cancel.cancel();
                join_set.shutdown().await;
                return Err(ProviderError::TransferFailed(format!(
                    "Range task panicked: {}",
                    e
                )));
            }
        };
        match task_result {
            Ok(ConcurrentRangeOutcome::Completed) => {
                if let Some(cb) = on_progress.as_ref() {
                    cb(aggregate.load(Ordering::Relaxed), total_size);
                }
            }
            Ok(ConcurrentRangeOutcome::ServerIgnoredRange) => {
                // Honest fallback: stop the other tasks, drop the temp, let
                // the caller do a single-stream download.
                cancel.cancel();
                join_set.shutdown().await;
                return Ok(ConcurrentRangeOutcome::ServerIgnoredRange);
            }
            Err(e) => {
                cancel.cancel();
                join_set.shutdown().await;
                return Err(e);
            }
        }
    }

    Ok(ConcurrentRangeOutcome::Completed)
}

/// DAG-P2-06: the single construction point for the segmented-range AIMD
/// controller. It binds learned tuning to `endpoint` under the
/// [`crate::transfer_dag::AdaptiveWorkload::SegmentedRange`] shape. Taking the
/// registry as a parameter lets the wire-level test inject a fresh instance and
/// assert the exact key this site records under; production passes the
/// process-global registry via [`crate::transfer_dag::global_profile_registry`].
fn range_aimd_controller(
    budget: &crate::transfer_dag::TransferBudget,
    provider_type: super::ProviderType,
    endpoint: crate::transfer_dag::EndpointIdentity,
    registry: Arc<crate::transfer_dag::AdaptiveProfileRegistry>,
    config: crate::transfer_dag::AimdConfig,
) -> Arc<crate::transfer_dag::AimdController> {
    use crate::transfer_dag::{AdaptiveProfileKey, AdaptiveWorkload, AimdController};
    Arc::new(AimdController::from_budget_for_profile(
        budget,
        Some(provider_type),
        AdaptiveProfileKey::new(endpoint, AdaptiveWorkload::SegmentedRange),
        registry,
        config,
    ))
}

/// Dispatch the same range plan as independent fan-out nodes on the transfer
/// node-graph executor (`transfer_dag::executor::execute_dag`). Each range is
/// one `DownloadRange` node with no dependencies and a `range_chunk` resource
/// request; the resource budget is sized so effective concurrency equals
/// `max_parallel`, exactly mirroring the JoinSet semaphore. The temp file is
/// pre-allocated by the caller, so the only real dependency is satisfied
/// before the graph runs and a Plan/Preallocate node would add nothing but a
/// serial point: the graph is an honest pure fan-out. Observer is no-op; the
/// AIMD controller (F3-T05) starts every class at its honest ceiling, so a
/// congestion-free download dispatches identically to the prior `None` and
/// only shrinks the in-flight range set under a real congestion signal.
/// Outcome, error variant and cancel semantics are regression-gated against
/// the test-only legacy scheduler; only the internal scheduling differs.
///
/// Returns the executor summary alongside the outcome (DAG-P2-07): its
/// metrics carry the executor timing attribution, and on a completed run the
/// byte triple is populated here (logical == wire == local == `total_size`,
/// since every range verified its exact window into the local temp file).
/// `ServerIgnoredRange` commits no bytes, so its metrics stay zeroed.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_ranges_via_graph<W, WFut>(
    ranges: &[(u64, u64)],
    temp_path: &Path,
    total_size: u64,
    max_parallel: usize,
    provider_type: super::ProviderType,
    endpoint: crate::transfer_dag::EndpointIdentity,
    write_one_range: Arc<W>,
    cancel: CancellationToken,
    on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
) -> Result<
    (
        ConcurrentRangeOutcome,
        crate::transfer_dag::executor::DagExecutionSummary,
    ),
    ProviderError,
>
where
    W: Fn(u64, u64, PathBuf, Arc<AtomicU64>, CancellationToken) -> WFut + Send + Sync + 'static,
    WFut: std::future::Future<Output = Result<ConcurrentRangeOutcome, ProviderError>>
        + Send
        + 'static,
{
    use crate::transfer_dag::executor::{
        execute_dag_with_options, DagExecuteOptions, DagNodeRunner, NodeFuture, NodeOutcome,
    };
    use crate::transfer_dag::graph::TransferNode;
    use crate::transfer_dag::{
        AimdConfig, DagObserver, FailureScope, NoopDagObserver, TransferBudget, TransferDagBuilder,
        TransferError,
    };
    use std::sync::atomic::AtomicBool;
    use std::sync::Mutex;

    // The segmented-download shape is fixed and capability-irrelevant
    // (every range is independent and reserves one `range_chunk` resource),
    // so the shaped builder is a thin pass-through. Routing through it
    // makes [`TransferDagBuilder`] the single source of truth for every
    // production graph shape: SG-T19 collapses the manual node
    // construction here now that the flag-gated legacy production path is
    // gone.
    let shaped = TransferDagBuilder::shaped_ranges(ranges.len());
    let dag = shaped.dag;

    // Size the controlled classes so up to `max_parallel` range_chunk nodes
    // run at once: identical effective concurrency to the JoinSet semaphore
    // (`range_chunk` requests chunk+http+disk_write = 1 each; buffer_bytes=0
    // because the active runner streams response chunks and does not own a
    // full-range allocation — do not invent full-range byte accounting here).
    let slots = max_parallel.max(1).min(u16::MAX as usize) as u16;
    // DAG-P2-01: the buffer-byte pool is the process-global governor's, shared
    // with every concurrent range job; the slot classes stay per-job.
    let manager = crate::transfer_dag::governor::global().child_manager(TransferBudget {
        chunk_slots: slots,
        http_slots: slots,
        disk_write_slots: slots,
        disk_read_slots: 1,
        ..TransferBudget::from_file_slots(1)
    });

    // AIMD backpressure (F3-T05). The controller's per-class ceilings are the
    // budget above, and every class starts at its ceiling: a download with no
    // congestion dispatches every range immediately, identical to the prior
    // `None`. It only ever shrinks the Chunk/Http dispatch target when a range
    // fails with a genuine congestion signal (429/503/timeout/reset), where a
    // smaller in-flight set is the safer, faster choice.
    let aimd = range_aimd_controller(
        &manager.budget(),
        provider_type,
        endpoint,
        crate::transfer_dag::global_profile_registry(),
        AimdConfig::runtime(),
    );

    let ranges: Arc<Vec<(u64, u64)>> = Arc::new(ranges.to_vec());
    let aggregate = Arc::new(AtomicU64::new(0));
    let temp_path: Arc<PathBuf> = Arc::new(temp_path.to_path_buf());
    let server_ignored = Arc::new(AtomicBool::new(false));
    let first_error: Arc<Mutex<Option<ProviderError>>> = Arc::new(Mutex::new(None));
    let progress: SharedProgress = Arc::new(Mutex::new(on_progress));

    let r_ranges = ranges.clone();
    let r_write = write_one_range.clone();
    let r_aggregate = aggregate.clone();
    let r_temp = temp_path.clone();
    let r_cancel = cancel.clone();
    let r_ignored = server_ignored.clone();
    let r_first_error = first_error.clone();
    let r_progress = progress.clone();

    let runner: Arc<dyn DagNodeRunner> = Arc::new(move |node: TransferNode| -> NodeFuture {
        let ranges = r_ranges.clone();
        let write_one_range = r_write.clone();
        let aggregate = r_aggregate.clone();
        let temp_path = r_temp.clone();
        let cancel = r_cancel.clone();
        let server_ignored = r_ignored.clone();
        let first_error = r_first_error.clone();
        let progress = r_progress.clone();
        Box::pin(async move {
            let (start, end) = ranges[node.id];
            // Mirror the JoinSet path's pre-write cancel check.
            if cancel.is_cancelled() {
                return NodeOutcome::Failed(
                    TransferError::cancelled().with_scope(FailureScope::Part),
                );
            }
            match write_one_range(
                start,
                end,
                (*temp_path).clone(),
                aggregate.clone(),
                cancel.clone(),
            )
            .await
            {
                Ok(ConcurrentRangeOutcome::Completed) => {
                    if let Some(cb) = progress.lock().unwrap().as_ref() {
                        cb(aggregate.load(Ordering::Relaxed), total_size);
                    }
                    NodeOutcome::Completed
                }
                Ok(ConcurrentRangeOutcome::ServerIgnoredRange) => {
                    // Honest fallback: cancel siblings; the caller skips the
                    // commit so the temp is removed, exactly like JoinSet.
                    // Parent cancel also fires the graph child token so the
                    // executor fail-fast path terminates residual ranges.
                    server_ignored.store(true, Ordering::SeqCst);
                    cancel.cancel();
                    NodeOutcome::Fallback
                }
                Err(e) => {
                    // ProviderError is not Clone (IoError wraps io::Error):
                    // lift a typed TransferError for the DAG/AIMD path, then
                    // move the owned error into the first-error slot to
                    // preserve the exact variant for the caller. Scope is
                    // Part: each range node is one independent byte range.
                    let typed = TransferError::from_provider(&e).with_scope(FailureScope::Part);
                    {
                        let mut slot = first_error.lock().unwrap();
                        if slot.is_none() {
                            *slot = Some(e);
                        }
                    }
                    // Cancel parent so JoinSet-parity sibling abort and the
                    // graph child token both fire; executor fail-fast is the
                    // hard bound (<2s).
                    cancel.cancel();
                    NodeOutcome::Failed(typed)
                }
            }
        })
    });

    // Parent cancel token is the caller's; the executor creates a graph child
    // for fail-fast. Default node_timeout is None (no arbitrary cut-off).
    let exec = execute_dag_with_options(
        &dag,
        &manager,
        runner,
        Arc::new(NoopDagObserver) as Arc<dyn DagObserver>,
        Some(aimd),
        DagExecuteOptions {
            parent_cancel: Some(cancel),
            ..DagExecuteOptions::default()
        },
    )
    .await;

    // Order matters: a ServerIgnoredRange cancels the token, so sibling nodes
    // then fail with "cancelled" and execute_dag returns NodeFailed. Check the
    // fallback flag before treating that as an error.
    if server_ignored.load(Ordering::SeqCst) {
        // No bytes are committed on the honest fallback; a cancelled sibling
        // drain may have turned the summary into an Err, which carries no
        // telemetry worth keeping here.
        let summary = exec.unwrap_or_default();
        return Ok((ConcurrentRangeOutcome::ServerIgnoredRange, summary));
    }
    match exec {
        Ok(mut summary) => {
            // Every range verified its exact window into the local temp, so
            // the whole object crossed the wire once and fully materialized
            // locally.
            summary.metrics.logical_bytes = total_size;
            summary.metrics.wire_bytes = total_size;
            summary.metrics.local_payload_bytes = total_size;
            summary.metrics.bytes_transferred = total_size;
            Ok((ConcurrentRangeOutcome::Completed, summary))
        }
        Err(dag_err) => {
            if let Some(e) = first_error.lock().unwrap().take() {
                Err(e)
            } else {
                Err(ProviderError::TransferFailed(dag_err.to_string()))
            }
        }
    }
}

/// Configuration for [`run_concurrent_part_upload`].
pub struct ConcurrentPartUploadConfig {
    /// Source file on disk; parts are read from here at their offsets.
    pub local_path: PathBuf,
    pub total_size: u64,
    /// Preferred part size; grown automatically if `max_parts` would be
    /// exceeded (see [`plan_fixed_size_parts`]).
    pub part_size: u64,
    /// Protocol hard cap on the number of parts (S3 and B2: 10000).
    pub max_parts: u32,
    /// Max part uploads in flight at once. `1` collapses to a sequential
    /// upload through the same path (no second engine, no special case).
    pub max_parallel: usize,
}

/// Transport-agnostic concurrent multipart-upload orchestrator (PD-UP-1).
///
/// The upload-side mirror of [`run_concurrent_range_download`]: the single
/// shared engine that plans fixed-size parts over a local file, reads each
/// part from disk at its offset, and uploads parts with bounded concurrency
/// in a true sliding window (a freed slot starts the next part immediately,
/// never a per-batch barrier). Everything that is *not* transport-specific
/// lives here exactly once: the part plan, the per-part disk read, bounded
/// concurrency, progress aggregation, cooperative cancellation, and
/// abort-all-siblings-on-first-error (a doomed upload must not keep burning
/// bandwidth and per-request billing).
///
/// `upload_one_part(part_number, data)` is the only transport-specific part.
/// It must upload `data` as part `part_number` and return the provider's part
/// identifier (S3 `ETag`, B2 part `contentSha1`) needed for the finalising
/// "complete/finish" call. The engine returns the identifiers **sorted by
/// part number**, ready for that call. The engine never creates, finalises,
/// or aborts the multipart session itself: that lifecycle is provider
/// specific and stays with the caller, so an `Err` here leaves the caller to
/// run its existing abort/cleanup exactly as before (no behaviour change on
/// the failure path).
///
/// The caller owns the decision of whether to call this at all (its own size
/// threshold). Below that threshold it keeps its single-stream PUT untouched:
/// honest fallback, no overclaim. S3 deliberately keeps its native multipart
/// path: it is already live-validated byte-identical and converging it here
/// would risk regressing a proven path for no measured gain (tracked, not
/// done in PD-UP-1; the same rev-3 principle the download side applied to
/// S3's native range path: no churn on a validated path).
pub async fn run_concurrent_part_upload<U, UFut>(
    cfg: ConcurrentPartUploadConfig,
    upload_one_part: U,
    cancel: CancellationToken,
    on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
) -> Result<Vec<(u32, String)>, ProviderError>
where
    U: Fn(u32, Vec<u8>) -> UFut + Send + Sync + 'static,
    UFut: std::future::Future<Output = Result<String, ProviderError>> + Send + 'static,
{
    let parts = plan_fixed_size_parts(cfg.total_size, cfg.part_size, cfg.max_parts);
    if parts.is_empty() {
        return Err(ProviderError::TransferFailed(
            "Concurrent part upload: empty part plan".to_string(),
        ));
    }

    let aggregate = Arc::new(AtomicU64::new(0));
    let semaphore = Arc::new(Semaphore::new(cfg.max_parallel.max(1)));
    let upload_one_part = Arc::new(upload_one_part);
    let local_path = Arc::new(cfg.local_path);
    let total_size = cfg.total_size;
    let mut join_set: JoinSet<Result<(u32, String), ProviderError>> = JoinSet::new();

    for (part_number, offset, len) in parts {
        let semaphore = semaphore.clone();
        let upload_one_part = upload_one_part.clone();
        let aggregate = aggregate.clone();
        let local_path = local_path.clone();
        let cancel = cancel.clone();
        join_set.spawn(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .map_err(|_| ProviderError::TransferFailed("part scheduler closed".to_string()))?;
            if cancel.is_cancelled() {
                return Err(ProviderError::TransferFailed(
                    "Transfer cancelled by user".to_string(),
                ));
            }
            // Read this part only after a slot is free, so peak RAM is bounded
            // to `max_parallel * part_size`, never the whole file.
            let data = read_part_from_disk(&local_path, offset, len).await?;
            let part_id = tokio::select! {
                _ = cancel.cancelled() => {
                    return Err(ProviderError::TransferFailed(
                        "Transfer cancelled by user".to_string(),
                    ));
                }
                r = upload_one_part(part_number, data) => r?,
            };
            aggregate.fetch_add(len, Ordering::Relaxed);
            Ok((part_number, part_id))
        });
    }

    let mut completed: Vec<(u32, String)> = Vec::new();
    while let Some(joined) = join_set.join_next().await {
        let task_result = match joined {
            Ok(r) => r,
            Err(e) => {
                cancel.cancel();
                join_set.shutdown().await;
                return Err(ProviderError::TransferFailed(format!(
                    "Part task panicked: {}",
                    e
                )));
            }
        };
        match task_result {
            Ok((part_number, part_id)) => {
                completed.push((part_number, part_id));
                if let Some(cb) = on_progress.as_ref() {
                    cb(aggregate.load(Ordering::Relaxed), total_size);
                }
            }
            Err(e) => {
                cancel.cancel();
                join_set.shutdown().await;
                return Err(e);
            }
        }
    }

    completed.sort_by_key(|(pn, _)| *pn);
    Ok(completed)
}

/// Read exactly `len` bytes at `offset` from the source file into a fresh
/// buffer. Each part gets its own file handle so concurrent reads never share
/// a cursor (the read-side mirror of the per-task handle the download engine
/// opens to write its window).
///
/// **Residual non-DAG debt (DAG-P0-06):** this legacy concurrent-upload helper
/// still allocates full-part buffers outside `TransferResourceManager` byte
/// credits. Peak RAM is bounded only by `max_parallel * part_size` via the
/// local semaphore. Do not treat P0-06 as process-wide complete while this
/// path remains. Wire into the same credit abstraction only when this
/// scheduler moves fully onto the DAG (or a shared credit helper is extracted
/// for both paths). DAG-P2-05 added streaming part bodies (`PartBody`) for
/// providers whose part upload is a single send without whole-part hashing; its
/// only caller here is B2, which must hold the whole part in memory to compute
/// the `x-bz-content-sha1` header before the send, so this path stays owned by
/// necessity and is out of scope for that streaming reuse.
async fn read_part_from_disk(path: &Path, offset: u64, len: u64) -> Result<Vec<u8>, ProviderError> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(ProviderError::IoError)?;
    file.seek(std::io::SeekFrom::Start(offset))
        .await
        .map_err(ProviderError::IoError)?;
    let mut buf = vec![0u8; len as usize];
    let mut filled = 0usize;
    while filled < buf.len() {
        let n = file
            .read(&mut buf[filled..])
            .await
            .map_err(ProviderError::IoError)?;
        if n == 0 {
            return Err(ProviderError::TransferFailed(format!(
                "Concurrent part upload: source truncated at offset {} (wanted {} bytes, got {})",
                offset, len, filled
            )));
        }
        filled += n;
    }
    Ok(buf)
}

/// Strict concurrent HTTP Range download.
///
/// `fetch_range(start, end)` must issue a single `Range: bytes=start-end`
/// request and return the raw `reqwest::Response`. The helper enforces the
/// strict gate, writes each window at its absolute offset in a pre-allocated
/// `.aerotmp`, aggregates progress, and cleans up on every non-success exit.
///
/// Returns [`ConcurrentRangeOutcome::ServerIgnoredRange`] (no bytes
/// committed, temp removed) the moment any task observes `200 OK`, so the
/// caller can perform an honest single-stream download instead.
///
/// This is now a thin HTTP adapter over [`run_concurrent_range_download`]:
/// the orchestration is shared with SFTP intra-file, the HTTP strict 206
/// gate stays here in [`download_one_strict_range`]. Behaviour is
/// byte-identical to the pre-PD-SFTP-2 inline version (the live-validated
/// WebDAV/Koofr path is unchanged).
pub(crate) async fn download_via_concurrent_range<F, Fut>(
    cfg: ConcurrentRangeConfig,
    fetch_range: F,
    cancel: CancellationToken,
    on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
) -> Result<ConcurrentRangeOutcome, ProviderError>
where
    F: Fn(u64, u64) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<reqwest::Response, ProviderError>> + Send + 'static,
{
    let fetch_range = Arc::new(fetch_range);
    let write_one_range = move |start: u64,
                                end: u64,
                                temp_path: PathBuf,
                                aggregate: Arc<AtomicU64>,
                                cancel: CancellationToken| {
        let fetch_range = fetch_range.clone();
        async move {
            download_one_strict_range(
                fetch_range.as_ref(),
                &temp_path,
                start,
                end,
                &aggregate,
                &cancel,
            )
            .await
        }
    };
    run_concurrent_range_download(cfg, write_one_range, cancel, on_progress).await
}

/// Download exactly one `[start, end]` window under the strict gate.
async fn download_one_strict_range<F, Fut>(
    fetch_range: &F,
    temp_path: &Path,
    start: u64,
    end: u64,
    aggregate: &AtomicU64,
    cancel: &CancellationToken,
) -> Result<ConcurrentRangeOutcome, ProviderError>
where
    F: Fn(u64, u64) -> Fut,
    Fut: std::future::Future<Output = Result<reqwest::Response, ProviderError>>,
{
    let response = fetch_range(start, end).await?;
    let status = response.status();

    if status == reqwest::StatusCode::OK {
        // Server ignored Range entirely. Do not write a full body at a chunk
        // offset: signal the honest single-stream fallback.
        return Ok(ConcurrentRangeOutcome::ServerIgnoredRange);
    }
    if status != reqwest::StatusCode::PARTIAL_CONTENT {
        if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
            return Err(ProviderError::NotSupported(
                "Server rejected Range request mid-flight (file may have changed)".to_string(),
            ));
        }
        return Err(ProviderError::TransferFailed(format!(
            "Concurrent range download failed with status: {}",
            status
        )));
    }

    // Strict 206: the Content-Range must exactly cover the requested window.
    let content_range = response
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    if !content_range_matches(content_range.as_deref(), start, end) {
        return Err(ProviderError::TransferFailed(format!(
            "Strict Range gate: 206 with incoherent Content-Range {:?} for bytes={}-{}",
            content_range, start, end
        )));
    }

    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(temp_path)
        .await
        .map_err(ProviderError::IoError)?;
    file.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(ProviderError::IoError)?;

    let expected = end - start + 1;
    let mut written: u64 = 0;
    let mut stream = response.bytes_stream();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                return Err(ProviderError::TransferFailed(
                    "Transfer cancelled by user".to_string(),
                ));
            }
            next = stream.next() => {
                let Some(chunk) = next else { break };
                let chunk = chunk.map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
                let chunk_len = chunk.len() as u64;
                if written + chunk_len > expected {
                    let allowed = (expected - written) as usize;
                    file.write_all(&chunk[..allowed])
                        .await
                        .map_err(ProviderError::IoError)?;
                    aggregate.fetch_add(allowed as u64, Ordering::Relaxed);
                    written = expected;
                    break;
                }
                file.write_all(&chunk)
                    .await
                    .map_err(ProviderError::IoError)?;
                aggregate.fetch_add(chunk_len, Ordering::Relaxed);
                written += chunk_len;
            }
        }
    }

    if written != expected {
        return Err(ProviderError::TransferFailed(format!(
            "Concurrent range download truncated: expected {} bytes, got {}",
            expected, written
        )));
    }

    file.flush().await.map_err(ProviderError::IoError)?;
    file.sync_all().await.map_err(ProviderError::IoError)?;
    Ok(ConcurrentRangeOutcome::Completed)
}

/// A stateless-HTTP provider's request recipe for concurrent Range download.
///
/// `headers` are the *static* per-request headers (e.g. a precomputed Basic
/// `Authorization`, an API version header). They must be safe to replay
/// concurrently: a WebDAV server using **Digest** auth mutates a per-request
/// nonce counter and is therefore excluded by the caller, never represented
/// here. No credential is reconstructed: the caller hands the live client and
/// the already-derived header values.
pub(crate) struct HttpRangeRequest {
    pub client: reqwest::Client,
    pub url: String,
    pub headers: Vec<(HeaderName, HeaderValue)>,
    pub local_path: String,
    pub provider_type: super::ProviderType,
    pub streams: usize,
    pub max_streams: usize,
    pub cutoff: u64,
}

/// Result of [`try_http_concurrent_range_download`].
pub(crate) enum HttpRangeAttempt {
    /// Concurrent Range download finished and the file is at `local_path`.
    Completed,
    /// The provider must run its normal single-stream download. The progress
    /// callback is handed back unconsumed when the strict probe declined
    /// before the concurrent helper took ownership (`Some`); it is `None`
    /// only on the rare post-probe inconsistency where the server honoured
    /// the probe but then ignored a real Range mid-flight.
    Fallback(Option<Box<dyn Fn(u64, u64) + Send>>),
    /// Hard failure from the concurrent path (already past a good probe).
    Failed(ProviderError),
}

/// Strict 206 probe + concurrent Range download for a stateless-HTTP
/// provider, with an honest single-stream fallback.
///
/// The probe is a single `Range: bytes=0-0` GET. The concurrent path is taken
/// **only** when the server answers `206` with a `Content-Range` that exactly
/// covers `0-0` and advertises a known total `>=` the multi-thread cutoff.
/// Any other outcome (a `200` that ignored Range, an unknown total, a
/// sub-cutoff file, a transport hiccup) yields `Fallback`, so the provider's
/// own retry-hardened single-stream path stays the single source of truth for
/// the canonical error. A `200` is logged as a `range_ignored` event
/// (observability hook for PD-ADAPT-1 metrics).
pub(crate) async fn try_http_concurrent_range_download(
    req: HttpRangeRequest,
    on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
) -> HttpRangeAttempt {
    if req.streams < 2 || req.max_streams == 0 {
        return HttpRangeAttempt::Fallback(on_progress);
    }

    let retry_cfg = HttpRetryConfig::default();

    // --- Strict 206 probe (one byte) -------------------------------------
    let probe_builder = {
        let mut rb = req.client.get(&req.url);
        for (k, v) in &req.headers {
            rb = rb.header(k.clone(), v.clone());
        }
        rb.header(RANGE, HeaderValue::from_static("bytes=0-0"))
    };
    let probe_req = match probe_builder.build() {
        Ok(r) => r,
        Err(_) => return HttpRangeAttempt::Fallback(on_progress),
    };
    let probe = match send_with_retry(&req.client, probe_req, &retry_cfg).await {
        Ok(r) => r,
        // An opportunistic probe must never be the error source: let the
        // provider's canonical path connect and report.
        Err(_) => return HttpRangeAttempt::Fallback(on_progress),
    };

    let status = probe.status();
    if status == reqwest::StatusCode::OK {
        tracing::warn!(
            "[multi-thread] range_ignored: server answered 200 OK to a Range probe for {}; single-stream fallback",
            req.url
        );
        return HttpRangeAttempt::Fallback(on_progress);
    }
    if status != reqwest::StatusCode::PARTIAL_CONTENT {
        // 416/4xx/5xx: defer to the canonical single-stream path.
        return HttpRangeAttempt::Fallback(on_progress);
    }

    let content_range = probe
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    if !content_range_matches(content_range.as_deref(), 0, 0) {
        tracing::warn!(
            "[multi-thread] 206 with incoherent Content-Range {:?} on probe for {}; single-stream fallback",
            content_range,
            req.url
        );
        return HttpRangeAttempt::Fallback(on_progress);
    }
    let total = match content_range
        .as_deref()
        .and_then(parse_content_range)
        .and_then(|(_, _, total)| total)
    {
        Some(t) if t > 0 && t >= req.cutoff => t,
        // Unknown length or below cutoff: not worth (or not safe to plan)
        // a concurrent split.
        _ => return HttpRangeAttempt::Fallback(on_progress),
    };

    // --- Concurrent path (server proved 206 honesty) ---------------------
    let parallel = req.streams.clamp(1, req.max_streams);
    let cfg = ConcurrentRangeConfig {
        final_path: PathBuf::from(&req.local_path),
        provider_type: req.provider_type,
        endpoint_identity: reqwest::Url::parse(&req.url)
            .ok()
            .and_then(|url| {
                url.host_str()
                    .map(|host| (url.scheme().to_string(), host.to_string()))
            })
            .map(|(protocol, host)| crate::transfer_dag::EndpointIdentity::new(protocol, host, ""))
            .unwrap_or_else(|| {
                crate::transfer_dag::EndpointIdentity::new(
                    req.provider_type.to_string(),
                    "http-range",
                    "",
                )
            }),
        total_size: total,
        streams: req.streams,
        max_streams: req.max_streams,
        max_parallel: parallel,
    };

    let client = req.client.clone();
    let url: Arc<str> = Arc::from(req.url.as_str());
    let headers = Arc::new(req.headers.clone());
    let fetch_range = move |start: u64, end: u64| {
        let client = client.clone();
        let url = url.clone();
        let headers = headers.clone();
        async move {
            let mut rb = client.get(url.as_ref());
            for (k, v) in headers.iter() {
                rb = rb.header(k.clone(), v.clone());
            }
            let range = HeaderValue::from_str(&format!("bytes={}-{}", start, end))
                .map_err(|e| ProviderError::TransferFailed(format!("Invalid Range: {}", e)))?;
            let request = rb
                .header(RANGE, range)
                .build()
                .map_err(|e| ProviderError::TransferFailed(format!("Build request: {}", e)))?;
            send_with_retry(&client, request, &HttpRetryConfig::default())
                .await
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))
        }
    };

    match download_via_concurrent_range(cfg, fetch_range, CancellationToken::new(), on_progress)
        .await
    {
        Ok(ConcurrentRangeOutcome::Completed) => {
            let temp = aerotmp_path_for(Path::new(&req.local_path));
            match tokio::fs::rename(&temp, &req.local_path).await {
                Ok(()) => HttpRangeAttempt::Completed,
                Err(e) => {
                    let _ = tokio::fs::remove_file(&temp).await;
                    HttpRangeAttempt::Failed(ProviderError::IoError(e))
                }
            }
        }
        Ok(ConcurrentRangeOutcome::ServerIgnoredRange) => {
            // Server honoured the probe then ignored a real Range: rare
            // inconsistency. The helper already removed the temp; the
            // progress callback was moved in, so the fallback runs without
            // it (degraded but correct).
            tracing::warn!(
                "[multi-thread] range_ignored mid-flight after a good probe for {}; single-stream fallback (no progress)",
                req.url
            );
            HttpRangeAttempt::Fallback(None)
        }
        Err(e) => HttpRangeAttempt::Failed(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX: usize = 16;

    // DAG-P2-06 (wire-level): the segmented-range construction point binds
    // learned tuning to its endpoint under the SegmentedRange workload key.
    #[test]
    fn range_aimd_controller_records_under_the_segmented_range_key() {
        use crate::transfer_dag::{
            AdaptiveClass, AdaptiveProfileConfig, AdaptiveProfileKey, AdaptiveProfileRegistry,
            AdaptiveWorkload, AimdConfig, EndpointIdentity, TransferBudget,
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
        let ctrl = range_aimd_controller(
            &budget,
            super::super::ProviderType::S3,
            endpoint.clone(),
            registry.clone(),
            AimdConfig::default(),
        );
        assert_eq!(
            ctrl.profile_key().map(|k| k.workload),
            Some(AdaptiveWorkload::SegmentedRange)
        );
        ctrl.on_congestion(AdaptiveClass::Chunk);
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(
            snapshot[0].key,
            AdaptiveProfileKey::new(endpoint, AdaptiveWorkload::SegmentedRange)
        );
        assert_eq!(snapshot[0].chunk, Some(4));
    }

    fn ranges_cover(total: u64, ranges: &[(u64, u64)]) -> bool {
        if ranges.is_empty() {
            return total == 0;
        }
        if ranges[0].0 != 0 {
            return false;
        }
        for w in ranges.windows(2) {
            if w[0].1 + 1 != w[1].0 {
                return false;
            }
        }
        ranges.last().unwrap().1 + 1 == total
    }

    #[test]
    fn plan_even_split_covers_whole_object() {
        let ranges = plan_multi_thread_ranges(1000, 4, MAX);
        assert_eq!(ranges.len(), 4);
        assert!(ranges_cover(1000, &ranges));
        assert_eq!(ranges[0], (0, 249));
    }

    #[test]
    fn plan_uneven_split_distributes_remainder_to_leading_ranges() {
        let ranges = plan_multi_thread_ranges(1003, 4, MAX);
        assert_eq!(ranges.len(), 4);
        assert!(ranges_cover(1003, &ranges));
        // 1003 / 4 = 250 r3 -> first three windows are 251 bytes, last 250.
        assert_eq!(ranges[0], (0, 250));
        assert_eq!(ranges[3].1, 1002);
    }

    #[test]
    fn plan_zero_size_or_zero_streams_or_zero_cap_is_empty() {
        assert!(plan_multi_thread_ranges(0, 4, MAX).is_empty());
        assert!(plan_multi_thread_ranges(1024, 0, MAX).is_empty());
        assert!(plan_multi_thread_ranges(1024, 4, 0).is_empty());
    }

    #[test]
    fn plan_caps_streams_to_max() {
        let ranges = plan_multi_thread_ranges(10_000_000, 999, MAX);
        assert!(ranges.len() <= MAX);
        assert!(ranges_cover(10_000_000, &ranges));
    }

    #[test]
    fn plan_collapses_when_streams_exceed_size() {
        let ranges = plan_multi_thread_ranges(3, 8, MAX);
        assert_eq!(ranges.len(), 3);
        assert!(ranges_cover(3, &ranges));
    }

    #[test]
    fn plan_single_stream_covers_whole_file() {
        let ranges = plan_multi_thread_ranges(12345, 1, MAX);
        assert_eq!(ranges, vec![(0, 12344)]);
    }

    /// Parts are 1-based, gap-free, contiguous, exactly cover `total`, and
    /// only the final part may be shorter than the effective part size.
    fn parts_well_formed(total: u64, parts: &[(u32, u64, u64)]) -> bool {
        if parts.is_empty() {
            return total == 0;
        }
        let mut expected_offset = 0u64;
        let effective = parts[0].2;
        for (i, &(pn, offset, len)) in parts.iter().enumerate() {
            if pn as usize != i + 1 || offset != expected_offset || len == 0 {
                return false;
            }
            let is_last = i + 1 == parts.len();
            if !is_last && len != effective {
                return false;
            }
            if is_last && len > effective {
                return false;
            }
            expected_offset += len;
        }
        expected_offset == total
    }

    #[test]
    fn parts_even_split_only_last_may_be_short() {
        let parts = plan_fixed_size_parts(1000, 250, 100);
        assert_eq!(parts.len(), 4);
        assert!(parts_well_formed(1000, &parts));
        assert_eq!(parts[0], (1, 0, 250));
        assert_eq!(parts[3], (4, 750, 250));
    }

    #[test]
    fn parts_remainder_lands_in_final_part() {
        let parts = plan_fixed_size_parts(1003, 250, 100);
        assert_eq!(parts.len(), 5);
        assert!(parts_well_formed(1003, &parts));
        assert_eq!(parts[4], (5, 1000, 3));
    }

    #[test]
    fn parts_zero_size_part_or_cap_is_empty() {
        assert!(plan_fixed_size_parts(0, 250, 100).is_empty());
        assert!(plan_fixed_size_parts(1000, 0, 100).is_empty());
        assert!(plan_fixed_size_parts(1000, 250, 0).is_empty());
    }

    #[test]
    fn parts_grow_part_size_to_respect_protocol_cap() {
        // 1000 / 10 = 100 parts would exceed the cap of 4: the planner must
        // grow the part size so the plan never breaches the protocol limit.
        let parts = plan_fixed_size_parts(1000, 10, 4);
        assert!(parts.len() <= 4);
        assert!(parts_well_formed(1000, &parts));
        assert_eq!(parts[0].2, 250);
    }

    #[test]
    fn parts_single_part_when_file_below_part_size() {
        let parts = plan_fixed_size_parts(4096, 100 * 1024, 10_000);
        assert_eq!(parts, vec![(1, 0, 4096)]);
    }

    #[test]
    fn parts_b2_realistic_250mib_keeps_non_final_above_min() {
        let mib = 1024 * 1024;
        let parts = plan_fixed_size_parts(250 * mib, 100 * mib, 10_000);
        assert_eq!(parts.len(), 3);
        assert!(parts_well_formed(250 * mib, &parts));
        // Every non-final part is well above the B2 5 MiB minimum.
        for &(_, _, len) in &parts[..parts.len() - 1] {
            assert!(len >= 5 * mib);
        }
        assert_eq!(parts[2].2, 50 * mib);
    }

    #[test]
    fn content_range_parses_canonical_form() {
        assert_eq!(
            parse_content_range("bytes 0-499/1234"),
            Some((0, 499, Some(1234)))
        );
        assert_eq!(
            parse_content_range("bytes 500-999/*"),
            Some((500, 999, None))
        );
    }

    #[test]
    fn content_range_rejects_malformed_or_inverted() {
        assert_eq!(parse_content_range("0-499/1234"), None);
        assert_eq!(parse_content_range("bytes 499-0/1234"), None);
        assert_eq!(parse_content_range("bytes abc-def/1234"), None);
        assert_eq!(parse_content_range("bytes 0-499"), None);
    }

    #[test]
    fn strict_gate_requires_exact_window_match() {
        assert!(content_range_matches(Some("bytes 0-499/1000"), 0, 499));
        // Wrong start.
        assert!(!content_range_matches(Some("bytes 1-499/1000"), 0, 499));
        // Short window (server clipped).
        assert!(!content_range_matches(Some("bytes 0-498/1000"), 0, 499));
        // Missing header on a 206 fails closed.
        assert!(!content_range_matches(None, 0, 499));
    }

    // --- PD-ADAPT-1d: JoinSet vs node-graph executor diff-0 proof ---------
    //
    // These call the production graph runner and test-only legacy reference
    // directly. There is no process-global runtime switch.

    type BoxedRangeFut = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ConcurrentRangeOutcome, ProviderError>> + Send>,
    >;

    /// Fills `[start, end]` at its absolute offset with `(offset % 251) as u8`,
    /// the same content regardless of which window writes it, so a correct
    /// run produces one deterministic file.
    fn deterministic_writer(
    ) -> impl Fn(u64, u64, PathBuf, Arc<AtomicU64>, CancellationToken) -> BoxedRangeFut
           + Send
           + Sync
           + Clone
           + 'static {
        |start, end, temp_path, aggregate, _cancel| {
            Box::pin(async move {
                let mut f = tokio::fs::OpenOptions::new()
                    .write(true)
                    .open(&temp_path)
                    .await
                    .map_err(ProviderError::IoError)?;
                f.seek(std::io::SeekFrom::Start(start))
                    .await
                    .map_err(ProviderError::IoError)?;
                let len = (end - start + 1) as usize;
                let mut buf = vec![0u8; len];
                for (i, b) in buf.iter_mut().enumerate() {
                    *b = ((start + i as u64) % 251) as u8;
                }
                f.write_all(&buf).await.map_err(ProviderError::IoError)?;
                f.flush().await.map_err(ProviderError::IoError)?;
                aggregate.fetch_add(len as u64, Ordering::Relaxed);
                Ok(ConcurrentRangeOutcome::Completed)
            })
        }
    }

    async fn prealloc(path: &Path, total: u64) {
        let f = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .await
            .unwrap();
        f.set_len(total).await.unwrap();
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("aero-rg-{}-{}", tag, std::process::id()))
    }

    #[tokio::test]
    async fn joinset_and_graph_produce_byte_identical_output() {
        let total: u64 = 1 << 20; // 1 MiB
        let ranges = plan_multi_thread_ranges(total, 4, 16);
        assert_eq!(ranges.len(), 4);

        let dir = scratch_dir("identical");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let temp_a = aerotmp_path_for(&dir.join("a.bin"));
        let temp_b = aerotmp_path_for(&dir.join("b.bin"));
        prealloc(&temp_a, total).await;
        prealloc(&temp_b, total).await;

        let w = Arc::new(deterministic_writer());
        let out_a = run_ranges_via_joinset(
            &ranges,
            &temp_a,
            total,
            4,
            w.clone(),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
        let out_b = run_ranges_via_graph(
            &ranges,
            &temp_b,
            total,
            4,
            super::super::ProviderType::S3,
            crate::transfer_dag::EndpointIdentity::new("s3", "equivalence-harness", ""),
            w.clone(),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap()
        .0;

        assert_eq!(out_a, ConcurrentRangeOutcome::Completed);
        assert_eq!(out_b, ConcurrentRangeOutcome::Completed);

        let bytes_a = tokio::fs::read(&temp_a).await.unwrap();
        let bytes_b = tokio::fs::read(&temp_b).await.unwrap();
        assert_eq!(bytes_a.len() as u64, total);
        assert_eq!(bytes_b.len() as u64, total);
        assert_eq!(
            bytes_a, bytes_b,
            "JoinSet and node-graph paths must be byte-identical"
        );
        for (o, byte) in bytes_a.iter().enumerate() {
            assert_eq!(*byte, ((o as u64) % 251) as u8, "byte mismatch at {o}");
        }

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// DAG-P2-07: a completed graph run attests the whole object in its
    /// returned summary (logical == wire == local == total), plus executor
    /// timing attribution.
    #[tokio::test]
    async fn graph_run_reports_range_bytes_and_timing_in_summary() {
        let total: u64 = 1 << 16;
        let ranges = plan_multi_thread_ranges(total, 4, 16);

        let dir = scratch_dir("metrics");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let temp = aerotmp_path_for(&dir.join("m.bin"));
        prealloc(&temp, total).await;

        let (outcome, summary) = run_ranges_via_graph(
            &ranges,
            &temp,
            total,
            4,
            super::super::ProviderType::S3,
            crate::transfer_dag::EndpointIdentity::new("s3", "metrics-test", ""),
            Arc::new(deterministic_writer()),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(outcome, ConcurrentRangeOutcome::Completed);
        assert_eq!(summary.metrics.bytes_transferred, total);
        assert_eq!(summary.metrics.logical_bytes, total);
        assert_eq!(summary.metrics.wire_bytes, total);
        assert_eq!(summary.metrics.local_payload_bytes, total);
        assert!(summary.metrics.run_nanos_total > 0);
        assert!(summary.metrics.slot_peak >= 1);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// DAG-P2-07 (block C): ranged segments fetch through the shared
    /// send_with_retry choke point, so a graph run of N segment requests
    /// reaching headers folds exactly N honest first-byte samples into the
    /// summary. Runtime-scoped recording keeps parallel-test fixture traffic
    /// out of these exact counts.
    #[tokio::test]
    async fn graph_run_reports_ttfb_for_ranged_segments() {
        use crate::transfer_dag::ttfb::test_fixture::{
            spawn_delayed_http_fixture, FixtureResponse,
        };

        let total: u64 = 96;
        let ranges = plan_multi_thread_ranges(total, 3, 16);
        assert_eq!(ranges.len(), 3);
        let window = (ranges[0].1 - ranges[0].0 + 1) as usize;

        let addr = spawn_delayed_http_fixture(
            std::time::Duration::from_millis(20),
            ranges.len(),
            move |_head| FixtureResponse {
                status: "200 OK".to_string(),
                headers: Vec::new(),
                body: vec![b'x'; window],
            },
        )
        .await;

        let dir = scratch_dir("ttfb");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let temp = aerotmp_path_for(&dir.join("t.bin"));
        prealloc(&temp, total).await;

        let writer = Arc::new(
            move |start: u64,
                  _end: u64,
                  temp_path: PathBuf,
                  aggregate: Arc<AtomicU64>,
                  _cancel: CancellationToken|
                  -> BoxedRangeFut {
                Box::pin(async move {
                    let client = reqwest::Client::new();
                    let request = client
                        .get(format!("http://{addr}/object"))
                        .build()
                        .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
                    let response = send_with_retry(&client, request, &HttpRetryConfig::default())
                        .await
                        .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
                    let body = response
                        .bytes()
                        .await
                        .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
                    let mut f = tokio::fs::OpenOptions::new()
                        .write(true)
                        .open(&temp_path)
                        .await
                        .map_err(ProviderError::IoError)?;
                    f.seek(std::io::SeekFrom::Start(start))
                        .await
                        .map_err(ProviderError::IoError)?;
                    f.write_all(&body).await.map_err(ProviderError::IoError)?;
                    f.flush().await.map_err(ProviderError::IoError)?;
                    aggregate.fetch_add(body.len() as u64, Ordering::Relaxed);
                    Ok(ConcurrentRangeOutcome::Completed)
                })
            },
        );

        let (outcome, summary) = run_ranges_via_graph(
            &ranges,
            &temp,
            total,
            3,
            super::super::ProviderType::WebDav,
            crate::transfer_dag::EndpointIdentity::new("http", "127.0.0.1", ""),
            writer,
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(outcome, ConcurrentRangeOutcome::Completed);
        assert_eq!(
            summary.metrics.ttfb_samples, 3,
            "three segment requests reaching headers are exactly three samples"
        );
        assert!(
            summary.metrics.ttfb_nanos_total
                >= std::time::Duration::from_millis(30).as_nanos() as u64,
            "three 20ms-delayed segments must total >= 30ms, got {}ns",
            summary.metrics.ttfb_nanos_total
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn joinset_and_graph_agree_on_server_ignored_range() {
        let total: u64 = 1 << 20;
        let ranges = plan_multi_thread_ranges(total, 4, 16);

        let dir = scratch_dir("sir");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let temp_a = aerotmp_path_for(&dir.join("a.bin"));
        let temp_b = aerotmp_path_for(&dir.join("b.bin"));
        prealloc(&temp_a, total).await;
        prealloc(&temp_b, total).await;

        let w = Arc::new(
            |start: u64,
             _end: u64,
             _p: PathBuf,
             _a: Arc<AtomicU64>,
             _c: CancellationToken|
             -> BoxedRangeFut {
                Box::pin(async move {
                    if start == 0 {
                        Ok(ConcurrentRangeOutcome::ServerIgnoredRange)
                    } else {
                        Ok(ConcurrentRangeOutcome::Completed)
                    }
                })
            },
        );
        let out_a = run_ranges_via_joinset(
            &ranges,
            &temp_a,
            total,
            4,
            w.clone(),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
        let out_b = run_ranges_via_graph(
            &ranges,
            &temp_b,
            total,
            4,
            super::super::ProviderType::S3,
            crate::transfer_dag::EndpointIdentity::new("s3", "equivalence-harness", ""),
            w.clone(),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap()
        .0;

        assert_eq!(out_a, ConcurrentRangeOutcome::ServerIgnoredRange);
        assert_eq!(out_b, ConcurrentRangeOutcome::ServerIgnoredRange);
        assert_eq!(out_a, out_b);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn joinset_and_graph_agree_on_hard_error_variant() {
        let total: u64 = 1 << 20;
        let ranges = plan_multi_thread_ranges(total, 4, 16);

        let dir = scratch_dir("err");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let temp_a = aerotmp_path_for(&dir.join("a.bin"));
        let temp_b = aerotmp_path_for(&dir.join("b.bin"));
        prealloc(&temp_a, total).await;
        prealloc(&temp_b, total).await;

        let w = Arc::new(
            |start: u64,
             _end: u64,
             _p: PathBuf,
             _a: Arc<AtomicU64>,
             _c: CancellationToken|
             -> BoxedRangeFut {
                Box::pin(async move {
                    if start == 0 {
                        Err(ProviderError::NotSupported(
                            "synthetic hard error".to_string(),
                        ))
                    } else {
                        Ok(ConcurrentRangeOutcome::Completed)
                    }
                })
            },
        );
        let err_a = run_ranges_via_joinset(
            &ranges,
            &temp_a,
            total,
            4,
            w.clone(),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap_err();
        let err_b = run_ranges_via_graph(
            &ranges,
            &temp_b,
            total,
            4,
            super::super::ProviderType::S3,
            crate::transfer_dag::EndpointIdentity::new("s3", "equivalence-harness", ""),
            w.clone(),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap_err();

        assert!(
            matches!(err_a, ProviderError::NotSupported(_)),
            "joinset returned {err_a:?}"
        );
        assert!(
            matches!(err_b, ProviderError::NotSupported(_)),
            "graph returned {err_b:?}"
        );

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    async fn assert_cancellation_parity(graph: bool, tag: &str) {
        use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
        use std::time::{Duration, Instant};
        use tokio::sync::Barrier;

        struct TerminationGuard(Arc<AtomicBool>);
        impl Drop for TerminationGuard {
            fn drop(&mut self) {
                self.0.store(true, AtomicOrdering::SeqCst);
            }
        }

        let ranges = vec![(0, 0), (1, 1)];
        let dir = scratch_dir(tag);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let temp = aerotmp_path_for(&dir.join("cancel.bin"));
        prealloc(&temp, 2).await;

        let both_started = Arc::new(Barrier::new(2));
        let sibling_terminated = Arc::new(AtomicBool::new(false));
        let writer = {
            let both_started = both_started.clone();
            let sibling_terminated = sibling_terminated.clone();
            Arc::new(
                move |start: u64,
                      _end: u64,
                      _path: PathBuf,
                      _aggregate: Arc<AtomicU64>,
                      cancel: CancellationToken|
                      -> BoxedRangeFut {
                    let both_started = both_started.clone();
                    let sibling_terminated = sibling_terminated.clone();
                    Box::pin(async move {
                        let _termination_guard =
                            (start == 1).then(|| TerminationGuard(sibling_terminated));
                        both_started.wait().await;
                        if start == 0 {
                            Err(ProviderError::NotSupported(
                                "synthetic cancellation trigger".to_string(),
                            ))
                        } else {
                            cancel.cancelled().await;
                            Err(ProviderError::TransferFailed(
                                "sibling observed cancellation".to_string(),
                            ))
                        }
                    })
                },
            )
        };
        let cancel = CancellationToken::new();
        let cancel_witness = cancel.clone();
        let started = Instant::now();
        let result = if graph {
            run_ranges_via_graph(
                &ranges,
                &temp,
                2,
                2,
                super::super::ProviderType::S3,
                crate::transfer_dag::EndpointIdentity::new("s3", "equivalence-harness", ""),
                writer,
                cancel,
                None,
            )
            .await
            .map(|(outcome, _summary)| outcome)
        } else {
            run_ranges_via_joinset(&ranges, &temp, 2, 2, writer, cancel, None).await
        };

        let err = result.unwrap_err();
        assert!(
            matches!(err, ProviderError::NotSupported(_)),
            "first typed error must survive sibling cancellation: {err:?}"
        );
        assert!(
            cancel_witness.is_cancelled(),
            "the runner did not fire the shared cancellation token"
        );
        assert!(
            sibling_terminated.load(AtomicOrdering::SeqCst),
            "the in-flight sibling did not terminate during fail-fast shutdown"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "sibling cancellation exceeded the P0-05 window"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn joinset_and_graph_cancel_in_flight_siblings_with_same_typed_error() {
        assert_cancellation_parity(false, "cancel-joinset").await;
        assert_cancellation_parity(true, "cancel-graph").await;
    }

    #[tokio::test]
    async fn joinset_and_graph_emit_identical_progress_sequence() {
        use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
        use tokio::sync::Notify;

        let total = 10;
        let ranges = plan_multi_thread_ranges(total, 3, 16);
        assert_eq!(ranges, vec![(0, 3), (4, 6), (7, 9)]);

        let dir = scratch_dir("progress");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let temp_a = aerotmp_path_for(&dir.join("a.bin"));
        let temp_b = aerotmp_path_for(&dir.join("b.bin"));
        prealloc(&temp_a, total).await;
        prealloc(&temp_b, total).await;

        let progress_a = Arc::new(std::sync::Mutex::new(Vec::new()));
        let progress_b = Arc::new(std::sync::Mutex::new(Vec::new()));
        let callbacks_a = Arc::new(AtomicUsize::new(0));
        let callbacks_b = Arc::new(AtomicUsize::new(0));
        let callback_notify_a = Arc::new(Notify::new());
        let callback_notify_b = Arc::new(Notify::new());
        // A completed task releases its scheduler slot before the coordinator
        // invokes the callback. Gate each next write on that callback so this
        // test proves the exact observable sequence without depending on
        // runtime polling order or allowing several completions to coalesce.
        let callback_a: Box<dyn Fn(u64, u64) + Send> = {
            let progress = progress_a.clone();
            let callbacks = callbacks_a.clone();
            let callback_notify = callback_notify_a.clone();
            Box::new(move |done, total| {
                progress.lock().unwrap().push((done, total));
                callbacks.fetch_add(1, AtomicOrdering::SeqCst);
                callback_notify.notify_waiters();
            })
        };
        let callback_b: Box<dyn Fn(u64, u64) + Send> = {
            let progress = progress_b.clone();
            let callbacks = callbacks_b.clone();
            let callback_notify = callback_notify_b.clone();
            Box::new(move |done, total| {
                progress.lock().unwrap().push((done, total));
                callbacks.fetch_add(1, AtomicOrdering::SeqCst);
                callback_notify.notify_waiters();
            })
        };
        let sequenced_writer = |callbacks: Arc<AtomicUsize>, callback_notify: Arc<Notify>| {
            move |start: u64,
                  end: u64,
                  temp_path: PathBuf,
                  aggregate: Arc<AtomicU64>,
                  _cancel: CancellationToken|
                  -> BoxedRangeFut {
                let callbacks = callbacks.clone();
                let callback_notify = callback_notify.clone();
                Box::pin(async move {
                    let ordinal = match start {
                        0 => 0,
                        4 => 1,
                        7 => 2,
                        _ => panic!("unexpected range start {start}"),
                    };
                    loop {
                        if callbacks.load(AtomicOrdering::SeqCst) >= ordinal {
                            break;
                        }
                        let notified = callback_notify.notified();
                        if callbacks.load(AtomicOrdering::SeqCst) >= ordinal {
                            break;
                        }
                        notified.await;
                    }

                    let mut f = tokio::fs::OpenOptions::new()
                        .write(true)
                        .open(&temp_path)
                        .await
                        .map_err(ProviderError::IoError)?;
                    f.seek(std::io::SeekFrom::Start(start))
                        .await
                        .map_err(ProviderError::IoError)?;
                    let len = (end - start + 1) as usize;
                    f.write_all(&vec![0u8; len])
                        .await
                        .map_err(ProviderError::IoError)?;
                    aggregate.fetch_add(len as u64, Ordering::Relaxed);
                    Ok(ConcurrentRangeOutcome::Completed)
                })
            }
        };
        let writer_a = Arc::new(sequenced_writer(callbacks_a, callback_notify_a));
        let writer_b = Arc::new(sequenced_writer(callbacks_b, callback_notify_b));

        let out_a = run_ranges_via_joinset(
            &ranges,
            &temp_a,
            total,
            1,
            writer_a,
            CancellationToken::new(),
            Some(callback_a),
        )
        .await
        .unwrap();
        let out_b = run_ranges_via_graph(
            &ranges,
            &temp_b,
            total,
            1,
            super::super::ProviderType::S3,
            crate::transfer_dag::EndpointIdentity::new("s3", "equivalence-harness", ""),
            writer_b,
            CancellationToken::new(),
            Some(callback_b),
        )
        .await
        .unwrap()
        .0;

        assert_eq!(out_a, ConcurrentRangeOutcome::Completed);
        assert_eq!(out_a, out_b);
        assert_eq!(
            *progress_a.lock().unwrap(),
            vec![(4, 10), (7, 10), (10, 10)]
        );
        assert_eq!(*progress_a.lock().unwrap(), *progress_b.lock().unwrap());
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn joinset_and_graph_agree_on_range_boundaries_and_empty_plan() {
        let cases = [
            ("single", 5, vec![(0, 4)]),
            ("partial", 10, vec![(0, 3), (4, 6), (7, 9)]),
            ("empty", 0, vec![]),
        ];

        for (tag, total, ranges) in cases {
            let dir = scratch_dir(&format!("boundary-{tag}"));
            tokio::fs::create_dir_all(&dir).await.unwrap();
            let temp_a = aerotmp_path_for(&dir.join("a.bin"));
            let temp_b = aerotmp_path_for(&dir.join("b.bin"));
            prealloc(&temp_a, total).await;
            prealloc(&temp_b, total).await;
            let writer = Arc::new(deterministic_writer());

            let out_a = run_ranges_via_joinset(
                &ranges,
                &temp_a,
                total,
                2,
                writer.clone(),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap();
            let out_b = run_ranges_via_graph(
                &ranges,
                &temp_b,
                total,
                2,
                super::super::ProviderType::S3,
                crate::transfer_dag::EndpointIdentity::new("s3", "equivalence-harness", ""),
                writer,
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap()
            .0;

            assert_eq!(out_a, ConcurrentRangeOutcome::Completed, "case {tag}");
            assert_eq!(out_a, out_b, "case {tag}");
            assert_eq!(
                tokio::fs::read(&temp_a).await.unwrap(),
                tokio::fs::read(&temp_b).await.unwrap(),
                "case {tag}"
            );
            let _ = tokio::fs::remove_dir_all(&dir).await;
        }
    }

    #[tokio::test]
    async fn joinset_and_graph_classify_task_panic_as_transfer_failed() {
        let ranges = vec![(0, 0)];
        let dir = scratch_dir("panic");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let temp_a = aerotmp_path_for(&dir.join("a.bin"));
        let temp_b = aerotmp_path_for(&dir.join("b.bin"));
        prealloc(&temp_a, 1).await;
        prealloc(&temp_b, 1).await;
        let writer = Arc::new(
            |_start: u64,
             _end: u64,
             _path: PathBuf,
             _aggregate: Arc<AtomicU64>,
             _cancel: CancellationToken|
             -> BoxedRangeFut {
                Box::pin(async move { panic!("synthetic range panic") })
            },
        );

        let err_a = run_ranges_via_joinset(
            &ranges,
            &temp_a,
            1,
            1,
            writer.clone(),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap_err();
        let err_b = run_ranges_via_graph(
            &ranges,
            &temp_b,
            1,
            1,
            super::super::ProviderType::S3,
            crate::transfer_dag::EndpointIdentity::new("s3", "equivalence-harness", ""),
            writer,
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap_err();

        assert!(
            matches!(err_a, ProviderError::TransferFailed(_)),
            "joinset returned {err_a:?}"
        );
        assert!(
            matches!(err_b, ProviderError::TransferFailed(_)),
            "graph returned {err_b:?}"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn concurrent_range_download_uses_promoted_graph_without_escape_hatch() {
        let dir = scratch_dir("promoted-default");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let final_path = dir.join("result.bin");
        let temp_path = aerotmp_path_for(&final_path);
        let config = ConcurrentRangeConfig {
            final_path,
            provider_type: super::super::ProviderType::S3,
            endpoint_identity: crate::transfer_dag::EndpointIdentity::new("s3", "test", ""),
            total_size: 10,
            streams: 3,
            max_streams: 16,
            max_parallel: 2,
        };

        let outcome = run_concurrent_range_download(
            config,
            deterministic_writer(),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(outcome, ConcurrentRangeOutcome::Completed);
        let bytes = tokio::fs::read(&temp_path).await.unwrap();
        assert_eq!(bytes.len(), 10);
        for (offset, byte) in bytes.iter().enumerate() {
            assert_eq!(*byte, (offset % 251) as u8);
        }
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    async fn assert_max_parallel_cap(graph: bool, tag: &str) {
        use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
        use tokio::sync::{Barrier, Semaphore as TokioSemaphore};

        let ranges = vec![(0, 0), (1, 1), (2, 2), (3, 3)];
        let dir = scratch_dir(tag);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let temp = aerotmp_path_for(&dir.join("cap.bin"));
        prealloc(&temp, 4).await;

        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(AtomicUsize::new(0));
        let first_wave = Arc::new(Barrier::new(3));
        let release = Arc::new(TokioSemaphore::new(0));
        let writer = {
            let active = active.clone();
            let peak = peak.clone();
            let entered = entered.clone();
            let first_wave = first_wave.clone();
            let release = release.clone();
            Arc::new(
                move |_start: u64,
                      _end: u64,
                      _path: PathBuf,
                      aggregate: Arc<AtomicU64>,
                      _cancel: CancellationToken|
                      -> BoxedRangeFut {
                    let active = active.clone();
                    let peak = peak.clone();
                    let entered = entered.clone();
                    let first_wave = first_wave.clone();
                    let release = release.clone();
                    Box::pin(async move {
                        let now = active.fetch_add(1, AtomicOrdering::SeqCst) + 1;
                        peak.fetch_max(now, AtomicOrdering::SeqCst);
                        if entered.fetch_add(1, AtomicOrdering::SeqCst) < 2 {
                            first_wave.wait().await;
                        }
                        let permit = release.acquire().await.unwrap();
                        permit.forget();
                        active.fetch_sub(1, AtomicOrdering::SeqCst);
                        aggregate.fetch_add(1, Ordering::Relaxed);
                        Ok(ConcurrentRangeOutcome::Completed)
                    })
                },
            )
        };
        let run_temp = temp.clone();
        let run_ranges = ranges.clone();
        let task = tokio::spawn(async move {
            if graph {
                run_ranges_via_graph(
                    &run_ranges,
                    &run_temp,
                    4,
                    2,
                    super::super::ProviderType::S3,
                    crate::transfer_dag::EndpointIdentity::new("s3", "equivalence-harness", ""),
                    writer,
                    CancellationToken::new(),
                    None,
                )
                .await
                .map(|(outcome, _summary)| outcome)
            } else {
                run_ranges_via_joinset(
                    &run_ranges,
                    &run_temp,
                    4,
                    2,
                    writer,
                    CancellationToken::new(),
                    None,
                )
                .await
            }
        });

        first_wave.wait().await;
        assert_eq!(active.load(AtomicOrdering::SeqCst), 2);
        assert_eq!(peak.load(AtomicOrdering::SeqCst), 2);
        release.add_permits(ranges.len());
        let outcome = task.await.unwrap().unwrap();
        assert_eq!(outcome, ConcurrentRangeOutcome::Completed);
        assert_eq!(peak.load(AtomicOrdering::SeqCst), 2);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn joinset_and_graph_enforce_identical_max_parallel_cap() {
        assert_max_parallel_cap(false, "cap-joinset").await;
        assert_max_parallel_cap(true, "cap-graph").await;
    }
}
