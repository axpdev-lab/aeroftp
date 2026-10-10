// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! The speed a progress event reports: the current rate, not the average.
//!
//! Every GUI progress callback used to compute `speed_bps` as
//! `transferred / seconds since the start`. That is the average of the whole
//! transfer, so after a few minutes the number barely moved, a real drop never
//! showed, the toast speed graph drew a straight line (25 GB upload, #658 test,
//! 2026-10-10: "85 MB/s, avg 85.2, peak 85.7") and the ETA trailed every
//! change of pace. [`SpeedMeter`] measures the bytes moved over the last
//! [`WINDOW`] instead, which is what the toast, the lanes and the ETA expect.
//!
//! Design rules:
//! - monotonic time only (`Instant`), injectable for tests;
//! - bounded memory: at most one sample per [`SAMPLE_EVERY`], so a callback
//!   fired for every chunk keeps a few dozen samples, not one per chunk;
//! - `&self` under one mutex, because progress callbacks are `Fn + Send`;
//! - the first reading is the reference, not "0 bytes at the start": a resumed
//!   transfer reports the bytes already on disk in its counter (the FTP and
//!   WebDAV resume paths start at the offset), and counting them as moved in
//!   the first 100 ms showed gigabytes per second for a whole window;
//! - a counter that goes backwards (a retry restarting the file) restarts the
//!   measurement instead of reporting a negative or huge rate.
//!
//! End-of-transfer summaries (CLI result lines, the cross-profile `complete`
//! event, benchmarks, the speed test result) report the average on purpose and
//! do not use this.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How far back the current speed looks. Long enough to smooth chunk bursts
/// and segment interleaving, short enough that a drop shows within seconds.
pub const WINDOW: Duration = Duration::from_secs(3);
/// Minimum spacing between stored samples.
pub const SAMPLE_EVERY: Duration = Duration::from_millis(100);
/// Below this span there is no rate to report yet (the callbacks used the
/// same 100 ms guard on the elapsed time).
const MIN_SPAN: Duration = Duration::from_millis(100);

/// Current transfer speed over a sliding [`WINDOW`].
#[derive(Debug)]
pub struct SpeedMeter {
    samples: Mutex<VecDeque<(Instant, u64)>>,
}

impl Default for SpeedMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl SpeedMeter {
    /// A meter with no reading yet: its first reading becomes the reference.
    pub fn new() -> Self {
        Self {
            samples: Mutex::new(VecDeque::with_capacity(32)),
        }
    }

    /// Bytes per second over the last [`WINDOW`], given the transfer's byte
    /// counter. 0 on the first reading, which only sets the reference.
    pub fn bps(&self, transferred: u64) -> u64 {
        self.bps_at(Instant::now(), transferred)
    }

    /// [`Self::bps`] at an explicit instant (tests).
    pub fn bps_at(&self, now: Instant, transferred: u64) -> u64 {
        let mut samples = self
            .samples
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // First reading, or a counter that went back: it is the new reference.
        if samples.back().is_none_or(|&(_, last)| transferred < last) {
            samples.clear();
            samples.push_back((now, transferred));
            return 0;
        }
        let due = samples
            .back()
            .is_none_or(|&(at, _)| now.saturating_duration_since(at) >= SAMPLE_EVERY);
        if due {
            samples.push_back((now, transferred));
        }
        // Keep exactly one sample at or before the window start as the anchor.
        while samples.len() >= 2 && now.saturating_duration_since(samples[1].0) >= WINDOW {
            samples.pop_front();
        }

        let Some(&(anchor_at, anchor_bytes)) = samples.front() else {
            return 0;
        };
        let span = now.saturating_duration_since(anchor_at);
        if span < MIN_SPAN {
            return 0;
        }
        (transferred.saturating_sub(anchor_bytes) as f64 / span.as_secs_f64()) as u64
    }
}

