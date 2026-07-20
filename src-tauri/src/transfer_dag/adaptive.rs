// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Prudent AIMD backpressure for the ready-frontier executor.
//!
//! Four pieces:
//!
//! 1. [`congestion_from_error`] maps a typed [`crate::transfer_dag::error::TransferError`]
//!    onto the narrow congestion trigger set (429, 503, request timeout,
//!    server-side max-connections, connection reset) by matching
//!    [`crate::transfer_dag::error::TransferErrorKind`] only — no message
//!    substring parsing in the controller. Generic 5xx is deliberately
//!    excluded. String classification happens once at the provider→DAG
//!    adapter ([`crate::transfer_dag::error::TransferError::from_provider`]).
//!
//! 2. [`DynamicSemaphore`] is a permit gate whose ceiling can be lowered and
//!    raised at runtime. Shrinking is *lazy*: it withholds future permits and
//!    absorbs permits as in-flight holders release them, never aborting work
//!    already running, and the live permit count never exceeds the immutable
//!    `hard_ceiling`.
//!
//! 3. [`AimdController`] applies additive-increase / multiplicative-decrease
//!    per controlled resource class. It is decrease-biased and cannot grant
//!    concurrency above the honest `effective_budget` ceiling: it can only
//!    ever reduce below it and grow back toward it. The live dispatch permits
//!    are always per-run.
//!
//! 4. DAG-P2-06: [`AdaptiveProfileRegistry`] is a bounded, process-local cache
//!    of the *safe target* a controller converged to, keyed by
//!    [`AdaptiveProfileKey`] (P2-01b [`EndpointIdentity`] + [`AdaptiveWorkload`]).
//!    It is a seed/feedback record only, never a shared semaphore and never a
//!    second dispatch loop. When [`AimdController::from_budget_for_profile`]
//!    builds a controller it seeds each class from the learned safe target for
//!    that key (clamped so first use is byte-for-byte the honest ceiling), and
//!    the typed congestion/healthy lifecycle mirrors moves back to the same key
//!    only. Entries are held for the process lifetime and up to a finite TTL,
//!    with a finite key cap and deterministic LRU eviction; an expired entry
//!    behaves like a first use. It holds no ownership over the P2-02 checkpoint
//!    state and never retains a secret, credential, path, or object name.
//!
//! The controller throttles the executor's *dispatch* step. A node parked
//! waiting for a dispatch permit has not begun its transfer yet, so shrinking
//! is always safe.

use std::cmp::Ordering as CmpOrdering;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::sleep;

use super::aimd_hints;
use super::error::{TransferError, TransferErrorKind};
use super::governor::EndpointIdentity;
use super::resources::{ResourceRequest, TransferBudget};
use crate::providers::ProviderType;

/// Resource classes the controller manages in v1 (decision D1). Checker,
/// disk, and hash slots stay static and are intentionally absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdaptiveClass {
    File,
    Chunk,
    Http,
    Api,
}

impl AdaptiveClass {
    /// Fixed acquisition order, matching the resource manager's ordered
    /// acquisition principle so multi-class nodes cannot deadlock.
    pub const ORDER: [AdaptiveClass; 4] = [
        AdaptiveClass::File,
        AdaptiveClass::Chunk,
        AdaptiveClass::Http,
        AdaptiveClass::Api,
    ];
}

/// DAG-P2-06: the workload shape half of an [`AdaptiveProfileKey`]. A congestion
/// signal learned while running one workload against an endpoint must not seed a
/// structurally different workload against the same endpoint: a segmented range
/// download tuning its chunk/http fan-out says nothing about the safe whole-file
/// concurrency for a batch of small files. We therefore key learned tuning by
/// the *actual* production graph shapes, not by a display label.
///
/// The three variants map one-to-one onto the four active AIMD construction
/// sites: batch and sync share [`BatchSyncFile`](Self::BatchSyncFile) because
/// both drive whole-file transfers dominated by the File class; the shaped
/// single-file / native multipart path is [`ShapedFile`](Self::ShapedFile); the
/// concurrent range engine is [`SegmentedRange`](Self::SegmentedRange).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdaptiveWorkload {
    /// Whole-file batch and tree-sync transfers (File-class dominated).
    BatchSyncFile,
    /// Shaped single-file uploads/downloads and native multipart fan-out.
    ShapedFile,
    /// Concurrent segmented range download (Chunk/Http fan-out).
    SegmentedRange,
}

/// DAG-P2-06: canonical key for a learned adaptive tuning profile. It pairs the
/// P2-01b [`EndpointIdentity`] (protocol + host + account, already the
/// process-global governor's key) with the explicit [`AdaptiveWorkload`] shape.
///
/// Privacy boundary: the key carries only the same authority identity the
/// endpoint governor already holds. It never contains a password, token, remote
/// path, or remote object name. A different account on the same host, a
/// different host, or a different workload is a distinct key by construction, so
/// tuning learned for one can never bleed into another.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AdaptiveProfileKey {
    pub endpoint: EndpointIdentity,
    pub workload: AdaptiveWorkload,
}

impl AdaptiveProfileKey {
    pub fn new(endpoint: EndpointIdentity, workload: AdaptiveWorkload) -> Self {
        Self { endpoint, workload }
    }
}

/// DAG-P2-07 (block F): one completed job's realized telemetry, distilled into
/// the three signals the slow optimization loop reads. Built by
/// [`JobThroughputObservation::from_metrics`] from the populated job-total
/// [`crate::transfer_dag::TransferDagMetrics`] plus the real wall clock, so
/// every field traces to a measured value; none is fabricated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobThroughputObservation {
    /// Concurrency high-water the executor actually reached (`slot_peak`),
    /// floored at 1.
    pub concurrency: usize,
    /// Payload bytes moved (`bytes_transferred`), guaranteed non-zero.
    pub bytes: u64,
    /// Real elapsed wall-clock nanoseconds for the whole job, non-zero.
    pub wall_clock_nanos: u64,
    /// Summed dispatch-to-runner-start wait nanos over dispatched nodes.
    pub wait_nanos: u64,
    /// Summed runner execution nanos over dispatched nodes.
    pub run_nanos: u64,
}

impl JobThroughputObservation {
    /// Distill a job's populated metrics and wall clock into an observation, or
    /// `None` when the job carries no usable signal (moved zero bytes, e.g. a
    /// skip-only run, or reported no elapsed time). Returning `None` here is the
    /// degenerate-job half of the no-poison guard; the caller supplies the
    /// not-cancelled half.
    pub fn from_metrics(
        metrics: &super::metrics::TransferDagMetrics,
        wall_clock: Duration,
    ) -> Option<Self> {
        let wall_clock_nanos = duration_to_nanos(wall_clock);
        if metrics.bytes_transferred == 0 || wall_clock_nanos == 0 {
            return None;
        }
        Some(Self {
            concurrency: (metrics.slot_peak.max(1)) as usize,
            bytes: metrics.bytes_transferred,
            wall_clock_nanos,
            wait_nanos: metrics.wait_nanos_total,
            run_nanos: metrics.run_nanos_total,
        })
    }
}

/// DAG-P2-07 (block F): what the slow optimization loop did with one
/// observation. Returned for diagnostics and deterministic tests; the live
/// effect is a write of the learned safe target into the shared registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlowLoopDecision {
    /// First observation for this key/class: adopted as the throughput
    /// baseline. The learned target is not moved (no comparison exists yet).
    Seeded,
    /// A proven-faster job (throughput up more than [`SLOW_LOOP_IMPROVE_PCT`],
    /// dispatch wait below [`SLOW_LOOP_WAIT_PLATEAU_PCT`]) raised the learned
    /// safe target one step toward the productive concurrency.
    Raised(usize),
    /// A plateau (more concurrency without more throughput) or a wait-dominated
    /// dispatch lowered the learned safe target one step (floor 1).
    Lowered(usize),
    /// Steady state: the target was left unchanged.
    Held,
    /// No learning happened: the controller has no profile, AIMD is disabled,
    /// or the observation carried no usable signal.
    NoSignal,
}

/// A job must beat the learned throughput baseline by more than this percent
/// (with low dispatch wait) before the slow loop treats its concurrency as
/// proven-faster. Set at the ">5% live convergence" evidence bar deferred out
/// of P2-06.
const SLOW_LOOP_IMPROVE_PCT: u64 = 5;
/// When dispatched-node time is at least this percent spent waiting for
/// permits/leases rather than running, the job over-subscribed its own
/// pipeline: the slow loop reads that as a plateau and shaves one slot.
const SLOW_LOOP_WAIT_PLATEAU_PCT: u64 = 50;

/// Saturating `Duration` to nanoseconds (a >584-year job is a bug, not a reason
/// to wrap telemetry).
fn duration_to_nanos(d: Duration) -> u64 {
    d.as_nanos().min(u64::MAX as u128) as u64
}

/// The narrow set of congestion signals (decision D2). No generic 5xx.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CongestionEvent {
    /// HTTP 429 / "too many requests" / explicit rate limit.
    TooManyRequests,
    /// HTTP 503 / "service unavailable".
    ServiceUnavailable,
    /// Request/operation timed out.
    Timeout,
    /// Server refused for too many connections (e.g. FTP 421).
    MaxConnections,
    /// Connection reset / dropped mid-flight.
    ConnectionReset,
}

/// Map a typed [`TransferError`] to a [`CongestionEvent`], or `None` if it is
/// not a congestion signal.
///
/// **Controller contract (DAG-P0-03):** this function matches only on
/// [`TransferErrorKind`]. It never inspects `message` text. Providers still
/// embed `Retry-After` markers in presentation strings for the adapter; the
/// adapter lifts them into [`TransferError::retry_after`] before the
/// executor calls this mapper.
///
/// A "504 Gateway Timeout" is classified as [`TransferErrorKind::Timeout`] at
/// the adapter and maps here to [`CongestionEvent::Timeout`]. Plain server
/// errors with no load/timeout/availability semantics (500, 502 →
/// [`TransferErrorKind::Unknown`] / [`TransferErrorKind::RemoteIo`]) return
/// `None`: only the D2 set throttles, never generic 5xx.
pub fn congestion_from_error(err: &TransferError) -> Option<CongestionEvent> {
    match err.kind {
        TransferErrorKind::RateLimited => Some(CongestionEvent::TooManyRequests),
        TransferErrorKind::ServiceUnavailable => Some(CongestionEvent::ServiceUnavailable),
        TransferErrorKind::Timeout => Some(CongestionEvent::Timeout),
        TransferErrorKind::MaxConnections => Some(CongestionEvent::MaxConnections),
        TransferErrorKind::ConnectionReset => Some(CongestionEvent::ConnectionReset),
        _ => None,
    }
}

/// Marker prefix for embedding a `Retry-After` hint inside a `ProviderError`
/// message string. Providers that parse `Retry-After` append the marker when
/// they emit the error; [`crate::transfer_dag::error::TransferError::from_provider`]
/// extracts it into the typed [`TransferError::retry_after`] field so the
/// executor never re-parses the presentation string.
///
/// Format: ` [retry-after-secs=NN]` appended to the error text, where `NN`
/// is a non-negative integer of seconds. Sub-second hints round up at the
/// provider site to avoid defeating the AIMD throttle, matching
/// [`AimdController::MIN_HINT_COOLDOWN`]. T-DEBT-05.
const RETRY_HINT_MARKER: &str = " [retry-after-secs=";

/// Build the marker substring to append to a `ProviderError` message after a
/// rate-limit response. The typed adapter ([`TransferError::from_provider`])
/// recovers the value into [`TransferError::retry_after`].
pub fn embed_retry_after_marker(seconds: u64) -> String {
    format!("{RETRY_HINT_MARKER}{seconds}]")
}

/// Extract a `Retry-After` hint that a provider embedded in the error
/// message via [`embed_retry_after_marker`]. Used by the provider→DAG adapter
/// only; the AIMD controller reads [`TransferError::retry_after`].
/// Returns `None` when the marker is absent, malformed, or carries a value
/// that does not parse as a u64.
pub fn parse_embedded_retry_after(message: &str) -> Option<Duration> {
    let start = message.find(RETRY_HINT_MARKER)?;
    let rest = &message[start + RETRY_HINT_MARKER.len()..];
    let end = rest.find(']')?;
    rest[..end].parse::<u64>().ok().map(Duration::from_secs)
}

/// RAII permit from a [`DynamicSemaphore`]. On drop the underlying permit is
/// returned, then if the semaphore still owes a shrink the freed slot is
/// absorbed instead of being handed to the next waiter.
pub struct DynamicPermit {
    sem: Arc<Semaphore>,
    deficit: Arc<AtomicUsize>,
    permit: Option<OwnedSemaphorePermit>,
}

impl Drop for DynamicPermit {
    fn drop(&mut self) {
        // Return the permit first (its own Drop calls add_permits(1)).
        drop(self.permit.take());
        // If a shrink is still owed, reclaim one freed slot now. Use a CAS so a
        // concurrent grow-path pay-down cannot drive the deficit below zero,
        // which on AtomicUsize wraps to usize::MAX and would permanently pin
        // concurrency at the floor for the rest of the run (audit AIMD-02).
        if self.deficit.load(Ordering::SeqCst) > 0 {
            if let Ok(p) = self.sem.try_acquire() {
                p.forget();
                let consumed = self
                    .deficit
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |cur| {
                        (cur > 0).then(|| cur - 1)
                    })
                    .is_ok();
                if !consumed {
                    // The deficit was cleared concurrently: hand the slot back
                    // instead of dropping it, so live concurrency stays intact.
                    self.sem.add_permits(1);
                }
            }
        }
    }
}

/// A semaphore whose effective size moves between `1` and `hard_ceiling`.
/// `add_permits` grows it (capped, deficit paid down first); a stable
/// `try_acquire + forget` loop shrinks it without touching in-flight permits,
/// and any slot that could not be reclaimed immediately is recorded as a
/// deficit and absorbed on the next release. Uses only stable tokio APIs.
#[derive(Clone)]
pub struct DynamicSemaphore {
    sem: Arc<Semaphore>,
    hard_ceiling: usize,
    live: Arc<Mutex<usize>>,
    deficit: Arc<AtomicUsize>,
}

