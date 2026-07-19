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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::Mutex;

use crate::providers::{MultipartHandle, ProviderError, StorageProvider, UploadedPart};
use crate::transfer_dag::multipart_part_byte_len;
use crate::transfer_domain::{
    transfer_failure_kind_from_sync, user_facing_transfer_failure_message, TransferFailure,
    TransferFailureKind,
};

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

    /// Store a successful part receipt. Fails closed on duplicates.
    pub async fn store_receipt(&self, receipt: UploadedPart) -> Result<(), TransferFailure> {
        let part_number = receipt.part_number;
        if part_number == 0 || part_number > self.layout.total_parts {
            return Err(TransferFailure {
                kind: TransferFailureKind::Unknown,
                message: format!(
                    "multipart receipt part {} out of range 1..={}",
                    part_number, self.layout.total_parts
                ),
                retryable: false,
            });
        }
        let mut parts = self.parts.lock().await;
        if parts.contains_key(&part_number) {
            return Err(TransferFailure {
                kind: TransferFailureKind::Unknown,
                message: format!("duplicate multipart receipt for part {}", part_number),
                retryable: false,
            });
        }
        parts.insert(part_number, receipt);
        Ok(())
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

/// Mint an independent worker for a concurrent part upload, or `None` when
/// parts must serialise on the shared session mutex.
pub fn clone_multipart_worker(provider: &dyn StorageProvider) -> Option<Box<dyn StorageProvider>> {
    provider.clone_for_transfer().ok()
}

/// Map a provider/string error into a typed [`TransferFailure`].
pub fn transfer_failure_from_message(message: &str, path_hint: Option<&str>) -> TransferFailure {
    if message.to_lowercase().contains("cancel") {
        return TransferFailure {
            kind: TransferFailureKind::Cancelled,
            message: "Transfer cancelled by user".to_string(),
            retryable: false,
        };
    }
    let error_info = crate::sync::classify_sync_error(message, path_hint);
    let kind = transfer_failure_kind_from_sync(&error_info.kind);
    TransferFailure {
        kind,
        message: user_facing_transfer_failure_message(&kind).to_string(),
        retryable: error_info.retryable,
    }
}

pub fn transfer_failure_from_provider(
    error: &ProviderError,
    path_hint: Option<&str>,
) -> TransferFailure {
    transfer_failure_from_message(&error.to_string(), path_hint)
}

pub fn cancelled_failure() -> TransferFailure {
    TransferFailure {
        kind: TransferFailureKind::Cancelled,
        message: "Transfer cancelled by user".to_string(),
        retryable: false,
    }
}

pub fn unsupported_multipart_failure() -> TransferFailure {
    TransferFailure {
        kind: TransferFailureKind::Unknown,
        message: "Executor does not implement multipart per-part wire I/O".to_string(),
        retryable: false,
    }
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
}
