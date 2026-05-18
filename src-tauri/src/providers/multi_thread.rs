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
//! `plan_multi_thread_ranges` is pure, fully unit-tested and already wired
//! (S3's `download_multi_thread` calls it). The strict download helper and
//! its support types are the convergence primitive WebDAV/Koofr will adopt
//! in **PD-HTTP-2** after the live 206 census; S3 keeps its native path for
//! now. They are therefore intentionally unused at PD-HTTP-1 close and
//! carry `#[allow(dead_code)]` with this rationale rather than a faked
//! consumer (rev 3 principle: no overclaim, untested live paths stay
//! honestly marked scaffold, not silently masked).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures_util::StreamExt;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::ProviderError;

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

/// Parse an HTTP `Content-Range: bytes start-end/total` header.
///
/// Returns `(start, end, total)` where `total` is `None` for the `*`
/// (unknown-length) form. Returns `None` on any deviation from the
/// `bytes <start>-<end>/<total|*>` grammar: the strict gate treats a
/// malformed header on a `206` as a hard failure, never a soft accept.
#[allow(dead_code)] // PD-HTTP-2 convergence primitive (see module docs)
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
#[allow(dead_code)] // PD-HTTP-2 convergence primitive (see module docs)
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
#[allow(dead_code)] // PD-HTTP-2 convergence primitive (see module docs)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConcurrentRangeOutcome {
    /// Every range came back as a strict `206` with a coherent
    /// `Content-Range`; the temp file is fully written.
    Completed,
    /// The server answered `200 OK` (ignored `Range`). No partial bytes were
    /// committed; the caller must fall back to a single-stream download.
    ServerIgnoredRange,
}

/// Configuration for [`download_via_concurrent_range`].
#[allow(dead_code)] // PD-HTTP-2 convergence primitive (see module docs)
pub(crate) struct ConcurrentRangeConfig {
    /// Final destination path (used only to derive the `.aerotmp` sibling).
    pub final_path: PathBuf,
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
#[allow(dead_code)] // PD-HTTP-2 convergence primitive (see module docs)
struct TempFileGuard {
    path: PathBuf,
    committed: bool,
}

#[allow(dead_code)] // PD-HTTP-2 convergence primitive (see module docs)
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
#[allow(dead_code)] // PD-HTTP-2 convergence primitive (see module docs)
pub(crate) fn aerotmp_path_for(final_path: &Path) -> PathBuf {
    let mut p = final_path.as_os_str().to_owned();
    p.push(".aerotmp");
    PathBuf::from(p)
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
#[allow(dead_code)] // PD-HTTP-2 convergence primitive (see module docs)
pub(crate) async fn download_via_concurrent_range<F, Fut>(
    cfg: ConcurrentRangeConfig,
    fetch_range: F,
    cancel: CancellationToken,
    on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
) -> Result<ConcurrentRangeOutcome, ProviderError>
where
    F: Fn(u64, u64) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<reqwest::Response, ProviderError>> + Send,
{
    let ranges = plan_multi_thread_ranges(cfg.total_size, cfg.streams, cfg.max_streams);
    if ranges.is_empty() {
        return Err(ProviderError::TransferFailed(
            "Concurrent range download: empty range plan".to_string(),
        ));
    }

    let temp_path = aerotmp_path_for(&cfg.final_path);
    if let Some(parent) = cfg.final_path.parent() {
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
        f.set_len(cfg.total_size)
            .await
            .map_err(ProviderError::IoError)?;
    }
    let mut guard = TempFileGuard::new(temp_path.clone());

    let aggregate = Arc::new(AtomicU64::new(0));
    let semaphore = Arc::new(Semaphore::new(cfg.max_parallel.max(1)));
    let fetch_range = Arc::new(fetch_range);
    let total_size = cfg.total_size;
    let mut join_set: JoinSet<Result<ConcurrentRangeOutcome, ProviderError>> = JoinSet::new();

    for (start, end) in ranges {
        let semaphore = semaphore.clone();
        let fetch_range = fetch_range.clone();
        let aggregate = aggregate.clone();
        let temp_path = temp_path.clone();
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
            download_one_strict_range(
                fetch_range.as_ref(),
                &temp_path,
                start,
                end,
                &aggregate,
                &cancel,
            )
            .await
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

    guard.commit();
    Ok(ConcurrentRangeOutcome::Completed)
}

/// Download exactly one `[start, end]` window under the strict gate.
#[allow(dead_code)] // PD-HTTP-2 convergence primitive (see module docs)
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

#[cfg(test)]
mod tests {
    use super::*;

    const MAX: usize = 16;

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
}
