// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Prudent AIMD backpressure for the ready-frontier executor.
//!
//! Three pieces:
//!
//! 1. [`congestion_from_error`] is a *thin* mapping layered on top of the
//!    existing [`crate::sync::classify_sync_error`] classifier. It is not a
//!    new classifier and changes no provider signature: it only recognises
//!    the narrow congestion trigger set (429, 503, request timeout,
//!    server-side max-connections, connection reset). Generic 5xx is
//!    deliberately excluded.
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
//!    ever reduce below it and grow back toward it. State is per-run; no
//!    cross-run persistence (out of scope for v1).
//!
//! The controller throttles the executor's *dispatch* step. A node parked
//! waiting for a dispatch permit has not begun its transfer yet, so shrinking
//! is always safe.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::resources::{ResourceRequest, TransferBudget};

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

/// Map a raw error string to a [`CongestionEvent`], or `None` if it is not a
/// congestion signal. Layered on top of [`crate::sync::classify_sync_error`]
/// for the cases it already isolates (rate limit, timeout, connection reset),
/// plus two explicit substring checks for signals the shared classifier folds
/// into broader buckets (503, max-connections). A "504 Gateway Timeout" maps
/// to [`CongestionEvent::Timeout`] because it is a timeout, which is in the
/// D2 set; a plain server error with no load/timeout/availability semantics
/// (500, 502) returns `None`: only the D2 set throttles, never generic 5xx.
pub fn congestion_from_error(raw: &str) -> Option<CongestionEvent> {
    let lower = raw.to_lowercase();

    // Signals the shared classifier does not single out.
    if lower.contains("503") || lower.contains("service unavailable") {
        return Some(CongestionEvent::ServiceUnavailable);
    }
    if lower.contains("too many connections")
        || lower.contains("max connections")
        || lower.contains("maximum number of connections")
        || lower.contains("421 ")
    {
        return Some(CongestionEvent::MaxConnections);
    }

    // Reuse the shared classifier for the rest. We only act on the subset
    // that is genuinely congestion; everything else is left to normal error
    // handling (no throttling).
    let info = crate::sync::classify_sync_error(raw, None);
    match info.kind {
        crate::sync::SyncErrorKind::RateLimit => Some(CongestionEvent::TooManyRequests),
        crate::sync::SyncErrorKind::Timeout => Some(CongestionEvent::Timeout),
        crate::sync::SyncErrorKind::Network if lower.contains("reset") => {
            Some(CongestionEvent::ConnectionReset)
        }
        _ => None,
    }
}

/// Marker prefix for embedding a `Retry-After` hint inside a `ProviderError`
/// message string. The call chain provider → executor doesn't carry typed
/// metadata, so per-provider helpers (`parse_retry_after_drive` /
/// `_dropbox` / `_onedrive` / `_box`) append the marker when they emit the
/// error, and [`parse_embedded_retry_after`] consumes it in the executor.
///
/// Format: ` [retry-after-secs=NN]` appended to the error text, where `NN`
/// is a non-negative integer of seconds. Sub-second hints round up at the
/// provider site to avoid defeating the AIMD throttle, matching
/// [`AimdController::MIN_HINT_COOLDOWN`]. T-DEBT-05.
const RETRY_HINT_MARKER: &str = " [retry-after-secs=";

/// Build the marker substring to append to a `ProviderError` message after a
/// rate-limit response. Pair with [`parse_embedded_retry_after`] in the
/// executor to recover the value without changing `ProviderError`'s shape.
pub fn embed_retry_after_marker(seconds: u64) -> String {
    format!("{RETRY_HINT_MARKER}{seconds}]")
}

