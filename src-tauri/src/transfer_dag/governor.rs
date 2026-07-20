// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Process-global hierarchical transfer governor (DAG-P2-01).
//!
//! # Why
//!
//! The per-job [`TransferResourceManager`](super::resources::TransferResourceManager)
//! (DAG-P0-06) bounds ONE job. Before this module, N concurrent jobs each built
//! their own manager and therefore each claimed the full byte-memory budget and
//! ran an independent bandwidth cap: K jobs could pin K times the intended
//! process memory and N times the intended wire rate. This module introduces a
//! single process-global governor so concurrent jobs respect ONE cap.
//!
//! # Hierarchy: process -> endpoint -> job
//!
//! * **process** owns the global byte-memory pool ([`MemoryPool`]) and the
//!   global bandwidth token bucket ([`BandwidthBucket`]).
//! * **endpoint** (keyed by a canonical [`EndpointIdentity`] of protocol, host,
//!   and account) owns a concurrency sub-cap so several jobs to one endpoint
//!   share an operation budget instead of hammering it.
//! * **job** is the existing [`TransferResourceManager`], now a CHILD built via
//!   [`GlobalTransferGovernor::child_manager`]: its slot classes stay per-job,
//!   but its buffer-byte pool is the shared process pool.
//!
//! # Single-job invariance
//!
//! A governor built with [`GovernorConfig::from_env`] sizes the memory pool with
//! the same P0-06 policy a standalone manager would resolve, leaves the
//! bandwidth bucket unlimited unless a global cap is configured, and sizes the
//! endpoint sub-cap generously. One job alone therefore behaves exactly as
//! before the governor: same buffer budget, same throughput, same oversize
//! semantics.
//!
//! # No new scheduler loop
//!
//! The memory pool and endpoint sub-cap are Tokio semaphores; the bandwidth
//! bucket refills lazily on acquire. There is no background task and no second
//! scheduler beyond the governor's own accounting (baton requirement).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

use super::resources::{
    buffer_budget_to_quanta, resolve_buffer_budget_bytes, TransferBudget, TransferResourceManager,
};

/// Env override: process-global bandwidth cap in **bytes/sec**. 0 / unset =
/// unlimited (single-job behaviour is unchanged).
pub const GLOBAL_BANDWIDTH_ENV: &str = "AEROFTP_GLOBAL_BANDWIDTH_BPS";

/// Env override: max concurrent transfer operation slots per endpoint identity.
/// 0 / unset = the generous default so a single job is never endpoint-bound.
pub const ENDPOINT_MAX_SLOTS_ENV: &str = "AEROFTP_ENDPOINT_MAX_SLOTS";

/// Generous default endpoint concurrency sub-cap. Chosen well above the
/// per-job slot counts real jobs use, so single-job throughput is unaffected;
/// it only bites when many jobs pile onto one endpoint.
pub const DEFAULT_ENDPOINT_MAX_SLOTS: u32 = 256;

/// Floor for the bandwidth bucket burst so a normal copy chunk (tens to
/// hundreds of KiB) never exceeds the burst and deadlocks (1 MiB).
const MIN_BANDWIDTH_BURST_BYTES: u64 = 1024 * 1024;

/// Process-global governor configuration.
#[derive(Debug, Clone, Copy)]
pub struct GovernorConfig {
    /// Byte-memory pool size in bytes (P0-06 policy). 0 disables buffer credits.
    pub buffer_bytes: u64,
    /// Global bandwidth cap in bytes/sec. 0 = unlimited.
    pub bandwidth_bps: u64,
    /// Max concurrent operation slots per endpoint identity.
    pub endpoint_slots: u32,
}

impl GovernorConfig {
    /// Resolve config from the environment / P0-06 policy.
    pub fn from_env() -> Self {
        let buffer_bytes = resolve_buffer_budget_bytes();
        let bandwidth_bps = parse_env_u64(GLOBAL_BANDWIDTH_ENV).unwrap_or(0);
        let endpoint_slots = parse_env_u64(ENDPOINT_MAX_SLOTS_ENV)
            .and_then(|v| u32::try_from(v).ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_ENDPOINT_MAX_SLOTS);
        Self {
            buffer_bytes,
            bandwidth_bps,
            endpoint_slots,
        }
    }
}

fn parse_env_u64(key: &str) -> Option<u64> {
    let raw = std::env::var(key).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed.parse::<u64>().ok()
}

