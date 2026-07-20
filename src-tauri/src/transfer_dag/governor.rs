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
use std::path::{Path, PathBuf};
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

/// Env override: concurrent I/O slots per physical local device and direction.
pub const DISK_DEVICE_SLOTS_ENV: &str = "AEROFTP_DISK_DEVICE_SLOTS";

/// Generous default endpoint concurrency sub-cap. Chosen well above the
/// per-job slot counts real jobs use, so single-job throughput is unaffected;
/// it only bites when many jobs pile onto one endpoint.
pub const DEFAULT_ENDPOINT_MAX_SLOTS: u32 = 256;

/// Default per-device, per-direction I/O concurrency. This matches the legacy
/// per-job disk-slot default while making concurrent jobs on the same device
/// share one ceiling.
pub const DEFAULT_DISK_DEVICE_SLOTS: u32 = 4;

/// Maximum consecutive foreground admissions while a background job waits.
/// The next available endpoint slot then goes to background work, preventing
/// starvation without weakening foreground preference under ordinary load.
const MAX_FOREGROUND_BYPASS: u32 = 8;

/// Scheduling class for a process-governed transfer job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferPriority {
    Foreground,
    Background,
}

/// Direction of local I/O for a device-governor lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DiskDirection {
    Read,
    Write,
}

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
    /// Max concurrent read or write jobs per local physical device.
    pub disk_device_slots: u32,
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
        let disk_device_slots = parse_env_u64(DISK_DEVICE_SLOTS_ENV)
            .and_then(|v| u32::try_from(v).ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_DISK_DEVICE_SLOTS);
        Self {
            buffer_bytes,
            bandwidth_bps,
            endpoint_slots,
            disk_device_slots,
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

/// Per-endpoint concurrency sub-cap. Jobs to the same identity share one
/// priority-aware operation budget.
pub struct EndpointGovernor {
    identity: EndpointIdentity,
    state: Mutex<EndpointState>,
    notify: tokio::sync::Notify,
    capacity: u32,
}

/// RAII handle for one endpoint operation slot.
pub struct EndpointOpLease {
    governor: Arc<EndpointGovernor>,
}

struct EndpointState {
    available: u32,
    foreground_waiters: u32,
    background_waiters: u32,
    foreground_bypass: u32,
}

/// A registered endpoint waiter. Drop unregisters it, so cancellation while
/// awaiting a priority turn cannot leave phantom waiters or starve a lane.
struct EndpointWaiter {
    governor: Arc<EndpointGovernor>,
    priority: TransferPriority,
    registered: bool,
}

impl EndpointWaiter {
    fn new(governor: Arc<EndpointGovernor>, priority: TransferPriority) -> Self {
        {
            let mut state = governor.state.lock().expect("endpoint state poisoned");
            match priority {
                TransferPriority::Foreground => state.foreground_waiters += 1,
                TransferPriority::Background => state.background_waiters += 1,
            }
        }
        Self {
            governor,
            priority,
            registered: true,
        }
    }

    fn admit(&mut self, state: &mut EndpointState) -> bool {
        if !priority_admits(state, self.priority) {
            return false;
        }

        state.available -= 1;
        match self.priority {
            TransferPriority::Foreground => {
                state.foreground_waiters -= 1;
                if state.background_waiters > 0 {
                    state.foreground_bypass += 1;
                } else {
                    state.foreground_bypass = 0;
                }
            }
            TransferPriority::Background => {
                state.background_waiters -= 1;
                state.foreground_bypass = 0;
            }
        }
        self.registered = false;
        true
    }
}

fn priority_admits(state: &EndpointState, priority: TransferPriority) -> bool {
    if state.available == 0 {
        return false;
    }
    let background_turn =
        state.background_waiters > 0 && state.foreground_bypass >= MAX_FOREGROUND_BYPASS;
    match priority {
        TransferPriority::Foreground => !background_turn,
        TransferPriority::Background => state.foreground_waiters == 0 || background_turn,
    }
}

impl Drop for EndpointWaiter {
    fn drop(&mut self) {
        if !self.registered {
            return;
        }
        let mut state = self.governor.state.lock().expect("endpoint state poisoned");
        match self.priority {
            TransferPriority::Foreground => {
                state.foreground_waiters = state.foreground_waiters.saturating_sub(1)
            }
            TransferPriority::Background => {
                state.background_waiters = state.background_waiters.saturating_sub(1)
            }
        }
        if state.background_waiters == 0 {
            state.foreground_bypass = 0;
        }
        drop(state);
        self.governor.notify.notify_waiters();
    }
}

impl Drop for EndpointOpLease {
    fn drop(&mut self) {
        self.governor.release_op();
    }
}

impl EndpointGovernor {
    fn new(identity: EndpointIdentity, capacity: u32) -> Self {
        let capacity = capacity.max(1);
        Self {
            identity,
            state: Mutex::new(EndpointState {
                available: capacity,
                foreground_waiters: 0,
                background_waiters: 0,
                foreground_bypass: 0,
            }),
            notify: tokio::sync::Notify::new(),
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
        self.state
            .lock()
            .expect("endpoint state poisoned")
            .available as usize
    }

    #[cfg(test)]
    fn waiter_counts(&self) -> (u32, u32) {
        let state = self.state.lock().expect("endpoint state poisoned");
        (state.foreground_waiters, state.background_waiters)
    }

    /// Acquire one endpoint operation slot as foreground work.
    pub async fn acquire_op(self: &Arc<Self>) -> EndpointOpLease {
        self.acquire_op_with_priority(TransferPriority::Foreground)
            .await
    }

    /// Acquire one endpoint operation slot. Foreground jobs are preferred;
    /// after a bounded number of bypasses a waiting background job gets the
    /// next slot, ensuring progress under sustained interactive traffic.
    pub async fn acquire_op_with_priority(
        self: &Arc<Self>,
        priority: TransferPriority,
    ) -> EndpointOpLease {
        let mut waiter = EndpointWaiter::new(Arc::clone(self), priority);
        loop {
            let notified = self.notify.notified();
            {
                let mut state = self.state.lock().expect("endpoint state poisoned");
                if waiter.admit(&mut state) {
                    return EndpointOpLease {
                        governor: Arc::clone(self),
                    };
                }
            }
            notified.await;
        }
    }

    fn release_op(&self) {
        let mut state = self.state.lock().expect("endpoint state poisoned");
        state.available = state.available.saturating_add(1).min(self.capacity);
        drop(state);
        self.notify.notify_waiters();
    }
}

/// Canonical local physical-device identity. Unix uses the kernel device id;
/// other platforms use the nearest existing path anchor, which is conservative
/// (it can merge devices but never assumes independent paths share no limit).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct DiskDeviceIdentity(String);

impl DiskDeviceIdentity {
    pub fn for_path(path: impl AsRef<Path>) -> Self {
        let anchor = nearest_existing_path(path.as_ref());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if let Ok(metadata) = std::fs::metadata(&anchor) {
                return Self(format!("unix-dev:{}", metadata.dev()));
            }
        }
        let label = std::fs::canonicalize(&anchor).unwrap_or(anchor);
        Self(format!("path:{}", label.to_string_lossy()))
    }
}

fn nearest_existing_path(path: &Path) -> PathBuf {
    let mut candidate = path.to_path_buf();
    loop {
        if candidate.exists() {
            return candidate;
        }
        match candidate.parent() {
            Some(parent) if parent != candidate => candidate = parent.to_path_buf(),
            _ => return path.to_path_buf(),
        }
    }
}

/// Per-device directional slots shared by every job touching that device.
pub struct DiskDeviceGovernor {
    identity: DiskDeviceIdentity,
    read_slots: Arc<Semaphore>,
    write_slots: Arc<Semaphore>,
    capacity: u32,
}

/// RAII handle for one local disk-direction slot.
pub struct DiskOpLease {
    _permit: OwnedSemaphorePermit,
}

impl DiskDeviceGovernor {
    fn new(identity: DiskDeviceIdentity, capacity: u32) -> Self {
        let capacity = capacity.max(1);
        Self {
            identity,
            read_slots: Arc::new(Semaphore::new(capacity as usize)),
            write_slots: Arc::new(Semaphore::new(capacity as usize)),
            capacity,
        }
    }

    pub fn identity(&self) -> &DiskDeviceIdentity {
        &self.identity
    }

    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    pub fn available(&self, direction: DiskDirection) -> usize {
        match direction {
            DiskDirection::Read => self.read_slots.available_permits(),
            DiskDirection::Write => self.write_slots.available_permits(),
        }
    }

    async fn acquire(&self, direction: DiskDirection) -> DiskOpLease {
        let semaphore = match direction {
            DiskDirection::Read => Arc::clone(&self.read_slots),
            DiskDirection::Write => Arc::clone(&self.write_slots),
        };
        let permit = semaphore
            .acquire_owned()
            .await
            .expect("disk governor semaphore never closed");
        DiskOpLease { _permit: permit }
    }

    #[cfg(test)]
    fn try_acquire(&self, direction: DiskDirection) -> Option<DiskOpLease> {
        let semaphore = match direction {
            DiskDirection::Read => Arc::clone(&self.read_slots),
            DiskDirection::Write => Arc::clone(&self.write_slots),
        };
        semaphore
            .try_acquire_owned()
            .ok()
            .map(|permit| DiskOpLease { _permit: permit })
    }
}

/// Input for a job-level disk lease. A job may touch one or two local devices;
/// the governor sorts and de-duplicates requests before awaiting permits, so
/// concurrent cross-device copies cannot deadlock each other.
#[derive(Clone, Debug)]
pub struct DiskLeaseRequest {
    pub path: PathBuf,
    pub direction: DiskDirection,
}

impl DiskLeaseRequest {
    pub fn read(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            direction: DiskDirection::Read,
        }
    }

    pub fn write(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            direction: DiskDirection::Write,
        }
    }
}