/// Seconds left for `remaining` bytes at `bps`, 0 when there is no rate.
pub fn eta_seconds(remaining: u64, bps: u64) -> u64 {
    if bps == 0 {
        0
    } else {
        (remaining as f64 / bps as f64) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1_000_000;

    fn at(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    /// A meter whose transfer starts at `start` from 0 bytes.
    fn started(start: Instant) -> SpeedMeter {
        let meter = SpeedMeter::new();
        assert_eq!(meter.bps_at(start, 0), 0, "the first reading has no rate");
        meter
    }

    /// Feed a constant rate in 100 ms steps from `from_ms` to `to_ms`,
    /// returning the last reading and the byte count reached.
    fn feed(
        meter: &SpeedMeter,
        start: Instant,
        from_ms: u64,
        to_ms: u64,
        mut bytes: u64,
        bytes_per_step: u64,
    ) -> (u64, u64) {
        let mut last = 0;
        let mut ms = from_ms;
        while ms < to_ms {
            ms += 100;
            bytes += bytes_per_step;
            last = meter.bps_at(at(start, ms), bytes);
        }
        (last, bytes)
    }

    #[test]
    fn a_drop_in_the_middle_shows_in_the_speed_and_not_in_the_average() {
        let start = Instant::now();
        let meter = started(start);
        // 60 s at 80 MB/s, then 10 s at 10 MB/s.
        let (fast, bytes) = feed(&meter, start, 0, 60_000, 0, 8 * MB);
        assert!((79 * MB..=81 * MB).contains(&fast), "steady phase: {fast}");
        let (slow, bytes) = feed(&meter, start, 60_000, 70_000, bytes, MB);
        assert!((9 * MB..=11 * MB).contains(&slow), "after the drop: {slow}");
        // What the callbacks used to report at the same instant.
        let average = bytes / 70;
        assert!(
            average > 65 * MB,
            "the old figure hides the drop: {average}"
        );
    }

    #[test]
    fn the_speed_follows_the_drop_within_the_window() {
        let start = Instant::now();
        let meter = started(start);
        let (_, bytes) = feed(&meter, start, 0, 30_000, 0, 8 * MB);
        // One window after the drop, nothing of the fast phase is left.
        let (slow, _) = feed(
            &meter,
            start,
            30_000,
            30_000 + WINDOW.as_millis() as u64 + 100,
            bytes,
            MB,
        );
        assert!((9 * MB..=11 * MB).contains(&slow), "{slow}");
    }

    #[test]
    fn a_stall_lowers_the_speed_when_bytes_resume() {
        let start = Instant::now();
        let meter = started(start);
        let (_, bytes) = feed(&meter, start, 0, 10_000, 0, 8 * MB);
        // Nothing for 10 s, then 1 MB in 100 ms: the window holds the stall.
        let after = meter.bps_at(at(start, 20_100), bytes + MB);
        assert!(after < 2 * MB, "{after}");
    }

    #[test]
    fn a_counter_that_goes_back_restarts_the_measurement() {
        let start = Instant::now();
        let meter = started(start);
        let (_, _) = feed(&meter, start, 0, 5_000, 0, 8 * MB);
        assert_eq!(meter.bps_at(at(start, 5_100), 0), 0, "retry from zero");
        let (again, _) = feed(&meter, start, 5_100, 8_100, 0, 2 * MB);
        assert!((19 * MB..=21 * MB).contains(&again), "{again}");
    }

    #[test]
    fn a_resumed_transfer_does_not_count_the_bytes_already_there() {
        let start = Instant::now();
        let meter = SpeedMeter::new();
        // Resumed at 5 GB: the first callback reports the offset plus a chunk.
        let offset = 5_000 * MB;
        assert_eq!(meter.bps_at(at(start, 100), offset + MB), 0);
        let (speed, _) = feed(&meter, start, 100, 1_000, offset + MB, MB);
        assert!((9 * MB..=11 * MB).contains(&speed), "{speed}");
    }

    #[test]
    fn no_rate_before_the_first_100_ms() {
        let start = Instant::now();
        let meter = started(start);
        assert_eq!(meter.bps_at(at(start, 50), MB), 0);
        assert!(meter.bps_at(at(start, 200), 2 * MB) > 0);
    }

    #[test]
    fn a_callback_per_chunk_keeps_the_sample_buffer_bounded() {
        let start = Instant::now();
        let meter = started(start);
        let mut bytes = 0;
        // One callback per millisecond for 20 s.
        for ms in 1..=20_000 {
            bytes += 64 * 1024;
            meter.bps_at(at(start, ms), bytes);
        }
        let stored = meter.samples.lock().unwrap().len();
        let bound = (WINDOW.as_millis() / SAMPLE_EVERY.as_millis()) as usize + 2;
        assert!(stored <= bound, "{stored} samples, bound {bound}");
    }

    #[test]
    fn eta_is_zero_without_a_rate() {
        assert_eq!(eta_seconds(10 * MB, 0), 0);
        assert_eq!(eta_seconds(10 * MB, 2 * MB), 5);
    }
}