/// Process-global byte-memory pool. Wraps the shared buffer-quanta semaphore and
/// the shared oversize lane handed to every child [`TransferResourceManager`].
pub struct MemoryPool {
    quanta: Arc<Semaphore>,
    oversize_lane: Arc<Semaphore>,
    capacity_quanta: u32,
    budget_bytes: u64,
}

impl MemoryPool {
    fn new(budget_bytes: u64) -> Self {
        let capacity_quanta = buffer_budget_to_quanta(budget_bytes);
        Self {
            quanta: Arc::new(Semaphore::new(capacity_quanta as usize)),
            oversize_lane: Arc::new(Semaphore::new(1)),
            capacity_quanta,
            budget_bytes,
        }
    }

    pub fn budget_bytes(&self) -> u64 {
        self.budget_bytes
    }

    pub fn capacity_quanta(&self) -> u32 {
        self.capacity_quanta
    }

    /// Buffer quanta available process-wide right now (test / diagnostics).
    pub fn available_quanta(&self) -> usize {
        self.quanta.available_permits()
    }

    /// Oversize-lane permits available process-wide (0 or 1).
    pub fn available_oversize_permits(&self) -> usize {
        self.oversize_lane.available_permits()
    }
}

/// Lazy-refill token bucket enforcing ONE process-global wire rate.
///
/// `rate_bps == 0` means unlimited: [`Self::acquire`] returns immediately, so a
/// process with no configured cap keeps today's throughput. Refill happens on
/// acquire (no background task). Deterministic tests drive the accounting with
/// an injected clock and assert grants stay within `rate * elapsed + burst`.
pub struct BandwidthBucket {
    rate_bps: u64,
    burst: u64,
    state: Mutex<BucketState>,
    notify: tokio::sync::Notify,
}

struct BucketState {
    tokens: f64,
    last_refill: Instant,
    granted_total: u64,
}

impl BandwidthBucket {
    fn new(rate_bps: u64) -> Self {
        let burst = if rate_bps == 0 {
            0
        } else {
            rate_bps.max(MIN_BANDWIDTH_BURST_BYTES)
        };
        Self {
            rate_bps,
            burst,
            state: Mutex::new(BucketState {
                tokens: burst as f64,
                last_refill: Instant::now(),
                granted_total: 0,
            }),
            notify: tokio::sync::Notify::new(),
        }
    }

    /// True when no global cap is configured (unlimited).
    pub fn is_unlimited(&self) -> bool {
        self.rate_bps == 0
    }

    pub fn rate_bps(&self) -> u64 {
        self.rate_bps
    }

    pub fn burst_bytes(&self) -> u64 {
        self.burst
    }

    /// Total bytes granted since construction (test / diagnostics).
    pub fn granted_bytes(&self) -> u64 {
        self.state
            .lock()
            .expect("bandwidth state poisoned")
            .granted_total
    }

    /// Wait until `bytes` may be sent under the global cap, then charge them.
    ///
    /// A request larger than the bucket burst is split into burst-sized grants.
    /// This avoids treating an arbitrarily large transfer as one burst while
    /// keeping the public API safe for callers that do not already stream in
    /// bounded chunks. Unlimited buckets and zero-byte requests are no-ops.
    pub async fn acquire(&self, bytes: u64) {
        if self.rate_bps == 0 || bytes == 0 {
            return;
        }
        let mut remaining = bytes;
        while remaining > 0 {
            let chunk = remaining.min(self.burst);
            self.acquire_one(chunk).await;
            remaining -= chunk;
        }
    }

