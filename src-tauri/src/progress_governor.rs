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
//! - only an explicit lifecycle start opens a lane, so late progress after
//!   terminal is dropped without retaining unbounded tombstones;
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
        self.offset_ns.store(d.as_nanos() as u64, Ordering::SeqCst);
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
// This value is created for every byte-progress callback. Keeping the
// TransferEvent inline avoids an additional heap allocation on the hot path;
// only one pending value per active lane is retained.
#[allow(clippy::large_enum_variant)]
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

    /// Full serialized payload used to skip only an exact duplicate on final
    /// flush. Counter-only fingerprints lose speed/ETA/path and can therefore
    /// suppress a materially newer sample.
    fn fingerprint(&self) -> Option<Vec<u8>> {
        match self {
            ProgressSample::Transfer(ev) => serde_json::to_vec(&(0u8, ev)).ok(),
            ProgressSample::Batch(s) => serde_json::to_vec(&(1u8, s)).ok(),
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

#[derive(Default)]
struct LaneState {
    last_emit_at: Option<Instant>,
    pending: Option<ProgressSample>,
    last_emitted_fp: Option<Vec<u8>>,
    /// Highest transferred/total observed (emitted or pending).
    high_transferred: u64,
    high_total: u64,
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

    /// Number of currently active lanes.
    pub fn lane_count(&self) -> usize {
        self.lanes.lock().expect("progress governor lock").len()
    }

    /// Active lane count.
    pub fn active_lane_count(&self) -> usize {
        self.lane_count()
    }

    /// Open (or explicitly restart) a progress lane.
    ///
    /// Progress never creates a lane implicitly: this lifecycle boundary is
    /// what lets terminal close remove all state while still rejecting every
    /// late callback.
    pub fn begin_lane(&self, lane_key: &str) {
        self.lanes
            .lock()
            .expect("progress governor lock")
            .insert(lane_key.to_string(), LaneState::default());
    }

    /// Admit a progress sample for `lane_key`.
    ///
    /// Returns `Some(sample)` when the caller should emit immediately (mutex
    /// is already released). Returns `None` when the sample was coalesced,
    /// the lane is unknown/closed, or the sample was rejected.
    pub fn admit_progress(&self, lane_key: &str, sample: ProgressSample) -> Option<ProgressSample> {
        let now = self.clock.now();
        let mut lanes = self.lanes.lock().expect("progress governor lock");
        let lane = lanes.get_mut(lane_key)?;

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

    /// Flush any pending sample and close the lane.
    ///
    /// Returns the pending sample when it differs from the last emitted one
    /// (so a final flush is not duplicated). After this call, further
    /// `admit_progress` for the same key returns `None` until a new explicit
    /// lifecycle start reopens it. No terminal tombstone is retained.
    pub fn flush_and_close(&self, lane_key: &str) -> Option<ProgressSample> {
        let mut lanes = self.lanes.lock().expect("progress governor lock");
        let mut lane = lanes.remove(lane_key)?;
        let pending = lane.pending.take();
        let last_fp = lane.last_emitted_fp.take();

        pending.and_then(|p| {
            let pending_fp = p.fingerprint();
            if pending_fp.is_some() && last_fp == pending_fp {
                None
            } else {
                Some(p)
            }
        })
    }

    /// Close a lane without emitting pending (used when the caller already
    /// synthesised a final sample, or for test cleanup). Drops pending.
    pub fn close_without_flush(&self, lane_key: &str) {
        self.lanes
            .lock()
            .expect("progress governor lock")
            .remove(lane_key);
    }

    /// Whether a lifecycle start currently owns this lane.
    pub fn is_active(&self, lane_key: &str) -> bool {
        self.lanes
            .lock()
            .expect("progress governor lock")
            .contains_key(lane_key)
    }

    fn mark_emitted(lane: &mut LaneState, now: Instant, sample: &ProgressSample) {
        lane.last_emit_at = Some(now);
        lane.last_emitted_fp = sample.fingerprint();
        lane.pending = None;
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

fn is_cross_profile(event: &TransferEvent) -> bool {
    event.direction == "cross-profile"
}

fn opens_transfer_lane(event: &TransferEvent) -> bool {
    event.event_type == "start" || (event.event_type == "file_start" && !is_cross_profile(event))
}

fn closes_transfer_lane(event: &TransferEvent) -> bool {
    matches!(
        event.event_type.as_str(),
        "complete" | "error" | "cancelled"
    ) || (matches!(event.event_type.as_str(), "file_complete" | "file_error")
        && !is_cross_profile(event))
}

/// Route one `TransferEvent` through the governor. Returns the ordered list
/// of events the sink must emit (0..=2). Pure helper for tests and
/// [`crate::transfer_event_sink::AppHandleSink`].
pub fn route_transfer_event(
    governor: &ProgressGovernor<impl MonotonicClock>,
    event: TransferEvent,
) -> Vec<TransferEvent> {
    let key = transfer_lane(&event.transfer_id);
    if opens_transfer_lane(&event) {
        governor.begin_lane(&key);
        vec![event]
    } else if is_progress_event_type(&event.event_type) {
        match governor.admit_progress(&key, ProgressSample::Transfer(event)) {
            Some(ProgressSample::Transfer(e)) => vec![e],
            _ => Vec::new(),
        }
    } else if closes_transfer_lane(&event) {
        let mut out = Vec::with_capacity(2);
        if let Some(ProgressSample::Transfer(p)) = governor.flush_and_close(&key) {
            out.push(p);
        }
        out.push(event);
        out
    } else {
        // Cross-profile file lifecycle and scanning events are immediate but
        // do not reopen/close the aggregate transfer lane.
        vec![event]
    }
}

/// Open a batch lane from `transfer_batch_started`.
pub fn route_batch_started(governor: &ProgressGovernor<impl MonotonicClock>, batch_id: &str) {
    if !batch_id.is_empty() {
        governor.begin_lane(&batch_lane(batch_id));
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
        let g = ProgressGovernor::with_clock(
            ManualClockHandle(Arc::clone(&clock)),
            DEFAULT_MIN_INTERVAL,
        );
        (clock, g)
    }

    fn start_event(transfer_id: &str, direction: &str) -> TransferEvent {
        TransferEvent {
            event_type: "start".to_string(),
            transfer_id: transfer_id.to_string(),
            filename: "f.bin".to_string(),
            direction: direction.to_string(),
            message: None,
            progress: None,
            path: None,
            delta_stats: None,
            fallback_reason: None,
        }
    }

    fn open_transfer(g: &ProgressGovernor<ManualClockHandle>, transfer_id: &str) {
        assert_eq!(
            route_transfer_event(g, start_event(transfer_id, "download")).len(),
            1
        );
    }

    #[test]
    fn first_sample_is_immediate() {
        let (_clock, g) = gov_handle();
        let key = transfer_lane("t1");
        g.begin_lane(&key);
        let out = g.admit_progress(
            &key,
            ProgressSample::Transfer(test_progress_event("t1", 1, 100)),
        );
        assert!(out.is_some());
    }

    #[test]
    fn burst_emits_at_most_one_per_interval() {
        let (clock, g) = gov_handle();
        let key = transfer_lane("t1");
        g.begin_lane(&key);
        let mut emitted = 0u32;
        for i in 1..=20 {
            if g.admit_progress(
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
            if g.admit_progress(
                &key,
                ProgressSample::Transfer(test_progress_event("t1", 500 + i, 1000)),
            )
            .is_some()
            {
                emitted += 1;
            }
        }
        assert_eq!(
            emitted, 0,
            "second burst inside the closed window must coalesce"
        );
    }

    #[test]
    fn two_transfer_ids_are_independent() {
        let (_clock, g) = gov_handle();
        let a = transfer_lane("a");
        let b = transfer_lane("b");
        g.begin_lane(&a);
        g.begin_lane(&b);
        assert!(g
            .admit_progress(
                &a,
                ProgressSample::Transfer(test_progress_event("a", 1, 10))
            )
            .is_some());
        assert!(g
            .admit_progress(
                &b,
                ProgressSample::Transfer(test_progress_event("b", 1, 10))
            )
            .is_some());
        // Same instant: each lane still had its first sample.
        assert!(g
            .admit_progress(
                &a,
                ProgressSample::Transfer(test_progress_event("a", 2, 10))
            )
            .is_none());
        assert!(g
            .admit_progress(
                &b,
                ProgressSample::Transfer(test_progress_event("b", 2, 10))
            )
            .is_none());
    }

    #[test]
    fn concurrent_callbacks_cannot_double_emit_one_slot() {
        let (clock, g) = gov_handle();
        let g = Arc::new(g);
        let key = transfer_lane("race");
        g.begin_lane(&key);
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
                if g.admit_progress(
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
        g.begin_lane(&key);
        assert!(g
            .admit_progress(
                &key,
                ProgressSample::Transfer(test_progress_event("c", 1, 100))
            )
            .is_some());
        assert!(g
            .admit_progress(
                &key,
                ProgressSample::Transfer(test_progress_event("c", 10, 100))
            )
            .is_none());
        assert!(g
            .admit_progress(
                &key,
                ProgressSample::Transfer(test_progress_event("c", 50, 100))
            )
            .is_none());
        let flushed = g.flush_and_close(&key);
        match flushed {
            Some(ProgressSample::Transfer(ev)) => {
                assert_eq!(ev.progress.as_ref().unwrap().transferred, 50);
            }
            Some(ProgressSample::Batch(s)) => {
                panic!(
                    "expected transfer sample, got batch {:?}",
                    s.bytes_transferred
                )
            }
            None => panic!("expected coalesced pending sample"),
        }
    }

    #[test]
    fn final_flush_emits_latest_once_and_not_duplicate() {
        let (clock, g) = gov_handle();
        let key = transfer_lane("f");
        g.begin_lane(&key);
        let first = g
            .admit_progress(
                &key,
                ProgressSample::Transfer(test_progress_event("f", 100, 100)),
            )
            .expect("first");
        assert_eq!(first.transferred_total(), (100, 100));
        // No further samples; flush has nothing pending → no duplicate 100%.
        assert!(g.flush_and_close(&key).is_none());

        let key2 = transfer_lane("f2");
        g.begin_lane(&key2);
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
        open_transfer(&g, "x");
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
        open_transfer(&g, "e");
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
        open_transfer(&g2, "c");
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
        open_transfer(&g, "z");
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
        assert!(!g.is_active(&transfer_lane("z")));
    }

    #[test]
    fn failed_transfer_does_not_fabricate_full_progress() {
        let (_clock, g) = gov_handle();
        open_transfer(&g, "bad");
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
    fn terminal_removes_state_and_rejects_late_progress_without_tombstones() {
        let (_clock, g) = gov_handle();
        for i in 0..10_000 {
            let id = format!("id-{i}");
            open_transfer(&g, &id);
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
            assert!(
                route_transfer_event(&g, test_progress_event(&format!("id-{i}"), 9, 10)).is_empty(),
                "late progress for {i} must stay rejected"
            );
        }
        assert_eq!(g.active_lane_count(), 0);
        assert_eq!(g.lane_count(), 0);
    }

    #[test]
    fn monotonic_counters_reject_regression() {
        let (_clock, g) = gov_handle();
        let key = transfer_lane("mono");
        g.begin_lane(&key);
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
        g.begin_lane(&key2);
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
        route_batch_started(&g, "batch-1");
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
        open_transfer(&g, "batch-1");
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
        assert!(g.is_active(&transfer_lane("s")));
        assert_eq!(
            route_transfer_event(&g, test_progress_event("s", 1, 10)).len(),
            1
        );
    }

    #[test]
    fn explicit_restart_reopens_a_closed_identifier() {
        let (_clock, g) = gov_handle();
        open_transfer(&g, "reuse");
        assert_eq!(
            route_transfer_event(&g, test_progress_event("reuse", 1, 10)).len(),
            1
        );
        let _ = route_transfer_event(
            &g,
            TransferEvent {
                event_type: "complete".to_string(),
                transfer_id: "reuse".to_string(),
                filename: "f.bin".to_string(),
                direction: "download".to_string(),
                message: None,
                progress: None,
                path: None,
                delta_stats: None,
                fallback_reason: None,
            },
        );
        assert!(route_transfer_event(&g, test_progress_event("reuse", 2, 10)).is_empty());

        open_transfer(&g, "reuse");
        assert_eq!(
            route_transfer_event(&g, test_progress_event("reuse", 1, 10)).len(),
            1
        );
    }

    #[test]
    fn final_dedup_compares_the_full_transfer_payload() {
        let (_clock, g) = gov_handle();
        let key = transfer_lane("payload");
        g.begin_lane(&key);
        let mut first = test_progress_event("payload", 10, 100);
        first.progress.as_mut().unwrap().speed_bps = 10;
        assert!(g
            .admit_progress(&key, ProgressSample::Transfer(first))
            .is_some());

        let mut latest = test_progress_event("payload", 10, 100);
        latest.progress.as_mut().unwrap().speed_bps = 20;
        assert!(g
            .admit_progress(&key, ProgressSample::Transfer(latest))
            .is_none());

        match g.flush_and_close(&key) {
            Some(ProgressSample::Transfer(event)) => {
                assert_eq!(event.progress.unwrap().speed_bps, 20);
            }
            _ => panic!("changed speed/ETA metadata must not be deduplicated"),
        }
    }

    #[test]
    fn cross_profile_file_terminals_do_not_close_the_aggregate_lane() {
        let (_clock, g) = gov_handle();
        assert_eq!(
            route_transfer_event(&g, start_event("cross", "cross-profile")).len(),
            1
        );

        let file_complete = TransferEvent {
            event_type: "file_complete".to_string(),
            transfer_id: "cross".to_string(),
            filename: "one.bin".to_string(),
            direction: "cross-profile".to_string(),
            message: None,
            progress: None,
            path: Some("/one.bin".to_string()),
            delta_stats: None,
            fallback_reason: None,
        };
        assert_eq!(route_transfer_event(&g, file_complete).len(), 1);
        assert!(g.is_active(&transfer_lane("cross")));

        let mut progress = test_progress_event("cross", 1, 2);
        progress.direction = "cross-profile".to_string();
        progress.progress.as_mut().unwrap().direction = "cross-profile".to_string();
        assert_eq!(route_transfer_event(&g, progress).len(), 1);

        let complete = TransferEvent {
            event_type: "complete".to_string(),
            transfer_id: "cross".to_string(),
            filename: "2 file(s)".to_string(),
            direction: "cross-profile".to_string(),
            message: None,
            progress: None,
            path: None,
            delta_stats: None,
            fallback_reason: None,
        };
        assert_eq!(route_transfer_event(&g, complete).len(), 1);
        assert!(!g.is_active(&transfer_lane("cross")));
    }
}