impl DynamicSemaphore {
    pub fn new(hard_ceiling: usize, initial: usize) -> Self {
        let hard_ceiling = hard_ceiling.max(1);
        let initial = initial.clamp(1, hard_ceiling);
        Self {
            sem: Arc::new(Semaphore::new(initial)),
            hard_ceiling,
            live: Arc::new(Mutex::new(initial)),
            deficit: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn hard_ceiling(&self) -> usize {
        self.hard_ceiling
    }

    /// Logical permit count currently provisioned (the AIMD `live`).
    pub fn live(&self) -> usize {
        *self.live.lock().expect("dynamic semaphore mutex poisoned")
    }

    /// Permits the semaphore still owes a shrink for (could not reclaim yet
    /// because they were checked out).
    pub fn pending_shrink(&self) -> usize {
        self.deficit.load(Ordering::SeqCst)
    }

    /// Move the live size toward `target`, clamped to `[1, hard_ceiling]`.
    /// Growing never exceeds the ceiling; shrinking is lazy.
    pub fn set_live(&self, target: usize) {
        let target = target.clamp(1, self.hard_ceiling);
        let mut live = self.live.lock().expect("dynamic semaphore mutex poisoned");
        if target > *live {
            let mut need = target - *live;
            // Pay down any owed shrink before adding real permits. A CAS loop so
            // a concurrent `DynamicPermit::drop` reclaiming a slot between our
            // read and write cannot underflow the deficit (audit AIMD-02).
            let mut paid = 0usize;
            let _ = self
                .deficit
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |cur| {
                    let pay = need.min(cur);
                    paid = pay;
                    Some(cur - pay)
                });
            need -= paid;
            if need > 0 {
                self.sem.add_permits(need);
            }
            *live = target;
        } else if target < *live {
            let want = *live - target;
            let mut removed = 0;
            while removed < want {
                match self.sem.try_acquire() {
                    Ok(p) => {
                        p.forget();
                        removed += 1;
                    }
                    Err(_) => break,
                }
            }
            if removed < want {
                self.deficit.fetch_add(want - removed, Ordering::SeqCst);
            }
            *live = target;
        }
    }

    /// Acquire one permit. Resolves only while the live size has room.
    pub async fn acquire(&self) -> Option<DynamicPermit> {
        let permit = self.sem.clone().acquire_owned().await.ok()?;
        Some(DynamicPermit {
            sem: self.sem.clone(),
            deficit: self.deficit.clone(),
            permit: Some(permit),
        })
    }

    #[cfg(test)]
    pub fn available(&self) -> usize {
        self.sem.available_permits()
    }
}

/// KE-D2: per-class override for the AIMD window sizing parameters.
///
/// - `min` overrides the floor applied to `target` after multiplicative
///   decrease. The default floor is `1` (a single permit per class).
/// - `max` caps the controller's effective ceiling for the class: it is
///   clamped against the budget-derived ceiling, never raises it. A value
///   below `1` is sanitized to `1` at build time.
/// - `step` overrides the additive increase amount applied once per
///   `healthy_window`. The default is `+1`.
///
/// All three fields are independent: an operator can lift the floor
/// without touching the step, or vice versa. Every field defaults to
/// `None`, meaning "use the historical built-in (1 / ceiling / 1)".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AimdClassWindow {
    pub min: Option<usize>,
    pub max: Option<usize>,
    pub step: Option<usize>,
}

/// KE-D2: bundle of per-class window overrides. Each [`AimdClassWindow`]
/// addresses one controlled resource class; classes are kept independent
/// so a power user can throttle the API class while leaving the File
/// class at its native ceiling. Defaults to "no overrides anywhere".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AimdClassOverrides {
    pub file: AimdClassWindow,
    pub chunk: AimdClassWindow,
    pub http: AimdClassWindow,
    pub api: AimdClassWindow,
}

impl AimdClassOverrides {
    /// Return the override bundle for a controlled class.
    pub fn for_class(&self, class: AdaptiveClass) -> AimdClassWindow {
        match class {
            AdaptiveClass::File => self.file,
            AdaptiveClass::Chunk => self.chunk,
            AdaptiveClass::Http => self.http,
            AdaptiveClass::Api => self.api,
        }
    }

    /// Apply a global override to every class. CLI uses this to lift the
    /// "broadcast to all four" sugar (`--aimd-min-window N`); the more
    /// explicit per-class TOML file fills individual entries instead.
    pub fn broadcast(window: AimdClassWindow) -> Self {
        Self {
            file: window,
            chunk: window,
            http: window,
            api: window,
        }
    }

    /// Merge `other` on top of `self`, with `other` winning on every
    /// field that is `Some`. Used to layer CLI flags (which broadcast
    /// to all classes) on top of a TOML file (which may target a
    /// single class), so `--aimd-min-window=2` from the command line
    /// reliably overrides whatever the file said.
    pub fn merge_on_top(self, other: AimdClassOverrides) -> Self {
        Self {
            file: merge_window(self.file, other.file),
            chunk: merge_window(self.chunk, other.chunk),
            http: merge_window(self.http, other.http),
            api: merge_window(self.api, other.api),
        }
    }
}

fn merge_window(base: AimdClassWindow, top: AimdClassWindow) -> AimdClassWindow {
    AimdClassWindow {
        min: top.min.or(base.min),
        max: top.max.or(base.max),
        step: top.step.or(base.step),
    }
}

/// Tuning for the AIMD controller. Defaults are prudent; tests inject short
/// or zero windows. `cooldown` blocks any increase after a decrease;
/// `healthy_window` is the minimum quiet interval before each `+1`.
///
/// `recovery_window` is the guard band against oscillation (decision F3-T09):
/// after a congestion event the controller will not let additive increase
/// climb straight back to the concurrency level that triggered congestion. It
/// caps regrowth one slot below that level until a full `recovery_window` of
/// uninterrupted quiet has elapsed, at which point the cap relaxes back to the
/// honest ceiling. A zero `recovery_window` disables the guard band (the cap
/// relaxes on the first healthy note), which is the pre-F3-T09 behaviour and
/// is what unit tests of the bare additive mechanism opt into.
///
/// `disabled` is the KE-D1 kill switch. When `true`, `on_congestion`,
/// `on_congestion_with_hint` and `note_healthy` are full no-ops: the
/// controller is built at the honest ceiling for every class and never
/// shrinks or regrows. Designed for dedicated/lab links where the operator
/// has out-of-band guarantees that the backend will not throttle and the
/// AIMD machinery would only mask a true throughput cap diagnostic.
///
/// `class_overrides` (KE-D2) lets the operator pin MIN / MAX / STEP per
/// controlled class. All-`None` entries fall back to the legacy
/// `(1, ceiling, +1)` triple, so an unconfigured controller is bit-identical
/// to its pre-KE-D2 self.
#[derive(Debug, Clone, Copy)]
pub struct AimdConfig {
    pub cooldown: Duration,
    pub healthy_window: Duration,
    pub recovery_window: Duration,
    pub disabled: bool,
    pub class_overrides: AimdClassOverrides,
}

impl Default for AimdConfig {
    fn default() -> Self {
        Self {
            cooldown: Duration::from_secs(5),
            healthy_window: Duration::from_secs(5),
            // Long enough that a provider's rate-limit window has demonstrably
            // cleared before AIMD risks the level that congested it again.
            recovery_window: Duration::from_secs(30),
            disabled: false,
            class_overrides: AimdClassOverrides::default(),
        }
    }
}

static RUNTIME_CONFIG: OnceLock<AimdConfig> = OnceLock::new();

impl AimdConfig {
    /// Install the process-wide AIMD config snapshot built from CLI flags
    /// (`--aimd-disable`, etc.). Subsequent calls are ignored so every
    /// downstream transfer surface observes a stable view for the entire
    /// process lifetime, matching [`aimd_hints::init`]. Idempotent.
    pub fn install_runtime(cfg: AimdConfig) {
        let _ = RUNTIME_CONFIG.set(cfg);
    }

    /// Read the installed runtime AIMD config, or fall back to
    /// [`AimdConfig::default`] when nothing has been installed (the GUI
    /// path that never calls [`install_runtime`]). Production transfer
    /// surfaces should call this instead of [`Default::default`] so CLI
    /// runtime knobs propagate without an explicit plumbing argument.
    pub fn runtime() -> AimdConfig {
        RUNTIME_CONFIG.get().copied().unwrap_or_default()
    }
}

/// DAG-P2-06: injectable monotonic clock for the [`AdaptiveProfileRegistry`] TTL
/// and eviction recency. Production uses [`SystemClock`]; deterministic tests
/// use a manual clock so TTL expiry can be proven without wall-clock sleeps.
///
/// This governs *only* the registry's own bookkeeping. The per-run
/// [`AimdController`] cooldown/healthy timers keep using real [`Instant`]s
/// exactly as before; the registry stores learned target *values* (plain
/// integers) and timestamps them with this clock.
pub trait AdaptiveClock: Send + Sync {
    fn now(&self) -> Instant;
}

/// Wall-clock implementation of [`AdaptiveClock`].
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl AdaptiveClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Manually advanced [`AdaptiveClock`] for deterministic TTL/eviction tests.
/// `advance` moves the observed instant forward without any real sleeping.
pub struct ManualClock {
    base: Instant,
    offset: Mutex<Duration>,
}

impl ManualClock {
    pub fn new() -> Self {
        Self {
            base: Instant::now(),
            offset: Mutex::new(Duration::ZERO),
        }
    }

    pub fn advance(&self, by: Duration) {
        let mut offset = self.offset.lock().expect("manual clock poisoned");
        *offset = offset.saturating_add(by);
    }
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::new()
    }
}

impl AdaptiveClock for ManualClock {
    fn now(&self) -> Instant {
        self.base + *self.offset.lock().expect("manual clock poisoned")
    }
}

/// DAG-P2-06: bounds for the process-local [`AdaptiveProfileRegistry`]. Both
/// bounds are finite by contract so the cache can never grow without limit nor
/// pin a stale throttle forever.
#[derive(Debug, Clone, Copy)]
pub struct AdaptiveProfileConfig {
    /// A learned entry older than this (since its last update) is treated as a
    /// first use again: the next controller for that key starts at its honest
    /// ceiling. The default is generous enough to carry a safe target across
    /// back-to-back jobs in one process, yet short enough that a provider whose
    /// rate limit has long since cleared is not throttled indefinitely (the
    /// controller would in any case recover conservatively toward the ceiling).
    pub ttl: Duration,
    /// Hard cap on distinct keys retained. On overflow the least-recently
    /// touched key is evicted deterministically. Bounds memory regardless of
    /// how many endpoints/workloads a long-running process touches.
    pub capacity: usize,
}

impl Default for AdaptiveProfileConfig {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(600),
            capacity: 256,
        }
    }
}

/// One learned entry: an optional safe target per controlled class, its last
/// update instant (for TTL) and a monotonic touch sequence (for deterministic
/// LRU eviction).
struct ProfileEntry {
    file: Option<usize>,
    chunk: Option<usize>,
    http: Option<usize>,
    api: Option<usize>,
    updated_at: Instant,
    touch_seq: u64,
    /// DAG-P2-07 (block F): best aggregate throughput observed for this key so
    /// far, in bytes per second. The slow loop compares each new job against
    /// this baseline. Cleared on TTL reset (via `new`), so a stale link speed
    /// never pins the comparison forever.
    best_throughput_bps: Option<u64>,
    /// The concurrency at which `best_throughput_bps` was observed, so the loop
    /// can tell "more width, same speed" (plateau) from "more width, more
    /// speed" (productive).
    best_concurrency: Option<usize>,
}

impl ProfileEntry {
    fn new(now: Instant, seq: u64) -> Self {
        Self {
            file: None,
            chunk: None,
            http: None,
            api: None,
            updated_at: now,
            touch_seq: seq,
            best_throughput_bps: None,
            best_concurrency: None,
        }
    }

    fn set(&mut self, class: AdaptiveClass, target: usize) {
        match class {
            AdaptiveClass::File => self.file = Some(target),
            AdaptiveClass::Chunk => self.chunk = Some(target),
            AdaptiveClass::Http => self.http = Some(target),
            AdaptiveClass::Api => self.api = Some(target),
        }
    }

    fn value(&self, class: AdaptiveClass) -> Option<usize> {
        match class {
            AdaptiveClass::File => self.file,
            AdaptiveClass::Chunk => self.chunk,
            AdaptiveClass::Http => self.http,
            AdaptiveClass::Api => self.api,
        }
    }
}

struct RegistryInner {
    entries: HashMap<AdaptiveProfileKey, ProfileEntry>,
    seq: u64,
}

impl RegistryInner {
    fn next_seq(&mut self) -> u64 {
        self.seq = self.seq.wrapping_add(1);
        self.seq
    }

    /// Remove the least-recently-touched entry. Deterministic: `touch_seq` is
    /// strictly monotonic, so there is exactly one minimum.
    fn evict_lru(&mut self) {
        if let Some(key) = self
            .entries
            .iter()
            .min_by_key(|(_, entry)| entry.touch_seq)
            .map(|(key, _)| key.clone())
        {
            self.entries.remove(&key);
        }
    }
}

/// A narrow, read-only view of one live profile entry for diagnostics and
/// future UI. Carries the same identity the endpoint governor already exposes;
/// it holds no credential, path, or object name.
#[derive(Debug, Clone)]
pub struct AdaptiveProfileSnapshot {
    pub key: AdaptiveProfileKey,
    pub file: Option<usize>,
    pub chunk: Option<usize>,
    pub http: Option<usize>,
    pub api: Option<usize>,
    /// Time since this entry was last updated, per the registry clock.
    pub age: Duration,
}

/// DAG-P2-06: the process-local, bounded, in-process cache of safe adaptive
/// targets keyed by [`AdaptiveProfileKey`]. It is a *seed/feedback record only*,
/// never a shared semaphore and never a second dispatch loop: the live dispatch
/// permits remain per-run inside each [`AimdController`], and the process-wide
/// resource cap remains the P2-01b endpoint/disk leases in the governor.
///
/// Boundary (documented for point 7 of the task): entries live only for the
/// process lifetime and only up to [`AdaptiveProfileConfig::ttl`]; at most
/// [`AdaptiveProfileConfig::capacity`] keys are retained (LRU eviction beyond
/// that). It is not durable and holds no ownership over the P2-02 checkpoint
/// state.
pub struct AdaptiveProfileRegistry {
    inner: Mutex<RegistryInner>,
    ttl: Duration,
    capacity: usize,
    clock: Arc<dyn AdaptiveClock>,
}

impl AdaptiveProfileRegistry {
    /// Build a registry on the wall clock.
    pub fn new(config: AdaptiveProfileConfig) -> Self {
        Self::with_clock(config, Arc::new(SystemClock))
    }