    async fn acquire_one(&self, bytes: u64) {
        debug_assert!(bytes > 0 && bytes <= self.burst);
        let need = bytes as f64;
        loop {
            let wait = {
                let mut s = self.state.lock().expect("bandwidth state poisoned");
                self.refill(&mut s);
                if s.tokens >= need {
                    s.tokens -= need;
                    s.granted_total = s.granted_total.saturating_add(bytes);
                    // Wake one other waiter to re-check in case tokens remain.
                    self.notify.notify_one();
                    return;
                }
                let deficit = need - s.tokens;
                Duration::from_secs_f64(deficit / self.rate_bps as f64)
            };
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = self.notify.notified() => {}
            }
        }
    }

    fn refill(&self, s: &mut BucketState) {
        let now = Instant::now();
        let dt = now.saturating_duration_since(s.last_refill).as_secs_f64();
        if dt > 0.0 {
            let added = dt * self.rate_bps as f64;
            s.tokens = (s.tokens + added).min(self.burst as f64);
            s.last_refill = now;
        }
    }

    /// Test hook: the bucket's current refill anchor, so tests can drive a
    /// deterministic virtual clock with `anchor + Duration` instants.
    #[cfg(test)]
    fn refill_anchor(&self) -> Instant {
        self.state
            .lock()
            .expect("bandwidth state poisoned")
            .last_refill
    }

    /// Test hook: refill to `now` and consume `bytes` if available, returning
    /// whether the grant succeeded. Same accounting as [`Self::acquire`] but
    /// with an injected clock, so the cap can be proven without real sleeps or
    /// the `tokio` test clock.
    #[cfg(test)]
    fn try_consume_at(&self, bytes: u64, now: Instant) -> bool {
        if self.rate_bps == 0 || bytes == 0 {
            return true;
        }
        let mut s = self.state.lock().expect("bandwidth state poisoned");
        let dt = now.saturating_duration_since(s.last_refill).as_secs_f64();
        if dt > 0.0 {
            s.tokens = (s.tokens + dt * self.rate_bps as f64).min(self.burst as f64);
            s.last_refill = now;
        }
        if bytes > self.burst {
            return false;
        }
        let need = bytes as f64;
        if s.tokens >= need {
            s.tokens -= need;
            s.granted_total = s.granted_total.saturating_add(bytes);
            true
        } else {
            false
        }
    }
}

/// Canonical endpoint identity: protocol + host + account. Case-insensitive and
/// trimmed so `SFTP` / `sftp ` and mixed-case hosts collapse to one key.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct EndpointIdentity {
    pub protocol: String,
    pub host: String,
    pub account: String,
}

impl EndpointIdentity {
    pub fn new(protocol: impl AsRef<str>, host: impl AsRef<str>, account: impl AsRef<str>) -> Self {
        Self {
            protocol: protocol.as_ref().trim().to_ascii_lowercase(),
            host: host.as_ref().trim().to_ascii_lowercase(),
            account: account.as_ref().trim().to_ascii_lowercase(),
        }
    }
}

/// Per-endpoint concurrency sub-cap. Jobs to the same identity share `op_slots`.
pub struct EndpointGovernor {
    identity: EndpointIdentity,
    op_slots: Arc<Semaphore>,
    capacity: u32,
}

/// RAII handle for one endpoint operation slot.
pub struct EndpointOpLease {
    _permit: OwnedSemaphorePermit,
}

impl EndpointGovernor {
    fn new(identity: EndpointIdentity, capacity: u32) -> Self {
        let capacity = capacity.max(1);
        Self {
            identity,
            op_slots: Arc::new(Semaphore::new(capacity as usize)),
            capacity,
        }
    }

    pub fn identity(&self) -> &EndpointIdentity {
        &self.identity
    }

    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    /// Operation slots available at this endpoint right now.
    pub fn available_ops(&self) -> usize {
        self.op_slots.available_permits()
    }

    /// Acquire one endpoint operation slot, waiting under contention.
    pub async fn acquire_op(&self) -> EndpointOpLease {
        let permit = self
            .op_slots
            .clone()
            .acquire_owned()
            .await
            .expect("endpoint semaphore never closed");
        EndpointOpLease { _permit: permit }
    }
}

/// The process-global hierarchical governor.
pub struct GlobalTransferGovernor {
    config: GovernorConfig,
    memory: MemoryPool,
    bandwidth: Arc<BandwidthBucket>,
    endpoints: Mutex<HashMap<EndpointIdentity, Arc<EndpointGovernor>>>,
}

