// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Per-operation transfer resource model (DAG-P0-06).
//!
//! # Directional disk slots
//!
//! Whole-file and part transfers reserve only the disk direction they actually
//! use: uploads and upload parts take `disk_read`; downloads and range nodes
//! take `disk_write`. Server-side copies reserve neither.
//!
//! # Byte-buffer credits
//!
//! Buffers whose size AeroFTP owns before allocation (today: multipart
//! `UploadPart` full-part `Vec<u8>`) request `buffer_bytes`. Credits are
//! acquired as weighted quanta (`BUFFER_QUANTUM_BYTES`), not one semaphore
//! permit per byte — Tokio's `acquire_many_owned` count is `u32`.
//!
//! ## Budget policy
//!
//! Per-manager default: `min(512 MiB, max(64 MiB, 10% MemAvailable))` on Linux
//! via `/proc/meminfo`, else a fixed 256 MiB cross-platform fallback. Override
//! with `AEROFTP_TRANSFER_BUFFER_BYTES` (parsed as raw bytes; clamped to
//! `[MIN_BUFFER_BUDGET_BYTES, MAX_BUFFER_BUDGET_BYTES]` when non-zero). This is
//! **not** a process-global governor (`DAG-P2-01`).
//!
//! ## Oversize-part policy
//!
//! A single part whose rounded credit demand exceeds the usable manager pool is
//! admitted through a dedicated one-at-a-time oversize lane: it takes the full
//! quantum pool plus the oversize permit. Concurrent oversize parts cannot hold
//! leases together. Pool capacity rounds down to whole quanta, so normal
//! allocations never exceed the configured byte budget.
//! Peak RSS may therefore briefly equal that one oversized part; the cap is
//! never silently disabled for in-budget concurrent parts.
//!
//! Zero-byte requests are a no-op. Dropping a [`ResourceLease`] returns every
//! credit (including oversize and quanta).

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::capabilities::TransferCapabilities;

/// Allocation quantum for buffer-byte credits (64 KiB).
///
/// Requests round **up** to the next quantum so accounting never under-books.
/// Chosen so a 512 MiB budget is 8192 quanta (well under `u32::MAX`) while
/// still coarse enough that acquire cost stays cheap.
pub const BUFFER_QUANTUM_BYTES: u64 = 64 * 1024;

/// Floor for automatic / env buffer budgets (64 MiB).
pub const MIN_BUFFER_BUDGET_BYTES: u64 = 64 * 1024 * 1024;

/// Ceiling for automatic / env buffer budgets (512 MiB).
pub const MAX_BUFFER_BUDGET_BYTES: u64 = 512 * 1024 * 1024;

/// Fixed cross-platform fallback when system memory is unavailable (256 MiB).
pub const DEFAULT_BUFFER_BUDGET_BYTES: u64 = 256 * 1024 * 1024;

/// Env override: raw byte count for the per-manager buffer budget.
pub const BUFFER_BUDGET_ENV: &str = "AEROFTP_TRANSFER_BUFFER_BYTES";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    FileSlot,
    CheckerSlot,
    ChunkSlot,
    HttpSlot,
    ApiSlot,
    DiskReadSlot,
    DiskWriteSlot,
    HashSlot,
    /// Weighted buffer-byte credits (acquired as quanta, not per-byte permits).
    BufferBytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferBudget {
    pub file_slots: u16,
    pub checker_slots: u16,
    pub chunk_slots: u16,
    pub http_slots: u16,
    pub api_slots: u16,
    pub disk_read_slots: u16,
    pub disk_write_slots: u16,
    pub hash_slots: u16,
    /// Soft peak for owned full-buffer allocations under this manager (bytes).
    #[serde(default = "default_buffer_budget_field")]
    pub buffer_bytes: u64,
}

fn default_buffer_budget_field() -> u64 {
    DEFAULT_BUFFER_BUDGET_BYTES
}

impl Default for TransferBudget {
    fn default() -> Self {
        Self {
            file_slots: 4,
            checker_slots: 8,
            chunk_slots: 1,
            http_slots: 8,
            api_slots: 4,
            disk_read_slots: 4,
            disk_write_slots: 4,
            hash_slots: 2,
            // Deterministic default for unit tests and callers that do not
            // resolve a system budget. Production runners should call
            // [`TransferBudget::with_resolved_buffer_budget`].
            buffer_bytes: DEFAULT_BUFFER_BUDGET_BYTES,
        }
    }
}

