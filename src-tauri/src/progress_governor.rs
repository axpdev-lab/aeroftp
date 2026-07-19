// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Shared progress-emission governor for GUI transfer events (DAG-P0-08).
//!
//! Caps progress pressure at ≤10 Hz **per logical transfer or batch lane**,
//! independently of other transfers. Lifecycle events (start / complete /
//! error / cancelled) are never rate-limited; before a terminal event the
//! governor flushes the latest pending sample so the UI always sees the last
//! counters.
//!
//! Design rules:
//! - monotonic time only (`Instant` or an injected clock) — never wall clock;
//! - admit decision under a single mutex so cloned concurrent callbacks cannot
//!   double-claim one 100 ms slot;
//! - emit **outside** the mutex (callers must not hold it while invoking
//!   `app.emit`);
//! - no per-callback sleeping tasks or timers;
//! - late progress after terminal is dropped;
//! - counters never regress within a lane (`transferred` / `total` are
//!   monotonic-max).
//!
//! This module is intentionally independent of Tauri. [`crate::transfer_event_sink::AppHandleSink`]
//! is the production adapter; CLI / MCP / archive progress stay on their own
//! contracts.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::transfer_domain::BatchProgressSnapshot;
use crate::TransferEvent;

/// Default minimum gap between progress emissions for one lane (10 Hz).
pub const DEFAULT_MIN_INTERVAL: Duration = Duration::from_millis(100);

/// Soft cap on retained terminal tombstones; excess tombstones are pruned so
/// the map cannot grow without bound across long sessions.
const TOMBSTONE_PRUNE_THRESHOLD: usize = 8192;

/// Monotonic clock used for cadence. Production uses [`SystemClock`]; tests
/// inject [`ManualClock`].
pub trait MonotonicClock: Send + Sync {
    fn now(&self) -> Instant;
}

/// Production clock: `Instant::now()`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl MonotonicClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Test clock: advance with [`ManualClock::advance`].
#[derive(Debug)]
pub struct ManualClock {
    /// Offset from the epoch instant stored at construction.
    offset_ns: AtomicU64,
    epoch: Instant,
}

impl ManualClock {
    pub fn new() -> Self {
        Self {
            offset_ns: AtomicU64::new(0),
            epoch: Instant::now(),
        }
    }

    pub fn advance(&self, d: Duration) {
        self.offset_ns
            .fetch_add(d.as_nanos() as u64, Ordering::SeqCst);
    }

    pub fn set(&self, d: Duration) {
        self.offset_ns
            .store(d.as_nanos() as u64, Ordering::SeqCst);
    }
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::new()
    }
}

impl MonotonicClock for ManualClock {
    fn now(&self) -> Instant {
        self.epoch + Duration::from_nanos(self.offset_ns.load(Ordering::SeqCst))
    }
}

/// Lane key for a single-file / folder `transfer_event` stream.
pub fn transfer_lane(transfer_id: &str) -> String {
    format!("t:{transfer_id}")
}

/// Lane key for a `transfer_batch_progress` stream.
pub fn batch_lane(batch_id: &str) -> String {
    format!("b:{batch_id}")
}

/// Opaque progress sample held pending or admitted for emission.
///
/// Not `Debug`: [`TransferEvent`] intentionally omits Debug (serde-only GUI
/// payload). Tests assert on counters via [`ProgressSample::transferred_total`].
#[derive(Clone)]
pub enum ProgressSample {
    Transfer(TransferEvent),
    Batch(BatchProgressSnapshot),
}

impl ProgressSample {
    fn transferred_total(&self) -> (u64, u64) {
        match self {
            ProgressSample::Transfer(ev) => match &ev.progress {
                Some(p) => (p.transferred, p.total),
                None => (0, 0),
            },
            ProgressSample::Batch(s) => (s.bytes_transferred, s.bytes_total),
        }
    }

