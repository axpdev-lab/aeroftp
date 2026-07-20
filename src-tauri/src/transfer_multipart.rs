// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Shared multipart upload lifecycle for single-file and batch DAG runners.
//!
//! This module owns the **provider-agnostic** session state and layout math so
//! single-file (`transfer_dag_single_file`) and batch (`transfer_dag_batch`) do
//! not diverge on begin/part/complete/abort once-semantics or byte ranges.
//!
//! It deliberately does **not** schedule work: no `JoinSet`, no work queue, no
//! nested DAG. The existing transfer DAG remains the only node scheduler; this
//! type is only the concurrent-safe file-scoped state those nodes share.

use std::collections::HashMap;
use std::io::SeekFrom;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};

use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::Mutex;

use crate::providers::{MultipartHandle, ProviderError, StorageProvider, UploadedPart};
use crate::transfer_dag::multipart_part_byte_len;
use crate::transfer_domain::{TransferFailure, TransferFailureKind};

/// Resolved multipart layout for one file (neutral, executor-facing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultipartLayout {
    pub total_size: u64,
    pub total_parts: u32,
    pub preferred_part_size: u64,
    pub content_type: String,
}

impl MultipartLayout {
    /// Build layout from shaped profile fields and the source path (for MIME).
    pub fn from_profile(
        total_size: u64,
        total_parts: usize,
        preferred_part_size: u64,
        local_path: &str,
    ) -> Self {
        let part_size = if preferred_part_size > 0 {
            preferred_part_size
        } else if total_parts > 0 {
            total_size.div_ceil(total_parts as u64).max(1)
        } else {
            total_size.max(1)
        };
        let content_type = mime_guess::from_path(local_path)
            .first_or_octet_stream()
            .to_string();
        Self {
            total_size,
            total_parts: total_parts.max(1) as u32,
            preferred_part_size: part_size,
            content_type,
        }
    }

    /// Exact 0-based offset and length for a 1-based part number.
    pub fn part_range(&self, part_number: u32) -> Result<(u64, u64), TransferFailure> {
        if part_number == 0 || part_number > self.total_parts {
            return Err(TransferFailure {
                kind: TransferFailureKind::Unknown,
                message: format!(
                    "multipart part {} out of range 1..={}",
                    part_number, self.total_parts
                ),
                retryable: false,
                retry_after_secs: None,
            });
        }
        let idx = (part_number - 1) as usize;
        let len = multipart_part_byte_len(
            self.total_size,
            idx,
            self.total_parts as usize,
            self.preferred_part_size,
        );
        if len == 0 {
            return Err(TransferFailure {
                kind: TransferFailureKind::Unknown,
                message: format!("multipart part {} has zero length", part_number),
                retryable: false,
                retry_after_secs: None,
            });
        }
        let offset = (part_number as u64 - 1) * self.preferred_part_size;
        Ok((offset, len))
    }
}

/// Concurrent-safe multipart session state for one shaped file.
///
/// Used by both the single-file and batch runners. Begin/complete/abort are
/// once-guarded; receipts are deduplicated by part number.
pub struct MultipartFileState {
    layout: MultipartLayout,
    node_to_part: HashMap<usize, u32>,
    handle: Mutex<Option<MultipartHandle>>,
    /// Serialises the lazy begin path so only one runner calls provider begin.
    begin_gate: Mutex<()>,
    /// True once a provider begin succeeded (handle installed).
    begun: AtomicBool,
    /// True once complete succeeded and the handle was cleared.
    completed: AtomicBool,
    /// True once abort took the handle (at most once).
    aborted: AtomicBool,
    /// Successful receipts keyed by 1-based part number (no duplicates).
    parts: Mutex<HashMap<u32, UploadedPart>>,
    /// First terminal failure or cancellation; abort errors stay diagnostic.
    first_failure: Mutex<Option<TransferFailure>>,
    /// Once-per-file batch accounting / progress emission.
    accounted: AtomicBool,
    /// Once-per-file start event emission.
    start_emitted: AtomicBool,
}