    /// Build a registry on an injected clock (deterministic tests).
    pub fn with_clock(config: AdaptiveProfileConfig, clock: Arc<dyn AdaptiveClock>) -> Self {
        Self {
            inner: Mutex::new(RegistryInner {
                entries: HashMap::new(),
                seq: 0,
            }),
            ttl: config.ttl,
            capacity: config.capacity.max(1),
            clock,
        }
    }

    /// Cached safe target for `key`/`class`, or `None` when the key is unknown,
    /// expired (treated as first use, and purged), or has no learned value for
    /// that class yet. A live read refreshes the entry's recency so an actively
    /// seeded key is not evicted out from under a fresh controller.
    pub fn seed(&self, key: &AdaptiveProfileKey, class: AdaptiveClass) -> Option<usize> {
        let now = self.clock.now();
        let mut inner = self
            .inner
            .lock()
            .expect("adaptive profile registry poisoned");
        let expired = {
            let entry = inner.entries.get(key)?;
            now.saturating_duration_since(entry.updated_at) >= self.ttl
        };
        if expired {
            inner.entries.remove(key);
            return None;
        }
        let seq = inner.next_seq();
        let entry = inner
            .entries
            .get_mut(key)
            .expect("entry present after non-expired check");
        entry.touch_seq = seq;
        entry.value(class)
    }

    /// Record a learned safe target for `key`/`class`. Called from the typed
    /// controller lifecycle after a congestion decrease or a healthy-recovery
    /// increase actually moved the class target. An expired entry is reset
    /// before the write so a stale sibling class cannot resurrect.
    pub fn record(&self, key: &AdaptiveProfileKey, class: AdaptiveClass, target: usize) {
        let now = self.clock.now();
        let mut inner = self
            .inner
            .lock()
            .expect("adaptive profile registry poisoned");
        let seq = inner.next_seq();
        if let Some(entry) = inner.entries.get_mut(key) {
            if now.saturating_duration_since(entry.updated_at) >= self.ttl {
                *entry = ProfileEntry::new(now, seq);
            }
            entry.set(class, target);
            entry.updated_at = now;
            entry.touch_seq = seq;
            return;
        }
        if inner.entries.len() >= self.capacity {
            inner.evict_lru();
        }
        let mut entry = ProfileEntry::new(now, seq);
        entry.set(class, target);
        inner.entries.insert(key.clone(), entry);
    }

    /// DAG-P2-07 (block F): the slow optimization loop. Fold one completed
    /// job's realized throughput/wait/plateau signal into the learned safe
    /// target for `key`/`class`.
    ///
    /// It shares the same key isolation, TTL reset, capacity/eviction and
    /// clock as [`Self::record`], so learned tuning stays bounded and per
    /// endpoint/workload exactly as in P2-06. It is decrease-biased: it lowers
    /// the target one step on a plateau or a wait-dominated dispatch, and
    /// raises it at most one step (toward the concurrency that actually ran)
    /// only when a job beat the throughput baseline by more than
    /// [`SLOW_LOOP_IMPROVE_PCT`] with low wait. The first observation only
    /// seeds the baseline; it never moves the target. Callers must not invoke
    /// this on a cancelled job (that is the caller's half of the no-poison
    /// guard); [`AimdController::observe_job`] additionally drops the call when
    /// AIMD is disabled.
    pub fn observe_job(
        &self,
        key: &AdaptiveProfileKey,
        class: AdaptiveClass,
        obs: JobThroughputObservation,
    ) -> SlowLoopDecision {
        let now = self.clock.now();
        let throughput_bps = (u128::from(obs.bytes) * 1_000_000_000
            / u128::from(obs.wall_clock_nanos.max(1)))
        .min(u128::from(u64::MAX)) as u64;
        let total_dispatch = obs.wait_nanos.saturating_add(obs.run_nanos);
        let wait_pct = if total_dispatch > 0 {
            (u128::from(obs.wait_nanos) * 100 / u128::from(total_dispatch)) as u64
        } else {
            0
        };
        let concurrency = obs.concurrency.max(1);

        let mut inner = self
            .inner
            .lock()
            .expect("adaptive profile registry poisoned");
        let seq = inner.next_seq();

        // Fetch-or-create honoring the same TTL reset / capacity rules as
        // `record`: an expired entry behaves like a first use.
        if let Some(entry) = inner.entries.get(key) {
            if now.saturating_duration_since(entry.updated_at) >= self.ttl {
                inner
                    .entries
                    .insert(key.clone(), ProfileEntry::new(now, seq));
            }
        } else {
            if inner.entries.len() >= self.capacity {
                inner.evict_lru();
            }
            inner
                .entries
                .insert(key.clone(), ProfileEntry::new(now, seq));
        }
        let entry = inner
            .entries
            .get_mut(key)
            .expect("entry present after fetch-or-create");
        entry.updated_at = now;
        entry.touch_seq = seq;

        match (entry.best_throughput_bps, entry.best_concurrency) {
            (Some(best_tp), Some(best_conc)) => {
                let improved = u128::from(throughput_bps) * 100
                    > u128::from(best_tp) * u128::from(100 + SLOW_LOOP_IMPROVE_PCT);
                let starved = wait_pct >= SLOW_LOOP_WAIT_PLATEAU_PCT;
                if improved && !starved {
                    // Proven-faster at low wait: step the learned target one
                    // toward the concurrency that achieved it (never above it:
                    // bounded recovery, additive like AIMD). On the first raise
                    // there is no learned target yet, so start from the prior
                    // productive concurrency (`best_conc`) and add one, rather
                    // than jumping straight to the new width.
                    let base = entry.value(class).unwrap_or(best_conc);
                    let raised = base.saturating_add(1).min(concurrency).max(1);
                    entry.set(class, raised);
                    entry.best_throughput_bps = Some(throughput_bps);
                    entry.best_concurrency = Some(concurrency);
                    SlowLoopDecision::Raised(raised)
                } else if starved || (concurrency > best_conc && throughput_bps <= best_tp) {
                    // Plateau or starvation: the extra width did not pay off, or
                    // the dispatch spent most of its life waiting for permits.
                    // Shave one slot; keep the throughput baseline (never raise
                    // it on a regression).
                    let base = entry.value(class).unwrap_or(concurrency);
                    let lowered = base.saturating_sub(1).max(1);
                    entry.set(class, lowered);
                    SlowLoopDecision::Lowered(lowered)
                } else {
                    // Marginal, equal, or slower-but-not-starved: hold the
                    // target. Let the baseline creep up on any real gain so it
                    // tracks the honest ceiling without moving concurrency.
                    if throughput_bps > best_tp {
                        entry.best_throughput_bps = Some(throughput_bps);
                        entry.best_concurrency = Some(concurrency);
                    }
                    SlowLoopDecision::Held
                }
            }
            // First real observation (or post-reset): adopt the baseline only.
            _ => {
                entry.best_throughput_bps = Some(throughput_bps);
                entry.best_concurrency = Some(concurrency);
                SlowLoopDecision::Seeded
            }
        }
    }

    /// Narrow diagnostic snapshot of every live (non-expired) entry.
    pub fn snapshot(&self) -> Vec<AdaptiveProfileSnapshot> {
        let now = self.clock.now();
        let inner = self
            .inner
            .lock()
            .expect("adaptive profile registry poisoned");
        inner
            .entries
            .iter()
            .filter(|(_, entry)| now.saturating_duration_since(entry.updated_at) < self.ttl)
            .map(|(key, entry)| AdaptiveProfileSnapshot {
                key: key.clone(),
                file: entry.file,
                chunk: entry.chunk,
                http: entry.http,
                api: entry.api,
                age: now.saturating_duration_since(entry.updated_at),
            })
            .collect()
    }