/// Extract a `Retry-After` hint that a provider embedded in the error
/// message via [`embed_retry_after_marker`]. Returns `None` when the marker
/// is absent, malformed, or carries a value that does not parse as a u64.
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
        // If a shrink is still owed, reclaim one freed slot now.
        if self.deficit.load(Ordering::SeqCst) > 0 {
            if let Ok(p) = self.sem.try_acquire() {
                p.forget();
                self.deficit.fetch_sub(1, Ordering::SeqCst);
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
            // Pay down any owed shrink before adding real permits.
            let owed = self.deficit.load(Ordering::SeqCst);
            let pay = need.min(owed);
            if pay > 0 {
                self.deficit.fetch_sub(pay, Ordering::SeqCst);
                need -= pay;
            }
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
#[derive(Debug, Clone, Copy)]
pub struct AimdConfig {
    pub cooldown: Duration,
    pub healthy_window: Duration,
    pub recovery_window: Duration,
}

impl Default for AimdConfig {
    fn default() -> Self {
        Self {
            cooldown: Duration::from_secs(5),
            healthy_window: Duration::from_secs(5),
            // Long enough that a provider's rate-limit window has demonstrably
            // cleared before AIMD risks the level that congested it again.
            recovery_window: Duration::from_secs(30),
        }
    }
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
}

impl ClassState {
    fn new(ceiling: usize) -> Self {
        let ceiling = ceiling.max(1);
        Self {
            // Start at the honest ceiling: AIMD is decrease-biased and only
            // ever reduces below the effective budget, never above it.
            sem: DynamicSemaphore::new(ceiling, ceiling),
            ceiling,
            target: ceiling,
            cooldown_until: None,
            healthy_since: None,
            regrowth_cap: ceiling,
            last_congestion: None,
        }
    }
}

/// Decrease-biased AIMD over the four controlled classes. Never grants more
/// than the per-class effective budget; halves on congestion with a cooldown;
/// grows back by one after a quiet window.
pub struct AimdController {
    config: AimdConfig,
    file: Mutex<ClassState>,
    chunk: Mutex<ClassState>,
    http: Mutex<ClassState>,
    api: Mutex<ClassState>,
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
        Self {
            config,
            file: Mutex::new(ClassState::new(file_ceiling)),
            chunk: Mutex::new(ClassState::new(chunk_ceiling)),
            http: Mutex::new(ClassState::new(http_ceiling)),
            api: Mutex::new(ClassState::new(api_ceiling)),
        }
    }

    /// Build a controller whose per-class ceilings are the honest effective
    /// budget (F3-T05). Every class starts at its ceiling — AIMD is
    /// decrease-biased — so a run with no congestion dispatches exactly as if
    /// no controller were wired.
    pub fn from_budget(budget: &TransferBudget, config: AimdConfig) -> Self {
        Self::new(
            budget.file_slots.max(1) as usize,
            budget.chunk_slots.max(1) as usize,
            budget.http_slots.max(1) as usize,
            budget.api_slots.max(1) as usize,
            config,
        )
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
    pub fn on_congestion_with_hint(
        &self,
        class: AdaptiveClass,
        cooldown_hint: Option<Duration>,
    ) {
        let now = Instant::now();
        let effective_cooldown = match cooldown_hint {
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
        st.target = (st.target / 2).max(1);
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
    }

    /// Note a healthy completion. After a quiet `healthy_window` with no
    /// congestion and no active cooldown, additively increase by one (never
    /// above the ceiling, and never above the [`AimdConfig::recovery_window`]
    /// guard band until the band has relaxed).
    pub fn note_healthy(&self, class: AdaptiveClass) {
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
                    st.target += 1;
                    st.sem.set_live(st.target);
                    st.healthy_since = Some(now);
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn congestion_mapping_covers_the_trigger_set_only() {
        assert_eq!(
            congestion_from_error("HTTP 429 Too Many Requests"),
            Some(CongestionEvent::TooManyRequests)
        );
        assert_eq!(
            congestion_from_error("rate limit exceeded"),
            Some(CongestionEvent::TooManyRequests)
        );
        assert_eq!(
            congestion_from_error("503 Service Unavailable"),
            Some(CongestionEvent::ServiceUnavailable)
        );
        assert_eq!(
            congestion_from_error("operation timed out after 30s"),
            Some(CongestionEvent::Timeout)
        );
        assert_eq!(
            congestion_from_error("421 too many connections from your IP"),
            Some(CongestionEvent::MaxConnections)
        );
        assert_eq!(
            congestion_from_error("connection reset by peer"),
            Some(CongestionEvent::ConnectionReset)
        );
        // 504 is a timeout, which is in the D2 set.
        assert_eq!(
            congestion_from_error("504 Gateway Timeout"),
            Some(CongestionEvent::Timeout)
        );
        // Plain server errors with no load/timeout semantics must NOT throttle.
        assert_eq!(congestion_from_error("500 Internal Server Error"), None);
        assert_eq!(congestion_from_error("502 Bad Gateway"), None);
        assert_eq!(congestion_from_error("404 not found"), None);
        assert_eq!(congestion_from_error("permission denied"), None);
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
        };

        // Lower clamp: a 200 ms hint must be raised to MIN_HINT_COOLDOWN
        // (1 s) so the throttle window is not effectively zero.
        let ctrl_lo = AimdController::new(8, 1, 1, 1, cfg);
        let before_lo = Instant::now();
        ctrl_lo.on_congestion_with_hint(AdaptiveClass::File, Some(Duration::from_millis(200)));
        let until_lo = ctrl_lo.cooldown_until(AdaptiveClass::File).unwrap();
        let elapsed_lo = until_lo.saturating_duration_since(before_lo);
        assert!(
            elapsed_lo >= Duration::from_millis(900)
                && elapsed_lo <= Duration::from_millis(1100),
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
}