impl MultipartFileState {
    pub fn new(layout: MultipartLayout, node_to_part: HashMap<usize, u32>) -> Arc<Self> {
        Arc::new(Self {
            layout,
            node_to_part,
            handle: Mutex::new(None),
            begin_gate: Mutex::new(()),
            begun: AtomicBool::new(false),
            completed: AtomicBool::new(false),
            aborted: AtomicBool::new(false),
            parts: Mutex::new(HashMap::new()),
            first_failure: Mutex::new(None),
            accounted: AtomicBool::new(false),
            start_emitted: AtomicBool::new(false),
        })
    }

    /// Run `op` under the begin gate (only one beginner at a time).
    pub async fn with_begin_gate<F, T>(&self, op: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        let _guard = self.begin_gate.lock().await;
        op.await
    }

    pub fn layout(&self) -> &MultipartLayout {
        &self.layout
    }

    pub fn part_number_for_node(&self, node_id: usize) -> Option<u32> {
        self.node_to_part.get(&node_id).copied()
    }

    pub fn has_part_mapping(&self, node_id: usize) -> bool {
        self.node_to_part.contains_key(&node_id)
    }

    pub fn is_begun(&self) -> bool {
        self.begun.load(Ordering::Acquire)
    }

    pub fn is_completed(&self) -> bool {
        self.completed.load(Ordering::Acquire)
    }

    pub fn is_aborted(&self) -> bool {
        self.aborted.load(Ordering::Acquire)
    }

    pub async fn has_failure(&self) -> bool {
        self.first_failure.lock().await.is_some()
    }

    /// Record the first meaningful failure; later failures are ignored.
    pub async fn record_failure(&self, failure: TransferFailure) {
        let mut slot = self.first_failure.lock().await;
        if slot.is_none() {
            *slot = Some(failure);
        }
    }

    pub async fn take_first_failure(&self) -> Option<TransferFailure> {
        self.first_failure.lock().await.take()
    }

    pub async fn peek_first_failure(&self) -> Option<TransferFailure> {
        self.first_failure.lock().await.clone()
    }

    /// Claim once-per-file start event. Returns true if this caller should emit.
    pub fn claim_start_event(&self) -> bool {
        !self.start_emitted.swap(true, Ordering::SeqCst)
    }

    /// Claim once-per-file batch accounting. Returns true if this caller owns it.
    pub fn claim_account(&self) -> bool {
        !self.accounted.swap(true, Ordering::SeqCst)
    }

    pub fn was_accounted(&self) -> bool {
        self.accounted.load(Ordering::Acquire)
    }

    /// Store a successful receipt for the requested part.
    ///
    /// The provider must echo the exact part number it was asked to upload.
    /// Accepting a different in-range number could make two swapped receipts
    /// look complete while binding verification tokens to the wrong byte ranges.
    pub async fn store_receipt_for_part(
        &self,
        expected_part_number: u32,
        receipt: UploadedPart,
    ) -> Result<(), TransferFailure> {
        let part_number = receipt.part_number;
        if part_number != expected_part_number {
            return Err(TransferFailure {
                kind: TransferFailureKind::Unknown,
                message: format!(
                    "multipart receipt part {} does not match requested part {}",
                    part_number, expected_part_number
                ),
                retryable: false,
                retry_after_secs: None,
            });
        }
        if part_number == 0 || part_number > self.layout.total_parts {
            return Err(TransferFailure {
                kind: TransferFailureKind::Unknown,
                message: format!(
                    "multipart receipt part {} out of range 1..={}",
                    part_number, self.layout.total_parts
                ),
                retryable: false,
                retry_after_secs: None,
            });
        }
        let mut parts = self.parts.lock().await;
        if parts.contains_key(&part_number) {
            return Err(TransferFailure {
                kind: TransferFailureKind::Unknown,
                message: format!("duplicate multipart receipt for part {}", part_number),
                retryable: false,
                retry_after_secs: None,
            });
        }
        parts.insert(part_number, receipt);
        Ok(())
    }

    /// Store a receipt when the caller already treats its part number as the
    /// requested number. Prefer [`Self::store_receipt_for_part`] at wire sites.
    pub async fn store_receipt(&self, receipt: UploadedPart) -> Result<(), TransferFailure> {
        let expected = receipt.part_number;
        self.store_receipt_for_part(expected, receipt).await
    }

    pub async fn receipt_count(&self) -> usize {
        self.parts.lock().await.len()
    }

