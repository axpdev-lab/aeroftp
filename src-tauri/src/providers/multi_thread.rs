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
    /// Final destination path (used only to derive the `.aerosegtmp` sibling).
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

/// How far one attempt at a range has written without a gap: every byte from
/// the attempt's start up to [`RangeMark::reached`] is in the temporary.
///
/// A range that fails with a recoverable error (a request timeout, a dropped
/// connection) is attempted again from its mark instead of failing the whole
/// download (see [`write_range_resuming`]). A writer that writes in order
/// advances the mark after each write; one that writes out of order (the SFTP
/// read-ahead) advances it to the first byte still missing. A writer that
/// never advances it is safe: its range is read again from the start.
#[derive(Clone, Debug)]
pub struct RangeMark(Arc<AtomicU64>);

impl RangeMark {
    pub(crate) fn new(start: u64) -> Self {
        Self(Arc::new(AtomicU64::new(start)))
    }

    /// Every byte before `offset` is written. The mark never goes back.
    pub fn advance(&self, offset: u64) {
        self.0.fetch_max(offset, Ordering::Relaxed);
    }

    /// The first byte not known to be written.
    pub fn reached(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// Failures in a row of one range, with none of its bytes moving on, before
/// the download fails. An attempt that moved the mark starts the count again,
/// so a server that stalls now and then over a long transfer does not use it
/// up.
pub(crate) const RANGE_RETRIES: u32 = 3;

/// Wait before the n-th retry in a row of a range: 1, 2, then 4 s.
fn range_retry_backoff(failures: u32) -> std::time::Duration {
    std::time::Duration::from_secs(1u64 << failures.saturating_sub(1).min(2))
}

/// Run `write_one_range` over the inclusive range `[start, end]`, resuming it
/// after a recoverable failure.
///
/// A single request that took too long used to fail the whole download, and
/// the temporary went with it: a 25 GB SFTP download from a NAS whose server
/// stopped answering one connection for more than 10 s failed twice, after 26
/// and 16 minutes, and started from zero each time (#658 re-test, 2026-10-10;
/// the late replies did arrive, the server was alive). The failed attempt's
/// connection is the worker's to close; the next attempt dials its own and
/// reads from the mark, so only the bytes in flight are read again.
///
/// `bytes` is this range's share of the progress: what an attempt wrote past
/// its mark is taken off, since it will be written again.
pub(crate) async fn write_range_resuming<W, WFut>(
    write_one_range: &W,
    start: u64,
    end: u64,
    temp_path: &Path,
    bytes: Arc<AtomicU64>,
    cancel: &CancellationToken,
) -> Result<ConcurrentRangeOutcome, ProviderError>
where
    W: Fn(u64, u64, PathBuf, Arc<AtomicU64>, CancellationToken, RangeMark) -> WFut,
    WFut: std::future::Future<Output = Result<ConcurrentRangeOutcome, ProviderError>>,
{
    let mut from = start;
    let mut failures = 0u32;
    loop {
        let mark = RangeMark::new(from);
        let error = match write_one_range(
            from,
            end,
            temp_path.to_path_buf(),
            bytes.clone(),
            cancel.clone(),
            mark.clone(),
        )
        .await
        {
            Err(e) if e.is_recoverable() && !cancel.is_cancelled() => e,
            outcome => return outcome,
        };
        let reached = mark.reached().clamp(from, end.saturating_add(1));
        bytes.store(reached - start, Ordering::Relaxed);
        if reached > end {
            // Every byte is in the temporary; what failed came after the last
            // write (closing the remote handle), which a download can lose.
            return Ok(ConcurrentRangeOutcome::Completed);
        }
        if reached > from {
            failures = 0;
        }
        failures += 1;
        if failures > RANGE_RETRIES {
            return Err(error);
        }
        let wait = range_retry_backoff(failures);
        tracing::warn!(
            "Range {}-{}: {} at offset {}; retry {} of {} from there in {:?}",
            start,
            end,
            error,
            reached,
            failures,
            RANGE_RETRIES,
            wait
        );
        tokio::select! {
            _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
            _ = tokio::time::sleep(wait) => {}
        }
        from = reached;
    }
}

/// Settle the writes a failed range attempt left in flight before its error
/// goes back. A `tokio::fs::File` write returns once its bytes are handed to
/// a blocking task, and a failure of that write shows only on the next write
/// or on `flush`; the mark already counts those bytes, so a write that failed
/// unseen would leave a hole the retry skips. A write that fails here fails
/// the range for good: an I/O error is not resumed.
pub async fn settle_range_attempt<T>(
    file: &mut tokio::fs::File,
    attempt: Result<T, ProviderError>,
) -> Result<T, ProviderError> {
    if attempt.is_err() {
        file.flush().await.map_err(ProviderError::IoError)?;
    }
    attempt
}

/// A range worker's failed `read_range`, with where it failed. The class is
/// kept: a timeout or a lost connection is resumed from the mark
/// ([`write_range_resuming`]) and a refusal stays one; anything else fails
/// the range.
pub fn range_read_failed(what: &str, offset: u64, e: ProviderError) -> ProviderError {
    let text = format!("{what}: read_range at offset {offset} failed: {e}");
    match e {
        ProviderError::Timeout | ProviderError::NotConnected => e,
        ProviderError::NetworkError(_) => ProviderError::NetworkError(text),
        ProviderError::ConnectionLost(_) => ProviderError::ConnectionLost(text),
        ProviderError::ParallelRefused(_) => ProviderError::ParallelRefused(text),
        _ => ProviderError::TransferFailed(text),
    }
}

/// How long a range worker waits to say goodbye on a connection that failed:
/// the server may be the one not answering, and the retry waits on it.
pub(crate) const FAILED_RANGE_DISCONNECT: std::time::Duration = std::time::Duration::from_secs(5);

/// The worker a failed range attempt leaves to the next one, for the engines
/// that take their workers from a pool, and the error the attempt reports.
/// The failed connection is closed either way: one left open keeps the
/// server serving a session nobody reads. The next worker is a fresh clone of
/// `worker` (a connection of its own, as the first attempt had) or, for a
/// provider that cannot clone, `worker` itself connected again when the range
/// will be resumed: not every provider dials by itself on its next call
/// (Koofr, Cloudinary and ImageKit answer `NotConnected` after a disconnect).
/// A reconnect that fails is what the attempt reports, with its own class: a
/// refused sign-in fails the range, a network still down is resumed, and the
/// next attempt dials again. `validator` pins the next worker to the object.
pub async fn replace_failed_range_worker(
    mut worker: Box<dyn super::StorageProvider>,
    validator: Option<String>,
    failure: ProviderError,
) -> (Box<dyn super::StorageProvider>, ProviderError) {
    let fresh = worker.clone_for_transfer().ok();
    let _ = tokio::time::timeout(FAILED_RANGE_DISCONNECT, worker.disconnect()).await;
    let (mut next, failure) = match fresh {
        Some(fresh) => (fresh, failure),
        None if failure.is_recoverable() => match worker.connect().await {
            Ok(()) => (worker, failure),
            Err(e) => {
                tracing::warn!("Range worker: reconnecting after {failure} failed: {e}");
                (worker, e)
            }
        },
        None => (worker, failure),
    };
    next.set_range_validator(validator);
    (next, failure)
}

/// Bytes of every range in the temporary, for the progress report.
pub(crate) fn total_range_bytes(range_bytes: &[Arc<AtomicU64>]) -> u64 {
    range_bytes.iter().map(|b| b.load(Ordering::Relaxed)).sum()
}

/// The window [`read_in_order`] asks for at a time: what it holds per stream,
/// and the most one failed attempt reads again.
pub const IN_ORDER_WINDOW: u64 = 8 * 1024 * 1024;

/// Read `[0, size)` of `remote_path` through `workers` and hand every byte to
/// `consume` once, in order, with no file in between.
///
/// `hashsum` used to download the whole object into the temporary folder and
/// hash it there: a 25.2 GB file filled a 27 GB tmpfs after 21 minutes and the
/// command failed with no hash (#658 re-test, 2026-10-10). Here the windows
/// are consumed in their order and at most one per worker is held at once,
/// in flight or read and waiting for an earlier one, so the memory is one
/// window per worker whatever the size of the object. A window that fails
/// with a recoverable error is read again from its mark on a fresh worker
/// ([`write_range_resuming`]), as a segmented download's range is.
///
/// The workers come back with the outcome, so the caller can read the object
/// once more through one of them before trusting the bytes, and close them.
/// After a failure, the workers of the windows still in flight are dropped
/// with them.
pub async fn read_in_order<F>(
    workers: Vec<Box<dyn super::StorageProvider>>,
    remote_path: &str,
    size: u64,
    validator: Option<String>,
    window: u64,
    cancel: &CancellationToken,
    mut consume: F,
) -> (
    Result<(), ProviderError>,
    Vec<Box<dyn super::StorageProvider>>,
)
where
    F: FnMut(&[u8]),
{
    let streams = workers.len();
    if streams == 0 {
        return (
            Err(ProviderError::Other(
                "read_in_order needs at least one worker".to_string(),
            )),
            workers,
        );
    }
    let window = window.max(1);
    let pool = tokio::sync::Mutex::new(std::collections::VecDeque::from(workers));
    let outcome = {
        let mut windows = futures_util::stream::iter(0..size.div_ceil(window))
            .map(|i| {
                let start = i * window;
                let end = (start + window).min(size) - 1;
                read_window_in_memory(&pool, remote_path, start, end, &validator, cancel)
            })
            .buffered(streams);
        let mut outcome = Ok(());
        while let Some(read) = windows.next().await {
            match read {
                Ok(bytes) => consume(&bytes),
                Err(e) => {
                    outcome = Err(e);
                    break;
                }
            }
        }
        outcome
    };
    (outcome, pool.into_inner().into())
}

/// One window of [`read_in_order`], `[start, end]` inclusive, into memory.
async fn read_window_in_memory(
    pool: &tokio::sync::Mutex<std::collections::VecDeque<Box<dyn super::StorageProvider>>>,
    remote_path: &str,
    start: u64,
    end: u64,
    validator: &Option<String>,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, ProviderError> {
    let buffer = std::sync::Mutex::new(vec![0u8; (end - start + 1) as usize]);
    let read_from = |from: u64,
                     end: u64,
                     _temp: PathBuf,
                     _bytes: Arc<AtomicU64>,
                     cancel: CancellationToken,
                     mark: RangeMark| {
        let buffer = &buffer;
        async move {
            let mut worker = pool.lock().await.pop_front().ok_or_else(|| {
                ProviderError::Other("read_in_order: no free worker (internal invariant)".into())
            })?;
            let mut at = from;
            let read: Result<(), ProviderError> = async {
                while at <= end {
                    if cancel.is_cancelled() {
                        return Err(ProviderError::Cancelled);
                    }
                    let len = end - at + 1;
                    let data = worker
                        .read_range(remote_path, at, len)
                        .await
                        .map_err(|e| range_read_failed("in-order read", at, e))?;
                    // Charged here, not in `read_range`, which leaves the
                    // limiter to its caller; for the bytes that came, so a
                    // short read is not paid as a full one.
                    crate::transfer_dag::throttle::charge(
                        crate::transfer_dag::governor::TransferDirection::Download,
                        data.len() as u64,
                    )
                    .await;
                    if data.is_empty() {
                        // The object ended before the size it was read for:
                        // a shorter file, which no retry makes longer.
                        return Err(ProviderError::TransferFailed(format!(
                            "{remote_path} ended at byte {at}, before the {} bytes it was read for",
                            end + 1
                        )));
                    }
                    let take = data.len().min(len as usize);
                    let offset = (at - start) as usize;
                    buffer.lock().unwrap_or_else(|e| e.into_inner())[offset..offset + take]
                        .copy_from_slice(&data[..take]);
                    at += take as u64;
                    mark.advance(at);
                }
                Ok(())
            }
            .await;
            let (worker, read) = match read {
                Ok(()) => (worker, Ok(())),
                Err(e) => {
                    let (next, e) = replace_failed_range_worker(worker, validator.clone(), e).await;
                    (next, Err(e))
                }
            };
            pool.lock().await.push_back(worker);
            read.map(|()| ConcurrentRangeOutcome::Completed)
        }
    };
    write_range_resuming(
        &read_from,
        start,
        end,
        Path::new(""),
        Arc::new(AtomicU64::new(0)),
        cancel,
    )
    .await?;
    Ok(buffer.into_inner().unwrap_or_else(|e| e.into_inner()))
}

/// RAII guard: removes the `.aerosegtmp` file on drop unless `commit()` is
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

/// What a segmented run hands back to its caller.
#[must_use]
#[derive(Debug)]
pub enum SegmentedRun {
    /// Every window is in the temporary, which now belongs to the caller: it
    /// reads the source again, then publishes the temporary or discards it.
    Completed(SegmentedTemp),
    /// The server ignored `Range`. Nothing was kept.
    ServerIgnoredRange,
}

impl SegmentedRun {
    pub fn outcome(&self) -> ConcurrentRangeOutcome {
        match self {
            Self::Completed(_) => ConcurrentRangeOutcome::Completed,
            Self::ServerIgnoredRange => ConcurrentRangeOutcome::ServerIgnoredRange,
        }
    }
}

/// The temporary of a completed segmented run, still claimed.
///
/// The claim taken when the run created the temporary stays with this value
/// until the temporary is published or discarded. A second segmented download
/// of the same file therefore finds it live through the caller's second
/// reading of the source and its rename, instead of taking it for stale and
/// replacing it with a pre-sized file of its own that the first caller would
/// then publish, holes included. Dropped without being published, the
/// temporary is removed while still claimed.
///
/// The claim is taken only where `atomic_write::temp_claim` uses locks (Linux,
/// local filesystems); elsewhere the temporary is handled as before it.
#[must_use = "a completed segmented run is published or discarded"]
#[derive(Debug)]
pub struct SegmentedTemp {
    held: Option<ClaimedTemp>,
}

/// The temporary's path and the claim on it. Whoever holds this value owns
/// the temporary; the claim is released when it is dropped.
#[derive(Debug)]
struct ClaimedTemp {
    path: PathBuf,
    #[cfg(unix)]
    _claim: std::fs::File,
}

impl ClaimedTemp {
    /// Remove the temporary; the claim is released afterwards, with `self`.
    fn remove(self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl SegmentedTemp {
    /// Rename the temporary to `final_path` while it is still claimed. When
    /// the rename fails the temporary is removed.
    ///
    /// The temporary and its claim move into the blocking task that renames
    /// it, so a caller that stops awaiting part way cannot leave a `Drop`
    /// behind that removes by name a temporary another run has created there
    /// since the rename.
    pub async fn publish(mut self, final_path: &Path) -> std::io::Result<()> {
        let Some(held) = self.held.take() else {
            return Err(std::io::Error::other(
                "segmented temporary already handed over",
            ));
        };
        let final_path = final_path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let renamed = std::fs::rename(&held.path, &final_path);
            if renamed.is_err() {
                held.remove();
            }
            renamed
        })
        .await
        .map_err(std::io::Error::other)?
    }

    /// Remove the temporary while it is still claimed.
    pub fn discard(self) {}
}

impl Drop for SegmentedTemp {
    fn drop(&mut self) {
        if let Some(held) = self.held.take() {
            held.remove();
        }
    }
}

/// What a remote object looked like at one moment: the validator the provider
/// publishes for it, plus the size and modification time every provider has.
///
/// A multi-stream download reads its windows in parallel, so an object
/// replaced while they are in flight is assembled out of two versions: each
/// window has the length it asked for, the total is exactly right, and
/// nothing downstream can tell. Reading the object before the windows start
/// and again before publishing turns that into a refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeSourceFingerprint {
    size: u64,
    modified: Option<String>,
    validator: Option<String>,
}

impl RangeSourceFingerprint {
    pub fn of(entry: &super::RemoteEntry) -> Self {
        Self {
            size: entry.size,
            modified: entry.modified.clone(),
            validator: entry
                .metadata
                .get("etag")
                .map(|tag| tag.trim().to_string())
                .filter(|tag| !tag.is_empty()),
        }
    }

    /// The validator to pin ranged reads to, when the provider published one.
    pub fn validator(&self) -> Option<String> {
        self.validator.clone()
    }

    /// The size this reading found.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// The plan about to run was built from a size the caller probed earlier.
    /// If the object has a different one now, the plan belongs to a different
    /// object: pinning the windows to this one would publish a file cut to the
    /// old length with every check green.
    pub fn matches_planned_size(&self, planned_size: u64) -> Result<(), String> {
        if self.size == planned_size {
            Ok(())
        } else {
            Err(format!(
                "the transfer was planned for {} bytes and the object now has {}",
                planned_size, self.size
            ))
        }
    }

    /// A named reason when the two readings cannot describe the same object,
    /// and `None` when nothing proves they differ.
    ///
    /// A value this reading carried and the other does not is itself a
    /// difference, not a missing comparison: an endpoint that stops answering
    /// with the entity tag it had just given is exactly how a replacement
    /// would hide from a check that only compares what both sides publish.
    pub fn differs_from(&self, other: &Self) -> Option<String> {
        match (&self.validator, &other.validator) {
            (Some(before), Some(after)) if before != after => {
                return Some(format!("entity tag {} became {}", before, after));
            }
            (Some(before), None) => {
                return Some(format!(
                    "entity tag {} is no longer reported, so nothing pins the bytes",
                    before
                ));
            }
            _ => {}
        }
        if self.size != other.size {
            return Some(format!("size {} became {}", self.size, other.size));
        }
        match (&self.modified, &other.modified) {
            (Some(before), Some(after)) if before != after => {
                return Some(format!("modification time {} became {}", before, after));
            }
            (Some(before), None) => {
                return Some(format!(
                    "modification time {} is no longer reported",
                    before
                ));
            }
            _ => {}
        }
        None
    }
}

/// The phrase every refusal of this kind carries, so a reader of a log or an
/// error can tell "the object moved under us" from a transport failure.
pub const SOURCE_CHANGED_MARKER: &str = "changed while it was being downloaded";

/// How long to wait before reading the object a second time, when the reading
/// that decides whether a finished download is published has just failed.
pub(crate) const AFTER_TRANSFER_READ_RETRY: std::time::Duration =
    std::time::Duration::from_millis(500);

/// The phrase a refusal to even start the parallel path carries.
pub const PARALLEL_REFUSED_MARKER: &str = "refusing to read it in parallel";

/// The message for a parallel path that refuses to start.
pub fn parallel_refused(scope: &str, remote_path: &str, why: &str) -> String {
    format!("{scope}: {PARALLEL_REFUSED_MARKER} ({remote_path}): {why}")
}

/// The message for an assembled file that must not be published.
pub fn source_changed(scope: &str, remote_path: &str, what: &str) -> String {
    format!("{scope}: {remote_path} {SOURCE_CHANGED_MARKER} ({what})")
}

/// A transfer progress callback, as every provider download takes it.
pub type ProgressCallback = Box<dyn Fn(u64, u64) + Send>;

/// Two callbacks that both report to `progress`: one goes with the parallel
/// attempt, the other stays behind for the single-stream path a refusal falls
/// back to. The callback is `Send` and not `Sync`, so it is shared behind a
/// lock rather than cloned; the two halves never run at the same time.
pub fn share_progress(
    progress: Option<ProgressCallback>,
) -> (Option<ProgressCallback>, Option<ProgressCallback>) {
    let Some(progress) = progress else {
        return (None, None);
    };
    let shared = Arc::new(std::sync::Mutex::new(progress));
    let half = |shared: Arc<std::sync::Mutex<ProgressCallback>>| -> ProgressCallback {
        Box::new(move |done, total| {
            let progress = shared.lock().unwrap_or_else(|e| e.into_inner());
            progress(done, total)
        })
    };
    (Some(half(Arc::clone(&shared))), Some(half(shared)))
}

/// Run `open`, and once more after [`AFTER_TRANSFER_READ_RETRY`] when it
/// fails: the session a finished download is checked through is opened right
/// after the window connections closed, and one refused connection must not
/// discard a complete transfer.
pub async fn open_after_transfer<T, E, F, Fut>(mut open: F) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    match open().await {
        Ok(session) => Ok(session),
        Err(_) => {
            tokio::time::sleep(AFTER_TRANSFER_READ_RETRY).await;
            open().await
        }
    }
}

/// What a ranged HTTP answer turned out to be.
#[derive(Debug)]
pub enum RangedAnswer {
    /// The window that was asked for, ready to be written at its offset.
    Window,
    /// The whole object: the server ignored the range. The caller has to cut
    /// the window out of it or refuse, never write it at the offset.
    WholeObject,
}

/// Cut the window out of a whole-object answer.
///
/// One implementation rather than one per provider: an off-by-one here would
/// otherwise have to be found and fixed seven times.
pub fn slice_whole_object(bytes: &[u8], offset: u64, len: u64) -> Vec<u8> {
    if offset >= bytes.len() as u64 {
        return Vec::new();
    }
    let start = offset as usize;
    let stop = std::cmp::min(start.saturating_add(len as usize), bytes.len());
    bytes[start..stop].to_vec()
}

/// Classify the answer to a ranged read.
///
/// The property has two halves and a reader that checks one is still open. A
/// 200 is the whole object, whose head written at the window's offset
/// corrupts the file to exactly the right length. A 206 has to say which
/// range it carries, and it has to be the one asked for: a server that
/// answers a different window is the same corruption with a success code.
pub fn ranged_answer(
    status: reqwest::StatusCode,
    content_range: Option<&str>,
    body_len: u64,
    start: u64,
    end: u64,
) -> Result<RangedAnswer, String> {
    match status {
        reqwest::StatusCode::PARTIAL_CONTENT => {
            // The whole grammar, not a prefix: `bytes 0-1/not-a-total` starts
            // like the right answer and is not one.
            if !content_range_matches(content_range, start, end) {
                return Err(format!(
                    "the server answered {:?} to a request for bytes {}-{}",
                    content_range.unwrap_or_default(),
                    start,
                    end
                ));
            }
            let window = end - start + 1;
            if body_len != window {
                return Err(format!(
                    "the server said bytes {}-{} and sent {} bytes instead of {}",
                    start, end, body_len, window
                ));
            }
            Ok(RangedAnswer::Window)
        }
        reqwest::StatusCode::OK => Ok(RangedAnswer::WholeObject),
        other => Err(format!("the server answered {} to a ranged read", other)),
    }
}

/// The same reading, taken through a session the caller owns. For a caller
/// that builds its own connections and has no template to clone from.
pub async fn read_range_source_through(
    session: &mut dyn super::StorageProvider,
    remote_path: &str,
) -> Result<RangeSourceFingerprint, String> {
    session
        .stat(remote_path)
        .await
        .map(|entry| RangeSourceFingerprint::of(&entry))
        .map_err(|e| format!("it could not be read ({})", e))
}

/// The comparison, taken through a session the caller already owns.
///
/// A caller that holds an open session on the object uses this rather than
/// [`range_source_changed`]: opening one of its own costs a full handshake,
/// measured at about 1.3 seconds on an SFTP link, on every transfer, whether
/// or not anything changed. That cost is fixed, so it weighs most exactly
/// where the transfer is fastest.
pub async fn range_source_changed_through(
    session: &mut dyn super::StorageProvider,
    remote_path: &str,
    before: &RangeSourceFingerprint,
) -> Option<String> {
    let first = match read_range_source_through(session, remote_path).await {
        Ok(after) => return before.differs_from(&after),
        Err(why) => why,
    };
    // This reading decides whether bytes already on disk are kept, so one
    // hiccup should not throw away a download that is finished and correct.
    // The reading before the windows pays a failure for nothing; this one
    // pays it after the expensive part. A session that is really gone fails
    // the second time too, and the refusal stands.
    log::warn!(
        "segmented download: {} could not be read after the transfer ({}), reading once more",
        remote_path,
        first
    );
    tokio::time::sleep(AFTER_TRANSFER_READ_RETRY).await;
    match read_range_source_through(session, remote_path).await {
        Ok(after) => before.differs_from(&after),
        Err(why) => Some(format!(
            "it could not be read again after the transfer: {why}"
        )),
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

/// The temporary the concurrent range engine writes: pre-sized to the whole
/// length, with holes until every window has landed. It has a name of its
/// own because `<final>.aerotmp` is the part a resumable download goes on
/// from, by its length: an engine temporary left at full size (a crash, a
/// kill, a second Ctrl-C) was taken for a complete part, and the next download
/// published it, holes included, as the file.
pub fn segmented_temp_path_for(final_path: &Path) -> PathBuf {
    let mut p = final_path.as_os_str().to_owned();
    p.push(".aerosegtmp");
    PathBuf::from(p)
}

/// Transport-agnostic concurrent range download orchestrator (PD-SFTP-2).
///
/// This is the single shared engine for splitting a large file into N
/// gap-free windows downloaded in parallel into a pre-allocated `.aerosegtmp`.
/// Everything that is *not* transport-specific lives here exactly once: the
/// range plan, the temp pre-allocation, the [`TempFileGuard`] RAII cleanup
/// (the F-1 lesson), bounded concurrency, progress aggregation, cooperative
/// cancellation, and the commit/atomic-ready temp.
///
/// `write_one_range(start, end, temp_path, bytes, cancel, mark)` is the only
/// transport-specific part. It must write **exactly** the inclusive
/// `[start, end]` window at its absolute offset into `temp_path`, bump
/// `bytes` (the range's share of the progress) by the bytes it wrote, advance
/// `mark` to the first byte it has not yet written without a gap (see
/// [`RangeMark`]; a range that fails with a recoverable error is called again
/// with `start` at the mark, at most [`RANGE_RETRIES`] times in a row without
/// progress), honour `cancel`, and return:
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
) -> Result<SegmentedRun, ProviderError>
where
    W: Fn(u64, u64, PathBuf, Arc<AtomicU64>, CancellationToken, RangeMark) -> WFut
        + Send
        + Sync
        + 'static,
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

    let temp_path = segmented_temp_path_for(&config.final_path);
    // DAG-P2-01b: a segmented transfer remains one job for the global
    // governor. When the caller already holds that job (a batch file, a
    // single-file graph, a sync tree), this acquire joins it instead of
    // taking a second endpoint or disk slot. On its own it still takes one
    // job, and range fan-out does not multiply permits. The publication is
    // the caller's, under the claim the run hands back with the temporary.
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
    // On Unix the temporary is claimed for the whole run (see
    // `atomic_write::temp_claim`): a second segmented download of the same
    // file is refused instead of truncating this one's windows, and a stale
    // one is replaced. The windows write through handles of their own, which
    // an advisory lock leaves alone; Windows locks are not advisory, so there
    // the temporary is created as before. A completed run hands the claim to
    // its caller with the temporary (`SegmentedTemp`).
    #[cfg(unix)]
    let claim = {
        let temp = temp_path.clone();
        let file = tokio::task::spawn_blocking(move || {
            crate::providers::atomic_write::temp_claim::create_fresh(&temp)
        })
        .await
        .map_err(|e| ProviderError::IoError(std::io::Error::other(e)))?
        .map_err(ProviderError::IoError)?;
        file.set_len(config.total_size)
            .map_err(ProviderError::IoError)?;
        file
    };
    #[cfg(not(unix))]
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
            Ok(SegmentedRun::Completed(SegmentedTemp {
                held: Some(ClaimedTemp {
                    path: temp_path,
                    #[cfg(unix)]
                    _claim: claim,
                }),
            }))
        }
        ConcurrentRangeOutcome::ServerIgnoredRange => Ok(SegmentedRun::ServerIgnoredRange),
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
    W: Fn(u64, u64, PathBuf, Arc<AtomicU64>, CancellationToken, RangeMark) -> WFut
        + Send
        + Sync
        + 'static,
    WFut: std::future::Future<Output = Result<ConcurrentRangeOutcome, ProviderError>>
        + Send
        + 'static,
{
    let range_bytes: Vec<Arc<AtomicU64>> =
        ranges.iter().map(|_| Arc::new(AtomicU64::new(0))).collect();
    let semaphore = Arc::new(Semaphore::new(max_parallel.max(1)));
    let mut join_set: JoinSet<Result<ConcurrentRangeOutcome, ProviderError>> = JoinSet::new();

    for (&(start, end), bytes) in ranges.iter().zip(&range_bytes) {
        let semaphore = semaphore.clone();
        let write_one_range = write_one_range.clone();
        let bytes = bytes.clone();
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
            write_range_resuming(&*write_one_range, start, end, &temp_path, bytes, &cancel).await
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
                    cb(total_range_bytes(&range_bytes), total_size);
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
    W: Fn(u64, u64, PathBuf, Arc<AtomicU64>, CancellationToken, RangeMark) -> WFut
        + Send
        + Sync
        + 'static,
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
    // Each range's share of the progress, summed for the report: a range read
    // again after a failure takes back what it wrote past its mark.
    let range_bytes: Arc<Vec<Arc<AtomicU64>>> =
        Arc::new(ranges.iter().map(|_| Arc::new(AtomicU64::new(0))).collect());
    let temp_path: Arc<PathBuf> = Arc::new(temp_path.to_path_buf());
    let server_ignored = Arc::new(AtomicBool::new(false));
    let first_error: Arc<Mutex<Option<ProviderError>>> = Arc::new(Mutex::new(None));
    let progress: SharedProgress = Arc::new(Mutex::new(on_progress));
    let report_progress = {
        let range_bytes = range_bytes.clone();
        let last_reported = AtomicU64::new(0);
        Arc::new(move || {
            // Read the range totals and update the watermark under the callback
            // lock, so timer ticks and range completions cannot duplicate or
            // regress a report. Empty/stalled ticks produce no callback.
            let callback = progress.lock().unwrap();
            let done = total_range_bytes(&range_bytes);
            if let Some(callback) = callback.as_ref() {
                if done > last_reported.load(Ordering::Relaxed) {
                    last_reported.store(done, Ordering::Relaxed);
                    callback(done, total_size);
                }
            }
        })
    };

    let r_ranges = ranges.clone();
    let r_write = write_one_range.clone();
    let r_range_bytes = range_bytes.clone();
    let r_temp = temp_path.clone();
    let r_cancel = cancel.clone();
    let r_ignored = server_ignored.clone();
    let r_first_error = first_error.clone();
    let r_report_progress = report_progress.clone();

    let runner: Arc<dyn DagNodeRunner> = Arc::new(move |node: TransferNode| -> NodeFuture {
        let ranges = r_ranges.clone();
        let write_one_range = r_write.clone();
        let range_bytes = r_range_bytes.clone();
        let temp_path = r_temp.clone();
        let cancel = r_cancel.clone();
        let server_ignored = r_ignored.clone();
        let first_error = r_first_error.clone();
        let report_progress = r_report_progress.clone();
        Box::pin(async move {
            let (start, end) = ranges[node.id];
            // Mirror the JoinSet path's pre-write cancel check.
            if cancel.is_cancelled() {
                return NodeOutcome::Failed(
                    TransferError::cancelled().with_scope(FailureScope::Part),
                );
            }
            match write_range_resuming(
                &*write_one_range,
                start,
                end,
                &temp_path,
                range_bytes[node.id].clone(),
                &cancel,
            )
            .await
            {
                Ok(ConcurrentRangeOutcome::Completed) => {
                    report_progress();
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
    );
    tokio::pin!(exec);
    let mut progress_tick = tokio::time::interval(std::time::Duration::from_millis(250));
    progress_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Drive the timer with the graph future, not a detached task: completion,
    // errors and cancellation end periodic reporting with the same lifetime.
    let exec = loop {
        tokio::select! {
            biased;
            result = &mut exec => break result,
            _ = progress_tick.tick() => report_progress(),
        }
    };

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
/// `.aerosegtmp`, aggregates progress, and cleans up on every non-success exit.
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
) -> Result<SegmentedRun, ProviderError>
where
    F: Fn(u64, u64) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<reqwest::Response, ProviderError>> + Send + 'static,
{
    let fetch_range = Arc::new(fetch_range);
    let write_one_range = move |start: u64,
                                end: u64,
                                temp_path: PathBuf,
                                aggregate: Arc<AtomicU64>,
                                cancel: CancellationToken,
                                mark: RangeMark| {
        let fetch_range = fetch_range.clone();
        async move {
            download_one_strict_range(
                fetch_range.as_ref(),
                &temp_path,
                start,
                end,
                &aggregate,
                &cancel,
                &mark,
            )
            .await
        }
    };
    run_concurrent_range_download(cfg, write_one_range, cancel, on_progress).await
}

/// A ranged read that failed on the wire, on its request or on its body: a
/// timeout, a request that got no answer or a body the connection dropped
/// halfway is recoverable, so the range is resumed from what it wrote
/// ([`write_range_resuming`]); anything else (a refused redirect, a body that
/// does not decode) fails it. Every HTTP `read_range` maps its request and
/// its body with it, and so do the windows of
/// [`try_http_concurrent_range_download`].
pub(crate) fn range_read_error(e: reqwest::Error) -> ProviderError {
    super::http_send_error(e, |e| {
        if e.is_body() || e.is_request() || e.is_connect() {
            ProviderError::NetworkError(e.to_string())
        } else if connection_ended_under(&e) {
            ProviderError::NetworkError(format!(
                "{e}: the connection closed before the window ended"
            ))
        } else {
            ProviderError::TransferFailed(e.to_string())
        }
    })
}

/// Whether an error was caused by the connection ending under it. A body cut
/// halfway reaches the reader as a decode error, since `reqwest` files every
/// failure of the body stream under it; its cause is hyper's
/// `IncompleteBody`, an `UnexpectedEof`.
fn connection_ended_under(e: &(dyn std::error::Error + 'static)) -> bool {
    use std::io::ErrorKind;
    let mut cause = e.source();
    while let Some(c) = cause {
        if let Some(io) = c.downcast_ref::<std::io::Error>() {
            if matches!(
                io.kind(),
                ErrorKind::UnexpectedEof
                    | ErrorKind::ConnectionReset
                    | ErrorKind::ConnectionAborted
                    | ErrorKind::BrokenPipe
                    | ErrorKind::TimedOut
            ) {
                return true;
            }
        }
        cause = c.source();
    }
    false
}

/// Download exactly one `[start, end]` window under the strict gate.
async fn download_one_strict_range<F, Fut>(
    fetch_range: &F,
    temp_path: &Path,
    start: u64,
    end: u64,
    aggregate: &AtomicU64,
    cancel: &CancellationToken,
    mark: &RangeMark,
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
    let copied: Result<(), ProviderError> = async {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    return Err(ProviderError::TransferFailed(
                        "Transfer cancelled by user".to_string(),
                    ));
                }
                next = stream.next() => {
                    let Some(chunk) = next else { break };
                    let chunk = chunk.map_err(range_read_error)?;
                    let chunk_len = chunk.len() as u64;
                    crate::transfer_dag::throttle::charge(crate::transfer_dag::governor::TransferDirection::Download, chunk_len.min(expected - written)).await;
                    if written + chunk_len > expected {
                        let allowed = (expected - written) as usize;
                        file.write_all(&chunk[..allowed])
                            .await
                            .map_err(ProviderError::IoError)?;
                        aggregate.fetch_add(allowed as u64, Ordering::Relaxed);
                        written = expected;
                        mark.advance(start + written);
                        break;
                    }
                    file.write_all(&chunk)
                        .await
                        .map_err(ProviderError::IoError)?;
                    aggregate.fetch_add(chunk_len, Ordering::Relaxed);
                    written += chunk_len;
                    mark.advance(start + written);
                }
            }
        }
        if written != expected {
            // The body ended before the window did: the connection closed
            // early, and the window goes on from its mark.
            return Err(ProviderError::NetworkError(format!(
                "Concurrent range download truncated: expected {} bytes, got {}",
                expected, written
            )));
        }
        Ok(())
    }
    .await;
    settle_range_attempt(&mut file, copied).await?;

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
    /// Size the caller already knows, if any. Below `cutoff` the probe is
    /// skipped entirely: no round trip is spent on a decision already made.
    pub known_size: Option<u64>,
}

/// True when a size the caller already knows rules out a multi-stream
/// download, so no server round trip (HEAD, stat, Range probe) is worth
/// spending on the decision. An unknown size never rules it out.
pub(crate) fn size_hint_rules_out_ranges(size_hint: Option<u64>, cutoff: u64) -> bool {
    matches!(size_hint, Some(size) if size < cutoff)
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
    // R21: the shared planner decides; a known size that cannot yield two
    // honest windows skips the probe entirely (no round trip spent on a
    // decision already made). Both callers (WebDAV, Koofr) have no provider
    // cutoff floor.
    if let Some(size) = req.known_size {
        if crate::provider_transfer_executor::plan_segment_count(
            size,
            req.streams,
            req.max_streams,
            crate::provider_transfer_executor::SegmentCutoff::Explicit(req.cutoff),
            0,
        ) < 2
        {
            return HttpRangeAttempt::Fallback(on_progress);
        }
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
    // The probe is also where the version comes from. HTTP has a validator,
    // so the windows can be pinned to the object the probe saw: without one,
    // an object replaced while they are in flight is assembled out of two
    // versions, with the right length and nothing to tell it apart. A weak
    // tag is not used, because If-Match compares strongly and would refuse
    // every window.
    let validator = probe
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty() && !tag.starts_with("W/"));
    let last_modified = probe
        .headers()
        .get(reqwest::header::LAST_MODIFIED)
        .and_then(|v| v.to_str().ok())
        .map(|value| value.to_string());
    if validator.is_none() && last_modified.is_none() {
        tracing::warn!(
            "[multi-thread] {} has no validator and no modification time: a same-size replacement while the windows read it cannot be detected",
            req.url
        );
    }

    let total = match content_range
        .as_deref()
        .and_then(parse_content_range)
        .and_then(|(_, _, total)| total)
    {
        Some(t) if t > 0 => t,
        // Unknown length: not safe to plan a concurrent split.
        _ => return HttpRangeAttempt::Fallback(on_progress),
    };
    // R21: same planner as the batch and pget paths: explicit cutoff, 16
    // clamp, every window at least 1 MiB, fewer than 2 windows falls back.
    let planned = crate::provider_transfer_executor::plan_segment_count(
        total,
        req.streams,
        req.max_streams,
        crate::provider_transfer_executor::SegmentCutoff::Explicit(req.cutoff),
        0,
    );
    if planned < 2 {
        return HttpRangeAttempt::Fallback(on_progress);
    }

    // --- Concurrent path (server proved 206 honesty) ---------------------
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
        streams: planned,
        max_streams: req.max_streams,
        max_parallel: planned,
    };

    let client = req.client.clone();
    let url: Arc<str> = Arc::from(req.url.as_str());
    let headers = Arc::new(req.headers.clone());
    let pin: Arc<Option<String>> = Arc::new(validator.clone());
    let fetch_range = move |start: u64, end: u64| {
        let client = client.clone();
        let url = url.clone();
        let headers = headers.clone();
        let pin = pin.clone();
        async move {
            let mut rb = client.get(url.as_ref());
            for (k, v) in headers.iter() {
                rb = rb.header(k.clone(), v.clone());
            }
            if let Some(tag) = pin.as_ref() {
                rb = rb.header(reqwest::header::IF_MATCH, tag.as_str());
            }
            let range = HeaderValue::from_str(&format!("bytes={}-{}", start, end))
                .map_err(|e| ProviderError::TransferFailed(format!("Invalid Range: {}", e)))?;
            let request = rb
                .header(RANGE, range)
                .build()
                .map_err(|e| ProviderError::TransferFailed(format!("Build request: {}", e)))?;
            let response = send_with_retry(&client, request, &HttpRetryConfig::default())
                .await
                .map_err(range_read_error)?;
            if response.status() == reqwest::StatusCode::PRECONDITION_FAILED {
                return Err(ProviderError::ParallelRefused(source_changed(
                    "http range",
                    url.as_ref(),
                    "the server refused a window pinned to the version the probe saw",
                )));
            }
            Ok(response)
        }
    };

    let (on_progress, fallback_progress) = share_progress(on_progress);
    match download_via_concurrent_range(cfg, fetch_range, CancellationToken::new(), on_progress)
        .await
    {
        Ok(SegmentedRun::Completed(temp)) => {
            if let Some(what) =
                http_range_source_moved(&req, validator.as_deref(), last_modified.as_deref(), total)
                    .await
            {
                temp.discard();
                tracing::warn!(
                    "[multi-thread] {}; single-stream fallback",
                    source_changed("http range", &req.url, &what)
                );
                return HttpRangeAttempt::Fallback(fallback_progress);
            }
            match temp.publish(Path::new(&req.local_path)).await {
                Ok(()) => HttpRangeAttempt::Completed,
                Err(e) => HttpRangeAttempt::Failed(ProviderError::IoError(e)),
            }
        }
        Ok(SegmentedRun::ServerIgnoredRange) => {
            // Server honoured the probe then ignored a real Range: rare
            // inconsistency. The helper already removed the temp.
            tracing::warn!(
                "[multi-thread] range_ignored mid-flight after a good probe for {}; single-stream fallback",
                req.url
            );
            HttpRangeAttempt::Fallback(fallback_progress)
        }
        Err(e @ ProviderError::ParallelRefused(_)) => {
            // A window was refused because the object moved. That is not a
            // failed download: one stream reads one consistent view, which is
            // exactly what the windows could not promise. The engine already
            // removed the staged file and preserved the typed refusal.
            tracing::warn!("[multi-thread] {}; single-stream fallback", e);
            HttpRangeAttempt::Fallback(fallback_progress)
        }
        Err(e) => HttpRangeAttempt::Failed(e),
    }
}

/// `Some(reason)` when the object changed or its previous evidence cannot
/// be checked anymore.
///
/// A second one-byte probe on the same client, so the cost is a request and
/// not a connection. It exists for the servers that ignore `If-Match`: where
/// the pin is honoured a window is refused mid-flight and never reaches here.
/// One retry, because this reading decides whether bytes already on disk are
/// kept.
async fn http_range_source_moved(
    req: &HttpRangeRequest,
    validator: Option<&str>,
    last_modified: Option<&str>,
    total: u64,
) -> Option<String> {
    // The initial probe always supplied the size used to plan the windows.
    // It remains evidence even when the server has no version validator.
    let mut last_error = None;
    for attempt in 0..2 {
        if attempt == 1 {
            tokio::time::sleep(AFTER_TRANSFER_READ_RETRY).await;
        }
        let mut rb = req.client.get(&req.url);
        for (k, v) in req.headers.iter() {
            rb = rb.header(k.clone(), v.clone());
        }
        let request = match rb
            .header(RANGE, HeaderValue::from_static("bytes=0-0"))
            .build()
        {
            Ok(request) => request,
            Err(e) => {
                last_error = Some(e.to_string());
                continue;
            }
        };
        let response =
            match send_with_retry(&req.client, request, &HttpRetryConfig::default()).await {
                Ok(response) => response,
                Err(e) => {
                    last_error = Some(e.to_string());
                    continue;
                }
            };
        // A non-success answer carries none of the headers compared below, so
        // every comparison would be skipped and the silence would read as "the
        // object did not change". An answer that cannot be compared is a
        // failed reading, not a passed one.
        if !matches!(
            response.status(),
            reqwest::StatusCode::PARTIAL_CONTENT | reqwest::StatusCode::OK
        ) {
            last_error = Some(format!("the probe answered {}", response.status()));
            continue;
        }
        let now_tag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|tag| tag.trim().to_string());
        match (validator, now_tag.as_deref()) {
            (Some(before), Some(after)) if before != after => {
                return Some(format!("entity tag {} became {}", before, after));
            }
            // A tag that was there and is not any more is a difference, not a
            // field to skip: it is how a replacement hides from a comparison
            // that only looks at what both answers publish.
            (Some(before), None) => {
                return Some(format!(
                    "entity tag {} is no longer reported, so nothing pins the bytes",
                    before
                ));
            }
            _ => {}
        }
        let now_modified = response
            .headers()
            .get(reqwest::header::LAST_MODIFIED)
            .and_then(|v| v.to_str().ok());
        let now_total = if response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
            response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_content_range)
                .filter(|(start, end, _)| *start == 0 && *end == 0)
                .and_then(|(_, _, total)| total)
                .filter(|total| *total > 0)
        } else {
            // A 200 ignores Range: its Content-Length describes the whole
            // object. On a 206 it would only describe the one-byte probe.
            response
                .headers()
                .get(reqwest::header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
        };
        let Some(now_total) = now_total else {
            last_error = Some("the probe no longer reports a comparable object size".to_string());
            continue;
        };
        let before = RangeSourceFingerprint {
            size: total,
            modified: last_modified.map(str::to_owned),
            validator: validator.map(str::to_owned),
        };
        let after = RangeSourceFingerprint {
            size: now_total,
            modified: now_modified.map(str::to_owned),
            validator: now_tag,
        };
        return before.differs_from(&after);
    }
    Some(format!(
        "it could not be read again after the transfer: {}",
        last_error.unwrap_or_else(|| "no answer".to_string())
    ))
}

/// A loopback server for the tests of every HTTP `read_range`: it answers a
/// ranged GET (`Range`, or Azure's `x-ms-range`) with that window of
/// [`OBJECT_SIZE`](window_server::OBJECT_SIZE) bytes of
/// [`byte_at`](window_server::byte_at), one request per connection, and fails
/// the first request for the window that starts at a given offset in the way
/// it is told. Raw HTTP rather than a router: a request left without an
/// answer is a closed connection, which a handler cannot produce.
#[cfg(test)]
pub(crate) mod window_server {
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    pub(crate) const OBJECT_SIZE: u64 = 4 * 1024 * 1024;

    /// The byte at `offset` of the object.
    pub(crate) fn byte_at(offset: u64) -> u8 {
        (offset % 251) as u8
    }

    /// How the first request for the failing window fails.
    #[derive(Clone, Copy, Debug)]
    pub(crate) enum WireFailure {
        /// The connection closes once the request is read, with no answer.
        NoAnswer,
        /// The head of the answer and half the window arrive, then the
        /// connection closes.
        CutHalfway,
    }

    pub(crate) struct WindowServer {
        pub(crate) addr: SocketAddr,
        /// Requests for the failing window, the failed one included.
        pub(crate) failing_window_requests: Arc<AtomicUsize>,
    }

    /// Start the server; the first request for the window starting at
    /// `fail_at` fails with `failure`, every other request is answered.
    pub(crate) async fn start(failure: WireFailure, fail_at: u64) -> WindowServer {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let failed = Arc::new(AtomicBool::new(false));
        let failing_window_requests = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&failing_window_requests);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let failed = Arc::clone(&failed);
                let counter = Arc::clone(&counter);
                tokio::spawn(async move {
                    let Some((start, end)) = read_window_request(&mut stream).await else {
                        let _ = stream
                            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .await;
                        return;
                    };
                    let end = end.min(OBJECT_SIZE - 1);
                    let fails = start == fail_at && {
                        counter.fetch_add(1, Ordering::SeqCst);
                        !failed.swap(true, Ordering::SeqCst)
                    };
                    if fails && matches!(failure, WireFailure::NoAnswer) {
                        return;
                    }
                    let body: Vec<u8> = (start..=end).map(byte_at).collect();
                    let head = format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{OBJECT_SIZE}\r\nContent-Length: {}\r\nETag: \"v1\"\r\nLast-Modified: Wed, 01 Jan 2025 00:00:00 GMT\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let sent = if fails {
                        &body[..body.len() / 2]
                    } else {
                        &body[..]
                    };
                    if stream.write_all(head.as_bytes()).await.is_ok()
                        && stream.write_all(sent).await.is_ok()
                    {
                        let _ = stream.flush().await;
                    }
                });
            }
        });
        WindowServer {
            addr,
            failing_window_requests,
        }
    }

    /// The property every HTTP `read_range` keeps, for both failures: the
    /// first request for a window fails with an error the range writer
    /// resumes from its mark ([`is_recoverable`]), and the window asked again
    /// is the right one. `connect` gives a connected provider that reads
    /// `/big.bin` from the server at the address.
    ///
    /// [`is_recoverable`]: crate::providers::ProviderError::is_recoverable
    pub(crate) async fn assert_a_dropped_window_is_recoverable(
        connect: impl Fn(SocketAddr) -> Box<dyn crate::providers::StorageProvider>,
    ) {
        const OFFSET: u64 = 1024 * 1024 + 4096;
        const LEN: u64 = 64 * 1024;
        for failure in [WireFailure::NoAnswer, WireFailure::CutHalfway] {
            let server = start(failure, OFFSET).await;
            let mut provider = connect(server.addr);
            let error = provider
                .read_range("/big.bin", OFFSET, LEN)
                .await
                .expect_err("the first request for the window fails");
            assert!(error.is_recoverable(), "{failure:?}: {error:?}");
            let window = provider
                .read_range("/big.bin", OFFSET, LEN)
                .await
                .unwrap_or_else(|e| panic!("{failure:?}: the window asked again: {e}"));
            assert_eq!(window.len() as u64, LEN, "{failure:?}");
            assert!(
                window.iter().zip(OFFSET..).all(|(&b, o)| b == byte_at(o)),
                "{failure:?}: the window asked again is not the right one"
            );
            assert_eq!(
                server.failing_window_requests.load(Ordering::SeqCst),
                2,
                "{failure:?}"
            );
        }
    }

    /// Read one request head; the window it asks for, or `None` for a
    /// request without a range.
    async fn read_window_request(stream: &mut tokio::net::TcpStream) -> Option<(u64, u64)> {
        let mut head = Vec::new();
        let mut chunk = [0u8; 4096];
        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = stream.read(&mut chunk).await.ok()?;
            if n == 0 {
                return None;
            }
            head.extend_from_slice(&chunk[..n]);
        }
        String::from_utf8_lossy(&head).lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            let name = name.trim().to_ascii_lowercase();
            if name != "range" && name != "x-ms-range" {
                return None;
            }
            let (start, end) = value.trim().strip_prefix("bytes=")?.split_once('-')?;
            Some((start.parse().ok()?, end.parse().ok()?))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::HttpResultExt;

    const MAX: usize = 16;

    /// The two halves of the rule, and the two ways an answer can look right
    /// and not be: a `Content-Range` that starts like the one asked for but
    /// does not parse, and a body that is not the length it just declared.
    #[test]
    fn a_ranged_answer_is_the_window_or_it_is_refused() {
        use reqwest::StatusCode;
        let ok = ranged_answer(
            StatusCode::PARTIAL_CONTENT,
            Some("bytes 1048576-2097151/8388608"),
            1048576,
            1048576,
            2097151,
        );
        assert!(matches!(ok, Ok(RangedAnswer::Window)), "{ok:?}");

        let malformed = ranged_answer(
            StatusCode::PARTIAL_CONTENT,
            Some("bytes 1048576-2097151/not-a-total"),
            1048576,
            1048576,
            2097151,
        )
        .expect_err("a Content-Range that does not parse is not the window");
        assert!(malformed.contains("not-a-total"), "{malformed}");

        let other_window = ranged_answer(
            StatusCode::PARTIAL_CONTENT,
            Some("bytes 0-1048575/8388608"),
            1048576,
            1048576,
            2097151,
        )
        .expect_err("another window is not this one");
        assert!(
            other_window.contains("bytes 1048576-2097151"),
            "{other_window}"
        );

        let short = ranged_answer(
            StatusCode::PARTIAL_CONTENT,
            Some("bytes 0-1023/8388608"),
            512,
            0,
            1023,
        )
        .expect_err("a body shorter than the window it declared");
        assert!(short.contains("512"), "{short}");

        let oversized = ranged_answer(
            StatusCode::PARTIAL_CONTENT,
            Some("bytes 0-1023/8388608"),
            4096,
            0,
            1023,
        )
        .expect_err("a body longer than the window it declared");
        assert!(oversized.contains("4096"), "{oversized}");

        assert!(matches!(
            ranged_answer(StatusCode::OK, None, 8388608, 1048576, 2097151),
            Ok(RangedAnswer::WholeObject)
        ));
        assert!(ranged_answer(StatusCode::NO_CONTENT, None, 0, 0, 1023).is_err());
    }

    /// Both refusals are written in one place so a log or an error reads the
    /// same wherever it comes from, and so a reworded message cannot drift
    /// apart from the one a reader greps for.
    #[test]
    fn both_refusals_name_themselves() {
        let refused = parallel_refused("segmented download", "/big.bin", "it timed out");
        assert!(refused.contains(PARALLEL_REFUSED_MARKER), "{refused}");
        assert!(refused.contains("/big.bin"), "{refused}");
        let changed = source_changed("segmented download", "/big.bin", "size 10 became 11");
        assert!(changed.contains(SOURCE_CHANGED_MARKER), "{changed}");
        assert!(changed.contains("size 10 became 11"), "{changed}");
    }

    /// The half kept for the fallback still reports after the half that went
    /// with the parallel attempt is gone, and both reach the one callback.
    #[test]
    fn a_shared_progress_callback_outlives_the_attempt_that_took_half() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let (attempt, fallback) = share_progress(Some(Box::new(move |done, total| {
            sink.lock().unwrap().push((done, total));
        })));
        let attempt = attempt.expect("a callback was given");
        attempt(1, 10);
        drop(attempt);
        fallback.expect("a callback was given")(7, 10);
        assert_eq!(*seen.lock().unwrap(), vec![(1, 10), (7, 10)]);
        let (none_a, none_b) = share_progress(None);
        assert!(none_a.is_none() && none_b.is_none());
    }

    /// One refused connection does not decide the fate of a finished
    /// download; two do.
    #[tokio::test]
    async fn the_after_transfer_session_is_opened_a_second_time() {
        let calls = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&calls);
        let opened: Result<&str, &str> = open_after_transfer(|| {
            let counter = Arc::clone(&counter);
            async move {
                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err("connection refused")
                } else {
                    Ok("session")
                }
            }
        })
        .await;
        assert_eq!(opened, Ok("session"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let never: Result<&str, &str> = open_after_transfer(|| async { Err("down") }).await;
        assert_eq!(never, Err("down"));
    }

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
    ) -> impl Fn(u64, u64, PathBuf, Arc<AtomicU64>, CancellationToken, RangeMark) -> BoxedRangeFut
           + Send
           + Sync
           + Clone
           + 'static {
        |start, end, temp_path, aggregate, _cancel, mark: RangeMark| {
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
                mark.advance(end + 1);
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
        let temp_a = segmented_temp_path_for(&dir.join("a.bin"));
        let temp_b = segmented_temp_path_for(&dir.join("b.bin"));
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
        let temp = segmented_temp_path_for(&dir.join("m.bin"));
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
        let temp = segmented_temp_path_for(&dir.join("t.bin"));
        prealloc(&temp, total).await;

        let writer = Arc::new(
            move |start: u64,
                  _end: u64,
                  temp_path: PathBuf,
                  aggregate: Arc<AtomicU64>,
                  _cancel: CancellationToken,
                  _mark: RangeMark|
                  -> BoxedRangeFut {
                Box::pin(async move {
                    let client = reqwest::Client::new();
                    let request = client
                        .get(format!("http://{addr}/object"))
                        .build()
                        .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
                    let response = send_with_retry(&client, request, &HttpRetryConfig::default())
                        .await
                        .map_http_err(|e| ProviderError::TransferFailed(e.to_string()))?;
                    let body = response
                        .bytes()
                        .await
                        .map_http_err(|e| ProviderError::TransferFailed(e.to_string()))?;
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
        let temp_a = segmented_temp_path_for(&dir.join("a.bin"));
        let temp_b = segmented_temp_path_for(&dir.join("b.bin"));
        prealloc(&temp_a, total).await;
        prealloc(&temp_b, total).await;

        let w = Arc::new(
            |start: u64,
             _end: u64,
             _p: PathBuf,
             _a: Arc<AtomicU64>,
             _c: CancellationToken,
             _mark: RangeMark|
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
        let (out_b, summary_b) = run_ranges_via_graph(
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
        .unwrap();

        assert_eq!(out_a, ConcurrentRangeOutcome::ServerIgnoredRange);
        assert_eq!(out_b, ConcurrentRangeOutcome::ServerIgnoredRange);
        assert_eq!(out_a, out_b);
        // The honest fallback commits no bytes, so every byte counter the
        // graph summary carries stays zeroed, not just the outcome variant.
        assert_eq!(summary_b.metrics.bytes_transferred, 0);
        assert_eq!(summary_b.metrics.logical_bytes, 0);
        assert_eq!(summary_b.metrics.wire_bytes, 0);
        assert_eq!(summary_b.metrics.local_payload_bytes, 0);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn joinset_and_graph_agree_on_hard_error_variant() {
        let total: u64 = 1 << 20;
        let ranges = plan_multi_thread_ranges(total, 4, 16);

        let dir = scratch_dir("err");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let temp_a = segmented_temp_path_for(&dir.join("a.bin"));
        let temp_b = segmented_temp_path_for(&dir.join("b.bin"));
        prealloc(&temp_a, total).await;
        prealloc(&temp_b, total).await;

        let w = Arc::new(
            |start: u64,
             _end: u64,
             _p: PathBuf,
             _a: Arc<AtomicU64>,
             _c: CancellationToken,
             _mark: RangeMark|
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
        let temp = segmented_temp_path_for(&dir.join("cancel.bin"));
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
                      cancel: CancellationToken,
                      _mark: RangeMark|
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

    // ── A failed range resumes from its mark ──────────────────────────────
    //
    // A 25 GB SFTP download failed twice, from zero, because one request on
    // one of its four connections had no answer within 10 s. These pin what
    // a range does after such a failure, on both schedulers.

    /// One scripted attempt of [`scripted_writer`]'s target range: write
    /// `write` bytes from the attempt's start, the first `mark` of them right
    /// and the rest garbage the next attempt has to overwrite, then fail with
    /// `fail`.
    #[derive(Clone, Copy)]
    struct ScriptedAttempt {
        write: u64,
        mark: u64,
        fail: fn() -> ProviderError,
    }

    fn timeout() -> ProviderError {
        ProviderError::Timeout
    }

    /// The window ending at `target_end` follows `script`, one entry per
    /// attempt, and completes once the script runs out; every other window
    /// completes at once. Content is `(offset % 251)`, as in
    /// [`deterministic_writer`]. `starts` gets the start of every attempt at
    /// the target.
    fn scripted_writer(
        target_end: u64,
        script: Vec<ScriptedAttempt>,
        starts: Arc<std::sync::Mutex<Vec<u64>>>,
    ) -> impl Fn(u64, u64, PathBuf, Arc<AtomicU64>, CancellationToken, RangeMark) -> BoxedRangeFut
           + Send
           + Sync
           + 'static {
        move |start, end, temp_path, bytes, _cancel, mark: RangeMark| {
            let step = if end == target_end {
                let mut starts = starts.lock().unwrap();
                starts.push(start);
                script.get(starts.len() - 1).copied()
            } else {
                None
            };
            Box::pin(async move {
                let len = end - start + 1;
                let (write, good) = match step {
                    Some(s) => (s.write.min(len), s.mark.min(s.write).min(len)),
                    None => (len, len),
                };
                let buf: Vec<u8> = (0..write)
                    .map(|i| {
                        if i < good {
                            ((start + i) % 251) as u8
                        } else {
                            0xEE
                        }
                    })
                    .collect();
                let mut f = tokio::fs::OpenOptions::new()
                    .write(true)
                    .open(&temp_path)
                    .await
                    .map_err(ProviderError::IoError)?;
                f.seek(std::io::SeekFrom::Start(start))
                    .await
                    .map_err(ProviderError::IoError)?;
                f.write_all(&buf).await.map_err(ProviderError::IoError)?;
                f.flush().await.map_err(ProviderError::IoError)?;
                bytes.fetch_add(write, Ordering::Relaxed);
                mark.advance(start + good);
                match step {
                    Some(s) => Err((s.fail)()),
                    None => Ok(ConcurrentRangeOutcome::Completed),
                }
            })
        }
    }

    struct ScriptedRun {
        result: Result<ConcurrentRangeOutcome, ProviderError>,
        starts: Vec<u64>,
        reports: Vec<u64>,
        file: Vec<u8>,
        target: (u64, u64),
    }

    /// Run a 64 KiB download in four windows, the second following `script`.
    async fn run_scripted(graph: bool, tag: &str, script: Vec<ScriptedAttempt>) -> ScriptedRun {
        let total: u64 = 1 << 16;
        let ranges = plan_multi_thread_ranges(total, 4, MAX);
        let target = ranges[1];
        let dir = scratch_dir(tag);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let temp = segmented_temp_path_for(&dir.join("r.bin"));
        prealloc(&temp, total).await;

        let starts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let reports = Arc::new(std::sync::Mutex::new(Vec::new()));
        let on_progress: Box<dyn Fn(u64, u64) + Send> = {
            let reports = reports.clone();
            Box::new(move |done, _total| reports.lock().unwrap().push(done))
        };
        let writer = Arc::new(scripted_writer(target.1, script, starts.clone()));
        let result = if graph {
            run_ranges_via_graph(
                &ranges,
                &temp,
                total,
                4,
                super::super::ProviderType::Sftp,
                crate::transfer_dag::EndpointIdentity::new("sftp", "resume-harness", ""),
                writer,
                CancellationToken::new(),
                Some(on_progress),
            )
            .await
            .map(|(outcome, _)| outcome)
        } else {
            run_ranges_via_joinset(
                &ranges,
                &temp,
                total,
                4,
                writer,
                CancellationToken::new(),
                Some(on_progress),
            )
            .await
        };
        let file = tokio::fs::read(&temp).await.unwrap();
        let _ = tokio::fs::remove_dir_all(&dir).await;
        let starts = starts.lock().unwrap().clone();
        let reports = reports.lock().unwrap().clone();
        ScriptedRun {
            result,
            starts,
            reports,
            file,
            target,
        }
    }

    fn assert_deterministic(file: &[u8]) {
        for (o, byte) in file.iter().enumerate() {
            assert_eq!(*byte, ((o as u64) % 251) as u8, "byte mismatch at {o}");
        }
    }

    /// A window that times out after writing goes on from its mark: the next
    /// attempt starts there, overwrites what the failed one left past it, and
    /// the progress counts every byte once.
    #[tokio::test(start_paused = true)]
    async fn a_range_that_times_out_resumes_from_its_mark() {
        for graph in [false, true] {
            let run = run_scripted(
                graph,
                &format!("resume-{graph}"),
                vec![ScriptedAttempt {
                    write: 3000,
                    mark: 1000,
                    fail: timeout,
                }],
            )
            .await;
            let (start, _) = run.target;
            assert_eq!(
                run.result.as_ref().ok(),
                Some(&ConcurrentRangeOutcome::Completed),
                "graph={graph}: {:?}",
                run.result
            );
            assert_eq!(run.starts, vec![start, start + 1000], "graph={graph}");
            assert_deterministic(&run.file);
            let total = run.file.len() as u64;
            assert_eq!(run.reports.last(), Some(&total), "graph={graph}");
            assert!(
                run.reports.iter().all(|&done| done <= total),
                "graph={graph}: a byte written twice was counted twice: {:?}",
                run.reports
            );
        }
    }

    /// Only a recoverable error is resumed: a hard one fails the download on
    /// its first occurrence, as before.
    #[tokio::test(start_paused = true)]
    async fn a_range_that_fails_hard_is_not_retried() {
        fn hard() -> ProviderError {
            ProviderError::TransferFailed("synthetic hard error".to_string())
        }
        for graph in [false, true] {
            let run = run_scripted(
                graph,
                &format!("hard-{graph}"),
                vec![ScriptedAttempt {
                    write: 1000,
                    mark: 1000,
                    fail: hard,
                }],
            )
            .await;
            assert!(
                matches!(run.result, Err(ProviderError::TransferFailed(_))),
                "graph={graph}: {:?}",
                run.result
            );
            assert_eq!(run.starts, vec![run.target.0], "graph={graph}");
        }
    }

    /// A provider that cannot clone and does not dial by itself, as Koofr,
    /// Cloudinary and ImageKit: after a disconnect every read answers
    /// `NotConnected` until `connect` runs. Its first read fails with
    /// `first`; the others serve the bytes of [`window_server::byte_at`].
    struct NoCloneWorker {
        connected: bool,
        first: Option<ProviderError>,
        reconnect: fn() -> Result<(), ProviderError>,
        calls: Arc<std::sync::Mutex<Vec<&'static str>>>,
    }

    #[async_trait::async_trait]
    impl super::super::StorageProvider for NoCloneWorker {
        async fn read_range(
            &mut self,
            _path: &str,
            offset: u64,
            len: u64,
        ) -> Result<Vec<u8>, ProviderError> {
            self.calls.lock().unwrap().push("read");
            if !self.connected {
                return Err(ProviderError::NotConnected);
            }
            if let Some(e) = self.first.take() {
                return Err(e);
            }
            Ok((offset..offset + len).map(window_server::byte_at).collect())
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn provider_type(&self) -> super::super::ProviderType {
            super::super::ProviderType::Koofr
        }
        fn display_name(&self) -> String {
            "no-clone".to_string()
        }
        async fn connect(&mut self) -> Result<(), ProviderError> {
            self.calls.lock().unwrap().push("connect");
            (self.reconnect)()?;
            self.connected = true;
            Ok(())
        }
        async fn disconnect(&mut self) -> Result<(), ProviderError> {
            self.calls.lock().unwrap().push("disconnect");
            self.connected = false;
            Ok(())
        }
        fn is_connected(&self) -> bool {
            self.connected
        }
        async fn list(
            &mut self,
            _path: &str,
        ) -> Result<Vec<super::super::RemoteEntry>, ProviderError> {
            Err(ProviderError::NotSupported("list".to_string()))
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
            _remote_path: &str,
            _local_path: &str,
            _progress: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("download".to_string()))
        }
        async fn download_to_bytes(
            &mut self,
            _remote_path: &str,
        ) -> Result<Vec<u8>, ProviderError> {
            Err(ProviderError::NotSupported("download_to_bytes".to_string()))
        }
        async fn upload(
            &mut self,
            _local_path: &str,
            _remote_path: &str,
            _progress: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("upload".to_string()))
        }
        async fn mkdir(&mut self, _path: &str) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("mkdir".to_string()))
        }
        async fn delete(&mut self, _path: &str) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("delete".to_string()))
        }
        async fn rmdir(&mut self, _path: &str) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("rmdir".to_string()))
        }
        async fn rmdir_recursive(&mut self, _path: &str) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("rmdir_recursive".to_string()))
        }
        async fn rename(&mut self, _from: &str, _to: &str) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("rename".to_string()))
        }
        async fn stat(&mut self, path: &str) -> Result<super::super::RemoteEntry, ProviderError> {
            Err(ProviderError::NotFound(path.to_string()))
        }
        async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
            Err(ProviderError::NotFound(path.to_string()))
        }
        async fn exists(&mut self, _path: &str) -> Result<bool, ProviderError> {
            Ok(true)
        }
        async fn keep_alive(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn server_info(&mut self) -> Result<String, ProviderError> {
            Ok("no-clone".to_string())
        }
    }

    /// One window of 64 KiB through a pool of one [`NoCloneWorker`], written
    /// the way `pget` writes its windows: a failed attempt hands its worker to
    /// [`replace_failed_range_worker`] and puts the one it gets back in the
    /// pool, and [`write_range_resuming`] runs the window again from its mark.
    async fn run_no_clone_pool(
        tag: &str,
        first: ProviderError,
        reconnect: fn() -> Result<(), ProviderError>,
    ) -> (
        Result<ConcurrentRangeOutcome, ProviderError>,
        Vec<u8>,
        Vec<&'static str>,
    ) {
        let total: u64 = 1 << 16;
        let dir = scratch_dir(tag);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let temp = segmented_temp_path_for(&dir.join("r.bin"));
        prealloc(&temp, total).await;
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let worker: Box<dyn super::super::StorageProvider> = Box::new(NoCloneWorker {
            connected: true,
            first: Some(first),
            reconnect,
            calls: calls.clone(),
        });
        let pool = Arc::new(tokio::sync::Mutex::new(std::collections::VecDeque::from(
            vec![worker],
        )));
        let write = |from: u64,
                     end: u64,
                     temp: PathBuf,
                     _bytes: Arc<AtomicU64>,
                     _cancel: CancellationToken,
                     mark: RangeMark| {
            let pool = pool.clone();
            async move {
                let mut provider = pool.lock().await.pop_front().expect("a worker");
                let mut file = tokio::fs::OpenOptions::new()
                    .write(true)
                    .open(&temp)
                    .await
                    .map_err(ProviderError::IoError)?;
                file.seek(std::io::SeekFrom::Start(from))
                    .await
                    .map_err(ProviderError::IoError)?;
                let copied: Result<(), ProviderError> = async {
                    let data = provider
                        .read_range("/big.bin", from, end - from + 1)
                        .await?;
                    file.write_all(&data)
                        .await
                        .map_err(ProviderError::IoError)?;
                    mark.advance(from + data.len() as u64);
                    Ok(())
                }
                .await;
                if let Err(e) = settle_range_attempt(&mut file, copied).await {
                    let (next, e) = replace_failed_range_worker(provider, None, e).await;
                    pool.lock().await.push_back(next);
                    return Err(e);
                }
                file.flush().await.map_err(ProviderError::IoError)?;
                pool.lock().await.push_back(provider);
                Ok(ConcurrentRangeOutcome::Completed)
            }
        };
        let result = write_range_resuming(
            &write,
            0,
            total - 1,
            &temp,
            Arc::new(AtomicU64::new(0)),
            &CancellationToken::new(),
        )
        .await;
        let file = tokio::fs::read(&temp).await.unwrap();
        let _ = tokio::fs::remove_dir_all(&dir).await;
        let calls = calls.lock().unwrap().clone();
        (result, file, calls)
    }

    /// `pget` takes any provider with ranged reads, Koofr, Cloudinary and
    /// ImageKit included, and none of them can clone or dials by itself after
    /// a disconnect: the worker a failed window left answered `NotConnected`
    /// to every retry, and the download failed as before the resume.
    #[tokio::test(start_paused = true)]
    async fn a_worker_that_cannot_clone_is_connected_again_for_the_retry() {
        let (result, file, calls) = run_no_clone_pool(
            "no-clone-retry",
            ProviderError::NetworkError("the connection closed".to_string()),
            || Ok(()),
        )
        .await;
        assert_eq!(result.ok(), Some(ConcurrentRangeOutcome::Completed));
        let expected: Vec<u8> = (0..file.len() as u64).map(window_server::byte_at).collect();
        assert!(file == expected, "the window holds the object's bytes");
        assert_eq!(calls, ["read", "disconnect", "connect", "read"]);
    }

    /// A reconnect that fails is what the range reports: a refused sign-in
    /// fails it at once, with its own error, not three `NotConnected` later.
    /// A hard failure is not resumed, so its worker is not dialled again.
    #[tokio::test(start_paused = true)]
    async fn a_reconnect_that_fails_is_what_the_range_reports() {
        let (result, _, calls) = run_no_clone_pool(
            "no-clone-refused",
            ProviderError::NetworkError("the connection closed".to_string()),
            || Err(ProviderError::AuthenticationFailed("refused".to_string())),
        )
        .await;
        assert!(
            matches!(result, Err(ProviderError::AuthenticationFailed(ref m)) if m == "refused"),
            "{result:?}"
        );
        assert_eq!(calls, ["read", "disconnect", "connect"]);

        let (result, _, calls) = run_no_clone_pool(
            "no-clone-hard",
            ProviderError::TransferFailed("refused range".to_string()),
            || Ok(()),
        )
        .await;
        assert!(
            matches!(result, Err(ProviderError::TransferFailed(ref m)) if m == "refused range"),
            "{result:?}"
        );
        assert_eq!(calls, ["read", "disconnect"]);
    }

    /// A range that fails without moving fails the download after
    /// `RANGE_RETRIES` retries, waiting 1, 2 and 4 s between them.
    #[tokio::test(start_paused = true)]
    async fn a_range_that_never_moves_fails_after_its_retries() {
        let stuck = ScriptedAttempt {
            write: 0,
            mark: 0,
            fail: timeout,
        };
        for graph in [false, true] {
            let began = tokio::time::Instant::now();
            let run = run_scripted(graph, &format!("stuck-{graph}"), vec![stuck; 10]).await;
            assert!(
                matches!(run.result, Err(ProviderError::Timeout)),
                "graph={graph}: {:?}",
                run.result
            );
            assert_eq!(
                run.starts,
                vec![run.target.0; RANGE_RETRIES as usize + 1],
                "graph={graph}"
            );
            assert!(
                began.elapsed() >= std::time::Duration::from_secs(7),
                "graph={graph}: retried after {:?}",
                began.elapsed()
            );
        }
    }

    /// An attempt that moved the mark starts the count again: a server that
    /// stalls now and then over a long transfer does not use the retries up.
    #[tokio::test(start_paused = true)]
    async fn a_range_that_keeps_moving_is_retried_past_the_limit() {
        let step = ScriptedAttempt {
            write: 100,
            mark: 100,
            fail: timeout,
        };
        let attempts = RANGE_RETRIES as usize * 2;
        for graph in [false, true] {
            let run = run_scripted(graph, &format!("moving-{graph}"), vec![step; attempts]).await;
            assert_eq!(
                run.result.as_ref().ok(),
                Some(&ConcurrentRangeOutcome::Completed),
                "graph={graph}: {:?}",
                run.result
            );
            let expected: Vec<u64> = (0..=attempts as u64)
                .map(|i| run.target.0 + 100 * i)
                .collect();
            assert_eq!(run.starts, expected, "graph={graph}");
            assert_deterministic(&run.file);
        }
    }

    /// A failure after the last byte (closing the remote handle) is not a
    /// reason to read the window again.
    #[tokio::test(start_paused = true)]
    async fn a_range_that_fails_after_its_last_byte_is_complete() {
        for graph in [false, true] {
            let run = run_scripted(
                graph,
                &format!("tail-{graph}"),
                vec![ScriptedAttempt {
                    write: u64::MAX,
                    mark: u64::MAX,
                    fail: timeout,
                }],
            )
            .await;
            assert_eq!(
                run.result.as_ref().ok(),
                Some(&ConcurrentRangeOutcome::Completed),
                "graph={graph}: {:?}",
                run.result
            );
            assert_eq!(run.starts, vec![run.target.0], "graph={graph}");
            assert_deterministic(&run.file);
        }
    }

    /// A cancel during the wait before a retry ends the range at once, as a
    /// cancel, and no further attempt is made.
    #[tokio::test(start_paused = true)]
    async fn a_cancel_during_the_retry_wait_ends_the_range() {
        let attempts = Arc::new(AtomicU64::new(0));
        let writer = {
            let attempts = attempts.clone();
            move |_start: u64,
                  _end: u64,
                  _path: PathBuf,
                  _bytes: Arc<AtomicU64>,
                  _cancel: CancellationToken,
                  _mark: RangeMark|
                  -> BoxedRangeFut {
                attempts.fetch_add(1, Ordering::Relaxed);
                Box::pin(async { Err(ProviderError::Timeout) })
            }
        };
        let cancel = CancellationToken::new();
        let began = tokio::time::Instant::now();
        let range = {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                write_range_resuming(
                    &writer,
                    0,
                    99,
                    Path::new("/nonexistent"),
                    Arc::new(AtomicU64::new(0)),
                    &cancel,
                )
                .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        cancel.cancel();
        let result = range.await.unwrap();
        assert!(
            matches!(result, Err(ProviderError::Cancelled)),
            "{result:?}"
        );
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
        assert!(began.elapsed() < std::time::Duration::from_secs(1));
    }

    /// A pool worker's failed read keeps its class, so a timeout or a lost
    /// connection is resumed and everything else still fails the range. They
    /// all came back as `TransferFailed`, which nothing resumes.
    #[test]
    fn a_failed_read_range_keeps_its_class() {
        let cases = [
            (ProviderError::Timeout, true),
            (ProviderError::NotConnected, true),
            (ProviderError::NetworkError("reset".into()), true),
            (ProviderError::ConnectionLost("closed".into()), true),
            (ProviderError::ParallelRefused("moved".into()), false),
            (ProviderError::ServerError("boom".into()), false),
            (ProviderError::NotFound("gone".into()), false),
        ];
        for (error, recoverable) in cases {
            let label = format!("{error:?}");
            let mapped = range_read_failed("pget", 42, error);
            assert_eq!(
                mapped.is_recoverable(),
                recoverable,
                "{label} -> {mapped:?}"
            );
        }
        assert!(matches!(
            range_read_failed("b2", 7, ProviderError::ParallelRefused("moved".into())),
            ProviderError::ParallelRefused(text) if text.contains("offset 7")
        ));
        assert!(matches!(
            range_read_failed("pget", 7, ProviderError::ServerError("boom".into())),
            ProviderError::TransferFailed(text) if text.contains("offset 7")
        ));
    }

    /// A write still in flight when an attempt failed is settled before the
    /// error goes back, and a write that fails there replaces the recoverable
    /// error with a hard one: the mark counted those bytes, and resuming past
    /// them would leave a hole. `/dev/full` accepts the write and fails it on
    /// flush, as a full disk does.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_failed_attempt_settles_its_writes_before_it_is_resumed() {
        let mut full = tokio::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .await
            .expect("/dev/full");
        full.write_all(&[1u8; 4096])
            .await
            .expect("the write is accepted");
        let settled = settle_range_attempt(&mut full, Err::<(), _>(ProviderError::Timeout)).await;
        assert!(
            matches!(settled, Err(ProviderError::IoError(_))),
            "{settled:?}"
        );

        let dir = tempfile::tempdir().unwrap();
        let mut file = tokio::fs::File::create(dir.path().join("ok.bin"))
            .await
            .unwrap();
        file.write_all(&[1u8; 4096]).await.unwrap();
        let settled = settle_range_attempt(&mut file, Err::<(), _>(ProviderError::Timeout)).await;
        assert!(
            matches!(settled, Err(ProviderError::Timeout)),
            "{settled:?}"
        );
    }

    /// A WebDAV window whose body stops halfway (the connection drops) is
    /// asked for again from where it stopped, and the download completes. The
    /// body error used to be a `TransferFailed` that failed the whole file.
    #[tokio::test]
    async fn an_http_window_cut_halfway_is_asked_again_from_its_mark() {
        const SIZE: u64 = 4 * 1024 * 1024;
        const WINDOW: u64 = SIZE / 4;
        let requests = Arc::new(std::sync::Mutex::new(Vec::<(u64, u64)>::new()));
        let cut_once = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let app = {
            let requests = requests.clone();
            axum::Router::new().fallback(axum::routing::any(move |req: axum::extract::Request| {
                let requests = requests.clone();
                let cut_once = cut_once.clone();
                async move {
                    let raw = req
                        .headers()
                        .get("range")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    let (start, end) = requested_window(&raw).unwrap_or((0, SIZE - 1));
                    requests.lock().unwrap().push((start, end));
                    let body: Vec<u8> = (start..=end).map(|o| (o % 251) as u8).collect();
                    let body = if start == WINDOW && !cut_once.swap(true, Ordering::SeqCst) {
                        let half = bytes::Bytes::from(body[..body.len() / 2].to_vec());
                        axum::body::Body::from_stream(futures_util::stream::iter(vec![
                            Ok::<_, std::io::Error>(half),
                            Err(std::io::Error::other("connection dropped")),
                        ]))
                    } else {
                        axum::body::Body::from(body)
                    };
                    axum::response::Response::builder()
                        .status(206)
                        .header("content-range", format!("bytes {start}-{end}/{SIZE}"))
                        .header("content-length", (end - start + 1).to_string())
                        .header("etag", "\"v1\"")
                        .body(body)
                        .unwrap()
                }
            }))
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("big.bin");
        let attempt = try_http_concurrent_range_download(
            HttpRangeRequest {
                client: reqwest::Client::new(),
                url: format!("http://{addr}/big.bin"),
                headers: Vec::new(),
                local_path: local.to_string_lossy().into_owned(),
                provider_type: super::super::ProviderType::WebDav,
                streams: 4,
                max_streams: 4,
                cutoff: 1024 * 1024,
                known_size: Some(SIZE),
            },
            None,
        )
        .await;
        assert!(
            matches!(attempt, HttpRangeAttempt::Completed),
            "{:?}",
            match attempt {
                HttpRangeAttempt::Failed(e) => format!("Failed({e})"),
                HttpRangeAttempt::Fallback(_) => "Fallback".to_string(),
                HttpRangeAttempt::Completed => "Completed".to_string(),
            }
        );
        let file = std::fs::read(&local).unwrap();
        assert_eq!(file.len() as u64, SIZE);
        assert_deterministic(&file);
        let resumed: Vec<u64> = requests
            .lock()
            .unwrap()
            .iter()
            .filter(|&&(start, end)| start > WINDOW && end == 2 * WINDOW - 1)
            .map(|&(start, _)| start)
            .collect();
        assert_eq!(resumed.len(), 1, "one retry of the cut window: {resumed:?}");
        assert!(
            resumed[0] <= WINDOW + WINDOW / 2,
            "the retry starts where the body stopped, not past it: {resumed:?}"
        );
    }

    /// A window whose request gets no answer is asked again too: the request
    /// of a window was mapped to `TransferFailed`, which failed the file.
    #[tokio::test]
    async fn an_http_window_with_no_answer_is_asked_again() {
        use window_server::{WireFailure, OBJECT_SIZE};
        let server = window_server::start(WireFailure::NoAnswer, OBJECT_SIZE / 4).await;
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("big.bin");
        let attempt = try_http_concurrent_range_download(
            HttpRangeRequest {
                client: reqwest::Client::new(),
                url: format!("http://{}/big.bin", server.addr),
                headers: Vec::new(),
                local_path: local.to_string_lossy().into_owned(),
                provider_type: super::super::ProviderType::WebDav,
                streams: 4,
                max_streams: 4,
                cutoff: 1024 * 1024,
                known_size: Some(OBJECT_SIZE),
            },
            None,
        )
        .await;
        assert!(
            matches!(attempt, HttpRangeAttempt::Completed),
            "{}",
            match attempt {
                HttpRangeAttempt::Failed(e) => format!("Failed({e})"),
                HttpRangeAttempt::Fallback(_) => "Fallback".to_string(),
                HttpRangeAttempt::Completed => "Completed".to_string(),
            }
        );
        let file = std::fs::read(&local).unwrap();
        assert_eq!(file.len() as u64, OBJECT_SIZE);
        assert!(file
            .iter()
            .zip(0u64..)
            .all(|(&b, o)| b == window_server::byte_at(o)));
        assert_eq!(
            server.failing_window_requests.load(Ordering::SeqCst),
            2,
            "the window was asked again once"
        );
    }

    #[tokio::test]
    async fn segmented_progress_advances_before_any_range_completes() {
        use std::sync::atomic::AtomicUsize;
        use tokio::sync::{Notify, Semaphore};

        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("partial.bin");
        let total = 16;
        let release = Arc::new(Semaphore::new(0));
        let partial_written = Arc::new(Notify::new());
        let completed = Arc::new(AtomicUsize::new(0));
        let reports = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel();
        let callback = {
            let reports = reports.clone();
            Box::new(move |done, total| {
                reports.lock().unwrap().push((done, total));
                progress_tx.send((done, total)).unwrap();
            }) as Box<dyn Fn(u64, u64) + Send>
        };
        let writer = {
            let release = release.clone();
            let partial_written = partial_written.clone();
            let completed = completed.clone();
            move |start: u64,
                  end: u64,
                  temp_path: PathBuf,
                  aggregate: Arc<AtomicU64>,
                  _cancel: CancellationToken,
                  _mark: RangeMark| {
                let release = release.clone();
                let partial_written = partial_written.clone();
                let completed = completed.clone();
                async move {
                    let mut file = tokio::fs::OpenOptions::new()
                        .write(true)
                        .open(temp_path)
                        .await
                        .map_err(ProviderError::IoError)?;
                    file.seek(std::io::SeekFrom::Start(start))
                        .await
                        .map_err(ProviderError::IoError)?;
                    let bytes: Vec<_> = (start..=end).map(|offset| offset as u8).collect();
                    let midpoint = bytes.len() / 2;
                    file.write_all(&bytes[..midpoint])
                        .await
                        .map_err(ProviderError::IoError)?;
                    aggregate.fetch_add(midpoint as u64, Ordering::Relaxed);
                    partial_written.notify_one();
                    release.acquire().await.unwrap().forget();
                    file.write_all(&bytes[midpoint..])
                        .await
                        .map_err(ProviderError::IoError)?;
                    file.flush().await.map_err(ProviderError::IoError)?;
                    aggregate.fetch_add((bytes.len() - midpoint) as u64, Ordering::Relaxed);
                    completed.fetch_add(1, Ordering::SeqCst);
                    Ok(ConcurrentRangeOutcome::Completed)
                }
            }
        };
        let download = run_concurrent_range_download(
            ConcurrentRangeConfig {
                total_size: total,
                streams: 2,
                max_streams: 2,
                max_parallel: 2,
                final_path: final_path.clone(),
                provider_type: super::super::ProviderType::WebDav,
                endpoint_identity: crate::transfer_dag::EndpointIdentity::new(
                    "webdav",
                    "partial-progress-test",
                    "",
                ),
            },
            writer,
            CancellationToken::new(),
            Some(callback),
        );
        tokio::pin!(download);
        let before_completion = async {
            partial_written.notified().await;
            tokio::time::timeout(std::time::Duration::from_secs(2), progress_rx.recv())
                .await
                .expect("no progress before a whole range completed")
                .expect("progress callback channel closed")
        };
        let (done, reported_total) = tokio::select! {
            result = &mut download => panic!("download completed before workers were released: {result:?}"),
            progress = before_completion => progress,
        };
        assert!(
            done > 0 && done < total,
            "expected partial progress, got {done}"
        );
        assert_eq!(reported_total, total);
        assert_eq!(completed.load(Ordering::SeqCst), 0);
        release.add_permits(2);
        let run = download.await.unwrap();
        assert_eq!(run.outcome(), ConcurrentRangeOutcome::Completed);
        {
            let reports = reports.lock().unwrap();
            assert_eq!(reports.last(), Some(&(total, total)));
            assert!(reports.windows(2).all(|pair| pair[0].0 < pair[1].0));
        }
        assert_eq!(
            tokio::fs::read(segmented_temp_path_for(&final_path))
                .await
                .unwrap(),
            (0..16).collect::<Vec<u8>>()
        );
        // The caller publishes the validated temporary; the range engine
        // leaves the destination untouched.
        assert!(!final_path.exists());
        drop(run);
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
        let temp_a = segmented_temp_path_for(&dir.join("a.bin"));
        let temp_b = segmented_temp_path_for(&dir.join("b.bin"));
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
                  _cancel: CancellationToken,
                  _mark: RangeMark|
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
            let temp_a = segmented_temp_path_for(&dir.join("a.bin"));
            let temp_b = segmented_temp_path_for(&dir.join("b.bin"));
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
        let temp_a = segmented_temp_path_for(&dir.join("a.bin"));
        let temp_b = segmented_temp_path_for(&dir.join("b.bin"));
        prealloc(&temp_a, 1).await;
        prealloc(&temp_b, 1).await;
        let writer = Arc::new(
            |_start: u64,
             _end: u64,
             _path: PathBuf,
             _aggregate: Arc<AtomicU64>,
             _cancel: CancellationToken,
             _mark: RangeMark|
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

    /// A completed run hands its temporary to the caller, which reads the
    /// source again and only then publishes it. Until that publication a
    /// second segmented download of the same file must find the temporary
    /// claimed: one that took it for stale would replace it with its own
    /// pre-sized file, holes included, and the first caller would rename
    /// that into place.
    /// Linux only, like the claim itself (`temp_claim` uses locks there).
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_completed_run_keeps_its_temporary_claimed_until_published() {
        let dir = scratch_dir("claim-until-published");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let final_path = dir.join("result.bin");
        let temp_path = segmented_temp_path_for(&final_path);
        let config = ConcurrentRangeConfig {
            final_path: final_path.clone(),
            provider_type: super::super::ProviderType::S3,
            endpoint_identity: crate::transfer_dag::EndpointIdentity::new("s3", "claim", ""),
            total_size: 10,
            streams: 3,
            max_streams: 16,
            max_parallel: 2,
        };

        let run = run_concurrent_range_download(
            config,
            deterministic_writer(),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();

        let second = crate::providers::atomic_write::temp_claim::create_fresh(&temp_path);
        assert!(
            second.is_err(),
            "a second segmented download took the temporary of a run not yet published"
        );
        assert_eq!(tokio::fs::read(&temp_path).await.unwrap().len(), 10);
        drop(second);
        drop(run);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// Publishing renames the temporary into place and releases the claim:
    /// the lock follows the inode, so a claim kept past the rename would
    /// still hold the published file.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn publishing_moves_the_temporary_and_releases_its_claim() {
        let dir = scratch_dir("publish-releases-claim");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let final_path = dir.join("result.bin");
        let temp_path = segmented_temp_path_for(&final_path);
        let config = ConcurrentRangeConfig {
            final_path: final_path.clone(),
            provider_type: super::super::ProviderType::S3,
            endpoint_identity: crate::transfer_dag::EndpointIdentity::new("s3", "publish", ""),
            total_size: 10,
            streams: 3,
            max_streams: 16,
            max_parallel: 2,
        };

        let run = run_concurrent_range_download(
            config,
            deterministic_writer(),
            CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
        let SegmentedRun::Completed(temp) = run else {
            panic!("expected a completed run");
        };
        temp.publish(&final_path).await.unwrap();

        assert_eq!(tokio::fs::read(&final_path).await.unwrap().len(), 10);
        assert!(!temp_path.exists());
        let published = std::fs::File::open(&final_path).unwrap();
        assert!(
            published.try_lock().is_ok(),
            "the claim outlived the publication"
        );
        drop(published);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn concurrent_range_download_uses_promoted_graph_without_escape_hatch() {
        let dir = scratch_dir("promoted-default");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let final_path = dir.join("result.bin");
        let temp_path = segmented_temp_path_for(&final_path);
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

        assert_eq!(outcome.outcome(), ConcurrentRangeOutcome::Completed);
        let bytes = tokio::fs::read(&temp_path).await.unwrap();
        assert_eq!(bytes.len(), 10);
        for (offset, byte) in bytes.iter().enumerate() {
            assert_eq!(*byte, (offset % 251) as u8);
        }
        drop(outcome);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    async fn assert_max_parallel_cap(graph: bool, tag: &str) {
        use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
        use tokio::sync::{Barrier, Semaphore as TokioSemaphore};

        let ranges = vec![(0, 0), (1, 1), (2, 2), (3, 3)];
        let dir = scratch_dir(tag);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let temp = segmented_temp_path_for(&dir.join("cap.bin"));
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
                      _cancel: CancellationToken,
                      _mark: RangeMark|
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

    #[test]
    fn a_size_hint_rules_out_ranges_only_when_known_and_below_the_cutoff() {
        let cutoff = 250 * 1024 * 1024;
        assert!(size_hint_rules_out_ranges(Some(4096), cutoff));
        assert!(size_hint_rules_out_ranges(Some(cutoff - 1), cutoff));
        assert!(!size_hint_rules_out_ranges(Some(cutoff), cutoff));
        assert!(!size_hint_rules_out_ranges(Some(cutoff * 4), cutoff));
        assert!(!size_hint_rules_out_ranges(None, cutoff));
    }

    /// The Range probe is one round trip per file. A caller that already
    /// knows the size is below the cutoff must get the single-stream fallback
    /// without the helper touching the server; an unknown size still probes.
    #[tokio::test]
    async fn a_known_small_size_returns_fallback_without_a_probe_request() {
        use std::sync::atomic::AtomicUsize;
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        let app = axum::Router::new().fallback(axum::routing::any(move || {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                axum::response::Response::new(axum::body::Body::from("x"))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let request = |known_size: Option<u64>| HttpRangeRequest {
            client: reqwest::Client::new(),
            url: format!("http://{addr}/file.bin"),
            headers: Vec::new(),
            local_path: std::env::temp_dir()
                .join(format!("aeroftp-mt-hint-{}", std::process::id()))
                .to_string_lossy()
                .into_owned(),
            provider_type: super::super::ProviderType::WebDav,
            streams: 4,
            max_streams: 4,
            cutoff: 250 * 1024 * 1024,
            known_size,
        };

        let attempt =
            try_http_concurrent_range_download(request(Some(4096)), Some(Box::new(|_, _| {})))
                .await;
        assert!(
            matches!(attempt, HttpRangeAttempt::Fallback(Some(_))),
            "a known small size falls back with the progress callback returned"
        );
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "no probe for a known small size"
        );

        let attempt = try_http_concurrent_range_download(request(None), None).await;
        assert!(matches!(attempt, HttpRangeAttempt::Fallback(_)));
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "an unknown size still probes once"
        );
    }

    /// Start and end of a `bytes=start-end` request header.
    fn requested_window(raw: &str) -> Option<(u64, u64)> {
        let (start, end) = raw.strip_prefix("bytes=")?.split_once('-')?;
        Some((start.parse().ok()?, end.parse().ok()?))
    }

    /// A reading that cannot be compared is a failed reading. If the probe
    /// that checks before publishing answers 500, none of the headers it
    /// would be compared on are there, and the silence must not read as "the
    /// object did not change".
    #[tokio::test]
    async fn a_final_probe_that_fails_does_not_read_as_unchanged() {
        use std::sync::atomic::AtomicUsize;
        const SIZE: u64 = 2 * 1024 * 1024;
        let probes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&probes);
        let app =
            axum::Router::new().fallback(axum::routing::any(move |req: axum::extract::Request| {
                let counter = Arc::clone(&counter);
                async move {
                    let raw = req
                        .headers()
                        .get("range")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    let (start, end) = requested_window(&raw).unwrap_or((0, SIZE - 1));
                    if start == 0 && end == 0 && counter.fetch_add(1, Ordering::SeqCst) > 0 {
                        return axum::response::Response::builder()
                            .status(500)
                            .body(axum::body::Body::empty())
                            .unwrap();
                    }
                    axum::response::Response::builder()
                        .status(206)
                        .header("content-range", format!("bytes {}-{}/{}", start, end, SIZE))
                        .header("etag", "\"v1\"")
                        .body(axum::body::Body::from(vec![
                            7u8;
                            (end - start + 1) as usize
                        ]))
                        .unwrap()
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        let dir = tempfile::tempdir().expect("tempdir");
        let local = dir.path().join("big.bin");
        let attempt = try_http_concurrent_range_download(
            HttpRangeRequest {
                client: reqwest::Client::new(),
                url: format!("http://{addr}/big.bin"),
                headers: Vec::new(),
                local_path: local.to_string_lossy().into_owned(),
                provider_type: super::super::ProviderType::WebDav,
                streams: 4,
                max_streams: 4,
                cutoff: 1024 * 1024,
                known_size: None,
            },
            None,
        )
        .await;
        assert!(
            matches!(attempt, HttpRangeAttempt::Fallback(None)),
            "a reading that could not be taken must not publish the file"
        );
        assert!(!local.exists(), "no file may be published");
    }

    async fn final_source_observation(
        status: u16,
        range: Option<&'static str>,
        length: Option<&'static str>,
        modified: Option<&'static str>,
    ) -> Option<String> {
        let app = axum::Router::new().fallback(axum::routing::any(move || async move {
            let mut response = axum::response::Response::builder().status(status);
            for (key, value) in [
                ("content-range", range),
                ("content-length", length),
                ("last-modified", modified),
            ] {
                if let Some(value) = value {
                    response = response.header(key, value);
                }
            }
            let body_len = if status == 200 {
                length.and_then(|n| n.parse::<usize>().ok()).unwrap_or(1)
            } else {
                1
            };
            response
                .body(axum::body::Body::from(vec![b'x'; body_len]))
                .unwrap()
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let request = HttpRangeRequest {
            client: reqwest::Client::new(),
            url: format!("http://{addr}/object"),
            headers: Vec::new(),
            local_path: String::new(),
            provider_type: super::super::ProviderType::WebDav,
            streams: 2,
            max_streams: 2,
            cutoff: 1,
            known_size: None,
        };
        let result =
            http_range_source_moved(&request, None, Some("Mon, 21 Sep 2026 00:00:00 GMT"), 2048)
                .await;
        server.abort();
        result
    }

    #[tokio::test]
    async fn source_observation_checks_whole_object_size() {
        // A server may answer 200 to the last probe; Content-Length is then
        // the object size, not the one-byte range size.
        assert!(final_source_observation(
            200,
            None,
            Some("1"),
            Some("Mon, 21 Sep 2026 00:00:00 GMT")
        )
        .await
        .is_some());
    }

    #[tokio::test]
    async fn source_observation_refuses_missing_range_total() {
        for range in [
            None,
            Some("bytes 0-0/*"),
            Some("bytes 1-1/2048"),
            Some("bytes 0-0/0"),
        ] {
            assert!(
                final_source_observation(
                    206,
                    range,
                    Some("1"),
                    Some("Mon, 21 Sep 2026 00:00:00 GMT")
                )
                .await
                .is_some(),
                "unusable final Content-Range: {range:?}"
            );
        }
    }

    #[tokio::test]
    async fn source_observation_accepts_unchanged_partial_and_whole_responses() {
        for (status, range, length) in [(206, Some("bytes 0-0/2048"), "1"), (200, None, "2048")] {
            assert!(final_source_observation(
                status,
                range,
                Some(length),
                Some("Mon, 21 Sep 2026 00:00:00 GMT")
            )
            .await
            .is_none());
        }
    }

    #[tokio::test]
    async fn source_observation_refuses_disappearing_http_timestamp() {
        assert!(
            final_source_observation(206, Some("bytes 0-0/2048"), Some("1"), None)
                .await
                .is_some()
        );
    }

    #[test]
    fn source_observation_refuses_disappearing_provider_timestamp() {
        let before = RangeSourceFingerprint {
            size: 2048,
            modified: Some("2026-09-21T00:00:00Z".into()),
            validator: None,
        };
        let after = RangeSourceFingerprint {
            size: 2048,
            modified: None,
            validator: None,
        };
        assert!(
            before.differs_from(&after).is_some(),
            "losing the only version evidence must refuse publication"
        );
    }

    /// No validator does not erase the size we learned from Content-Range.
    /// The server finishes a larger replacement between the range requests.
    #[tokio::test]
    async fn http_range_replacement_without_validators_is_not_published() {
        http_range_without_validators(true).await;
    }

    #[tokio::test]
    async fn http_range_without_validators_preserves_stable_object() {
        http_range_without_validators(false).await;
    }

    async fn http_range_without_validators(replace: bool) {
        use std::sync::atomic::AtomicUsize;
        // 4 MiB: the shared planner's 1 MiB minimum window keeps 4 streams.
        const SIZE: u64 = 4 * 1024 * 1024;
        let windows = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&windows);
        let app =
            axum::Router::new().fallback(axum::routing::any(move |req: axum::extract::Request| {
                let count = Arc::clone(&count);
                async move {
                    let raw = req.headers().get("range").unwrap().to_str().unwrap();
                    let (start, end) = requested_window(raw).unwrap();
                    let is_probe = start == 0 && end == 0;
                    let generation = if is_probe {
                        count.load(Ordering::SeqCst)
                    } else {
                        count.fetch_add(1, Ordering::SeqCst)
                    };
                    let replaced = replace && generation > 0;
                    let total = if replaced { SIZE + 1024 } else { SIZE };
                    let byte = if replaced { 9u8 } else { 7u8 };
                    axum::response::Response::builder()
                        .status(206)
                        .header("content-range", format!("bytes {start}-{end}/{total}"))
                        .body(axum::body::Body::from(vec![
                            byte;
                            (end - start + 1) as usize
                        ]))
                        .unwrap()
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("replaced.bin");
        let attempt = try_http_concurrent_range_download(
            HttpRangeRequest {
                client: reqwest::Client::new(),
                url: format!("http://{addr}/replaced.bin"),
                headers: Vec::new(),
                local_path: local.to_string_lossy().into_owned(),
                provider_type: super::super::ProviderType::WebDav,
                streams: 4,
                max_streams: 4,
                cutoff: 1024 * 1024,
                known_size: None,
            },
            None,
        )
        .await;
        server.abort();
        assert_eq!(windows.load(Ordering::SeqCst), 4);
        if replace {
            if local.exists() {
                let bytes = tokio::fs::read(&local).await.unwrap();
                assert!(
                    bytes.contains(&7) && bytes.contains(&9),
                    "fixture must produce mixed generations"
                );
            }
            assert!(
                matches!(attempt, HttpRangeAttempt::Fallback(None)),
                "a larger, fully replaced object must not publish mixed generations"
            );
            assert!(!local.exists());
        } else {
            assert!(matches!(attempt, HttpRangeAttempt::Completed));
            assert_eq!(
                tokio::fs::read(&local).await.unwrap(),
                vec![7u8; SIZE as usize]
            );
        }
        assert!(!segmented_temp_path_for(&local).exists());
    }

    /// This is the path WebDAV and Koofr take, and it holds a validator from
    /// its own probe. A server that ignores `If-Match` must still not get a
    /// file published out of two versions: the windows all answer with the
    /// length they asked for and the total is exactly right.
    #[tokio::test]
    async fn the_http_range_path_refuses_an_object_replaced_under_it() {
        use std::sync::atomic::AtomicUsize;
        const SIZE: u64 = 2 * 1024 * 1024;
        let probes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&probes);
        let app =
            axum::Router::new().fallback(axum::routing::any(move |req: axum::extract::Request| {
                let counter = Arc::clone(&counter);
                async move {
                    let raw = req
                        .headers()
                        .get("range")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    let (start, end) = requested_window(&raw).unwrap_or((0, SIZE - 1));
                    // The object is replaced while the windows are in flight:
                    // the probe that plans them sees v1, the one that checks
                    // before publishing sees v2. If-Match is ignored, as a
                    // server that does not implement it would.
                    let tag = if start == 0 && end == 0 {
                        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                            "\"v1\""
                        } else {
                            "\"v2\""
                        }
                    } else {
                        "\"v1\""
                    };
                    axum::response::Response::builder()
                        .status(206)
                        .header("content-range", format!("bytes {}-{}/{}", start, end, SIZE))
                        .header("etag", tag)
                        .body(axum::body::Body::from(vec![
                            7u8;
                            (end - start + 1) as usize
                        ]))
                        .unwrap()
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        let dir = tempfile::tempdir().expect("tempdir");
        let local = dir.path().join("big.bin");
        let attempt = try_http_concurrent_range_download(
            HttpRangeRequest {
                client: reqwest::Client::new(),
                url: format!("http://{addr}/big.bin"),
                headers: Vec::new(),
                local_path: local.to_string_lossy().into_owned(),
                provider_type: super::super::ProviderType::WebDav,
                streams: 4,
                max_streams: 4,
                cutoff: 1024 * 1024,
                known_size: None,
            },
            None,
        )
        .await;
        assert!(
            matches!(attempt, HttpRangeAttempt::Fallback(None)),
            "an object that moved must be handed back to a single stream"
        );
        assert!(!local.exists(), "no file may be published");
    }
}