impl GlobalTransferGovernor {
    /// Build a standalone governor (also used directly by tests).
    pub fn new(config: GovernorConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            memory: MemoryPool::new(config.buffer_bytes),
            bandwidth: Arc::new(BandwidthBucket::new(config.bandwidth_bps)),
            endpoints: Mutex::new(HashMap::new()),
        })
    }

    /// Build a governor from the environment / P0-06 policy.
    pub fn from_env() -> Arc<Self> {
        Self::new(GovernorConfig::from_env())
    }

    pub fn config(&self) -> GovernorConfig {
        self.config
    }

    pub fn memory(&self) -> &MemoryPool {
        &self.memory
    }

    pub fn bandwidth(&self) -> Arc<BandwidthBucket> {
        Arc::clone(&self.bandwidth)
    }

    /// Get (or lazily create) the endpoint sub-governor for `identity`.
    pub fn endpoint(&self, identity: &EndpointIdentity) -> Arc<EndpointGovernor> {
        let mut map = self.endpoints.lock().expect("endpoint registry poisoned");
        if let Some(existing) = map.get(identity) {
            return Arc::clone(existing);
        }
        let created = Arc::new(EndpointGovernor::new(
            identity.clone(),
            self.config.endpoint_slots,
        ));
        map.insert(identity.clone(), Arc::clone(&created));
        created
    }

    /// Number of distinct endpoints registered (test / diagnostics).
    pub fn endpoint_count(&self) -> usize {
        self.endpoints
            .lock()
            .expect("endpoint registry poisoned")
            .len()
    }

    /// Build a child job manager that shares this governor's byte-memory pool.
    ///
    /// The per-job slot classes come from `budget`; the buffer-byte pool is the
    /// shared process pool, so K concurrent children cannot each claim the full
    /// budget and the one oversize lane is serialised across every job.
    pub fn child_manager(&self, budget: TransferBudget) -> TransferResourceManager {
        TransferResourceManager::with_shared_buffer_pool(
            budget,
            self.memory.quanta.clone(),
            self.memory.capacity_quanta,
            self.memory.oversize_lane.clone(),
            self.memory.budget_bytes,
        )
    }
}

// ---------------------------------------------------------------------------
// Process singleton
// ---------------------------------------------------------------------------

static GOVERNOR: OnceLock<Arc<GlobalTransferGovernor>> = OnceLock::new();

/// The process-global governor, reachable from every context root (Tauri
/// `AppState`, the CLI/MCP context, the CLI TUI). Lazily initialised from the
/// environment on first use; [`init`] pins an explicit config at startup.
pub fn global() -> Arc<GlobalTransferGovernor> {
    Arc::clone(GOVERNOR.get_or_init(GlobalTransferGovernor::from_env))
}