    /// Take all receipts sorted by part number (for complete).
    pub async fn take_sorted_receipts(&self) -> Vec<UploadedPart> {
        let mut parts: Vec<UploadedPart> =
            self.parts.lock().await.drain().map(|(_, r)| r).collect();
        parts.sort_by_key(|p| p.part_number);
        parts
    }

    /// Snapshot receipts sorted by part number without draining (for validation).
    pub async fn sorted_receipts_snapshot(&self) -> Vec<UploadedPart> {
        let mut parts: Vec<UploadedPart> = self.parts.lock().await.values().cloned().collect();
        parts.sort_by_key(|p| p.part_number);
        parts
    }

    /// Whether every expected part number has a receipt.
    pub async fn has_all_receipts(&self) -> bool {
        let parts = self.parts.lock().await;
        if parts.len() != self.layout.total_parts as usize {
            return false;
        }
        (1..=self.layout.total_parts).all(|n| parts.contains_key(&n))
    }

    /// Install handle after a successful begin. Marks begun.
    pub async fn install_handle(&self, handle: MultipartHandle) {
        let mut slot = self.handle.lock().await;
        *slot = Some(handle);
        self.begun.store(true, Ordering::Release);
    }

    /// Restore a durable multipart session before the DAG starts. The caller
    /// has already validated source/destination/layout identity in the durable
    /// checkpoint store. Receipts still pass the same strict part-number and
    /// duplicate checks as newly uploaded parts, so a corrupt journal cannot
    /// make the commit node accept a fabricated complete upload.
    pub async fn restore_session(
        &self,
        handle: MultipartHandle,
        receipts: Vec<UploadedPart>,
    ) -> Result<(), TransferFailure> {
        if handle.remote_path.is_empty() {
            return Err(TransferFailure {
                kind: TransferFailureKind::Unknown,
                message: "durable multipart checkpoint has an empty remote path".to_string(),
                retryable: false,
                retry_after_secs: None,
            });
        }
        self.install_handle(handle).await;
        for receipt in receipts {
            let expected = receipt.part_number;
            self.store_receipt_for_part(expected, receipt).await?;
        }
        Ok(())
    }

    /// Clone the current handle if present.
    pub async fn clone_handle(&self) -> Option<MultipartHandle> {
        self.handle.lock().await.clone()
    }

    /// Whether the handle slot is still empty (caller may begin).
    pub async fn needs_begin(&self) -> bool {
        self.handle.lock().await.is_none() && !self.completed.load(Ordering::Acquire)
    }

    /// Clear handle after successful complete (abort becomes a no-op).
    pub async fn clear_handle_after_complete(&self) {
        let mut slot = self.handle.lock().await;
        *slot = None;
        self.completed.store(true, Ordering::Release);
    }

    /// Take leftover handle for best-effort abort at most once.
    pub async fn take_for_abort(&self) -> Option<MultipartHandle> {
        if self.completed.load(Ordering::Acquire) {
            return None;
        }
        if self.aborted.swap(true, Ordering::SeqCst) {
            return None;
        }
        self.handle.lock().await.take()
    }
}

/// Read `len` bytes from `path` starting at `offset`.
///
/// Call only while the matching `ResourceRequest.buffer_bytes` lease is held.
pub async fn read_chunk(path: &str, offset: u64, len: u64) -> Result<Vec<u8>, ProviderError> {
    let mut file = tokio::fs::File::open(path).await?;
    file.seek(SeekFrom::Start(offset)).await?;
    let mut data = vec![0u8; len as usize];
    file.read_exact(&mut data).await?;
    Ok(data)
}

// ============================================================================
// DAG-P2-05: reusable buffer pool + streaming part body
// ============================================================================

/// Bounded read window used when streaming a part body from disk.
///
/// A streamed part is read one window at a time, so peak resident memory for an
/// in-flight streamed part is this window, not the whole part. It is a small
/// fixed size (two 64 KiB buffer-credit quanta) so a streamed part can reserve
/// a single, honest `buffer_bytes` window instead of the whole part size.
pub const PART_STREAM_WINDOW_BYTES: usize = 128 * 1024;