impl TransferBudget {
    pub fn from_file_slots(file_slots: u16) -> Self {
        let file_slots = file_slots.max(1);
        Self {
            file_slots,
            disk_read_slots: file_slots,
            disk_write_slots: file_slots,
            ..Self::default()
        }
    }

    /// Replace `buffer_bytes` with the runtime policy (env / system / fallback).
    pub fn with_resolved_buffer_budget(mut self) -> Self {
        self.buffer_bytes = resolve_buffer_budget_bytes();
        self
    }

    /// Explicit buffer budget (e.g. tests). Zero is preserved so callers can
    /// construct a manager that rejects every non-zero buffer request.
    pub fn with_buffer_bytes(mut self, buffer_bytes: u64) -> Self {
        self.buffer_bytes = buffer_bytes;
        self
    }

    pub fn clamped_for_capabilities(mut self, caps: &TransferCapabilities) -> Self {
        if let Some(max) = caps.max_file_slots {
            self.file_slots = self.file_slots.min(max.max(1));
        }
        if let Some(max) = caps.max_checker_slots {
            self.checker_slots = self.checker_slots.min(max.max(1));
        }
        if let Some(max) = caps.max_chunk_slots {
            self.chunk_slots = self.chunk_slots.min(max.max(1));
        }
        self.http_slots = self.http_slots.max(1);
        self.api_slots = self.api_slots.max(1);
        self.disk_read_slots = self.disk_read_slots.max(1);
        self.disk_write_slots = self.disk_write_slots.max(1);
        self.hash_slots = self.hash_slots.max(1);
        self
    }
}

/// Resolve the per-manager buffer budget: env override, then Linux MemAvailable
/// heuristic, else [`DEFAULT_BUFFER_BUDGET_BYTES`].
pub fn resolve_buffer_budget_bytes() -> u64 {
    if let Ok(raw) = std::env::var(BUFFER_BUDGET_ENV) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            if let Ok(parsed) = trimmed.parse::<u64>() {
                if parsed == 0 {
                    return DEFAULT_BUFFER_BUDGET_BYTES;
                }
                return parsed.clamp(MIN_BUFFER_BUDGET_BYTES, MAX_BUFFER_BUDGET_BYTES);
            }
        }
    }
    match system_available_memory_bytes() {
        Some(available) => {
            // 10% of available RAM, floored at 64 MiB, capped at 512 MiB.
            let tenth = available / 10;
            tenth.clamp(MIN_BUFFER_BUDGET_BYTES, MAX_BUFFER_BUDGET_BYTES)
        }
        None => DEFAULT_BUFFER_BUDGET_BYTES,
    }
}

/// Best-effort available memory. Linux: `MemAvailable` from `/proc/meminfo`.
fn system_available_memory_bytes() -> Option<u64> {
    let contents = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("MemAvailable:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return kb.checked_mul(1024);
        }
    }
    None
}

/// Convert a byte request into quanta, rounding up. Zero bytes → zero quanta.
/// Saturates at `u32::MAX` so `acquire_many_owned` never overflows.
pub fn buffer_bytes_to_quanta(bytes: u64) -> u32 {
    if bytes == 0 {
        return 0;
    }
    let q = bytes.div_ceil(BUFFER_QUANTUM_BYTES);
    u32::try_from(q).unwrap_or(u32::MAX)
}

/// Quanta capacity for a budgeted byte pool.
///
/// Capacity rounds down so ordinary leases can never represent more bytes
/// than the configured budget. A positive sub-quantum budget still gets one
/// quantum; requests larger than its raw byte budget use the oversize lane.
pub fn buffer_budget_to_quanta(budget_bytes: u64) -> u32 {
    if budget_bytes == 0 {
        return 0;
    }
    u32::try_from(budget_bytes / BUFFER_QUANTUM_BYTES)
        .unwrap_or(u32::MAX)
        .max(1)
}