/// Initialise the process singleton with an explicit config. Idempotent: the
/// first caller wins, so context roots may all call it at startup without
/// constructing more than one governor.
pub fn init(config: GovernorConfig) -> Arc<GlobalTransferGovernor> {
    Arc::clone(GOVERNOR.get_or_init(|| GlobalTransferGovernor::new(config)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer_dag::resources::{ResourceRequest, BUFFER_QUANTUM_BYTES};
    use std::sync::Arc;
    use tokio::sync::Barrier;
    use tokio_util::sync::CancellationToken;

    fn test_config(buffer_bytes: u64, bandwidth_bps: u64, endpoint_slots: u32) -> GovernorConfig {
        GovernorConfig {
            buffer_bytes,
            bandwidth_bps,
            endpoint_slots,
        }
    }

    #[test]
    fn endpoint_identity_is_case_insensitive_and_trimmed() {
        let a = EndpointIdentity::new("SFTP", "Host.Example ", "  Alice");
        let b = EndpointIdentity::new("sftp", "host.example", "alice");
        assert_eq!(a, b);
    }

    #[test]
    fn single_job_child_pool_is_budget_identical_to_standalone() {
        // Test #4: a child manager under a default-policy governor exposes the
        // exact same buffer pool a standalone manager would resolve.
        let budget = TransferBudget::from_file_slots(4).with_resolved_buffer_budget();
        let standalone = TransferResourceManager::new(budget);

        let gov = GlobalTransferGovernor::new(test_config(
            budget.buffer_bytes,
            0,
            DEFAULT_ENDPOINT_MAX_SLOTS,
        ));
        let child = gov.child_manager(TransferBudget::from_file_slots(4));

        assert_eq!(
            child.available_buffer_quanta(),
            standalone.available_buffer_quanta()
        );
        assert_eq!(child.budget().file_slots, standalone.budget().file_slots);
        assert_eq!(
            child.budget().buffer_bytes,
            standalone.budget().buffer_bytes
        );
    }

    #[tokio::test]
    async fn concurrent_children_share_one_memory_pool() {
        // Test #2: K children draw from ONE pool; aggregate borrowed memory
        // never exceeds the global capacity; a single oversize allowance holds.
        let quantum = BUFFER_QUANTUM_BYTES;
        let gov =
            GlobalTransferGovernor::new(test_config(quantum * 4, 0, DEFAULT_ENDPOINT_MAX_SLOTS));
        assert_eq!(gov.memory().available_quanta(), 4);

        let job_a = Arc::new(gov.child_manager(TransferBudget::from_file_slots(1)));
        let job_b = Arc::new(gov.child_manager(TransferBudget::from_file_slots(1)));

        let lease_a = job_a
            .acquire(ResourceRequest {
                buffer_bytes: quantum * 3,
                ..ResourceRequest::default()
            })
            .await
            .unwrap();
        // Only 1 quantum left process-wide, even though job_b's slot budget is
        // independent: job_b cannot borrow 3 quanta while job_a holds them.
        assert_eq!(gov.memory().available_quanta(), 1);

        let job_b2 = Arc::clone(&job_b);
        let barrier = Arc::new(Barrier::new(2));
        let b2 = barrier.clone();
        let waiter = tokio::spawn(async move {
            b2.wait().await;
            job_b2
                .acquire(ResourceRequest {
                    buffer_bytes: quantum * 3,
                    ..ResourceRequest::default()
                })
                .await
        });
        barrier.wait().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished(), "aggregate exceeded the global pool");

        drop(lease_a);
        let lease_b = waiter.await.unwrap().unwrap();
        assert_eq!(gov.memory().available_quanta(), 1);
        drop(lease_b);
        assert_eq!(gov.memory().available_quanta(), 4);
    }

    #[tokio::test]
    async fn oversize_is_one_at_a_time_across_jobs() {
        // Test #2 (oversize): the single oversize allowance is serialised across
        // DIFFERENT jobs, not merely within one job.
        let quantum = BUFFER_QUANTUM_BYTES;
        let gov =
            GlobalTransferGovernor::new(test_config(quantum * 2, 0, DEFAULT_ENDPOINT_MAX_SLOTS));
        let job_a = Arc::new(gov.child_manager(TransferBudget::from_file_slots(1)));
        let job_b = Arc::new(gov.child_manager(TransferBudget::from_file_slots(1)));

        let first = job_a
            .acquire(ResourceRequest {
                buffer_bytes: quantum * 10,
                ..ResourceRequest::default()
            })
            .await
            .unwrap();
        assert_eq!(gov.memory().available_oversize_permits(), 0);

        let job_b2 = Arc::clone(&job_b);
        let second = tokio::spawn(async move {
            job_b2
                .acquire(ResourceRequest {
                    buffer_bytes: quantum * 10,
                    ..ResourceRequest::default()
                })
                .await
        });
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(!second.is_finished(), "two oversize parts held together");

        drop(first);
        let lease = second.await.unwrap().unwrap();
        drop(lease);
        assert_eq!(gov.memory().available_oversize_permits(), 1);
        assert_eq!(gov.memory().available_quanta(), 2);
    }

    #[tokio::test]
    async fn same_endpoint_jobs_share_subcap_distinct_do_not() {
        // Test #3: two jobs to one endpoint identity share the sub-cap; two jobs
        // to different endpoints acquire independently.
        let gov = GlobalTransferGovernor::new(test_config(BUFFER_QUANTUM_BYTES, 0, 1));
        let ep = EndpointIdentity::new("sftp", "host.a", "alice");
        let ep_other = EndpointIdentity::new("sftp", "host.b", "alice");

        let g1 = gov.endpoint(&ep);
        let g2 = gov.endpoint(&ep);
        // Same identity resolves to the same shared sub-governor.
        assert!(Arc::ptr_eq(&g1, &g2));
        assert_eq!(gov.endpoint_count(), 1);

        let held = g1.acquire_op().await;
        assert_eq!(g1.available_ops(), 0);

        let g2b = Arc::clone(&g2);
        let waiter = tokio::spawn(async move { g2b.acquire_op().await });
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(!waiter.is_finished(), "same-endpoint jobs did not contend");

        // A different endpoint is not blocked by the first.
        let other = gov.endpoint(&ep_other);
        let other_lease =
            tokio::time::timeout(Duration::from_millis(100), other.acquire_op()).await;
        assert!(other_lease.is_ok(), "distinct endpoints must not contend");

        drop(held);
        let unblocked = waiter.await.unwrap();
        drop(unblocked);
        assert_eq!(g1.available_ops(), 1);
    }

    #[test]
    fn bandwidth_token_accounting_bounds_aggregate() {
        // Test #1: the token pool every transfer draws from grants at most
        // `burst + rate * elapsed` bytes over any window, regardless of how many
        // concurrent callers drain it. Proven by accounting on an injected clock
        // (deterministic, not sleep-inferred).
        let rate = 10 * 1024 * 1024; // 10 MiB/s
        let bucket = BandwidthBucket::new(rate);
        let burst = bucket.burst_bytes();
        let t0 = bucket.refill_anchor();

        // The whole burst is grantable at t0, and nothing more.
        assert!(bucket.try_consume_at(burst, t0));
        assert_eq!(bucket.granted_bytes(), burst);
        assert!(
            !bucket.try_consume_at(1, t0),
            "burst exhausted: no further grant without elapsed time"
        );

        // After 100 ms exactly `rate / 10` bytes have refilled. Many small
        // grants may share it, but their sum cannot exceed that budget.
        let t1 = t0 + Duration::from_millis(100);
        let refill_budget = rate / 10;
        let chunk = 256 * 1024u64;
        let mut granted_after = 0u64;
        for _ in 0..1000 {
            if bucket.try_consume_at(chunk, t1) {
                granted_after += chunk;
            } else {
                break;
            }
        }
        assert!(
            granted_after <= refill_budget,
            "granted {granted_after} over one window exceeded refill budget {refill_budget}"
        );
        assert_eq!(bucket.granted_bytes(), burst + granted_after);
        // The window was genuinely productive (not a vacuous zero-grant pass).
        assert!(granted_after >= refill_budget - chunk);
    }

    #[test]
    fn bandwidth_accounting_never_treats_a_large_request_as_one_burst() {
        let rate = 4 * 1024 * 1024;
        let bucket = BandwidthBucket::new(rate);
        let t0 = bucket.refill_anchor();

        assert!(
            !bucket.try_consume_at(bucket.burst_bytes() + 1, t0),
            "a request larger than the available burst must not be granted at one instant"
        );
        assert_eq!(bucket.granted_bytes(), 0);
    }

    #[tokio::test]
    async fn bandwidth_second_waiter_blocks_until_refill() {
        // Concurrency shape of Test #1: once the burst is drained, a second
        // caller on the SAME bucket must wait for tokens to refill. Rate is set
        // so one chunk needs ~1s, and a 100 ms timeout deterministically fails.
        let chunk = 256 * 1024u64;
        let rate = chunk; // one chunk per second
        let bucket = Arc::new(BandwidthBucket::new(rate));
        let burst = bucket.burst_bytes();

        bucket.acquire(burst).await; // drains the pool instantly
        let started = bucket.granted_bytes();

        let b2 = Arc::clone(&bucket);
        let waiter = tokio::spawn(async move { b2.acquire(chunk).await });
        let blocked = tokio::time::timeout(Duration::from_millis(100), async {
            // Poll: the waiter should not finish within the window.
            loop {
                if bucket.granted_bytes() > started {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(blocked.is_err(), "second waiter was granted before refill");
        waiter.abort();
    }

    #[tokio::test]
    async fn unlimited_bandwidth_is_a_noop() {
        // Single-job invariance: no configured cap means no throttle at all.
        let bucket = BandwidthBucket::new(0);
        assert!(bucket.is_unlimited());
        bucket.acquire(1024 * 1024 * 1024).await; // returns immediately
        assert_eq!(bucket.granted_bytes(), 0);
    }

    #[tokio::test]
    async fn cancellation_propagates_through_child_manager() {
        // Test #7: cancelling an acquire on a governor child leaks no permits and
        // surfaces the caller's typed cancel, exactly as the standalone path.
        let quantum = BUFFER_QUANTUM_BYTES;
        let gov =
            GlobalTransferGovernor::new(test_config(quantum * 2, 0, DEFAULT_ENDPOINT_MAX_SLOTS));
        let job = Arc::new(gov.child_manager(TransferBudget::from_file_slots(1)));

        let held = job
            .acquire(ResourceRequest {
                buffer_bytes: quantum * 2,
                ..ResourceRequest::default()
            })
            .await
            .unwrap();
        assert_eq!(gov.memory().available_quanta(), 0);

        let cancel = CancellationToken::new();
        let job2 = Arc::clone(&job);
        let cancel2 = cancel.clone();
        let waiter = tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = cancel2.cancelled() => Err("cancelled".to_string()),
                r = job2.acquire(ResourceRequest {
                    buffer_bytes: quantum,
                    ..ResourceRequest::default()
                }) => r,
            }
        });

        tokio::time::sleep(Duration::from_millis(30)).await;
        cancel.cancel();
        assert!(waiter.await.unwrap().is_err());

        // No partial lease retained: the full pool returns when the holder drops.
        drop(held);
        assert_eq!(gov.memory().available_quanta(), 2);
        assert_eq!(gov.memory().available_oversize_permits(), 1);
    }
}