/// Bounded pool of reusable read-window buffers for streamed part bodies.
///
/// The pool recycles the scratch `Vec<u8>` a streamed part reads windows into,
/// so N sequential streamed parts reuse a bounded set of allocations instead of
/// allocating and freeing N fresh windows. It adds **no** byte accounting of its
/// own: how many part buffers may be live at once, and how large, is already
/// bounded by the `buffer_bytes` governor credit each part holds while its node
/// runs (DAG-P0-06 / DAG-P2-01). The pool only avoids re-allocating a window.
pub struct PartBufferPool {
    free: StdMutex<Vec<Vec<u8>>>,
    max_idle: usize,
    window: usize,
    allocations: AtomicU64,
}

impl PartBufferPool {
    /// Pool that parks up to `max_idle` idle buffers of `window` bytes each.
    pub fn new(max_idle: usize, window: usize) -> Self {
        Self {
            free: StdMutex::new(Vec::new()),
            max_idle: max_idle.max(1),
            window: window.max(1),
            allocations: AtomicU64::new(0),
        }
    }

    /// Take a reusable window buffer, recycled from the free list when one is
    /// idle or freshly allocated otherwise. It returns to the pool on drop.
    pub fn acquire(self: &Arc<Self>) -> PooledWindow {
        let recycled = self.free.lock().unwrap().pop();
        let buf = match recycled {
            Some(mut buf) => {
                if buf.len() != self.window {
                    buf.resize(self.window, 0);
                }
                buf
            }
            None => {
                self.allocations.fetch_add(1, Ordering::Relaxed);
                vec![0u8; self.window]
            }
        };
        PooledWindow {
            buf: Some(buf),
            pool: Arc::clone(self),
        }
    }

    /// Total window buffers ever allocated (diagnostics and reuse tests). A
    /// value well below the number of parts streamed proves real reuse.
    pub fn allocations(&self) -> u64 {
        self.allocations.load(Ordering::Relaxed)
    }

    /// Idle buffers currently parked for reuse.
    pub fn idle(&self) -> usize {
        self.free.lock().unwrap().len()
    }

    fn recycle(&self, buf: Vec<u8>) {
        let mut free = self.free.lock().unwrap();
        if free.len() < self.max_idle {
            free.push(buf);
        }
    }
}

/// A window buffer borrowed from a [`PartBufferPool`]; returned on drop.
pub struct PooledWindow {
    buf: Option<Vec<u8>>,
    pool: Arc<PartBufferPool>,
}

impl PooledWindow {
    fn window_mut(&mut self) -> &mut [u8] {
        self.buf
            .as_mut()
            .expect("pooled window present")
            .as_mut_slice()
    }

    fn window_ref(&self) -> &[u8] {
        self.buf.as_ref().expect("pooled window present").as_slice()
    }
}

impl Drop for PooledWindow {
    fn drop(&mut self) {
        if let Some(buf) = self.buf.take() {
            self.pool.recycle(buf);
        }
    }
}

static PART_BUFFER_POOL: OnceLock<Arc<PartBufferPool>> = OnceLock::new();

/// Process-global reusable window pool for streamed part bodies. Idle capacity
/// covers the default concurrent active-file / part window; the byte budget is
/// governed elsewhere (the `buffer_bytes` credit each streamed part holds).
pub fn part_buffer_pool() -> Arc<PartBufferPool> {
    PART_BUFFER_POOL
        .get_or_init(|| Arc::new(PartBufferPool::new(64, PART_STREAM_WINDOW_BYTES)))
        .clone()
}

/// A byte range `[offset, offset + len)` of a local file used as a replayable
/// streaming source for one multipart part. Re-opening and re-seeking the file
/// per attempt makes the source honestly replayable for node-level retry.
#[derive(Debug, Clone)]
pub struct DiskSlicePart {
    path: Arc<PathBuf>,
    offset: u64,
    len: u64,
}

impl DiskSlicePart {
    /// Name the byte range without touching the disk yet.
    pub fn new(path: impl Into<PathBuf>, offset: u64, len: u64) -> Self {
        Self {
            path: Arc::new(path.into()),
            offset,
            len,
        }
    }

    /// Exact byte length of the slice (known without reading it).
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the slice is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Read the whole slice into an owned buffer (materialize). Used by the
    /// compat path for providers that must own the whole part.
    pub async fn read_to_vec(&self) -> Result<Vec<u8>, ProviderError> {
        read_chunk(&self.path.to_string_lossy(), self.offset, self.len).await
    }