    /// Number of live (non-expired) entries currently retained.
    pub fn len(&self) -> usize {
        let now = self.clock.now();
        let inner = self
            .inner
            .lock()
            .expect("adaptive profile registry poisoned");
        inner
            .entries
            .values()
            .filter(|entry| now.saturating_duration_since(entry.updated_at) < self.ttl)
            .count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

static PROFILE_REGISTRY: OnceLock<Arc<AdaptiveProfileRegistry>> = OnceLock::new();

/// The process-global adaptive profile registry, lazily built on first use with
/// [`AdaptiveProfileConfig::default`] on the wall clock. Every production AIMD
/// construction site shares this one instance so a safe target learned by one
/// job seeds the next job to the same endpoint/workload within the process.
pub fn global_profile_registry() -> Arc<AdaptiveProfileRegistry> {
    PROFILE_REGISTRY
        .get_or_init(|| {
            Arc::new(AdaptiveProfileRegistry::new(
                AdaptiveProfileConfig::default(),
            ))
        })
        .clone()
}

struct ClassState {
    sem: DynamicSemaphore,
    ceiling: usize,
    target: usize,
    cooldown_until: Option<Instant>,
    healthy_since: Option<Instant>,
    /// Guard band (F3-T09): the highest target additive increase may reach.
    /// Starts at `ceiling`; a congestion event drops it one below the level
    /// that congested; a full `recovery_window` of quiet relaxes it back.
    regrowth_cap: usize,
    /// Instant of the most recent congestion event, for the recovery timer.
    last_congestion: Option<Instant>,
    /// KE-D2 floor for `target` after multiplicative decrease. Always
    /// `>= 1` and `<= ceiling`. Pre-KE-D2 behaviour is the default `1`.
    min_target: usize,
    /// KE-D2 additive increase amount applied once per quiet
    /// `healthy_window`. Always `>= 1`. Pre-KE-D2 behaviour is `1`.
    step: usize,
}

#[derive(Debug, Default)]
struct ApiPacerState {
    window_started_at: Option<Instant>,
    grants_in_window: usize,
}

impl ApiPacerState {
    fn reserve_delay(&mut self, now: Instant, burst: usize, min_sleep: Duration) -> Duration {
        let burst = burst.max(1);
        match self.window_started_at {
            None => {
                self.window_started_at = Some(now);
                self.grants_in_window = 1;
                Duration::ZERO
            }
            Some(start) => {
                let window_end = start + min_sleep;
                match now.cmp(&window_end) {
                    CmpOrdering::Greater | CmpOrdering::Equal => {
                        self.window_started_at = Some(now);
                        self.grants_in_window = 1;
                        Duration::ZERO
                    }
                    CmpOrdering::Less if self.grants_in_window < burst => {
                        self.grants_in_window += 1;
                        Duration::ZERO
                    }
                    CmpOrdering::Less => {
                        self.window_started_at = Some(window_end);
                        self.grants_in_window = 1;
                        window_end.duration_since(now)
                    }
                }
            }
        }
    }
}

impl ClassState {
    /// KE-D2 + DAG-P2-06: build a class with operator-supplied window overrides,
    /// starting from a cached safe target instead of the honest ceiling. `seed`
    /// is the learned safe target for this class from a prior job to the same
    /// endpoint/workload. It is clamped into `[min_target, ceiling]`, so a seed
    /// can only ever *lower* the starting point: AIMD stays decrease-biased and
    /// can never begin above the effective budget or an operator cap. `seed ==
    /// None` (every first use, and every disabled controller) is byte-for-byte
    /// the historical behaviour: start at the honest ceiling.
    ///
    /// Clamping rules (applied here so overrides can never yield an unsafe
    /// controller state):
    /// - `max` only ever shrinks the budget-derived `ceiling`; it cannot raise
    ///   it above the honest cap (AIMD is decrease-biased).
    /// - `min` is clamped to `[1, ceiling]`; values below `1` collapse to `1`,
    ///   values above the clamped ceiling collapse to the ceiling.
    /// - `step` is clamped to `[1, usize::MAX]`; a `step == 0` request is
    ///   coerced to `1` so additive increase still makes forward progress.
    fn with_overrides_seeded(
        ceiling: usize,
        overrides: AimdClassWindow,
        seed: Option<usize>,
    ) -> Self {
        let budget_ceiling = ceiling.max(1);
        let ceiling = match overrides.max {
            Some(cap) => budget_ceiling.min(cap.max(1)),
            None => budget_ceiling,
        };
        let min_target = overrides.min.unwrap_or(1).clamp(1, ceiling);
        let step = overrides.step.unwrap_or(1).max(1);
        // A cached safe target seeds the *starting* point only; it is clamped
        // so it can never exceed the honest ceiling or drop below the floor.
        let start = match seed {
            Some(cached) => cached.clamp(min_target, ceiling),
            None => ceiling,
        };
        Self {
            // Start at the seeded target (== the honest ceiling on first use):
            // AIMD is decrease-biased and only ever reduces below the effective
            // budget, never above it.
            sem: DynamicSemaphore::new(ceiling, start),
            ceiling,
            target: start,
            cooldown_until: None,
            healthy_since: None,
            // The guard band still starts at the honest ceiling: a seeded-low
            // start recovers conservatively (+step per quiet window) back up to
            // the ceiling once the endpoint proves healthy again.
            regrowth_cap: ceiling,
            last_congestion: None,
            min_target,
            step,
        }
    }
}

/// Decrease-biased AIMD over the four controlled classes. Never grants more
/// than the per-class effective budget; halves on congestion with a cooldown;
/// grows back by one after a quiet window.
pub struct AimdController {
    config: AimdConfig,
    current_provider: Option<ProviderType>,
    file: Mutex<ClassState>,
    chunk: Mutex<ClassState>,
    http: Mutex<ClassState>,
    api: Mutex<ClassState>,
    api_pacer: Mutex<ApiPacerState>,
    /// DAG-P2-06: shared safe-target cache this controller seeds from and
    /// mirrors feedback into. `None` for every per-run constructor: those
    /// controllers neither read nor write learned tuning (byte-for-byte the
    /// pre-P2-06 behaviour).
    registry: Option<Arc<AdaptiveProfileRegistry>>,
    /// DAG-P2-06: the endpoint/workload key this controller learns against.
    /// Feedback is mirrored to this key only; another endpoint or workload is a
    /// different key and is never touched.
    profile_key: Option<AdaptiveProfileKey>,
}

impl AimdController {
    /// Build a controller from the per-class effective budget (already the
    /// honest cap = provider caps clamped by the user `--parallel`).
    pub fn new(
        file_ceiling: usize,
        chunk_ceiling: usize,
        http_ceiling: usize,
        api_ceiling: usize,
        config: AimdConfig,
    ) -> Self {
        Self::new_for_provider(
            file_ceiling,
            chunk_ceiling,
            http_ceiling,
            api_ceiling,
            None,
            config,
        )
    }

    /// Build a controller bound to a specific provider so per-provider AIMD
    /// overlays can tune cooldown and API pacing. KE-D2 per-class window
    /// overrides from `config.class_overrides` are applied here so callers
    /// stay oblivious of the override plumbing.
    pub fn new_for_provider(
        file_ceiling: usize,
        chunk_ceiling: usize,
        http_ceiling: usize,
        api_ceiling: usize,
        current_provider: Option<ProviderType>,
        config: AimdConfig,
    ) -> Self {
        Self::build(
            file_ceiling,
            chunk_ceiling,
            http_ceiling,
            api_ceiling,
            current_provider,
            config,
            None,
        )
    }

    /// Shared construction core (DAG-P2-06). `profile`, when `Some`, seeds each
    /// class from the registry's learned safe target for that key (unless AIMD
    /// is disabled, which stays pinned at the honest ceiling) and attaches the
    /// key + registry so the typed lifecycle can mirror feedback back.
    fn build(
        file_ceiling: usize,
        chunk_ceiling: usize,
        http_ceiling: usize,
        api_ceiling: usize,
        current_provider: Option<ProviderType>,
        config: AimdConfig,
        profile: Option<(AdaptiveProfileKey, Arc<AdaptiveProfileRegistry>)>,
    ) -> Self {
        let overrides = config.class_overrides;
        // KE-D1: a disabled controller never consults the learned cache. It
        // must remain pinned at the honest ceiling for the whole run so the
        // "is throughput capped by AIMD?" diagnostic stays truthful. Resolve
        // every class seed up front so no borrow of `profile` outlives the
        // point where it is moved into the controller below.
        let (file_seed, chunk_seed, http_seed, api_seed) = match (&profile, config.disabled) {
            (Some((key, registry)), false) => (
                registry.seed(key, AdaptiveClass::File),
                registry.seed(key, AdaptiveClass::Chunk),
                registry.seed(key, AdaptiveClass::Http),
                registry.seed(key, AdaptiveClass::Api),
            ),
            _ => (None, None, None, None),
        };
        let file = Mutex::new(ClassState::with_overrides_seeded(
            file_ceiling,
            overrides.for_class(AdaptiveClass::File),
            file_seed,
        ));
        let chunk = Mutex::new(ClassState::with_overrides_seeded(
            chunk_ceiling,
            overrides.for_class(AdaptiveClass::Chunk),
            chunk_seed,
        ));
        let http = Mutex::new(ClassState::with_overrides_seeded(
            http_ceiling,
            overrides.for_class(AdaptiveClass::Http),
            http_seed,
        ));
        let api = Mutex::new(ClassState::with_overrides_seeded(
            api_ceiling,
            overrides.for_class(AdaptiveClass::Api),
            api_seed,
        ));
        let (profile_key, registry) = match profile {
            Some((key, registry)) => (Some(key), Some(registry)),
            None => (None, None),
        };
        Self {
            config,
            current_provider,
            file,
            chunk,
            http,
            api,
            api_pacer: Mutex::new(ApiPacerState::default()),
            registry,
            profile_key,
        }
    }

    /// Build a controller whose per-class ceilings are the honest effective
    /// budget (F3-T05). Every class starts at its ceiling — AIMD is
    /// decrease-biased — so a run with no congestion dispatches exactly as if
    /// no controller were wired.
    pub fn from_budget(budget: &TransferBudget, config: AimdConfig) -> Self {
        Self::from_budget_for_provider(budget, None, config)
    }

    /// Build a controller from the effective budget plus an optional provider
    /// type so per-provider overlays can tune cooldown and pacing.
    pub fn from_budget_for_provider(
        budget: &TransferBudget,
        current_provider: Option<ProviderType>,
        config: AimdConfig,
    ) -> Self {
        Self::new_for_provider(
            budget.file_slots.max(1) as usize,
            budget.chunk_slots.max(1) as usize,
            budget.http_slots.max(1) as usize,
            budget.api_slots.max(1) as usize,
            current_provider,
            config,
        )
    }

    /// DAG-P2-06: build a controller bound to an endpoint/workload profile and
    /// the shared [`AdaptiveProfileRegistry`]. Each class starts from the
    /// intersection of the honest effective ceiling, the operator max (both
    /// already folded into the budget-derived ceiling and `class_overrides`),
    /// and the learned safe target for this exact key. On first use (an empty
    /// or expired cache entry) the seed is `None` and every class starts at its
    /// honest ceiling, byte-for-byte the current behaviour. Congestion and
    /// healthy-recovery feedback are mirrored back to this key only.
    pub fn from_budget_for_profile(
        budget: &TransferBudget,
        current_provider: Option<ProviderType>,
        profile_key: AdaptiveProfileKey,
        registry: Arc<AdaptiveProfileRegistry>,
        config: AimdConfig,
    ) -> Self {
        Self::build(
            budget.file_slots.max(1) as usize,
            budget.chunk_slots.max(1) as usize,
            budget.http_slots.max(1) as usize,
            budget.api_slots.max(1) as usize,
            current_provider,
            config,
            Some((profile_key, registry)),
        )
    }

    /// The endpoint/workload profile this controller learns against, if any.
    /// Exposed for diagnostics and wire-level tests; `None` for per-run
    /// controllers built without a profile.
    pub fn profile_key(&self) -> Option<&AdaptiveProfileKey> {
        self.profile_key.as_ref()
    }

    /// Mirror a class's new safe target back to the shared registry for this
    /// controller's profile. A no-op for a controller built without a profile,
    /// and never reached while AIMD is disabled (both feedback entry points
    /// short-circuit on `config.disabled` before any target moves).
    fn record_learned(&self, class: AdaptiveClass, target: usize) {
        if let (Some(registry), Some(key)) = (&self.registry, &self.profile_key) {
            registry.record(key, class, target);
        }
    }

    /// DAG-P2-07 (block F): feed a completed job's realized throughput/wait
    /// telemetry into the slow optimization loop for this controller's profile.
    ///
    /// A no-op returning [`SlowLoopDecision::NoSignal`] for a controller without
    /// a profile (test-injected) and, like the per-event congestion/healthy
    /// feedback, a full no-op while AIMD is disabled so `--aimd-disable`
    /// records nothing. Callers must not invoke this on a cancelled job.
    pub fn observe_job(
        &self,
        class: AdaptiveClass,
        obs: JobThroughputObservation,
    ) -> SlowLoopDecision {
        if self.config.disabled {
            return SlowLoopDecision::NoSignal;
        }
        match (&self.registry, &self.profile_key) {
            (Some(registry), Some(key)) => registry.observe_job(key, class, obs),
            _ => SlowLoopDecision::NoSignal,
        }
    }

    fn provider_hint(&self) -> Option<aimd_hints::AimdHint> {
        self.current_provider.map(aimd_hints::for_provider)
    }

    fn effective_cooldown_hint(&self, cooldown_hint: Option<Duration>) -> Option<Duration> {
        cooldown_hint.or_else(|| {
            self.provider_hint()
                .and_then(|hint| hint.retry_after_secs)
                .map(Duration::from_secs)
        })
    }

    fn api_pacing_delay_at(&self, now: Instant) -> Option<Duration> {
        let hint = self.provider_hint()?;
        let min_sleep = hint.min_sleep?;
        let burst = hint.burst.unwrap_or(1);
        let mut pacer = self
            .api_pacer
            .lock()
            .expect("aimd api pacer mutex poisoned");
        Some(pacer.reserve_delay(now, burst, min_sleep))
    }

    fn state(&self, class: AdaptiveClass) -> &Mutex<ClassState> {
        match class {
            AdaptiveClass::File => &self.file,
            AdaptiveClass::Chunk => &self.chunk,
            AdaptiveClass::Http => &self.http,
            AdaptiveClass::Api => &self.api,
        }
    }

    /// Multiplicative decrease: halve the target (floor 1) and arm a cooldown
    /// that blocks any increase until it elapses.
    ///
    /// The guard band (F3-T09) also records the concurrency level that
    /// congested and drops `regrowth_cap` one slot below it, so additive
    /// increase cannot immediately climb back into the same congestion.
    ///
    /// Equivalent to `on_congestion_with_hint(class, None)`. Callers that
    /// have parsed a server-side `Retry-After` header (or equivalent
    /// cooldown hint from the provider's rate-limit response body) should
    /// use [`Self::on_congestion_with_hint`] so AIMD honors the hint
    /// instead of pacing against its own fixed default.
    pub fn on_congestion(&self, class: AdaptiveClass) {
        self.on_congestion_with_hint(class, None);
    }

    /// Lower bound for a server-provided cooldown hint. A `Retry-After: 0`
    /// or sub-second value from a misconfigured provider must not collapse
    /// the cooldown into immediate retry: there's no point in armoring the
    /// AIMD decrease if the cooldown window is effectively zero. T-DEBT-05.
    const MIN_HINT_COOLDOWN: Duration = Duration::from_secs(1);
    /// Upper bound multiplier vs. the configured default cooldown. A
    /// pathological `Retry-After: 3600` from a 1-second-default provider
    /// would otherwise stall the entire transfer for an hour on a single
    /// transient rate limit. Cap at 10× the default and let the operator
    /// see the throttling in `RUST_LOG=transfer_dag::adaptive=debug`.
    const MAX_HINT_MULTIPLIER: u32 = 10;

    /// Multiplicative decrease with an explicit cooldown hint. Identical
    /// to [`Self::on_congestion`] except the arming cooldown is the hint
    /// (clamped to a safe band) instead of `AimdConfig::cooldown`.
    ///
    /// Hint semantics (T-DEBT-05):
    /// - `None`: use `self.config.cooldown` verbatim (legacy behavior).
    /// - `Some(d)`: use `d` clamped to `[MIN_HINT_COOLDOWN, MAX_HINT_MULTIPLIER × config.cooldown]`.
    ///
    /// The clamp protects against both a misconfigured provider returning
    /// near-zero `Retry-After` (which would defeat the throttle) and a
    /// pathological provider returning an hours-long hint (which would
    /// stall the whole transfer). Operators see clamped values in debug
    /// logs; the underlying call site (Google Drive / Dropbox / OneDrive
    /// / Box parse helpers) should still log the raw hint when it diverges.
    pub fn on_congestion_with_hint(&self, class: AdaptiveClass, cooldown_hint: Option<Duration>) {
        // KE-D1: when the operator has explicitly disabled AIMD the
        // controller must not halve or arm a cooldown. The window stays at
        // the honest ceiling for the full run and congestion feedback is
        // dropped on the floor. This keeps the diagnostic value of
        // "throughput is capped — is it AIMD?" intact for dedicated links.
        if self.config.disabled {
            return;
        }
        let now = Instant::now();
        let effective_cooldown = match self.effective_cooldown_hint(cooldown_hint) {
            None => self.config.cooldown,
            Some(hint) => {
                let upper = self
                    .config
                    .cooldown
                    .saturating_mul(Self::MAX_HINT_MULTIPLIER);
                hint.clamp(Self::MIN_HINT_COOLDOWN, upper)
            }
        };
        let mut st = self.state(class).lock().expect("aimd mutex poisoned");
        let level_at_congestion = st.target;
        // KE-D2: the operator-supplied floor wins over the historical `1`.
        let floor = st.min_target.max(1).min(st.ceiling);
        st.target = (st.target / 2).max(floor);
        st.sem.set_live(st.target);
        st.cooldown_until = Some(now + effective_cooldown);
        st.healthy_since = None;
        // Ratchet the regrowth cap down: never above one slot below the level
        // that just congested, and a repeated congestion only tightens it.
        st.regrowth_cap = level_at_congestion
            .saturating_sub(1)
            .max(1)
            .min(st.regrowth_cap);
        st.last_congestion = Some(now);
        // DAG-P2-06: mirror the lowered safe target to the shared registry so
        // the next job to this same endpoint/workload seeds here, not at the
        // ceiling that just congested. Release the class lock first: the
        // registry mutex is a strict leaf and never touches a class lock.
        let learned = st.target;
        drop(st);
        self.record_learned(class, learned);
    }

    /// Note a healthy completion. After a quiet `healthy_window` with no
    /// congestion and no active cooldown, additively increase by one (never
    /// above the ceiling, and never above the [`AimdConfig::recovery_window`]
    /// guard band until the band has relaxed).
    pub fn note_healthy(&self, class: AdaptiveClass) {
        // KE-D1: the kill switch silences additive growth too. The window is
        // already pinned at the honest ceiling (built that way at `new`); a
        // healthy note must not change anything.
        if self.config.disabled {
            return;
        }
        let now = Instant::now();
        let mut st = self.state(class).lock().expect("aimd mutex poisoned");
        // Guard band relaxation: a full recovery window of quiet since the
        // last congestion lifts the regrowth cap back to the honest ceiling.
        if let Some(congested_at) = st.last_congestion {
            if now.duration_since(congested_at) >= self.config.recovery_window {
                st.regrowth_cap = st.ceiling;
                st.last_congestion = None;
            }
        }
        if let Some(until) = st.cooldown_until {
            if now < until {
                return;
            }
            st.cooldown_until = None;
        }
        let growth_cap = st.ceiling.min(st.regrowth_cap);
        if st.target >= growth_cap {
            return;
        }
        match st.healthy_since {
            None => st.healthy_since = Some(now),
            Some(since) => {
                if now.duration_since(since) >= self.config.healthy_window {
                    // KE-D2: additive step is operator-tunable; default is `+1`.
                    let step = st.step.max(1);
                    st.target = st.target.saturating_add(step).min(growth_cap);
                    st.sem.set_live(st.target);
                    st.healthy_since = Some(now);
                    // DAG-P2-06: real recovery raises the learned safe target
                    // for this key at the existing quiet cadence, so a later
                    // job to the same endpoint/workload resumes from the higher
                    // proven-safe level. Only fires when the target actually
                    // moved; a controller pinned at its ceiling records nothing.
                    let learned = st.target;
                    drop(st);
                    self.record_learned(class, learned);
                }
            }
        }
    }

    /// Acquire dispatch permits for every controlled class the node requests,
    /// in a fixed order. Held for the node's lifetime by the caller.
    pub async fn acquire(&self, request: &ResourceRequest) -> Vec<DynamicPermit> {
        let mut held = Vec::new();
        for class in AdaptiveClass::ORDER {
            let wants = match class {
                AdaptiveClass::File => request.file_slots,
                AdaptiveClass::Chunk => request.chunk_slots,
                AdaptiveClass::Http => request.http_slots,
                AdaptiveClass::Api => request.api_slots,
            };
            if wants == 0 {
                continue;
            }
            if class == AdaptiveClass::Api {
                if let Some(delay) = self.api_pacing_delay_at(Instant::now()) {
                    if !delay.is_zero() {
                        sleep(delay).await;
                    }
                }
            }
            let sem = {
                let st = self.state(class).lock().expect("aimd mutex poisoned");
                st.sem.clone()
            };
            if let Some(p) = sem.acquire().await {
                held.push(p);
            }
        }
        held
    }

    /// Controlled classes a node touches, for congestion / healthy feedback.
    pub fn classes_for(request: &ResourceRequest) -> Vec<AdaptiveClass> {
        let mut out = Vec::new();
        if request.file_slots > 0 {
            out.push(AdaptiveClass::File);
        }
        if request.chunk_slots > 0 {
            out.push(AdaptiveClass::Chunk);
        }
        if request.http_slots > 0 {
            out.push(AdaptiveClass::Http);
        }
        if request.api_slots > 0 {
            out.push(AdaptiveClass::Api);
        }
        out
    }

    #[cfg(test)]
    pub fn target(&self, class: AdaptiveClass) -> usize {
        self.state(class).lock().unwrap().target
    }

    #[cfg(test)]
    pub fn live(&self, class: AdaptiveClass) -> usize {
        self.state(class).lock().unwrap().sem.live()
    }

    #[cfg(test)]
    pub fn cooldown_until(&self, class: AdaptiveClass) -> Option<Instant> {
        self.state(class).lock().unwrap().cooldown_until
    }

    #[cfg(test)]
    pub fn api_pacing_delay_for_test(&self, now: Instant) -> Option<Duration> {
        self.api_pacing_delay_at(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install_google_drive_test_hint() {
        aimd_hints::init(std::collections::HashMap::from([(
            ProviderType::GoogleDrive,
            aimd_hints::AimdHint {
                burst: Some(2),
                min_sleep: Some(Duration::from_millis(500)),
                retry_after_secs: Some(30),
            },
        )]));
    }

    #[test]
    fn congestion_mapping_covers_the_trigger_set_only() {
        // Controller matches kind only; adapter classification is covered in
        // transfer_dag::error tests. Use typed kinds here.
        assert_eq!(
            congestion_from_error(&TransferError::new(
                TransferErrorKind::RateLimited,
                "HTTP 429 Too Many Requests"
            )),
            Some(CongestionEvent::TooManyRequests)
        );
        assert_eq!(
            congestion_from_error(&TransferError::new(
                TransferErrorKind::RateLimited,
                "rate limit exceeded"
            )),
            Some(CongestionEvent::TooManyRequests)
        );
        assert_eq!(
            congestion_from_error(&TransferError::new(
                TransferErrorKind::ServiceUnavailable,
                "503 Service Unavailable"
            )),
            Some(CongestionEvent::ServiceUnavailable)
        );
        assert_eq!(
            congestion_from_error(&TransferError::new(
                TransferErrorKind::Timeout,
                "operation timed out after 30s"
            )),
            Some(CongestionEvent::Timeout)
        );
        assert_eq!(
            congestion_from_error(&TransferError::new(
                TransferErrorKind::MaxConnections,
                "421 too many connections from your IP"
            )),
            Some(CongestionEvent::MaxConnections)
        );
        assert_eq!(
            congestion_from_error(&TransferError::new(
                TransferErrorKind::ConnectionReset,
                "connection reset by peer"
            )),
            Some(CongestionEvent::ConnectionReset)
        );
        // 504 is a timeout, which is in the D2 set.
        assert_eq!(
            congestion_from_error(&TransferError::new(
                TransferErrorKind::Timeout,
                "504 Gateway Timeout"
            )),
            Some(CongestionEvent::Timeout)
        );
        // Plain server errors with no load/timeout semantics must NOT throttle.
        assert_eq!(
            congestion_from_error(&TransferError::new(
                TransferErrorKind::Unknown,
                "500 Internal Server Error"
            )),
            None
        );
        assert_eq!(
            congestion_from_error(&TransferError::new(
                TransferErrorKind::RemoteIo,
                "502 Bad Gateway"
            )),
            None
        );
        assert_eq!(
            congestion_from_error(&TransferError::new(
                TransferErrorKind::NotFound,
                "404 not found"
            )),
            None
        );
        assert_eq!(
            congestion_from_error(&TransferError::new(
                TransferErrorKind::PermissionDenied,
                "permission denied"
            )),
            None
        );
        // Controller must ignore misleading presentation text when kind is
        // non-congestion (no substring fallback).
        assert_eq!(
            congestion_from_error(&TransferError::new(
                TransferErrorKind::NotFound,
                "HTTP 429 Too Many Requests"
            )),
            None
        );
    }

    #[test]
    fn congestion_mapping_via_adapter_preserves_d2_set() {
        // End-to-end: free-form messages classified once at the adapter still
        // feed the controller correctly.
        assert_eq!(
            congestion_from_error(&TransferError::from_message("HTTP 429 Too Many Requests")),
            Some(CongestionEvent::TooManyRequests)
        );
        assert_eq!(
            congestion_from_error(&TransferError::from_message("503 Service Unavailable")),
            Some(CongestionEvent::ServiceUnavailable)
        );
        assert_eq!(
            congestion_from_error(&TransferError::from_message("504 Gateway Timeout")),
            Some(CongestionEvent::Timeout)
        );
        assert_eq!(
            congestion_from_error(&TransferError::from_message("500 Internal Server Error")),
            None
        );
        assert_eq!(
            congestion_from_error(&TransferError::from_message("404 not found")),
            None
        );
    }

    #[tokio::test]
    async fn dynamic_semaphore_grows_and_shrinks_within_ceiling() {
        let ds = DynamicSemaphore::new(4, 2);
        assert_eq!(ds.live(), 2);
        ds.set_live(10); // clamped to ceiling
        assert_eq!(ds.live(), 4);
        assert_eq!(ds.available(), 4);
        ds.set_live(1);
        assert_eq!(ds.live(), 1);
        assert_eq!(ds.available(), 1);
        assert_eq!(ds.pending_shrink(), 0);
    }

    #[tokio::test]
    async fn shrink_is_lazy_and_absorbs_in_flight_on_release() {
        let ds = DynamicSemaphore::new(4, 4);
        // Hold 3 permits; only 1 is free.
        let p1 = ds.acquire().await.unwrap();
        let _p2 = ds.acquire().await.unwrap();
        let _p3 = ds.acquire().await.unwrap();
        assert_eq!(ds.available(), 1);

        // Ask to shrink to 1 (drop 3). Only the 1 free slot can be reclaimed
        // immediately; the other 2 are owed.
        ds.set_live(1);
        assert_eq!(ds.available(), 0);
        assert_eq!(ds.pending_shrink(), 2);

        // Releasing in-flight permits absorbs the deficit instead of
        // re-growing concurrency.
        drop(p1);
        assert_eq!(ds.pending_shrink(), 1);
        assert_eq!(ds.available(), 0);

        // Once the deficit is paid, the live size is honoured (1 permit).
        let ds2 = DynamicSemaphore::new(2, 2);
        let a = ds2.acquire().await.unwrap();
        ds2.set_live(1);
        drop(a);
        assert_eq!(ds2.pending_shrink(), 0);
        assert_eq!(ds2.available(), 1);
    }

    #[tokio::test]
    async fn deficit_paydown_then_release_never_wraps_and_loses_no_permits() {
        // AIMD-02 invariant: when the grow path pays down the whole owed
        // shrink and the in-flight permits are then released, the deficit must
        // settle at exactly 0 (never wrap below zero, which on AtomicUsize is
        // usize::MAX and would pin concurrency at the floor for the rest of the
        // run) and no permit may be lost.
        let ds = DynamicSemaphore::new(4, 4);
        let p1 = ds.acquire().await.unwrap();
        let p2 = ds.acquire().await.unwrap();
        let p3 = ds.acquire().await.unwrap();
        let p4 = ds.acquire().await.unwrap();
        assert_eq!(ds.available(), 0);

        // Shrink to 1 with everything checked out: 0 reclaimable now, owe 3.
        ds.set_live(1);
        assert_eq!(ds.pending_shrink(), 3);

        // Grow back to the ceiling: the pay-down clears all 3 owed slots via a
        // saturating CAS and never underflows.
        ds.set_live(4);
        assert_eq!(ds.pending_shrink(), 0);

        // Releasing the in-flight permits now that nothing is owed must return
        // them to the pool, not absorb them, and must keep the deficit at 0.
        drop(p1);
        drop(p2);
        drop(p3);
        drop(p4);
        assert_eq!(ds.pending_shrink(), 0, "deficit must not wrap below zero");
        assert_eq!(ds.available(), 4, "all permits restored, none lost");
    }

    #[tokio::test]
    async fn aimd_halves_on_congestion_and_floors_at_one() {
        let ctrl = AimdController::new(8, 1, 1, 1, AimdConfig::default());
        assert_eq!(ctrl.target(AdaptiveClass::File), 8);
        ctrl.on_congestion(AdaptiveClass::File);
        assert_eq!(ctrl.target(AdaptiveClass::File), 4);
        ctrl.on_congestion(AdaptiveClass::File);
        assert_eq!(ctrl.target(AdaptiveClass::File), 2);
        ctrl.on_congestion(AdaptiveClass::File);
        assert_eq!(ctrl.target(AdaptiveClass::File), 1);
        ctrl.on_congestion(AdaptiveClass::File);
        assert_eq!(ctrl.target(AdaptiveClass::File), 1, "floor is 1");
        assert_eq!(ctrl.live(AdaptiveClass::File), 1);
    }

    #[tokio::test]
    async fn aimd_cooldown_blocks_immediate_increase() {
        // Long cooldown, zero healthy window: even a "quiet" note must not
        // grow while the cooldown is active.
        let cfg = AimdConfig {
            cooldown: Duration::from_secs(3600),
            healthy_window: Duration::from_secs(0),
            recovery_window: Duration::from_secs(0),
            disabled: false,
            class_overrides: AimdClassOverrides::default(),
        };
        let ctrl = AimdController::new(8, 1, 1, 1, cfg);
        ctrl.on_congestion(AdaptiveClass::File);
        assert_eq!(ctrl.target(AdaptiveClass::File), 4);
        ctrl.note_healthy(AdaptiveClass::File);
        ctrl.note_healthy(AdaptiveClass::File);
        assert_eq!(
            ctrl.target(AdaptiveClass::File),
            4,
            "cooldown must gate the increase"
        );
    }

    #[tokio::test]
    async fn aimd_additive_increase_after_quiet_window_capped_at_ceiling() {
        // No cooldown, zero healthy/recovery window: each pair of notes yields
        // +1, but never above the ceiling. A zero recovery window disables the
        // guard band so this test exercises the bare additive mechanism.
        let cfg = AimdConfig {
            cooldown: Duration::from_secs(0),
            healthy_window: Duration::from_secs(0),
            recovery_window: Duration::from_secs(0),
            disabled: false,
            class_overrides: AimdClassOverrides::default(),
        };
        let ctrl = AimdController::new(3, 1, 1, 1, cfg);
        ctrl.on_congestion(AdaptiveClass::File); // 3 -> 1 (floor)
        assert_eq!(ctrl.target(AdaptiveClass::File), 1);
        // First note arms healthy_since, second crosses the (zero) window.
        ctrl.note_healthy(AdaptiveClass::File);
        ctrl.note_healthy(AdaptiveClass::File);
        assert_eq!(ctrl.target(AdaptiveClass::File), 2);
        ctrl.note_healthy(AdaptiveClass::File);
        assert_eq!(ctrl.target(AdaptiveClass::File), 3);
        // At the ceiling: further notes cannot overclaim.
        ctrl.note_healthy(AdaptiveClass::File);
        ctrl.note_healthy(AdaptiveClass::File);
        assert_eq!(ctrl.target(AdaptiveClass::File), 3, "never above ceiling");
        assert_eq!(ctrl.live(AdaptiveClass::File), 3);
    }

    #[tokio::test]
    async fn acquire_only_touches_requested_classes() {
        let ctrl = AimdController::new(1, 1, 1, 1, AimdConfig::default());
        let req = ResourceRequest::range_chunk(); // chunk + http + disk_write
        assert_eq!(
            AimdController::classes_for(&req),
            vec![AdaptiveClass::Chunk, AdaptiveClass::Http]
        );
        let held = ctrl.acquire(&req).await;
        assert_eq!(held.len(), 2, "chunk + http permits, not file/api");
    }

    #[tokio::test]
    async fn guard_band_blocks_regrowth_to_the_congested_level() {
        // A long recovery window keeps the guard band armed: additive
        // increase may climb back up but must stop one slot below the level
        // that congested, never re-entering the same congestion.
        let cfg = AimdConfig {
            cooldown: Duration::from_secs(0),
            healthy_window: Duration::from_secs(0),
            recovery_window: Duration::from_secs(3600),
            disabled: false,
            class_overrides: AimdClassOverrides::default(),
        };
        let ctrl = AimdController::new(8, 1, 1, 1, cfg);
        ctrl.on_congestion(AdaptiveClass::File); // 8 -> 4, regrowth cap -> 7
        assert_eq!(ctrl.target(AdaptiveClass::File), 4);
        for _ in 0..40 {
            ctrl.note_healthy(AdaptiveClass::File);
        }
        assert_eq!(
            ctrl.target(AdaptiveClass::File),
            7,
            "guard band holds regrowth one slot below the congested level"
        );
    }

    #[tokio::test]
    async fn guard_band_relaxes_after_recovery_window() {
        // A zero recovery window relaxes the guard band on the first healthy
        // note, so additive increase may climb all the way to the ceiling.
        let cfg = AimdConfig {
            cooldown: Duration::from_secs(0),
            healthy_window: Duration::from_secs(0),
            recovery_window: Duration::from_secs(0),
            disabled: false,
            class_overrides: AimdClassOverrides::default(),
        };
        let ctrl = AimdController::new(8, 1, 1, 1, cfg);
        ctrl.on_congestion(AdaptiveClass::File); // 8 -> 4
        for _ in 0..40 {
            ctrl.note_healthy(AdaptiveClass::File);
        }
        assert_eq!(
            ctrl.target(AdaptiveClass::File),
            8,
            "a relaxed guard band lets regrowth reach the honest ceiling"
        );
    }

    #[tokio::test]
    async fn guard_band_ratchets_down_on_repeated_congestion() {
        let cfg = AimdConfig {
            cooldown: Duration::from_secs(0),
            healthy_window: Duration::from_secs(0),
            recovery_window: Duration::from_secs(3600),
            disabled: false,
            class_overrides: AimdClassOverrides::default(),
        };
        let ctrl = AimdController::new(16, 1, 1, 1, cfg);
        ctrl.on_congestion(AdaptiveClass::File); // 16 -> 8, cap -> 15
        for _ in 0..40 {
            ctrl.note_healthy(AdaptiveClass::File);
        }
        assert_eq!(ctrl.target(AdaptiveClass::File), 15);
        ctrl.on_congestion(AdaptiveClass::File); // 15 -> 7, cap -> 14
        for _ in 0..40 {
            ctrl.note_healthy(AdaptiveClass::File);
        }
        assert_eq!(
            ctrl.target(AdaptiveClass::File),
            14,
            "a second congestion tightens the guard band further"
        );
    }

    #[test]
    fn from_budget_seeds_per_class_ceilings_from_the_effective_budget() {
        let budget = TransferBudget {
            file_slots: 6,
            chunk_slots: 3,
            http_slots: 9,
            api_slots: 2,
            ..TransferBudget::default()
        };
        let ctrl = AimdController::from_budget(&budget, AimdConfig::default());
        // Every class starts at its ceiling: a no-congestion run is identical
        // to having no controller at all.
        assert_eq!(ctrl.target(AdaptiveClass::File), 6);
        assert_eq!(ctrl.target(AdaptiveClass::Chunk), 3);
        assert_eq!(ctrl.target(AdaptiveClass::Http), 9);
        assert_eq!(ctrl.target(AdaptiveClass::Api), 2);
    }

    /// T-DEBT-05: `on_congestion_with_hint(class, Some(d))` must arm the
    /// cooldown for `d` instead of the configured default, so the AIMD
    /// throttle honors a server-provided `Retry-After` instead of pacing
    /// against its own fixed timer.
    #[tokio::test]
    async fn aimd_respects_cooldown_hint_when_provided() {
        let cfg = AimdConfig {
            cooldown: Duration::from_secs(5),
            healthy_window: Duration::from_secs(5),
            recovery_window: Duration::from_secs(30),
            disabled: false,
            class_overrides: AimdClassOverrides::default(),
        };
        let ctrl = AimdController::new(8, 1, 1, 1, cfg);
        let before = Instant::now();
        ctrl.on_congestion_with_hint(AdaptiveClass::File, Some(Duration::from_secs(45)));
        let until = ctrl
            .cooldown_until(AdaptiveClass::File)
            .expect("on_congestion_with_hint must arm cooldown_until");
        let elapsed = until.saturating_duration_since(before);
        // 45 s honored, ±1 s slack for the Instant::now() calls bracketing
        // the operation.
        assert!(
            elapsed >= Duration::from_secs(44) && elapsed <= Duration::from_secs(46),
            "expected ~45s cooldown, got {:?}",
            elapsed
        );
        // Multiplicative decrease still happens (8 -> 4) regardless of hint.
        assert_eq!(ctrl.target(AdaptiveClass::File), 4);
    }

    /// T-DEBT-05: hint clamping protects against both pathological
    /// near-zero hints (would defeat the throttle) and pathological
    /// hours-long hints (would stall the whole transfer).
    #[tokio::test]
    async fn aimd_clamps_outlier_cooldown_hints() {
        let cfg = AimdConfig {
            cooldown: Duration::from_secs(5),
            healthy_window: Duration::from_secs(5),
            recovery_window: Duration::from_secs(30),
            disabled: false,
            class_overrides: AimdClassOverrides::default(),
        };

        // Lower clamp: a 200 ms hint must be raised to MIN_HINT_COOLDOWN
        // (1 s) so the throttle window is not effectively zero.
        let ctrl_lo = AimdController::new(8, 1, 1, 1, cfg);
        let before_lo = Instant::now();
        ctrl_lo.on_congestion_with_hint(AdaptiveClass::File, Some(Duration::from_millis(200)));
        let until_lo = ctrl_lo.cooldown_until(AdaptiveClass::File).unwrap();
        let elapsed_lo = until_lo.saturating_duration_since(before_lo);
        assert!(
            elapsed_lo >= Duration::from_millis(900) && elapsed_lo <= Duration::from_millis(1100),
            "expected ~1s (lower clamp), got {:?}",
            elapsed_lo
        );

        // Upper clamp: a 3600 s hint against a 5 s default cooldown must
        // be clamped to MAX_HINT_MULTIPLIER × default = 50 s.
        let ctrl_hi = AimdController::new(8, 1, 1, 1, cfg);
        let before_hi = Instant::now();
        ctrl_hi.on_congestion_with_hint(AdaptiveClass::File, Some(Duration::from_secs(3600)));
        let until_hi = ctrl_hi.cooldown_until(AdaptiveClass::File).unwrap();
        let elapsed_hi = until_hi.saturating_duration_since(before_hi);
        assert!(
            elapsed_hi >= Duration::from_secs(49) && elapsed_hi <= Duration::from_secs(51),
            "expected ~50s (upper clamp), got {:?}",
            elapsed_hi
        );

        // None hint: behavior unchanged, cooldown matches the default.
        let ctrl_default = AimdController::new(8, 1, 1, 1, cfg);
        let before_d = Instant::now();
        ctrl_default.on_congestion_with_hint(AdaptiveClass::File, None);
        let until_d = ctrl_default.cooldown_until(AdaptiveClass::File).unwrap();
        let elapsed_d = until_d.saturating_duration_since(before_d);
        assert!(
            elapsed_d >= Duration::from_secs(4) && elapsed_d <= Duration::from_secs(6),
            "expected ~5s (default cooldown), got {:?}",
            elapsed_d
        );
    }

    #[test]
    fn provider_retry_after_hint_applies_when_server_hint_is_missing() {
        install_google_drive_test_hint();

        let cfg = AimdConfig {
            cooldown: Duration::from_secs(5),
            healthy_window: Duration::from_secs(5),
            recovery_window: Duration::from_secs(30),
            disabled: false,
            class_overrides: AimdClassOverrides::default(),
        };
        let ctrl =
            AimdController::new_for_provider(8, 1, 1, 1, Some(ProviderType::GoogleDrive), cfg);
        let before = Instant::now();
        ctrl.on_congestion_with_hint(AdaptiveClass::Api, None);
        let until = ctrl
            .cooldown_until(AdaptiveClass::Api)
            .expect("provider retry-after hint must arm cooldown_until");
        let elapsed = until.saturating_duration_since(before);
        assert!(elapsed >= Duration::from_secs(29) && elapsed <= Duration::from_secs(31));
    }

    #[test]
    fn provider_api_pacer_delays_after_burst() {
        install_google_drive_test_hint();

        let ctrl = AimdController::new_for_provider(
            1,
            1,
            1,
            1,
            Some(ProviderType::GoogleDrive),
            AimdConfig::default(),
        );
        let t0 = Instant::now();
        assert_eq!(ctrl.api_pacing_delay_for_test(t0), Some(Duration::ZERO));
        assert_eq!(ctrl.api_pacing_delay_for_test(t0), Some(Duration::ZERO));
        assert_eq!(
            ctrl.api_pacing_delay_for_test(t0),
            Some(Duration::from_millis(500))
        );
        assert_eq!(
            ctrl.api_pacing_delay_for_test(t0 + Duration::from_millis(500)),
            Some(Duration::ZERO)
        );
    }

    // T-DEBT-05 S1-T02f: marker convention helpers. The marker carries the
    // server-provided Retry-After across the provider→executor boundary
    // without requiring a typed field on ProviderError.

    #[test]
    fn marker_roundtrips_seconds_value() {
        let msg = format!("HTTP 429 Too Many Requests{}", embed_retry_after_marker(45));
        assert_eq!(
            parse_embedded_retry_after(&msg),
            Some(Duration::from_secs(45))
        );
    }

    #[test]
    fn marker_returns_none_when_absent() {
        let msg = "HTTP 429 Too Many Requests";
        assert_eq!(parse_embedded_retry_after(msg), None);
    }

    #[test]
    fn marker_returns_none_on_malformed_value() {
        // Negative is not a valid u64.
        assert_eq!(
            parse_embedded_retry_after("err [retry-after-secs=-5]"),
            None
        );
        // Non-numeric.
        assert_eq!(
            parse_embedded_retry_after("err [retry-after-secs=abc]"),
            None
        );
        // Missing closing bracket.
        assert_eq!(parse_embedded_retry_after("err [retry-after-secs=30"), None);
    }

    #[test]
    fn marker_extracts_first_occurrence_only() {
        // If a provider double-appends (defensive: should not happen), the
        // first valid value wins. This keeps the helper deterministic.
        let msg = format!(
            "err{}{}",
            embed_retry_after_marker(30),
            embed_retry_after_marker(60)
        );
        assert_eq!(
            parse_embedded_retry_after(&msg),
            Some(Duration::from_secs(30))
        );
    }

    // KE-D1: `--aimd-disable` kill switch. When the operator installs
    // `AimdConfig { disabled: true, .. }` the controller must remain pinned
    // at the honest ceiling for the whole run: congestion feedback is a
    // no-op (no halving, no cooldown armed) and healthy notes do not grow
    // the window further. Use case: dedicated/lab links where AIMD masks a
    // genuine throughput cap diagnostic.

    #[tokio::test]
    async fn aimd_disable_skips_congestion_reset() {
        let cfg = AimdConfig {
            cooldown: Duration::from_secs(5),
            healthy_window: Duration::from_secs(5),
            recovery_window: Duration::from_secs(30),
            disabled: true,
            class_overrides: AimdClassOverrides::default(),
        };
        let ctrl = AimdController::new(8, 1, 1, 1, cfg);
        // Even after a congestion event the target must not halve and no
        // cooldown must be armed.
        ctrl.on_congestion(AdaptiveClass::File);
        assert_eq!(
            ctrl.target(AdaptiveClass::File),
            8,
            "disabled controller must not halve on congestion"
        );
        assert!(
            ctrl.cooldown_until(AdaptiveClass::File).is_none(),
            "disabled controller must not arm a cooldown"
        );
        // Repeated congestion is still a no-op (no ratchet).
        ctrl.on_congestion(AdaptiveClass::File);
        ctrl.on_congestion(AdaptiveClass::File);
        assert_eq!(ctrl.target(AdaptiveClass::File), 8);
        // Healthy notes are also no-ops (window already at ceiling).
        ctrl.note_healthy(AdaptiveClass::File);
        ctrl.note_healthy(AdaptiveClass::File);
        assert_eq!(ctrl.target(AdaptiveClass::File), 8);
    }

    // KE-D2: per-class window overrides (`--aimd-min-window`,
    // `--aimd-max-window`, `--aimd-step-window`, plus the per-class TOML
    // file). The CLI layer is covered by tests in `aeroftp_cli.rs`; here
    // we cover the controller's runtime behaviour once the overrides
    // have been baked into `AimdConfig`.

    fn cfg_with_window(window: AimdClassWindow) -> AimdConfig {
        AimdConfig {
            cooldown: Duration::from_secs(0),
            healthy_window: Duration::from_secs(0),
            recovery_window: Duration::from_secs(0),
            disabled: false,
            class_overrides: AimdClassOverrides::broadcast(window),
        }
    }

    #[tokio::test]
    async fn aimd_min_window_floors_target_after_halving() {
        // min=4 says "never let `target` drop below 4 after multiplicative
        // decrease". Halving 8 normally goes to 4, then 2, then 1; the
        // floor pins the second and third events at 4.
        let cfg = cfg_with_window(AimdClassWindow {
            min: Some(4),
            ..Default::default()
        });
        let ctrl = AimdController::new(8, 1, 1, 1, cfg);
        assert_eq!(ctrl.target(AdaptiveClass::File), 8);
        ctrl.on_congestion(AdaptiveClass::File);
        assert_eq!(ctrl.target(AdaptiveClass::File), 4);
        ctrl.on_congestion(AdaptiveClass::File);
        assert_eq!(
            ctrl.target(AdaptiveClass::File),
            4,
            "the operator-supplied floor must hold across repeated congestion"
        );
        assert_eq!(ctrl.live(AdaptiveClass::File), 4);
    }

    #[tokio::test]
    async fn aimd_max_window_clamps_ceiling() {
        // max=3 says "never grow ceiling above 3" even though the budget
        // would allow up to 8. AIMD is decrease-biased, so max only
        // shrinks the ceiling, never raises it.
        let cfg = cfg_with_window(AimdClassWindow {
            max: Some(3),
            ..Default::default()
        });
        let ctrl = AimdController::new(8, 1, 1, 1, cfg);
        assert_eq!(
            ctrl.target(AdaptiveClass::File),
            3,
            "`--aimd-max-window` must clamp the budget-derived ceiling"
        );
        // Healthy notes cannot push past the clamped ceiling.
        for _ in 0..10 {
            ctrl.note_healthy(AdaptiveClass::File);
        }
        assert_eq!(ctrl.target(AdaptiveClass::File), 3);
    }

    #[tokio::test]
    async fn aimd_step_window_multiplies_additive_increase() {
        // step=4 means each quiet `healthy_window` adds 4 instead of 1.
        // Starting from a halved target of 1 (8 -> 1 with a min=1 floor),
        // two healthy notes climb to 5, three notes to 8 (clamped to
        // ceiling), four notes still 8.
        let cfg = cfg_with_window(AimdClassWindow {
            step: Some(4),
            ..Default::default()
        });
        let ctrl = AimdController::new(8, 1, 1, 1, cfg);
        ctrl.on_congestion(AdaptiveClass::File); // 8 -> 4
        ctrl.on_congestion(AdaptiveClass::File); // 4 -> 2
        ctrl.on_congestion(AdaptiveClass::File); // 2 -> 1
        assert_eq!(ctrl.target(AdaptiveClass::File), 1);
        // First note arms `healthy_since`, second crosses the (zero)
        // window: +4 to 5.
        ctrl.note_healthy(AdaptiveClass::File);
        ctrl.note_healthy(AdaptiveClass::File);
        assert_eq!(ctrl.target(AdaptiveClass::File), 5);
        ctrl.note_healthy(AdaptiveClass::File);
        // Next step would push to 9 but the honest ceiling is 8.
        assert_eq!(ctrl.target(AdaptiveClass::File), 8);
        ctrl.note_healthy(AdaptiveClass::File);
        assert_eq!(ctrl.target(AdaptiveClass::File), 8);
    }

    #[tokio::test]
    async fn aimd_overrides_clamp_to_safe_range() {
        // Pathological `min = 0`, `max = 0`, `step = 0` from a config
        // file or CLI flag must not collapse the controller: `min` and
        // `step` are sanitized to `1`, `max` is sanitized to `1` (so the
        // ceiling becomes `1` too — AIMD is decrease-biased and cannot
        // raise the ceiling above the budget anyway).
        let cfg = cfg_with_window(AimdClassWindow {
            min: Some(0),
            max: Some(0),
            step: Some(0),
        });
        let ctrl = AimdController::new(8, 8, 8, 8, cfg);
        assert_eq!(
            ctrl.target(AdaptiveClass::File),
            1,
            "max=0 must clamp the ceiling to 1, never below"
        );
        ctrl.on_congestion(AdaptiveClass::File);
        assert_eq!(
            ctrl.target(AdaptiveClass::File),
            1,
            "min=0 must clamp the floor to 1, halving cannot go below"
        );
        ctrl.note_healthy(AdaptiveClass::File);
        ctrl.note_healthy(AdaptiveClass::File);
        assert_eq!(
            ctrl.target(AdaptiveClass::File),
            1,
            "step=0 must coerce to +1; ceiling is 1 so no growth happens"
        );
    }

    #[tokio::test]
    async fn aimd_class_overrides_are_independent_per_class() {
        // The api class lifts its floor to 4; the file class is unmodified.
        let cfg = AimdConfig {
            cooldown: Duration::from_secs(0),
            healthy_window: Duration::from_secs(0),
            recovery_window: Duration::from_secs(0),
            disabled: false,
            class_overrides: AimdClassOverrides {
                api: AimdClassWindow {
                    min: Some(4),
                    ..Default::default()
                },
                ..Default::default()
            },
        };
        let ctrl = AimdController::new(8, 8, 8, 8, cfg);
        // File class halves all the way to 1: no override there.
        ctrl.on_congestion(AdaptiveClass::File);
        ctrl.on_congestion(AdaptiveClass::File);
        ctrl.on_congestion(AdaptiveClass::File);
        ctrl.on_congestion(AdaptiveClass::File);
        assert_eq!(ctrl.target(AdaptiveClass::File), 1);
        // Api class halves only down to its operator-supplied floor of 4.
        ctrl.on_congestion(AdaptiveClass::Api);
        ctrl.on_congestion(AdaptiveClass::Api);
        ctrl.on_congestion(AdaptiveClass::Api);
        assert_eq!(ctrl.target(AdaptiveClass::Api), 4);
    }

    #[tokio::test]
    async fn aimd_disable_ignores_retry_after_hint() {
        let cfg = AimdConfig {
            cooldown: Duration::from_secs(5),
            healthy_window: Duration::from_secs(5),
            recovery_window: Duration::from_secs(30),
            disabled: true,
            class_overrides: AimdClassOverrides::default(),
        };
        let ctrl = AimdController::new(8, 1, 1, 1, cfg);
        // A server-provided Retry-After hint is dropped on the floor when
        // disabled — same as a hint-less congestion event.
        ctrl.on_congestion_with_hint(AdaptiveClass::File, Some(Duration::from_secs(60)));
        assert_eq!(ctrl.target(AdaptiveClass::File), 8);
        assert!(ctrl.cooldown_until(AdaptiveClass::File).is_none());
    }

    // DAG-P2-06: endpoint/workload-aware adaptive profiles. The registry is a
    // seed/feedback record; the live dispatch permits stay per-run. Every test
    // uses a fresh registry instance (and an injected clock where TTL matters)
    // so it never touches the process-global registry and is order-independent.

    fn profile_registry() -> Arc<AdaptiveProfileRegistry> {
        Arc::new(AdaptiveProfileRegistry::new(
            AdaptiveProfileConfig::default(),
        ))
    }

    fn endpoint(host: &str, account: &str) -> EndpointIdentity {
        EndpointIdentity::new("s3", host, account)
    }

    fn range_key(host: &str, account: &str) -> AdaptiveProfileKey {
        AdaptiveProfileKey::new(endpoint(host, account), AdaptiveWorkload::SegmentedRange)
    }

    fn profile_budget(file: u16, chunk: u16, http: u16, api: u16) -> TransferBudget {
        TransferBudget {
            file_slots: file,
            chunk_slots: chunk,
            http_slots: http,
            api_slots: api,
            ..TransferBudget::default()
        }
    }

    fn zero_windows() -> AimdConfig {
        AimdConfig {
            cooldown: Duration::from_secs(0),
            healthy_window: Duration::from_secs(0),
            recovery_window: Duration::from_secs(0),
            disabled: false,
            class_overrides: AimdClassOverrides::default(),
        }
    }

    // (Acceptance 1) First profile use is byte-for-byte the per-run ceiling and
    // records nothing on the happy path, so a single-job run is unchanged.
    #[test]
    fn first_profile_use_matches_the_per_run_ceiling_and_records_nothing() {
        let registry = profile_registry();
        let budget = profile_budget(6, 3, 9, 2);
        let key = range_key("host-a", "acct-1");
        let profiled = AimdController::from_budget_for_profile(
            &budget,
            None,
            key,
            registry.clone(),
            AimdConfig::default(),
        );
        let per_run = AimdController::from_budget(&budget, AimdConfig::default());
        for class in AdaptiveClass::ORDER {
            assert_eq!(
                profiled.target(class),
                per_run.target(class),
                "first use must match the per-run ceiling for {class:?}"
            );
            assert_eq!(profiled.live(class), per_run.live(class));
        }
        assert!(
            registry.is_empty(),
            "a congestion-free first use records nothing"
        );
    }

    // (Acceptance 2) Congestion on (A, range) seeds only the next (A, range)
    // controller; endpoint B, a different account on A, and (A, batch) are
    // untouched.
    #[test]
    fn congestion_seeds_only_the_same_endpoint_and_workload() {
        let registry = profile_registry();
        let budget = profile_budget(1, 8, 8, 1);
        let job1 = AimdController::from_budget_for_profile(
            &budget,
            None,
            range_key("host-a", "acct-1"),
            registry.clone(),
            AimdConfig::default(),
        );
        job1.on_congestion(AdaptiveClass::Chunk); // 8 -> 4
        job1.on_congestion(AdaptiveClass::Http); // 8 -> 4

        let job2 = AimdController::from_budget_for_profile(
            &budget,
            None,
            range_key("host-a", "acct-1"),
            registry.clone(),
            AimdConfig::default(),
        );
        assert_eq!(
            job2.target(AdaptiveClass::Chunk),
            4,
            "same key seeds the lowered chunk target"
        );
        assert_eq!(
            job2.target(AdaptiveClass::Http),
            4,
            "same key seeds the lowered http target"
        );
        // File/Api were never congested for this key: still at their ceiling.
        assert_eq!(job2.target(AdaptiveClass::File), 1);

        let other_host = AimdController::from_budget_for_profile(
            &budget,
            None,
            range_key("host-b", "acct-1"),
            registry.clone(),
            AimdConfig::default(),
        );
        assert_eq!(
            other_host.target(AdaptiveClass::Chunk),
            8,
            "a different endpoint host is a different key"
        );

        let other_account = AimdController::from_budget_for_profile(
            &budget,
            None,
            range_key("host-a", "acct-2"),
            registry.clone(),
            AimdConfig::default(),
        );
        assert_eq!(
            other_account.target(AdaptiveClass::Chunk),
            8,
            "a different account on the same host is a different key"
        );

        let batch_key = AdaptiveProfileKey::new(
            endpoint("host-a", "acct-1"),
            AdaptiveWorkload::BatchSyncFile,
        );
        let batch = AimdController::from_budget_for_profile(
            &budget,
            None,
            batch_key,
            registry.clone(),
            AimdConfig::default(),
        );
        assert_eq!(
            batch.target(AdaptiveClass::Chunk),
            8,
            "the same endpoint under a different workload is a different key"
        );
    }

    // (Acceptance 3) Healthy feedback recovers one bounded step, and only after
    // the quiet window, and never above the current budget or explicit max.
    #[test]
    fn healthy_recovery_raises_the_learned_seed_one_bounded_step() {
        let registry = profile_registry();
        let budget = profile_budget(8, 1, 1, 1);
        let key = AdaptiveProfileKey::new(
            endpoint("host-a", "acct-1"),
            AdaptiveWorkload::BatchSyncFile,
        );
        let job1 = AimdController::from_budget_for_profile(
            &budget,
            None,
            key.clone(),
            registry.clone(),
            zero_windows(),
        );
        job1.on_congestion(AdaptiveClass::File); // 8 -> 4, learns 4
                                                 // First note arms, second crosses the (zero) window: 4 -> 5.
        job1.note_healthy(AdaptiveClass::File);
        job1.note_healthy(AdaptiveClass::File);
        assert_eq!(job1.target(AdaptiveClass::File), 5);

        let job2 = AimdController::from_budget_for_profile(
            &budget,
            None,
            key.clone(),
            registry.clone(),
            zero_windows(),
        );
        assert_eq!(
            job2.target(AdaptiveClass::File),
            5,
            "recovery raised the learned seed one step (not back to 4, not to 8)"
        );

        for _ in 0..40 {
            job2.note_healthy(AdaptiveClass::File);
        }
        assert_eq!(job2.target(AdaptiveClass::File), 8);
        let job3 = AimdController::from_budget_for_profile(
            &budget,
            None,
            key,
            registry.clone(),
            zero_windows(),
        );
        assert_eq!(
            job3.target(AdaptiveClass::File),
            8,
            "a learned seed never exceeds the honest ceiling"
        );
    }

    #[test]
    fn recovery_is_gated_on_the_quiet_window_before_it_is_learned() {
        let registry = profile_registry();
        let budget = profile_budget(8, 1, 1, 1);
        let key = AdaptiveProfileKey::new(
            endpoint("host-a", "acct-1"),
            AdaptiveWorkload::BatchSyncFile,
        );
        // A long healthy window: two quick notes do not cross it, so nothing
        // grows and nothing new is learned beyond the congested level.
        let cfg = AimdConfig {
            cooldown: Duration::from_secs(0),
            healthy_window: Duration::from_secs(3600),
            recovery_window: Duration::from_secs(0),
            disabled: false,
            class_overrides: AimdClassOverrides::default(),
        };
        let job = AimdController::from_budget_for_profile(
            &budget,
            None,
            key.clone(),
            registry.clone(),
            cfg,
        );
        job.on_congestion(AdaptiveClass::File); // 8 -> 4, learns 4
        job.note_healthy(AdaptiveClass::File);
        job.note_healthy(AdaptiveClass::File); // window not elapsed -> no growth
        assert_eq!(job.target(AdaptiveClass::File), 4);
        let seeded =
            AimdController::from_budget_for_profile(&budget, None, key, registry.clone(), cfg);
        assert_eq!(
            seeded.target(AdaptiveClass::File),
            4,
            "no premature recovery is learned before the quiet window elapses"
        );
    }

    #[test]
    fn operator_max_caps_the_seeded_target() {
        let registry = profile_registry();
        let budget = profile_budget(8, 1, 1, 1);
        let key = AdaptiveProfileKey::new(
            endpoint("host-a", "acct-1"),
            AdaptiveWorkload::BatchSyncFile,
        );
        // A prior run without a max recovers the learned File target above 3.
        let prior = AimdController::from_budget_for_profile(
            &budget,
            None,
            key.clone(),
            registry.clone(),
            zero_windows(),
        );
        prior.on_congestion(AdaptiveClass::File); // 8 -> 4
        for _ in 0..4 {
            prior.note_healthy(AdaptiveClass::File);
        }
        assert!(prior.target(AdaptiveClass::File) >= 6);
        // A new run with an explicit File max of 3 must never start above 3,
        // even though the learned safe target is higher: the operator cap wins.
        let capped_cfg = AimdConfig {
            class_overrides: AimdClassOverrides {
                file: AimdClassWindow {
                    max: Some(3),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..zero_windows()
        };
        let capped = AimdController::from_budget_for_profile(
            &budget,
            None,
            key,
            registry.clone(),
            capped_cfg,
        );
        assert_eq!(
            capped.target(AdaptiveClass::File),
            3,
            "an explicit operator max caps the learned seed"
        );
    }

    // (Acceptance 4) TTL expiry returns a key to first-use behaviour; capacity
    // eviction is deterministic and retains no unbounded identities.
    #[test]
    fn expired_profile_returns_to_first_use_behavior() {
        let clock = Arc::new(ManualClock::new());
        let cfg = AdaptiveProfileConfig {
            ttl: Duration::from_secs(100),
            capacity: 8,
        };
        let registry = Arc::new(AdaptiveProfileRegistry::with_clock(cfg, clock.clone()));
        let budget = profile_budget(1, 8, 8, 1);
        let key = range_key("host-a", "acct-1");
        let job = AimdController::from_budget_for_profile(
            &budget,
            None,
            key.clone(),
            registry.clone(),
            AimdConfig::default(),
        );
        job.on_congestion(AdaptiveClass::Chunk); // learns chunk=4 at t=0

        clock.advance(Duration::from_secs(99));
        let before = AimdController::from_budget_for_profile(
            &budget,
            None,
            key.clone(),
            registry.clone(),
            AimdConfig::default(),
        );
        assert_eq!(
            before.target(AdaptiveClass::Chunk),
            4,
            "seed applies before TTL"
        );

        clock.advance(Duration::from_secs(2)); // total 101 >= 100
        let after = AimdController::from_budget_for_profile(
            &budget,
            None,
            key,
            registry.clone(),
            AimdConfig::default(),
        );
        assert_eq!(
            after.target(AdaptiveClass::Chunk),
            8,
            "an expired entry behaves like a first use"
        );
        assert!(registry.is_empty(), "expired entries are purged on access");
    }

    #[test]
    fn registry_capacity_evicts_least_recently_used_deterministically() {
        let clock = Arc::new(ManualClock::new());
        let cfg = AdaptiveProfileConfig {
            ttl: Duration::from_secs(10_000),
            capacity: 2,
        };
        let registry = AdaptiveProfileRegistry::with_clock(cfg, clock);
        let k1 = range_key("host-1", "acct");
        let k2 = range_key("host-2", "acct");
        let k3 = range_key("host-3", "acct");
        registry.record(&k1, AdaptiveClass::Chunk, 3);
        registry.record(&k2, AdaptiveClass::Chunk, 4);
        assert_eq!(registry.len(), 2);
        // Touch k1 so k2 becomes the least-recently-used entry.
        assert_eq!(registry.seed(&k1, AdaptiveClass::Chunk), Some(3));
        // A third key over capacity evicts k2, never k1 or k3.
        registry.record(&k3, AdaptiveClass::Chunk, 5);
        assert_eq!(registry.len(), 2, "capacity is a hard cap");
        assert_eq!(
            registry.seed(&k1, AdaptiveClass::Chunk),
            Some(3),
            "a recently touched key is retained"
        );
        assert_eq!(
            registry.seed(&k2, AdaptiveClass::Chunk),
            None,
            "the least-recently-used key is evicted"
        );
        assert_eq!(
            registry.seed(&k3, AdaptiveClass::Chunk),
            Some(5),
            "the new key is inserted"
        );
    }

    // (Acceptance 5) Retry-After, non-D2 failures, cancellation and
    // `--aimd-disable` preserve their behaviour and never poison a profile.
    #[test]
    fn disabled_controller_neither_seeds_nor_records() {
        let registry = profile_registry();
        let budget = profile_budget(1, 8, 8, 1);
        let key = range_key("host-a", "acct-1");
        // Warm the profile with a real (enabled) congestion: chunk learns 4.
        let warm = AimdController::from_budget_for_profile(
            &budget,
            None,
            key.clone(),
            registry.clone(),
            AimdConfig::default(),
        );
        warm.on_congestion(AdaptiveClass::Chunk);
        assert_eq!(warm.target(AdaptiveClass::Chunk), 4);

        // A disabled controller ignores the learned seed and pins at the honest
        // ceiling (the KE-D1 diagnostic contract).
        let disabled_cfg = AimdConfig {
            disabled: true,
            ..AimdConfig::default()
        };
        let disabled = AimdController::from_budget_for_profile(
            &budget,
            None,
            key.clone(),
            registry.clone(),
            disabled_cfg,
        );
        assert_eq!(
            disabled.target(AdaptiveClass::Chunk),
            8,
            "a disabled controller ignores the learned seed"
        );
        // It must not mutate the profile either: both feedback paths are no-ops.
        disabled.on_congestion(AdaptiveClass::Chunk);
        disabled.note_healthy(AdaptiveClass::Chunk);

        let probe = AimdController::from_budget_for_profile(
            &budget,
            None,
            key,
            registry.clone(),
            AimdConfig::default(),
        );
        assert_eq!(
            probe.target(AdaptiveClass::Chunk),
            4,
            "the disabled run left the learned profile untouched"
        );
    }

    #[test]
    fn typed_retry_after_congestion_learns_the_halved_target_without_poisoning() {
        let registry = profile_registry();
        let budget = profile_budget(1, 8, 8, 1);
        let key = range_key("host-a", "acct-1");
        let job = AimdController::from_budget_for_profile(
            &budget,
            None,
            key.clone(),
            registry.clone(),
            AimdConfig::default(),
        );
        // A server Retry-After is a legitimate D2 congestion: it lowers the
        // target and arms the hinted cooldown, but only the honest halved safe
        // target is written to the profile (never the hint value).
        job.on_congestion_with_hint(AdaptiveClass::Chunk, Some(Duration::from_secs(45)));
        assert_eq!(job.target(AdaptiveClass::Chunk), 4);
        assert!(job.cooldown_until(AdaptiveClass::Chunk).is_some());
        let next = AimdController::from_budget_for_profile(
            &budget,
            None,
            key,
            registry.clone(),
            AimdConfig::default(),
        );
        assert_eq!(
            next.target(AdaptiveClass::Chunk),
            4,
            "the profile learns the halved safe target, not the Retry-After value"
        );
    }

    #[test]
    fn a_run_without_d2_congestion_records_nothing() {
        // Generic failures, cancellation and plain success never call
        // on_congestion (the executor gates on `congestion_from_error`, covered
        // by the mapping tests above), so the target never leaves the ceiling
        // and the profile stays empty. Healthy notes at the ceiling are inert.
        let registry = profile_registry();
        let budget = profile_budget(8, 1, 1, 1);
        let key = AdaptiveProfileKey::new(
            endpoint("host-a", "acct-1"),
            AdaptiveWorkload::BatchSyncFile,
        );
        let job = AimdController::from_budget_for_profile(
            &budget,
            None,
            key,
            registry.clone(),
            zero_windows(),
        );
        for _ in 0..10 {
            job.note_healthy(AdaptiveClass::File);
        }
        assert_eq!(
            job.target(AdaptiveClass::File),
            8,
            "healthy notes at the ceiling do not grow"
        );
        assert!(
            registry.is_empty(),
            "a run with no D2 congestion learns nothing"
        );
    }

    #[test]
    fn snapshot_exposes_live_learned_targets() {
        let registry = profile_registry();
        let budget = profile_budget(1, 8, 8, 1);
        let key = range_key("host-a", "acct-1");
        let job = AimdController::from_budget_for_profile(
            &budget,
            None,
            key.clone(),
            registry.clone(),
            AimdConfig::default(),
        );
        assert_eq!(job.profile_key(), Some(&key));
        job.on_congestion(AdaptiveClass::Chunk);
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].key, key);
        assert_eq!(snapshot[0].chunk, Some(4));
        assert_eq!(snapshot[0].file, None);
    }

    // ── DAG-P2-07 (block F): slow optimization loop ──────────────────────

    /// Whole-file (batch/sync) key, the workload the slow loop feeds from the
    /// job-total telemetry.
    fn file_key(host: &str, account: &str) -> AdaptiveProfileKey {
        AdaptiveProfileKey::new(endpoint(host, account), AdaptiveWorkload::BatchSyncFile)
    }

    /// Build an observation. `wall_ms` sets the wall clock; `wait`/`run` set the
    /// dispatch wait ratio.
    fn obs(
        concurrency: usize,
        bytes: u64,
        wall_ms: u64,
        wait_nanos: u64,
        run_nanos: u64,
    ) -> JobThroughputObservation {
        JobThroughputObservation {
            concurrency,
            bytes,
            wall_clock_nanos: wall_ms * 1_000_000,
            wait_nanos,
            run_nanos,
        }
    }

    #[test]
    fn slow_loop_first_observation_only_seeds_the_baseline() {
        let registry = profile_registry();
        let key = file_key("host-a", "acct-1");
        // 1000 bytes in 1s at concurrency 2, negligible dispatch wait.
        let d = registry.observe_job(&key, AdaptiveClass::File, obs(2, 1000, 1000, 0, 900));
        assert_eq!(d, SlowLoopDecision::Seeded);
        assert_eq!(
            registry.seed(&key, AdaptiveClass::File),
            None,
            "seeding the baseline must not move the learned target"
        );
    }

    #[test]
    fn slow_loop_raises_target_on_proven_faster_low_wait() {
        let registry = profile_registry();
        let key = file_key("host-a", "acct-1");
        registry.observe_job(&key, AdaptiveClass::File, obs(2, 1000, 1000, 0, 900));
        // 3x throughput at higher concurrency with low wait: proven productive.
        // The first raise steps one above the prior productive concurrency (2).
        let d = registry.observe_job(&key, AdaptiveClass::File, obs(4, 3000, 1000, 0, 900));
        assert_eq!(d, SlowLoopDecision::Raised(3));
        assert_eq!(
            registry.seed(&key, AdaptiveClass::File),
            Some(3),
            "a proven-faster job steps the target one toward the productive width"
        );
    }

    #[test]
    fn slow_loop_raise_is_bounded_by_the_observed_concurrency() {
        let registry = profile_registry();
        let key = file_key("host-a", "acct-1");
        registry.observe_job(&key, AdaptiveClass::File, obs(2, 1000, 1000, 0, 900));
        // Progressively faster jobs step the learned target up by at most one
        // per observation and never above the concurrency that ran.
        registry.observe_job(&key, AdaptiveClass::File, obs(8, 3000, 1000, 0, 900));
        let first = registry.seed(&key, AdaptiveClass::File).unwrap();
        registry.observe_job(&key, AdaptiveClass::File, obs(8, 9000, 1000, 0, 900));
        let second = registry.seed(&key, AdaptiveClass::File).unwrap();
        assert!(second <= 8, "never above the observed concurrency");
        assert_eq!(second, first + 1, "one step per observation");
    }

    #[test]
    fn slow_loop_lowers_target_on_plateau() {
        let registry = profile_registry();
        let key = file_key("host-a", "acct-1");
        registry.observe_job(&key, AdaptiveClass::File, obs(2, 1000, 1000, 0, 900));
        // More concurrency (4 > 2) but no more throughput: a plateau.
        let d = registry.observe_job(&key, AdaptiveClass::File, obs(4, 1000, 1000, 0, 900));
        assert_eq!(d, SlowLoopDecision::Lowered(3));
        assert_eq!(registry.seed(&key, AdaptiveClass::File), Some(3));
    }

    #[test]
    fn slow_loop_lowers_target_on_starved_dispatch() {
        let registry = profile_registry();
        let key = file_key("host-a", "acct-1");
        registry.observe_job(&key, AdaptiveClass::File, obs(2, 1000, 1000, 0, 900));
        // Even a faster job is read as over-subscription when the dispatch spent
        // most of its life waiting for permits (wait >= 50%).
        let d = registry.observe_job(&key, AdaptiveClass::File, obs(2, 2000, 1000, 800, 200));
        assert_eq!(d, SlowLoopDecision::Lowered(1));
    }

    #[test]
    fn slow_loop_holds_on_marginal_gain() {
        let registry = profile_registry();
        let key = file_key("host-a", "acct-1");
        registry.observe_job(&key, AdaptiveClass::File, obs(2, 1000, 1000, 0, 900));
        // +2% throughput (< the 5% bar) at the same concurrency, low wait.
        let d = registry.observe_job(&key, AdaptiveClass::File, obs(2, 1020, 1000, 0, 900));
        assert_eq!(d, SlowLoopDecision::Held);
        assert_eq!(
            registry.seed(&key, AdaptiveClass::File),
            None,
            "a marginal gain does not move the learned target"
        );
    }

    #[test]
    fn slow_loop_is_isolated_by_key() {
        let registry = profile_registry();
        let k1 = file_key("host-a", "acct-1");
        let k2 = file_key("host-b", "acct-1");
        registry.observe_job(&k1, AdaptiveClass::File, obs(2, 1000, 1000, 0, 900));
        registry.observe_job(&k1, AdaptiveClass::File, obs(4, 1000, 1000, 0, 900)); // plateau k1
        assert_eq!(registry.seed(&k1, AdaptiveClass::File), Some(3));
        // k2 has seen nothing: its first observation still just seeds.
        let d2 = registry.observe_job(&k2, AdaptiveClass::File, obs(2, 1000, 1000, 0, 900));
        assert_eq!(d2, SlowLoopDecision::Seeded);
        assert_eq!(registry.seed(&k2, AdaptiveClass::File), None);
    }

    #[test]
    fn slow_loop_ttl_expiry_resets_the_baseline() {
        let clock = Arc::new(ManualClock::new());
        let cfg = AdaptiveProfileConfig {
            ttl: Duration::from_secs(100),
            capacity: 8,
        };
        let registry = AdaptiveProfileRegistry::with_clock(cfg, clock.clone());
        let key = file_key("host-a", "acct-1");
        registry.observe_job(&key, AdaptiveClass::File, obs(2, 1000, 1000, 0, 900));
        clock.advance(Duration::from_secs(101));
        // The expired entry behaves like a first use again: seeds, does not
        // raise/lower off a stale baseline.
        let d = registry.observe_job(&key, AdaptiveClass::File, obs(4, 3000, 1000, 0, 900));
        assert_eq!(d, SlowLoopDecision::Seeded);
    }

    #[test]
    fn slow_loop_no_signal_when_aimd_disabled() {
        let registry = profile_registry();
        let budget = profile_budget(8, 8, 8, 8);
        let key = file_key("host-a", "acct-1");
        let disabled = AimdConfig {
            disabled: true,
            ..AimdConfig::default()
        };
        let controller = AimdController::from_budget_for_profile(
            &budget,
            None,
            key.clone(),
            registry.clone(),
            disabled,
        );
        let d = controller.observe_job(AdaptiveClass::File, obs(4, 3000, 1000, 0, 900));
        assert_eq!(d, SlowLoopDecision::NoSignal);
        assert!(
            registry.is_empty(),
            "--aimd-disable records nothing in the slow loop"
        );
    }

    #[test]
    fn slow_loop_no_signal_without_a_profile() {
        // A per-run controller built with no profile has nowhere to learn.
        let controller =
            AimdController::from_budget(&profile_budget(8, 8, 8, 8), AimdConfig::default());
        let d = controller.observe_job(AdaptiveClass::File, obs(4, 3000, 1000, 0, 900));
        assert_eq!(d, SlowLoopDecision::NoSignal);
    }

    #[test]
    fn slow_loop_controller_feeds_the_registry_end_to_end() {
        let registry = profile_registry();
        let budget = profile_budget(8, 8, 8, 8);
        let key = file_key("host-a", "acct-1");
        let controller = AimdController::from_budget_for_profile(
            &budget,
            None,
            key.clone(),
            registry.clone(),
            AimdConfig::default(),
        );
        assert_eq!(
            controller.observe_job(AdaptiveClass::File, obs(2, 1000, 1000, 0, 900)),
            SlowLoopDecision::Seeded
        );
        assert_eq!(
            controller.observe_job(AdaptiveClass::File, obs(4, 3000, 1000, 0, 900)),
            SlowLoopDecision::Raised(3)
        );
        assert_eq!(registry.seed(&key, AdaptiveClass::File), Some(3));
    }

    #[test]
    fn observation_is_none_on_a_zero_byte_job() {
        let metrics = super::super::metrics::TransferDagMetrics {
            slot_peak: 4,
            ..Default::default()
        };
        assert!(
            JobThroughputObservation::from_metrics(&metrics, Duration::from_secs(1)).is_none(),
            "a job that moved no bytes yields no slow-loop signal"
        );
    }
}