    /// Fingerprint used to skip a final flush that would duplicate the last
    /// emitted counters (and, for transfer samples, the progress payload).
    fn fingerprint(&self) -> SampleFingerprint {
        let (transferred, total) = self.transferred_total();
        match self {
            ProgressSample::Transfer(ev) => SampleFingerprint {
                kind: 0,
                transferred,
                total,
                extra: ev
                    .progress
                    .as_ref()
                    .map(|p| p.percentage as u64)
                    .unwrap_or(0),
            },
            ProgressSample::Batch(s) => SampleFingerprint {
                kind: 1,
                transferred,
                total,
                extra: ((s.completed as u64) << 32)
                    | ((s.failed as u64) << 16)
                    | (s.skipped as u64),
            },
        }
    }

    /// Apply monotonic-max on progress counters so out-of-order multipart
    /// callbacks cannot make the UI go backwards.
    fn apply_monotonic(self, prev_transferred: u64, prev_total: u64) -> Self {
        match self {
            ProgressSample::Transfer(mut ev) => {
                if let Some(ref mut p) = ev.progress {
                    p.transferred = p.transferred.max(prev_transferred);
                    p.total = p.total.max(prev_total);
                    p.percentage = if p.total > 0 {
                        ((p.transferred as f64 / p.total as f64) * 100.0).min(100.0) as u8
                    } else {
                        p.percentage
                    };
                }
                ProgressSample::Transfer(ev)
            }
            ProgressSample::Batch(mut s) => {
                s.bytes_transferred = s.bytes_transferred.max(prev_transferred);
                s.bytes_total = s.bytes_total.max(prev_total);
                ProgressSample::Batch(s)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SampleFingerprint {
    kind: u8,
    transferred: u64,
    total: u64,
    extra: u64,
}

#[derive(Default)]
struct LaneState {
    last_emit_at: Option<Instant>,
    pending: Option<ProgressSample>,
    last_emitted_fp: Option<SampleFingerprint>,
    /// Highest transferred/total observed (emitted or pending).
    high_transferred: u64,
    high_total: u64,
    terminal: bool,
}

/// Thread-safe progress governor. One process-wide instance backs the GUI
/// [`crate::transfer_event_sink::AppHandleSink`]; tests construct their own.
pub struct ProgressGovernor<C: MonotonicClock = SystemClock> {
    lanes: Mutex<HashMap<String, LaneState>>,
    clock: C,
    min_interval: Duration,
}

impl ProgressGovernor<SystemClock> {
    pub fn new() -> Self {
        Self::with_clock(SystemClock, DEFAULT_MIN_INTERVAL)
    }
}

impl Default for ProgressGovernor<SystemClock> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C: MonotonicClock> ProgressGovernor<C> {
    pub fn with_clock(clock: C, min_interval: Duration) -> Self {
        Self {
            lanes: Mutex::new(HashMap::new()),
            clock,
            min_interval,
        }
    }

    pub fn clock(&self) -> &C {
        &self.clock
    }

    pub fn min_interval(&self) -> Duration {
        self.min_interval
    }

    /// Number of lanes currently retained (active + terminal tombstones).
    pub fn lane_count(&self) -> usize {
        self.lanes.lock().expect("progress governor lock").len()
    }

    /// Active (non-terminal) lane count.
    pub fn active_lane_count(&self) -> usize {
        self.lanes
            .lock()
            .expect("progress governor lock")
            .values()
            .filter(|l| !l.terminal)
            .count()
    }

    /// Admit a progress sample for `lane_key`.
    ///
    /// Returns `Some(sample)` when the caller should emit immediately (mutex
    /// is already released). Returns `None` when the sample was coalesced,
    /// the lane is terminal, or the sample was rejected.
    pub fn admit_progress(&self, lane_key: &str, sample: ProgressSample) -> Option<ProgressSample> {
        let now = self.clock.now();
        let mut lanes = self.lanes.lock().expect("progress governor lock");
        let lane = lanes.entry(lane_key.to_string()).or_default();
        if lane.terminal {
            return None;
        }

        let sample = sample.apply_monotonic(lane.high_transferred, lane.high_total);
        let (t, tot) = sample.transferred_total();
        lane.high_transferred = t;
        lane.high_total = tot;

        match lane.last_emit_at {
            None => {
                // First sample for this lane is always immediate.
                Self::mark_emitted(lane, now, &sample);
                Some(sample)
            }
            Some(last) if now.saturating_duration_since(last) >= self.min_interval => {
                Self::mark_emitted(lane, now, &sample);
                Some(sample)
            }
            Some(_) => {
                // Closed interval: keep only the latest sample.
                lane.pending = Some(sample);
                None
            }
        }
    }

    /// Flush any pending sample and mark the lane terminal.
    ///
    /// Returns the pending sample when it differs from the last emitted one
    /// (so a final flush is not duplicated). After this call, further
    /// `admit_progress` for the same key returns `None`. The active payload
    /// is dropped; a compact terminal tombstone remains until pruned.
    pub fn flush_and_close(&self, lane_key: &str) -> Option<ProgressSample> {
        let mut lanes = self.lanes.lock().expect("progress governor lock");
        let lane = lanes.entry(lane_key.to_string()).or_default();
        if lane.terminal {
            return None;
        }
        let pending = lane.pending.take();
        let last_fp = lane.last_emitted_fp;
        // Drop active fields; keep tombstone.
        lane.last_emit_at = None;
        lane.high_transferred = 0;
        lane.high_total = 0;
        lane.last_emitted_fp = None;
        lane.terminal = true;

        let out = pending.and_then(|p| {
            if last_fp == Some(p.fingerprint()) {
                None
            } else {
                Some(p)
            }
        });

        Self::prune_tombstones_if_needed(&mut lanes);
        out
    }

    /// Close a lane without emitting pending (used when the caller already
    /// synthesised a final sample, or for test cleanup). Drops pending.
    pub fn close_without_flush(&self, lane_key: &str) {
        let mut lanes = self.lanes.lock().expect("progress governor lock");
        let lane = lanes.entry(lane_key.to_string()).or_default();
        lane.pending = None;
        lane.last_emit_at = None;
        lane.high_transferred = 0;
        lane.high_total = 0;
        lane.last_emitted_fp = None;
        lane.terminal = true;
        Self::prune_tombstones_if_needed(&mut lanes);
    }

    /// Whether the lane is known-terminal.
    pub fn is_terminal(&self, lane_key: &str) -> bool {
        self.lanes
            .lock()
            .expect("progress governor lock")
            .get(lane_key)
            .map(|l| l.terminal)
            .unwrap_or(false)
    }

    fn mark_emitted(lane: &mut LaneState, now: Instant, sample: &ProgressSample) {
        lane.last_emit_at = Some(now);
        lane.last_emitted_fp = Some(sample.fingerprint());
        lane.pending = None;
    }

    fn prune_tombstones_if_needed(lanes: &mut HashMap<String, LaneState>) {
        let tombstones = lanes.values().filter(|l| l.terminal).count();
        if tombstones <= TOMBSTONE_PRUNE_THRESHOLD {
            return;
        }
        lanes.retain(|_, l| !l.terminal);
    }
}

/// True when a `transfer_event.event_type` carries byte/file progress and
/// must pass through the governor.
pub fn is_progress_event_type(event_type: &str) -> bool {
    event_type == "progress"
}

/// True when a `transfer_event.event_type` ends the logical transfer lane
/// (flush pending, then emit terminal, then reject late progress).
pub fn is_terminal_event_type(event_type: &str) -> bool {
    matches!(
        event_type,
        "complete" | "error" | "cancelled" | "file_complete" | "file_error"
    )
}

/// Route one `TransferEvent` through the governor. Returns the ordered list
/// of events the sink must emit (0..=2). Pure helper for tests and
/// [`crate::transfer_event_sink::AppHandleSink`].
pub fn route_transfer_event(
    governor: &ProgressGovernor<impl MonotonicClock>,
    event: TransferEvent,
) -> Vec<TransferEvent> {
    let key = transfer_lane(&event.transfer_id);
    if is_progress_event_type(&event.event_type) {
        match governor.admit_progress(&key, ProgressSample::Transfer(event)) {
            Some(ProgressSample::Transfer(e)) => vec![e],
            _ => Vec::new(),
        }
    } else if is_terminal_event_type(&event.event_type) {
        let mut out = Vec::with_capacity(2);
        if let Some(ProgressSample::Transfer(p)) = governor.flush_and_close(&key) {
            out.push(p);
        }
        out.push(event);
        out
    } else {
        // start / file_start / scanning / … — immediate, no close.
        vec![event]
    }
}

/// Route one batch progress snapshot. Returns `Some` when it should emit.
pub fn route_batch_progress(
    governor: &ProgressGovernor<impl MonotonicClock>,
    snapshot: BatchProgressSnapshot,
) -> Option<BatchProgressSnapshot> {
    let key = batch_lane(&snapshot.batch_id);
    match governor.admit_progress(&key, ProgressSample::Batch(snapshot)) {
        Some(ProgressSample::Batch(s)) => Some(s),
        _ => None,
    }
}

/// Flush pending batch progress for `batch_id` and close the lane. Call
/// before emitting `transfer_batch_completed`.
pub fn route_batch_terminal_flush(
    governor: &ProgressGovernor<impl MonotonicClock>,
    batch_id: &str,
) -> Option<BatchProgressSnapshot> {
    let key = batch_lane(batch_id);
    match governor.flush_and_close(&key) {
        Some(ProgressSample::Batch(s)) => Some(s),
        _ => None,
    }
}

/// Build a synthetic progress `TransferEvent` for tests.
#[cfg(test)]
pub fn test_progress_event(transfer_id: &str, transferred: u64, total: u64) -> TransferEvent {
    let percentage = if total > 0 {
        ((transferred as f64 / total as f64) * 100.0).min(100.0) as u8
    } else {
        0
    };
    TransferEvent {
        event_type: "progress".to_string(),
        transfer_id: transfer_id.to_string(),
        filename: "f.bin".to_string(),
        direction: "download".to_string(),
        message: None,
        progress: Some(crate::TransferProgress {
            transfer_id: transfer_id.to_string(),
            filename: "f.bin".to_string(),
            transferred,
            total,
            percentage,
            speed_bps: 0,
            eta_seconds: 0,
            direction: "download".to_string(),
            total_files: None,
            path: None,
        }),
        path: None,
        delta_stats: None,
        fallback_reason: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    /// Arc-backed manual clock so tests can advance while the governor holds a clone.
    #[derive(Clone)]
    struct ManualClockHandle(Arc<ManualClock>);

    impl MonotonicClock for ManualClockHandle {
        fn now(&self) -> Instant {
            self.0.now()
        }
    }

    fn gov_handle() -> (Arc<ManualClock>, ProgressGovernor<ManualClockHandle>) {
        let clock = Arc::new(ManualClock::new());
        let g = ProgressGovernor::with_clock(ManualClockHandle(Arc::clone(&clock)), DEFAULT_MIN_INTERVAL);
        (clock, g)
    }

    #[test]
    fn first_sample_is_immediate() {
        let (_clock, g) = gov_handle();
        let key = transfer_lane("t1");
        let out = g.admit_progress(&key, ProgressSample::Transfer(test_progress_event("t1", 1, 100)));
        assert!(out.is_some());
    }

    #[test]
    fn burst_emits_at_most_one_per_interval() {
        let (clock, g) = gov_handle();
        let key = transfer_lane("t1");
        let mut emitted = 0u32;
        for i in 1..=20 {
            if g
                .admit_progress(
                    &key,
                    ProgressSample::Transfer(test_progress_event("t1", i * 10, 1000)),
                )
                .is_some()
            {
                emitted += 1;
            }
        }
        // First immediate; rest within the same 0 ms window coalesce.
        assert_eq!(emitted, 1);

        clock.advance(Duration::from_millis(100));
        assert!(g
            .admit_progress(
                &key,
                ProgressSample::Transfer(test_progress_event("t1", 500, 1000)),
            )
            .is_some());
        emitted = 0;
        for i in 1..=10 {
            if g
                .admit_progress(
                    &key,
                    ProgressSample::Transfer(test_progress_event("t1", 500 + i, 1000)),
                )
                .is_some()
            {
                emitted += 1;
            }
        }
        assert_eq!(emitted, 0, "second burst inside the closed window must coalesce");
    }

    #[test]
    fn two_transfer_ids_are_independent() {
        let (_clock, g) = gov_handle();
        let a = transfer_lane("a");
        let b = transfer_lane("b");
        assert!(g
            .admit_progress(&a, ProgressSample::Transfer(test_progress_event("a", 1, 10)))
            .is_some());
        assert!(g
            .admit_progress(&b, ProgressSample::Transfer(test_progress_event("b", 1, 10)))
            .is_some());
        // Same instant: each lane still had its first sample.
        assert!(g
            .admit_progress(&a, ProgressSample::Transfer(test_progress_event("a", 2, 10)))
            .is_none());
        assert!(g
            .admit_progress(&b, ProgressSample::Transfer(test_progress_event("b", 2, 10)))
            .is_none());
    }

    #[test]
    fn concurrent_callbacks_cannot_double_emit_one_slot() {
        let (clock, g) = gov_handle();
        let g = Arc::new(g);
        let key = transfer_lane("race");
        // Seed first emission so the window is closed.
        assert!(g
            .admit_progress(
                &key,
                ProgressSample::Transfer(test_progress_event("race", 1, 1000)),
            )
            .is_some());
        clock.advance(Duration::from_millis(100));

        let emits = Arc::new(AtomicU64::new(0));
        let mut handles = Vec::new();
        for i in 0..8 {
            let g = Arc::clone(&g);
            let emits = Arc::clone(&emits);
            handles.push(thread::spawn(move || {
                if g
                    .admit_progress(
                        &transfer_lane("race"),
                        ProgressSample::Transfer(test_progress_event("race", 100 + i, 1000)),
                    )
                    .is_some()
                {
                    emits.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(
            emits.load(Ordering::SeqCst),
            1,
            "exactly one concurrent admit may claim the open slot"
        );
    }

    #[test]
    fn intermediate_samples_coalesce_to_latest() {
        let (_clock, g) = gov_handle();
        let key = transfer_lane("c");
        assert!(g
            .admit_progress(&key, ProgressSample::Transfer(test_progress_event("c", 1, 100)))
            .is_some());
        assert!(g
            .admit_progress(&key, ProgressSample::Transfer(test_progress_event("c", 10, 100)))
            .is_none());
        assert!(g
            .admit_progress(&key, ProgressSample::Transfer(test_progress_event("c", 50, 100)))
            .is_none());
        let flushed = g.flush_and_close(&key);
        match flushed {
            Some(ProgressSample::Transfer(ev)) => {
                assert_eq!(ev.progress.as_ref().unwrap().transferred, 50);
            }
            Some(ProgressSample::Batch(s)) => {
                panic!("expected transfer sample, got batch {:?}", s.bytes_transferred)
            }
            None => panic!("expected coalesced pending sample"),
        }
    }

    #[test]
    fn final_flush_emits_latest_once_and_not_duplicate() {
        let (clock, g) = gov_handle();
        let key = transfer_lane("f");
        let first = g
            .admit_progress(
                &key,
                ProgressSample::Transfer(test_progress_event("f", 100, 100)),
            )
            .expect("first");
        assert_eq!(
            first.transferred_total(),
            (100, 100)
        );
        // No further samples; flush has nothing pending → no duplicate 100%.
        assert!(g.flush_and_close(&key).is_none());

        let key2 = transfer_lane("f2");
        assert!(g
            .admit_progress(
                &key2,
                ProgressSample::Transfer(test_progress_event("f2", 10, 100)),
            )
            .is_some());
        clock.advance(Duration::from_millis(10));
        assert!(g
            .admit_progress(
                &key2,
                ProgressSample::Transfer(test_progress_event("f2", 100, 100)),
            )
            .is_none());
        let flushed = g.flush_and_close(&key2).expect("pending final");
        assert_eq!(flushed.transferred_total(), (100, 100));
    }

    #[test]
    fn terminal_route_orders_flush_before_complete() {
        let (_clock, g) = gov_handle();
        let mut events = route_transfer_event(&g, test_progress_event("x", 1, 100));
        assert_eq!(events.len(), 1);
        assert!(route_transfer_event(&g, test_progress_event("x", 40, 100)).is_empty());
        events = route_transfer_event(
            &g,
            TransferEvent {
                event_type: "complete".to_string(),
                transfer_id: "x".to_string(),
                filename: "f.bin".to_string(),
                direction: "download".to_string(),
                message: Some("done".into()),
                progress: None,
                path: None,
                delta_stats: None,
                fallback_reason: None,
            },
        );
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, "progress");
        assert_eq!(events[0].progress.as_ref().unwrap().transferred, 40);
        assert_eq!(events[1].event_type, "complete");
    }

    #[test]
    fn error_and_cancel_are_immediate_after_flush() {
        let (_clock, g) = gov_handle();
        let _ = route_transfer_event(&g, test_progress_event("e", 1, 100));
        let _ = route_transfer_event(&g, test_progress_event("e", 20, 100));
        let events = route_transfer_event(
            &g,
            TransferEvent {
                event_type: "error".to_string(),
                transfer_id: "e".to_string(),
                filename: "f.bin".to_string(),
                direction: "download".to_string(),
                message: Some("boom".into()),
                progress: None,
                path: None,
                delta_stats: None,
                fallback_reason: None,
            },
        );
        assert_eq!(events[0].event_type, "progress");
        assert_eq!(events[0].progress.as_ref().unwrap().transferred, 20);
        assert_ne!(
            events[0].progress.as_ref().unwrap().percentage,
            100,
            "error must not fabricate 100%"
        );
        assert_eq!(events[1].event_type, "error");

        let (_c2, g2) = gov_handle();
        let _ = route_transfer_event(&g2, test_progress_event("c", 1, 50));
        let _ = route_transfer_event(&g2, test_progress_event("c", 10, 50));
        let events = route_transfer_event(
            &g2,
            TransferEvent {
                event_type: "cancelled".to_string(),
                transfer_id: "c".to_string(),
                filename: "f.bin".to_string(),
                direction: "download".to_string(),
                message: Some("stop".into()),
                progress: None,
                path: None,
                delta_stats: None,
                fallback_reason: None,
            },
        );
        assert_eq!(events[0].event_type, "progress");
        assert!(events[0].progress.as_ref().unwrap().percentage < 100);
        assert_eq!(events[1].event_type, "cancelled");
    }

    #[test]
    fn late_progress_after_terminal_is_dropped() {
        let (_clock, g) = gov_handle();
        let _ = route_transfer_event(&g, test_progress_event("z", 1, 10));
        let _ = route_transfer_event(
            &g,
            TransferEvent {
                event_type: "complete".to_string(),
                transfer_id: "z".to_string(),
                filename: "f.bin".to_string(),
                direction: "download".to_string(),
                message: None,
                progress: None,
                path: None,
                delta_stats: None,
                fallback_reason: None,
            },
        );
        assert!(route_transfer_event(&g, test_progress_event("z", 10, 10)).is_empty());
        assert!(g.is_terminal(&transfer_lane("z")));
    }

    #[test]
    fn failed_transfer_does_not_fabricate_full_progress() {
        let (_clock, g) = gov_handle();
        let _ = route_transfer_event(&g, test_progress_event("bad", 5, 1000));
        let events = route_transfer_event(
            &g,
            TransferEvent {
                event_type: "error".to_string(),
                transfer_id: "bad".to_string(),
                filename: "f.bin".to_string(),
                direction: "upload".to_string(),
                message: Some("fail".into()),
                progress: None,
                path: None,
                delta_stats: None,
                fallback_reason: None,
            },
        );
        // Only terminal (no pending beyond first which was already emitted).
        // First was emitted; second never came; flush has nothing if nothing pending.
        // We did only one progress then error — flush empty.
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "error");
        assert!(events[0].progress.is_none());
    }

    #[test]
    fn state_removed_payload_and_tombstones_bounded() {
        let (_clock, g) = gov_handle();
        for i in 0..100 {
            let id = format!("id-{i}");
            let _ = route_transfer_event(&g, test_progress_event(&id, 1, 10));
            let _ = route_transfer_event(&g, test_progress_event(&id, 5, 10));
            let _ = route_transfer_event(
                &g,
                TransferEvent {
                    event_type: "complete".to_string(),
                    transfer_id: id,
                    filename: "f.bin".to_string(),
                    direction: "download".to_string(),
                    message: None,
                    progress: None,
                    path: None,
                    delta_stats: None,
                    fallback_reason: None,
                },
            );
        }
        assert_eq!(g.active_lane_count(), 0);
        // Tombstones remain but active payload is gone.
        assert!(g.lane_count() <= 100);
        // Force prune path by filling past threshold with closed lanes.
        for i in 0..(TOMBSTONE_PRUNE_THRESHOLD + 10) {
            let id = format!("prune-{i}");
            g.close_without_flush(&transfer_lane(&id));
        }
        assert!(
            g.lane_count() <= TOMBSTONE_PRUNE_THRESHOLD + 100,
            "tombstones must prune under pressure, count={}",
            g.lane_count()
        );
    }

    #[test]
    fn monotonic_counters_reject_regression() {
        let (_clock, g) = gov_handle();
        let key = transfer_lane("mono");
        let e1 = g
            .admit_progress(
                &key,
                ProgressSample::Transfer(test_progress_event("mono", 50, 100)),
            )
            .unwrap();
        assert_eq!(e1.transferred_total(), (50, 100));
        // Out-of-order lower sample is raised to high-water (50); a later
        // higher sample then wins the pending slot at 70.
        assert!(g
            .admit_progress(
                &key,
                ProgressSample::Transfer(test_progress_event("mono", 10, 100)),
            )
            .is_none());
        assert!(g
            .admit_progress(
                &key,
                ProgressSample::Transfer(test_progress_event("mono", 70, 100)),
            )
            .is_none());
        let flushed = g.flush_and_close(&key).unwrap();
        assert_eq!(flushed.transferred_total().0, 70);
        // A pure regression that only restates the last emitted counters is
        // not re-emitted on flush (dedup).
        let key2 = transfer_lane("mono2");
        assert!(g
            .admit_progress(
                &key2,
                ProgressSample::Transfer(test_progress_event("mono2", 40, 100)),
            )
            .is_some());
        assert!(g
            .admit_progress(
                &key2,
                ProgressSample::Transfer(test_progress_event("mono2", 5, 100)),
            )
            .is_none());
        assert!(
            g.flush_and_close(&key2).is_none(),
            "pending equal to last emitted after mono-max must not duplicate"
        );
    }

    #[test]
    fn batch_lane_independent_of_transfer_lane() {
        let (_clock, g) = gov_handle();
        let snap = BatchProgressSnapshot {
            batch_id: "batch-1".into(),
            completed: 1,
            skipped: 0,
            failed: 0,
            active: 0,
            total: 10,
            bytes_transferred: 100,
            bytes_total: 1000,
        };
        assert!(route_batch_progress(&g, snap.clone()).is_some());
        assert!(route_batch_progress(
            &g,
            BatchProgressSnapshot {
                completed: 2,
                bytes_transferred: 200,
                ..snap.clone()
            }
        )
        .is_none());
        // Transfer lane still free for first sample.
        assert!(route_transfer_event(&g, test_progress_event("batch-1", 1, 10)).len() == 1);
        let flushed = route_batch_terminal_flush(&g, "batch-1").unwrap();
        assert_eq!(flushed.completed, 2);
        assert_eq!(flushed.bytes_transferred, 200);
    }

    #[test]
    fn start_lifecycle_is_not_throttled_or_closed() {
        let (_clock, g) = gov_handle();
        let start = TransferEvent {
            event_type: "start".to_string(),
            transfer_id: "s".into(),
            filename: "f".into(),
            direction: "download".into(),
            message: None,
            progress: None,
            path: None,
            delta_stats: None,
            fallback_reason: None,
        };
        assert_eq!(route_transfer_event(&g, start).len(), 1);
        assert!(!g.is_terminal(&transfer_lane("s")));
        assert_eq!(
            route_transfer_event(&g, test_progress_event("s", 1, 10)).len(),
            1
        );
    }
}