    /// A fresh bounded reader over the slice: opens the file and seeks to the
    /// part offset. Replayable, because each call re-opens from scratch.
    async fn open_reader(&self) -> std::io::Result<tokio::io::Take<tokio::fs::File>> {
        let mut file = tokio::fs::File::open(self.path.as_ref()).await?;
        file.seek(SeekFrom::Start(self.offset)).await?;
        Ok(file.take(self.len))
    }
}

/// State machine threaded through the disk-slice window stream so the pooled
/// scratch buffer and the file reader are reused across every window of one
/// part (the scratch returns to the pool when the stream is dropped).
enum DiskSliceStream {
    Init(DiskSlicePart, PooledWindow),
    Reading(tokio::io::Take<tokio::fs::File>, PooledWindow),
}

/// Stream one part from disk, one pooled window at a time. Each yielded chunk is
/// a fresh owned copy of the bytes just read (the transient wire chunk that
/// `reqwest` sends and drops); the scratch window itself is reused for the next
/// read and recycled to the pool at the end. Exact total bytes yielded equal the
/// slice length. `Vec<u8>` is yielded because `reqwest` converts it to `Bytes`
/// internally, so the crate needs no direct `bytes` dependency.
fn disk_slice_window_stream(
    slice: DiskSlicePart,
    scratch: PooledWindow,
) -> impl futures_util::Stream<Item = std::io::Result<Vec<u8>>> + Send {
    futures_util::stream::try_unfold(DiskSliceStream::Init(slice, scratch), |state| async move {
        let (mut reader, mut scratch) = match state {
            DiskSliceStream::Init(slice, scratch) => (slice.open_reader().await?, scratch),
            DiskSliceStream::Reading(reader, scratch) => (reader, scratch),
        };
        let read = reader.read(scratch.window_mut()).await?;
        if read == 0 {
            Ok(None)
        } else {
            let chunk = scratch.window_ref()[..read].to_vec();
            Ok(Some((chunk, DiskSliceStream::Reading(reader, scratch))))
        }
    })
}

/// The body of one multipart upload part, decoupled from buffer ownership
/// (DAG-P2-05).
///
/// The runner no longer forces a fully-owned `Vec<u8>` across the provider
/// boundary for every part. It hands a `PartBody`:
///
/// * [`PartBody::Owned`] carries the whole part resident in memory. Providers
///   that must hash, encrypt, or sign the entire part before sending (S3 signed
///   payload, B2/Box SHA-1, MEGA, Filen) consume it by value exactly as before,
///   bounded by the same `buffer_bytes` governor credit (DAG-P0-06 / DAG-P2-01).
/// * [`PartBody::DiskSlice`] names a byte range of a local file. Providers that
///   can honestly stream a bounded window (single-`send` PUT/POST with a known
///   length) open a fresh reader per attempt and stream it, so peak resident
///   memory is one [`PART_STREAM_WINDOW_BYTES`] window, not the whole part.
///
/// Both variants are **replayable** (owned re-sends its buffer; disk re-opens
/// and re-seeks), so there is deliberately no one-shot variant and no provider
/// can claim a retry-by-reread it cannot honour.
pub enum PartBody {
    Owned(Vec<u8>),
    DiskSlice(DiskSlicePart),
}

impl PartBody {
    /// Wrap an already-read, owned buffer (compat path).
    pub fn owned(data: Vec<u8>) -> Self {
        PartBody::Owned(data)
    }

    /// Name a disk slice to stream without reading it yet.
    pub fn disk_slice(path: impl Into<PathBuf>, offset: u64, len: u64) -> Self {
        PartBody::DiskSlice(DiskSlicePart::new(path, offset, len))
    }

    /// Exact byte length of the part (known for both variants without reading).
    pub fn len(&self) -> u64 {
        match self {
            PartBody::Owned(data) => data.len() as u64,
            PartBody::DiskSlice(slice) => slice.len(),
        }
    }

    /// Whether the part is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Both variants are replayable across attempts.
    pub fn is_replayable(&self) -> bool {
        true
    }

    /// Materialize the whole part into an owned buffer. Instant for `Owned`; a
    /// bounded disk read for `DiskSlice`. The default provider `upload_part_body`
    /// uses this so any provider that has not been migrated to streaming keeps
    /// its existing owned-buffer `upload_part` behaviour byte for byte.
    pub async fn into_owned_bytes(self) -> Result<Vec<u8>, ProviderError> {
        match self {
            PartBody::Owned(data) => Ok(data),
            PartBody::DiskSlice(slice) => slice.read_to_vec().await,
        }
    }