/// RAII aggregate for the process endpoint and local-device leases held by one
/// transfer job. Dropping it returns every child permit in reverse field order.
pub struct GovernorJobLease {
    _endpoint: EndpointOpLease,
    _disk: Vec<DiskOpLease>,
}

/// The process-global hierarchical governor.
pub struct GlobalTransferGovernor {
    config: GovernorConfig,
    memory: MemoryPool,
    bandwidth: Arc<BandwidthBucket>,
    endpoints: Mutex<HashMap<EndpointIdentity, Arc<EndpointGovernor>>>,
    disks: Mutex<HashMap<DiskDeviceIdentity, Arc<DiskDeviceGovernor>>>,
}

impl GlobalTransferGovernor {
    /// Build a standalone governor (also used directly by tests).
    pub fn new(config: GovernorConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            memory: MemoryPool::new(config.buffer_bytes),
            bandwidth: Arc::new(BandwidthBucket::new(config.bandwidth_bps)),
            endpoints: Mutex::new(HashMap::new()),
            disks: Mutex::new(HashMap::new()),
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

    /// Get (or lazily create) the local physical-device governor for `path`.
    pub fn disk_for_path(&self, path: impl AsRef<Path>) -> Arc<DiskDeviceGovernor> {
        self.disk(&DiskDeviceIdentity::for_path(path))
    }

    fn disk(&self, identity: &DiskDeviceIdentity) -> Arc<DiskDeviceGovernor> {
        let mut map = self.disks.lock().expect("disk registry poisoned");
        if let Some(existing) = map.get(identity) {
            return Arc::clone(existing);
        }
        let created = Arc::new(DiskDeviceGovernor::new(
            identity.clone(),
            self.config.disk_device_slots,
        ));
        map.insert(identity.clone(), Arc::clone(&created));
        created
    }

    /// Number of physical-device identities registered (test / diagnostics).
    pub fn disk_count(&self) -> usize {
        self.disks.lock().expect("disk registry poisoned").len()
    }

    /// Acquire the endpoint and local-device leases for one transfer job.
    ///
    /// The endpoint lease is priority-aware. Disk requests are resolved,
    /// sorted and de-duplicated before acquisition, giving same-device jobs one
    /// shared directional cap and avoiding lock-order cycles for copies that
    /// touch two devices.
    pub async fn acquire_job(
        &self,
        endpoint: EndpointIdentity,
        priority: TransferPriority,
        disk_requests: impl IntoIterator<Item = DiskLeaseRequest>,
    ) -> GovernorJobLease {
        let endpoint = self.endpoint(&endpoint);
        let endpoint_lease = endpoint.acquire_op_with_priority(priority).await;

        let mut requests: Vec<(DiskDeviceIdentity, DiskDirection)> = disk_requests
            .into_iter()
            .map(|request| {
                (
                    DiskDeviceIdentity::for_path(request.path),
                    request.direction,
                )
            })
            .collect();
        requests.sort_unstable();
        requests.dedup();

        let mut disk = Vec::with_capacity(requests.len());
        for (identity, direction) in requests {
            disk.push(self.disk(&identity).acquire(direction).await);
        }
        GovernorJobLease {
            _endpoint: endpoint_lease,
            _disk: disk,
        }
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

#[cfg(test)]
fn singleton_config() -> GovernorConfig {
    // Rust unit tests share one process and run their unrelated transfer
    // fixtures in parallel. Keep that harness singleton deliberately roomy so
    // a batch test cannot borrow another test's endpoint or device slot and
    // turn its own concurrency assertion flaky. Governor cap proofs construct
    // a fresh `GlobalTransferGovernor` with an explicit small configuration.
    let mut config = GovernorConfig::from_env();
    config.endpoint_slots = config.endpoint_slots.max(4096);
    config.disk_device_slots = config.disk_device_slots.max(4096);
    config
}

#[cfg(not(test))]
fn singleton_config() -> GovernorConfig {
    GovernorConfig::from_env()
}

/// The process-global governor, reachable from every context root (Tauri
/// `AppState`, the CLI/MCP context, the CLI TUI). Lazily initialised from the
/// environment on first use; [`init`] pins an explicit config at startup.
pub fn global() -> Arc<GlobalTransferGovernor> {
    Arc::clone(GOVERNOR.get_or_init(|| GlobalTransferGovernor::new(singleton_config())))
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
            disk_device_slots: DEFAULT_DISK_DEVICE_SLOTS,
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

    #[tokio::test]
    async fn cancelled_endpoint_waiter_unregisters_without_leaking_a_turn() {
        let gov = GlobalTransferGovernor::new(test_config(BUFFER_QUANTUM_BYTES, 0, 1));
        let endpoint = gov.endpoint(&EndpointIdentity::new("sftp", "cancel.test", "alice"));
        let held = endpoint.acquire_op().await;

        let waiting_endpoint = Arc::clone(&endpoint);
        let waiter = tokio::spawn(async move {
            waiting_endpoint
                .acquire_op_with_priority(TransferPriority::Background)
                .await
        });
        for _ in 0..16 {
            if endpoint.waiter_counts().1 == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(endpoint.waiter_counts(), (0, 1));

        waiter.abort();
        let _ = waiter.await;
        assert_eq!(endpoint.waiter_counts(), (0, 0));

        drop(held);
        let next = tokio::time::timeout(Duration::from_millis(100), endpoint.acquire_op()).await;
        assert!(
            next.is_ok(),
            "cancelled waiter left the endpoint unavailable"
        );
    }

    #[test]
    fn foreground_is_preferred_but_background_gets_a_bounded_turn() {
        let mut state = EndpointState {
            available: 1,
            foreground_waiters: 1,
            background_waiters: 1,
            foreground_bypass: 0,
        };
        assert!(priority_admits(&state, TransferPriority::Foreground));
        assert!(!priority_admits(&state, TransferPriority::Background));

        state.foreground_bypass = MAX_FOREGROUND_BYPASS;
        assert!(!priority_admits(&state, TransferPriority::Foreground));
        assert!(priority_admits(&state, TransferPriority::Background));
    }

    #[tokio::test]
    async fn same_disk_direction_is_shared_but_opposite_direction_is_independent() {
        let gov = GlobalTransferGovernor::new(test_config(
            BUFFER_QUANTUM_BYTES,
            0,
            DEFAULT_ENDPOINT_MAX_SLOTS,
        ));
        let disk = gov.disk_for_path(std::env::temp_dir());
        assert_eq!(
            disk.available(DiskDirection::Read),
            DEFAULT_DISK_DEVICE_SLOTS as usize
        );

        let mut held = Vec::new();
        for _ in 0..DEFAULT_DISK_DEVICE_SLOTS {
            held.push(disk.acquire(DiskDirection::Read).await);
        }
        assert_eq!(disk.available(DiskDirection::Read), 0);
        assert!(disk.try_acquire(DiskDirection::Read).is_none());
        assert!(disk.try_acquire(DiskDirection::Write).is_some());
        drop(held);
        assert_eq!(
            disk.available(DiskDirection::Read),
            DEFAULT_DISK_DEVICE_SLOTS as usize
        );
    }

    #[tokio::test]
    async fn job_leases_hold_one_shared_endpoint_and_device_cap() {
        let config = GovernorConfig {
            buffer_bytes: BUFFER_QUANTUM_BYTES,
            bandwidth_bps: 0,
            endpoint_slots: 1,
            disk_device_slots: 1,
        };
        let gov = GlobalTransferGovernor::new(config);
        let endpoint_a = EndpointIdentity::new("sftp", "lease.test", "alice");
        let endpoint_b = EndpointIdentity::new("sftp", "lease.test", "bob");
        let path = std::env::temp_dir().join("aeroftp-governor-lease-test");

        let held = gov
            .acquire_job(
                endpoint_a.clone(),
                TransferPriority::Foreground,
                [DiskLeaseRequest::write(path.clone())],
            )
            .await;

        // Same endpoint queues before it can acquire a duplicate device slot.
        let same_endpoint = gov.endpoint(&endpoint_a);
        let same_governor = Arc::clone(&gov);
        let same_path = path.clone();
        let same_waiter = tokio::spawn(async move {
            same_governor
                .acquire_job(
                    endpoint_a,
                    TransferPriority::Background,
                    [DiskLeaseRequest::write(same_path)],
                )
                .await
        });
        for _ in 0..16 {
            if same_endpoint.waiter_counts().1 == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(same_endpoint.waiter_counts(), (0, 1));

        // A different endpoint gets past its own cap but still waits on the
        // one physical-device write slot held by the first job.
        let other_governor = Arc::clone(&gov);
        let other_path = path.clone();
        let other_waiter = tokio::spawn(async move {
            other_governor
                .acquire_job(
                    endpoint_b,
                    TransferPriority::Foreground,
                    [DiskLeaseRequest::write(other_path)],
                )
                .await
        });
        for _ in 0..16 {
            if !other_waiter.is_finished() {
                tokio::task::yield_now().await;
            }
        }
        assert!(
            !other_waiter.is_finished(),
            "distinct endpoints bypassed disk cap"
        );
        other_waiter.abort();
        let _ = other_waiter.await;

        drop(held);
        let same_lease = tokio::time::timeout(Duration::from_millis(100), same_waiter)
            .await
            .expect("same endpoint waiter did not release after job lease drop")
            .expect("same endpoint task panicked");
        drop(same_lease);
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