/// Byte length of multipart part `part_index` (0-based) under the shared
/// runner/builder sizing formula: fixed `preferred_chunk_size` for all but the
/// last part, which may be shorter. Out-of-range indices yield 0.
pub fn multipart_part_byte_len(
    file_size: u64,
    part_index: usize,
    part_count: usize,
    preferred_chunk_size: u64,
) -> u64 {
    if part_count == 0 || preferred_chunk_size == 0 || part_index >= part_count {
        return 0;
    }
    let offset = (part_index as u64).saturating_mul(preferred_chunk_size);
    if offset >= file_size {
        return 0;
    }
    preferred_chunk_size.min(file_size - offset)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResourceRequest {
    pub file_slots: u16,
    pub checker_slots: u16,
    pub chunk_slots: u16,
    pub http_slots: u16,
    pub api_slots: u16,
    pub disk_read_slots: u16,
    pub disk_write_slots: u16,
    pub hash_slots: u16,
    /// Owned buffer bytes this node may allocate while the lease is held.
    pub buffer_bytes: u64,
}

impl ResourceRequest {
    /// Upload whole-file: read local disk, no local write.
    pub fn upload_file() -> Self {
        Self {
            file_slots: 1,
            disk_read_slots: 1,
            ..Self::default()
        }
    }

    /// Download whole-file: write local disk, no local read of payload.
    pub fn download_file() -> Self {
        Self {
            file_slots: 1,
            disk_write_slots: 1,
            ..Self::default()
        }
    }

    /// One multipart upload part: chunk slot + disk read + exact part buffer.
    pub fn upload_part(buffer_bytes: u64) -> Self {
        Self {
            chunk_slots: 1,
            disk_read_slots: 1,
            buffer_bytes,
            ..Self::default()
        }
    }

    /// Segmented download range: chunk + http + local write. Streaming runners
    /// do not own a full-range buffer, so `buffer_bytes` stays 0.
    pub fn range_chunk() -> Self {
        Self {
            chunk_slots: 1,
            http_slots: 1,
            disk_write_slots: 1,
            ..Self::default()
        }
    }

    /// Server-side copy: API only, no disk, no local buffer.
    pub fn server_copy(api_slots: u16) -> Self {
        Self {
            api_slots: api_slots.max(1),
            ..Self::default()
        }
    }

    pub fn checker() -> Self {
        Self {
            checker_slots: 1,
            ..Self::default()
        }
    }

    /// Attach optional rate-limit API slots to an existing request.
    pub fn with_api_slots(mut self, api_slots: u16) -> Self {
        self.api_slots = api_slots;
        self
    }
}

#[derive(Clone)]
pub struct TransferResourceManager {
    budget: TransferBudget,
    /// Total quanta this manager was constructed with (for oversize full-pool).
    buffer_quanta_capacity: u32,
    file_slots: Arc<Semaphore>,
    checker_slots: Arc<Semaphore>,
    chunk_slots: Arc<Semaphore>,
    http_slots: Arc<Semaphore>,
    api_slots: Arc<Semaphore>,
    disk_read_slots: Arc<Semaphore>,
    disk_write_slots: Arc<Semaphore>,
    hash_slots: Arc<Semaphore>,
    buffer_quanta: Arc<Semaphore>,
    /// Serialises parts larger than the configured buffer budget.
    oversize_lane: Arc<Semaphore>,
}

pub struct ResourceLease {
    _permits: Vec<OwnedSemaphorePermit>,
}

impl TransferResourceManager {
    pub fn new(budget: TransferBudget) -> Self {
        let quanta = buffer_budget_to_quanta(budget.buffer_bytes);
        Self {
            budget,
            buffer_quanta_capacity: quanta,
            file_slots: Arc::new(Semaphore::new(budget.file_slots.max(1) as usize)),
            checker_slots: Arc::new(Semaphore::new(budget.checker_slots.max(1) as usize)),
            chunk_slots: Arc::new(Semaphore::new(budget.chunk_slots.max(1) as usize)),
            http_slots: Arc::new(Semaphore::new(budget.http_slots.max(1) as usize)),
            api_slots: Arc::new(Semaphore::new(budget.api_slots.max(1) as usize)),
            disk_read_slots: Arc::new(Semaphore::new(budget.disk_read_slots.max(1) as usize)),
            disk_write_slots: Arc::new(Semaphore::new(budget.disk_write_slots.max(1) as usize)),
            hash_slots: Arc::new(Semaphore::new(budget.hash_slots.max(1) as usize)),
            // Zero-capacity semaphore: acquire waits forever; preflight should
            // reject non-zero buffer requests when budget is 0. We still install
            // a semaphore so drop paths stay uniform.
            buffer_quanta: Arc::new(Semaphore::new(quanta as usize)),
            oversize_lane: Arc::new(Semaphore::new(1)),
        }
    }

    pub fn budget(&self) -> TransferBudget {
        self.budget
    }

    /// Available buffer quanta right now (test / diagnostics).
    pub fn available_buffer_quanta(&self) -> usize {
        self.buffer_quanta.available_permits()
    }

    /// Available oversize-lane permits (0 or 1).
    pub fn available_oversize_permits(&self) -> usize {
        self.oversize_lane.available_permits()
    }

    pub async fn acquire(&self, request: ResourceRequest) -> Result<ResourceLease, String> {
        let mut permits = Vec::new();

        acquire_many(
            &self.file_slots,
            request.file_slots,
            &mut permits,
            ResourceKind::FileSlot,
        )
        .await?;
        acquire_many(
            &self.checker_slots,
            request.checker_slots,
            &mut permits,
            ResourceKind::CheckerSlot,
        )
        .await?;
        acquire_many(
            &self.chunk_slots,
            request.chunk_slots,
            &mut permits,
            ResourceKind::ChunkSlot,
        )
        .await?;
        acquire_many(
            &self.http_slots,
            request.http_slots,
            &mut permits,
            ResourceKind::HttpSlot,
        )
        .await?;
        acquire_many(
            &self.api_slots,
            request.api_slots,
            &mut permits,
            ResourceKind::ApiSlot,
        )
        .await?;
        acquire_many(
            &self.disk_read_slots,
            request.disk_read_slots,
            &mut permits,
            ResourceKind::DiskReadSlot,
        )
        .await?;
        acquire_many(
            &self.disk_write_slots,
            request.disk_write_slots,
            &mut permits,
            ResourceKind::DiskWriteSlot,
        )
        .await?;
        acquire_many(
            &self.hash_slots,
            request.hash_slots,
            &mut permits,
            ResourceKind::HashSlot,
        )
        .await?;
        self.acquire_buffer_credits(request.buffer_bytes, &mut permits)
            .await?;

        Ok(ResourceLease { _permits: permits })
    }

    async fn acquire_buffer_credits(
        &self,
        buffer_bytes: u64,
        permits: &mut Vec<OwnedSemaphorePermit>,
    ) -> Result<(), String> {
        if buffer_bytes == 0 {
            return Ok(());
        }

        let budget = self.budget.buffer_bytes;
        if budget == 0 || self.buffer_quanta_capacity == 0 {
            return Err("buffer_bytes requested but the manager buffer budget is zero".to_string());
        }

        let requested_quanta = buffer_bytes_to_quanta(buffer_bytes);
        if buffer_bytes > budget || requested_quanta > self.buffer_quanta_capacity {
            // Oversize one-at-a-time: exclusive lane + entire quantum pool so
            // concurrent in-budget parts cannot run beside an oversize part.
            let oversize = self
                .oversize_lane
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| {
                    "resource manager closed while acquiring oversize buffer lane".to_string()
                })?;
            permits.push(oversize);

            let all = self
                .buffer_quanta
                .clone()
                .acquire_many_owned(self.buffer_quanta_capacity)
                .await
                .map_err(|_| {
                    format!(
                        "resource manager closed while acquiring {:?}",
                        ResourceKind::BufferBytes
                    )
                })?;
            permits.push(all);
            return Ok(());
        }

        if requested_quanta == 0 {
            return Ok(());
        }
        let permit = self
            .buffer_quanta
            .clone()
            .acquire_many_owned(requested_quanta)
            .await
            .map_err(|_| {
                format!(
                    "resource manager closed while acquiring {:?}",
                    ResourceKind::BufferBytes
                )
            })?;
        permits.push(permit);
        Ok(())
    }
}