    /// A `reqwest` body for streamable providers. `Owned` sends the in-memory
    /// buffer; `DiskSlice` streams one pooled window at a time. Set the
    /// `Content-Length` from [`len`](Self::len); the streamed body carries no
    /// implicit length.
    pub fn into_reqwest_body(self) -> reqwest::Body {
        match self {
            PartBody::Owned(data) => reqwest::Body::from(data),
            PartBody::DiskSlice(slice) => {
                let scratch = part_buffer_pool().acquire();
                reqwest::Body::wrap_stream(disk_slice_window_stream(slice, scratch))
            }
        }
    }
}

/// Mint an independent worker for a concurrent part upload, or `None` when
/// parts must serialise on the shared session mutex.
pub fn clone_multipart_worker(provider: &dyn StorageProvider) -> Option<Box<dyn StorageProvider>> {
    provider.clone_for_transfer().ok()
}

/// Map a provider/string error into a typed [`TransferFailure`].
///
/// Classification happens once at this adapter boundary (via the shared
/// [`TransferError`] taxonomy). Controllers later read only machine fields.
pub fn transfer_failure_from_message(message: &str, _path_hint: Option<&str>) -> TransferFailure {
    if message.to_lowercase().contains("cancel") {
        return cancelled_failure();
    }
    // Lift typed congestion / Retry-After from the raw message before the
    // redacted user-facing string replaces it.
    TransferFailure::from_raw_message(message)
}

pub fn transfer_failure_from_provider(
    error: &ProviderError,
    _path_hint: Option<&str>,
) -> TransferFailure {
    use crate::transfer_dag::TransferError;
    // Discriminant-first mapping (Timeout, Cancelled, ConnectionLost, …) with
    // a single text classification only for string-bearing variants.
    let typed = TransferError::from_provider(error);
    TransferFailure::from_transfer_error(&typed)
}

pub fn cancelled_failure() -> TransferFailure {
    TransferFailure::new(
        TransferFailureKind::Cancelled,
        "Transfer cancelled by user",
        false,
    )
}

pub fn unsupported_multipart_failure() -> TransferFailure {
    TransferFailure::new(
        TransferFailureKind::Unknown,
        "Executor does not implement multipart per-part wire I/O",
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_part_ranges_cover_file_without_gap_or_overlap() {
        let layout = MultipartLayout {
            total_size: 25 * 1024 * 1024,
            total_parts: 4,
            preferred_part_size: 8 * 1024 * 1024,
            content_type: "application/octet-stream".into(),
        };
        let mut covered = 0u64;
        let mut prev_end = 0u64;
        for part in 1..=4 {
            let (off, len) = layout.part_range(part).expect("range");
            assert_eq!(off, prev_end);
            covered += len;
            prev_end = off + len;
        }
        assert_eq!(covered, layout.total_size);
        assert_eq!(
            layout.part_range(4).unwrap().1,
            layout.total_size - 3 * layout.preferred_part_size
        );
    }

    #[tokio::test]
    async fn receipts_sort_and_dedupe() {
        let layout = MultipartLayout {
            total_size: 30,
            total_parts: 3,
            preferred_part_size: 10,
            content_type: "application/octet-stream".into(),
        };
        let state = MultipartFileState::new(layout, HashMap::from([(1, 1), (2, 2), (3, 3)]));
        state
            .store_receipt(UploadedPart {
                part_number: 2,
                etag: "b".into(),
            })
            .await
            .unwrap();
        state
            .store_receipt(UploadedPart {
                part_number: 1,
                etag: "a".into(),
            })
            .await
            .unwrap();
        state
            .store_receipt(UploadedPart {
                part_number: 3,
                etag: "c".into(),
            })
            .await
            .unwrap();
        assert!(state
            .store_receipt(UploadedPart {
                part_number: 2,
                etag: "dup".into(),
            })
            .await
            .is_err());
        let sorted = state.take_sorted_receipts().await;
        assert_eq!(
            sorted.iter().map(|p| p.part_number).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[tokio::test]
    async fn receipt_must_match_the_requested_part() {
        let layout = MultipartLayout {
            total_size: 20,
            total_parts: 2,
            preferred_part_size: 10,
            content_type: "application/octet-stream".into(),
        };
        let state = MultipartFileState::new(layout, HashMap::from([(1, 1), (2, 2)]));

        let failure = state
            .store_receipt_for_part(
                1,
                UploadedPart {
                    part_number: 2,
                    etag: "wrong-part".into(),
                },
            )
            .await
            .expect_err("mismatched receipt must fail closed");

        assert!(failure.message.contains("does not match requested part 1"));
        assert_eq!(state.receipt_count().await, 0);
    }

    #[tokio::test]
    async fn abort_take_is_once() {
        let layout = MultipartLayout {
            total_size: 10,
            total_parts: 1,
            preferred_part_size: 10,
            content_type: "application/octet-stream".into(),
        };
        let state = MultipartFileState::new(layout, HashMap::from([(1, 1)]));
        state
            .install_handle(MultipartHandle {
                upload_id: "u1".into(),
                remote_path: "/r".into(),
            })
            .await;
        assert!(state.take_for_abort().await.is_some());
        assert!(state.take_for_abort().await.is_none());
    }

    // ------------------------------------------------------------------
    // DAG-P2-05: part body + reusable window pool
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn part_buffer_pool_reuses_one_window_across_sequential_parts() {
        let pool = Arc::new(PartBufferPool::new(2, 4096));
        // Ten sequential parts, each acquiring then releasing before the next:
        // the pool must reuse a single window, not allocate ten distinct ones.
        for _ in 0..10 {
            let window = pool.acquire();
            assert_eq!(window.window_ref().len(), 4096);
            drop(window);
        }
        assert_eq!(
            pool.allocations(),
            1,
            "sequential parts must reuse one pooled window, not allocate per part"
        );
        assert!(pool.idle() <= 2);
    }

    #[tokio::test]
    async fn part_buffer_pool_allocations_track_concurrent_demand_then_reuse() {
        let pool = Arc::new(PartBufferPool::new(4, 1024));
        let a = pool.acquire();
        let b = pool.acquire();
        let c = pool.acquire();
        assert_eq!(
            pool.allocations(),
            3,
            "three concurrently held windows need three buffers"
        );
        drop((a, b, c));
        // Released buffers are parked; a fresh acquire allocates none.
        let _d = pool.acquire();
        assert_eq!(pool.allocations(), 3);
    }

    #[tokio::test]
    async fn disk_slice_reads_and_streams_exact_bytes_reusing_one_window() {
        use futures_util::StreamExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("part.bin");
        let content: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        tokio::fs::write(&path, &content).await.expect("write");

        let offset = 10_000u64;
        let len = 200_000u64;
        let expected = &content[offset as usize..(offset + len) as usize];

        // Materialize path yields the exact slice.
        let slice = DiskSlicePart::new(path.clone(), offset, len);
        let owned = slice.read_to_vec().await.expect("read slice");
        assert_eq!(owned.as_slice(), expected);

        // Streaming path yields the same bytes, reusing a single pooled window
        // across the windows the 200 KiB slice spans (window = 128 KiB).
        let pool = Arc::new(PartBufferPool::new(4, PART_STREAM_WINDOW_BYTES));
        let scratch = pool.acquire();
        let mut stream = Box::pin(disk_slice_window_stream(slice, scratch));
        let mut streamed = Vec::new();
        while let Some(chunk) = stream.next().await {
            streamed.extend_from_slice(&chunk.expect("chunk"));
        }
        drop(stream);
        assert_eq!(streamed.as_slice(), expected);
        assert_eq!(
            pool.allocations(),
            1,
            "one streamed part flows through a single reused window"
        );
    }

    #[tokio::test]
    async fn part_body_owned_and_disk_slice_agree_on_len_and_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("p.bin");
        let content: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
        tokio::fs::write(&path, &content).await.expect("write");

        let owned = PartBody::owned(content[100..400].to_vec());
        let disk = PartBody::disk_slice(path, 100, 300);
        assert_eq!(owned.len(), 300);
        assert_eq!(disk.len(), 300);
        assert!(owned.is_replayable() && disk.is_replayable());
        assert_eq!(
            owned.into_owned_bytes().await.unwrap(),
            disk.into_owned_bytes().await.unwrap()
        );
    }
}