async fn acquire_many(
    semaphore: &Arc<Semaphore>,
    count: u16,
    permits: &mut Vec<OwnedSemaphorePermit>,
    kind: ResourceKind,
) -> Result<(), String> {
    for _ in 0..count {
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| format!("resource manager closed while acquiring {:?}", kind))?;
        permits.push(permit);
    }
    Ok(())
}

/// Returns a human-readable reason if `request` can never be satisfied.
///
/// Slot classes fail when the request asks for more than the manager owns.
/// Buffer credits: zero-budget + non-zero request is unschedulable. A request
/// larger than the budget is **schedulable** via the oversize lane (see module
/// docs) and is not rejected here.
pub fn request_exceeds_budget(
    request: &ResourceRequest,
    budget: &TransferBudget,
) -> Option<String> {
    // Each slot semaphore is sized at `budget.X.max(1)` (see TransferResourceManager).
    let checks: [(&str, u16, u16); 8] = [
        ("file_slots", request.file_slots, budget.file_slots.max(1)),
        (
            "checker_slots",
            request.checker_slots,
            budget.checker_slots.max(1),
        ),
        (
            "chunk_slots",
            request.chunk_slots,
            budget.chunk_slots.max(1),
        ),
        ("http_slots", request.http_slots, budget.http_slots.max(1)),
        ("api_slots", request.api_slots, budget.api_slots.max(1)),
        (
            "disk_read_slots",
            request.disk_read_slots,
            budget.disk_read_slots.max(1),
        ),
        (
            "disk_write_slots",
            request.disk_write_slots,
            budget.disk_write_slots.max(1),
        ),
        ("hash_slots", request.hash_slots, budget.hash_slots.max(1)),
    ];
    for (name, want, have) in checks {
        if want > have {
            return Some(format!(
                "requests {want} {name} but the budget owns only {have}"
            ));
        }
    }
    if request.buffer_bytes > 0 && budget.buffer_bytes == 0 {
        return Some(
            "requests buffer_bytes but the budget owns a zero-byte buffer pool".to_string(),
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Barrier;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn clamps_budget_to_capabilities() {
        let budget =
            TransferBudget::from_file_slots(8).clamped_for_capabilities(&TransferCapabilities {
                max_file_slots: Some(1),
                max_checker_slots: Some(2),
                max_chunk_slots: Some(1),
                ..TransferCapabilities::default()
            });

        assert_eq!(budget.file_slots, 1);
        assert_eq!(budget.checker_slots, 2);
        assert_eq!(budget.chunk_slots, 1);
    }

    #[test]
    fn file_slot_budget_preserves_legacy_batch_parallelism() {
        let budget = TransferBudget::from_file_slots(8);

        assert_eq!(budget.file_slots, 8);
        assert_eq!(budget.disk_read_slots, 8);
        assert_eq!(budget.disk_write_slots, 8);
        assert_eq!(budget.buffer_bytes, DEFAULT_BUFFER_BUDGET_BYTES);
    }

    #[test]
    fn upload_request_is_disk_read_only() {
        let r = ResourceRequest::upload_file();
        assert_eq!(r.file_slots, 1);
        assert_eq!(r.disk_read_slots, 1);
        assert_eq!(r.disk_write_slots, 0);
        assert_eq!(r.buffer_bytes, 0);
    }

    #[test]
    fn download_request_is_disk_write_only() {
        let r = ResourceRequest::download_file();
        assert_eq!(r.file_slots, 1);
        assert_eq!(r.disk_read_slots, 0);
        assert_eq!(r.disk_write_slots, 1);
        assert_eq!(r.buffer_bytes, 0);
    }

    #[test]
    fn upload_part_request_is_disk_read_with_buffer() {
        let r = ResourceRequest::upload_part(8 * 1024 * 1024);
        assert_eq!(r.chunk_slots, 1);
        assert_eq!(r.disk_read_slots, 1);
        assert_eq!(r.disk_write_slots, 0);
        assert_eq!(r.buffer_bytes, 8 * 1024 * 1024);
    }

    #[test]
    fn range_request_is_disk_write_without_full_buffer() {
        let r = ResourceRequest::range_chunk();
        assert_eq!(r.chunk_slots, 1);
        assert_eq!(r.http_slots, 1);
        assert_eq!(r.disk_write_slots, 1);
        assert_eq!(r.disk_read_slots, 0);
        assert_eq!(r.buffer_bytes, 0);
    }

    #[test]
    fn server_copy_reserves_neither_disk_direction() {
        let r = ResourceRequest::server_copy(1);
        assert_eq!(r.api_slots, 1);
        assert_eq!(r.disk_read_slots, 0);
        assert_eq!(r.disk_write_slots, 0);
        assert_eq!(r.file_slots, 0);
        assert_eq!(r.buffer_bytes, 0);
    }

    #[test]
    fn quanta_round_up_and_zero_is_noop() {
        assert_eq!(buffer_bytes_to_quanta(0), 0);
        assert_eq!(buffer_bytes_to_quanta(1), 1);
        assert_eq!(buffer_bytes_to_quanta(BUFFER_QUANTUM_BYTES), 1);
        assert_eq!(buffer_bytes_to_quanta(BUFFER_QUANTUM_BYTES + 1), 2);
    }

    #[test]
    fn budget_quanta_never_round_above_configured_bytes() {
        let q = BUFFER_QUANTUM_BYTES;
        assert_eq!(buffer_budget_to_quanta(0), 0);
        assert_eq!(buffer_budget_to_quanta(1), 1);
        assert_eq!(buffer_budget_to_quanta(q), 1);
        assert_eq!(buffer_budget_to_quanta(q + 1), 1);
        assert_eq!(buffer_budget_to_quanta(q * 2 - 1), 1);
        assert_eq!(buffer_budget_to_quanta(q * 2), 2);
    }

    #[test]
    fn multipart_part_byte_len_covers_full_and_short_tail() {
        let file_size = 25 * 1024 * 1024;
        let chunk = 8 * 1024 * 1024;
        let parts = 4usize; // 8+8+8+1 MiB
        assert_eq!(
            multipart_part_byte_len(file_size, 0, parts, chunk),
            8 * 1024 * 1024
        );
        assert_eq!(
            multipart_part_byte_len(file_size, 3, parts, chunk),
            file_size - 3 * chunk
        );
        assert_eq!(multipart_part_byte_len(file_size, 4, parts, chunk), 0);
    }

    #[test]
    fn multipart_part_byte_len_part_count_clamp_case() {
        // When preferred_chunk_size was grown to cover the file after a part
        // clamp, the last index still uses the short-tail formula.
        let file_size = 100u64;
        let parts = 3usize;
        let chunk = 40u64; // 40+40+20
        assert_eq!(multipart_part_byte_len(file_size, 0, parts, chunk), 40);
        assert_eq!(multipart_part_byte_len(file_size, 1, parts, chunk), 40);
        assert_eq!(multipart_part_byte_len(file_size, 2, parts, chunk), 20);
    }

    #[test]
    fn preflight_rejects_zero_buffer_budget_but_not_oversize() {
        let budget = TransferBudget::from_file_slots(1).with_buffer_bytes(0);
        let want = ResourceRequest::upload_part(1024);
        assert!(request_exceeds_budget(&want, &budget).is_some());

        let budget = TransferBudget::from_file_slots(1).with_buffer_bytes(1024);
        let oversize = ResourceRequest::upload_part(4096);
        assert!(request_exceeds_budget(&oversize, &budget).is_none());
    }

    #[tokio::test]
    async fn resource_lease_returns_permits_on_drop() {
        let manager = TransferResourceManager::new(TransferBudget::from_file_slots(1));
        let lease = manager
            .acquire(ResourceRequest::upload_file())
            .await
            .unwrap();
        assert_eq!(manager.file_slots.available_permits(), 0);
        drop(lease);
        assert_eq!(manager.file_slots.available_permits(), 1);
    }

    #[tokio::test]
    async fn two_buffer_requests_over_budget_cannot_hold_together() {
        let quantum = BUFFER_QUANTUM_BYTES;
        let budget = TransferBudget::from_file_slots(1).with_buffer_bytes(quantum * 3);
        let manager = Arc::new(TransferResourceManager::new(budget));

        let a = manager
            .acquire(ResourceRequest {
                buffer_bytes: quantum * 2,
                ..ResourceRequest::default()
            })
            .await
            .unwrap();
        assert!(manager.available_buffer_quanta() < 3);

        let manager2 = manager.clone();
        let barrier = Arc::new(Barrier::new(2));
        let b_barrier = barrier.clone();
        let waiter = tokio::spawn(async move {
            b_barrier.wait().await;
            manager2
                .acquire(ResourceRequest {
                    buffer_bytes: quantum * 2,
                    ..ResourceRequest::default()
                })
                .await
        });

        // Waiter cannot complete while `a` holds 2 of 3 quanta.
        barrier.wait().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished());

        drop(a);
        let b = waiter.await.unwrap().unwrap();
        drop(b);
        assert_eq!(manager.available_buffer_quanta(), 3);
    }

    #[tokio::test]
    async fn cancel_while_waiting_for_byte_credits_leaks_no_permits() {
        let quantum = BUFFER_QUANTUM_BYTES;
        let budget = TransferBudget::from_file_slots(1).with_buffer_bytes(quantum * 2);
        let manager = Arc::new(TransferResourceManager::new(budget));

        let held = manager
            .acquire(ResourceRequest {
                buffer_bytes: quantum * 2,
                ..ResourceRequest::default()
            })
            .await
            .unwrap();
        assert_eq!(manager.available_buffer_quanta(), 0);

        let cancel = CancellationToken::new();
        let manager2 = manager.clone();
        let cancel2 = cancel.clone();
        let waiter = tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = cancel2.cancelled() => Err("cancelled".to_string()),
                r = manager2.acquire(ResourceRequest {
                    buffer_bytes: quantum,
                    ..ResourceRequest::default()
                }) => r.map_err(|e| e),
            }
        });

        tokio::time::sleep(Duration::from_millis(30)).await;
        cancel.cancel();
        let result = waiter.await.unwrap();
        assert!(result.is_err());

        // No partial lease retained; full pool returns when holder drops.
        drop(held);
        assert_eq!(manager.available_buffer_quanta(), 2);
        assert_eq!(manager.available_oversize_permits(), 1);
    }

    #[tokio::test]
    async fn oversize_part_is_one_at_a_time() {
        let quantum = BUFFER_QUANTUM_BYTES;
        let budget = TransferBudget::from_file_slots(1).with_buffer_bytes(quantum * 2);
        let manager = Arc::new(TransferResourceManager::new(budget));

        let first = manager
            .acquire(ResourceRequest {
                buffer_bytes: quantum * 10, // > budget
                ..ResourceRequest::default()
            })
            .await
            .unwrap();
        assert_eq!(manager.available_oversize_permits(), 0);
        assert_eq!(manager.available_buffer_quanta(), 0);

        let manager2 = manager.clone();
        let second = tokio::spawn(async move {
            manager2
                .acquire(ResourceRequest {
                    buffer_bytes: quantum * 10,
                    ..ResourceRequest::default()
                })
                .await
        });
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(!second.is_finished());

        drop(first);
        let lease = second.await.unwrap().unwrap();
        drop(lease);
        assert_eq!(manager.available_oversize_permits(), 1);
        assert_eq!(manager.available_buffer_quanta(), 2);
    }

    #[tokio::test]
    async fn non_aligned_budget_routes_unrepresentable_tail_through_oversize_lane() {
        let quantum = BUFFER_QUANTUM_BYTES;
        // Usable normal pool rounds down to one quantum. A request equal to
        // the raw byte budget needs two rounded quanta and must not hang.
        let budget = TransferBudget::from_file_slots(1).with_buffer_bytes(quantum + 1);
        let manager = TransferResourceManager::new(budget);
        assert_eq!(manager.available_buffer_quanta(), 1);

        let lease = tokio::time::timeout(
            Duration::from_secs(1),
            manager.acquire(ResourceRequest {
                buffer_bytes: quantum + 1,
                ..ResourceRequest::default()
            }),
        )
        .await
        .expect("non-aligned request must route to oversize instead of hanging")
        .unwrap();
        assert_eq!(manager.available_oversize_permits(), 0);
        assert_eq!(manager.available_buffer_quanta(), 0);

        drop(lease);
        assert_eq!(manager.available_oversize_permits(), 1);
        assert_eq!(manager.available_buffer_quanta(), 1);
    }

    #[tokio::test]
    async fn zero_byte_buffer_request_is_noop() {
        let manager = TransferResourceManager::new(
            TransferBudget::from_file_slots(1).with_buffer_bytes(BUFFER_QUANTUM_BYTES),
        );
        let before = manager.available_buffer_quanta();
        let lease = manager
            .acquire(ResourceRequest {
                buffer_bytes: 0,
                ..ResourceRequest::default()
            })
            .await
            .unwrap();
        assert_eq!(manager.available_buffer_quanta(), before);
        drop(lease);
    }
}
