//! Progress reporting contract of the aerorsync driver.
//!
//! The driver calls the sink with `(transferred_wire_bytes, total_hint)` as
//! the transfer makes network progress. For an upload `total_hint` is the
//! full delta payload size; for a download it is the remote file size hint
//! (wire bytes may be fewer than the file on a real delta hit, so a caller
//! drawing a bar may see it under-fill and complete at reconstruction).
//! The driver paces the calls with [`ProgressThrottle`]
//! (`native_driver::report_wire_progress`). `None` costs nothing per chunk:
//! a single `is_none()` check.
//!
//! The application keeps its own structurally identical alias, because
//! the module that holds it also compiles with the `aerorsync` feature
//! off; the two must stay the same type, which the application-side test
//! `delta_transport::tests::aerorsync_delta_progress_sink_is_the_crate_progress_sink`
//! pins at compile time. This module never names the application path,
//! comments included.

// SPDX-License-Identifier: MPL-2.0 OR GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

#![cfg(feature = "aerorsync")]

/// Optional per-byte progress callback for a delta transfer.
pub type ProgressSink = Box<dyn FnMut(u64, u64) + Send>;

/// Longest the sink stays silent while bytes keep moving. The GUI derives the
/// current speed from these calls, so a gap here is a speed that stands still.
pub(crate) const PROGRESS_MAX_SILENCE: std::time::Duration = std::time::Duration::from_millis(150);

/// Smallest byte step that calls the sink before the interval has elapsed.
const PROGRESS_MIN_STEP: u64 = 256 * 1024;

/// Decides when the driver calls the [`ProgressSink`]: on the first bytes,
/// then when one percent of `total` (at least 256 KiB) has moved or
/// [`PROGRESS_MAX_SILENCE`] has passed with new bytes, whichever comes first,
/// and on the final byte. A step of one percent alone was too coarse for a
/// large file on a slow link: 75 MB on 7.5 GB, about 19 s at 4 MB/s.
#[derive(Debug, Default)]
pub(crate) struct ProgressThrottle {
    /// `transferred` of the last admitted call.
    last_bytes: u64,
    /// When the last admitted call happened; `None` before the first.
    last_at: Option<std::time::Instant>,
}

impl ProgressThrottle {
    /// Whether the sink should hear about `transferred` (out of `total`, a
    /// hint) at `now`. Records the call when it does.
    pub(crate) fn admit(&mut self, now: std::time::Instant, transferred: u64, total: u64) -> bool {
        let step = if total > 0 {
            (total / 100).max(PROGRESS_MIN_STEP)
        } else {
            PROGRESS_MIN_STEP
        };
        if let Some(at) = self.last_at {
            // Nothing new since the last call, a repeated final byte included.
            if transferred <= self.last_bytes {
                return false;
            }
            let final_tick = total > 0 && transferred >= total;
            let due = final_tick
                || transferred >= self.last_bytes.saturating_add(step)
                || now.saturating_duration_since(at) >= PROGRESS_MAX_SILENCE;
            if !due {
                return false;
            }
        }
        self.last_bytes = transferred;
        self.last_at = Some(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    const MIB: u64 = 1024 * 1024;

    /// Times of the admitted calls, as offsets from the first sample.
    fn admitted(samples: impl IntoIterator<Item = (Duration, u64)>, total: u64) -> Vec<Duration> {
        let t0 = Instant::now();
        let mut throttle = ProgressThrottle::default();
        samples
            .into_iter()
            .filter(|&(at, bytes)| throttle.admit(t0 + at, bytes, total))
            .map(|(at, _)| at)
            .collect()
    }

    #[test]
    fn a_slow_large_upload_reports_every_slab() {
        // 7.5 GB at 4 MB/s: the upload reports once per 4 MiB slab, about once
        // a second. A step of one percent (75 MB) alone kept the toast, and the
        // speed measured from it, still for about 19 s between calls.
        let total = 7_500_000_000;
        let calls = admitted(
            (1..=20u64).map(|i| (Duration::from_secs(i), i * 4 * MIB)),
            total,
        );
        assert_eq!(
            calls.len(),
            20,
            "every slab a second apart reaches the sink: {calls:?}"
        );
    }

    #[test]
    fn a_slow_download_reports_at_the_interval_not_per_frame() {
        // A frame of 40 KB every 10 ms (4 MB/s) for 3 s on a 7.5 GB file: the
        // sink hears the first frame, then one frame per interval, never two
        // within it and never a gap longer than the interval plus one frame.
        let total = 7_500_000_000;
        let frame_gap = Duration::from_millis(10);
        let calls = admitted(
            (1..=300u64).map(|i| (frame_gap * i as u32, i * 40_000)),
            total,
        );
        assert_eq!(
            calls.first(),
            Some(&frame_gap),
            "the first movement is reported at once"
        );
        for pair in calls.windows(2) {
            let gap = pair[1] - pair[0];
            assert!(
                gap >= PROGRESS_MAX_SILENCE && gap < PROGRESS_MAX_SILENCE + frame_gap,
                "gap {gap:?} between calls must be the interval: {calls:?}"
            );
        }
        assert!(
            calls.last().unwrap() > &(Duration::from_secs(3) - PROGRESS_MAX_SILENCE - frame_gap)
        );
    }

    #[test]
    fn a_fast_transfer_still_reports_each_percent() {
        // 1 GB at 1 GB/s in 1 MiB chunks: the byte step (1 %) fires well
        // before the interval, so a short fast transfer still fills its bar
        // in steps rather than jumping from the first call to the last.
        let total = 1_000 * MIB;
        let calls = admitted(
            (1..=100u64).map(|i| (Duration::from_millis(i), i * MIB)),
            total,
        );
        assert!(
            calls.len() >= 9,
            "100 MiB in 100 ms must report about every 10 MiB: {calls:?}"
        );
    }

    #[test]
    fn no_new_bytes_no_call_and_the_last_byte_calls_once() {
        let t0 = Instant::now();
        let total = 7_500_000_000;
        let mut throttle = ProgressThrottle::default();
        assert!(throttle.admit(t0, MIB, total));
        assert!(
            !throttle.admit(t0 + Duration::from_secs(5), MIB, total),
            "a call with nothing new has nothing to say"
        );
        let end = t0 + Duration::from_secs(5) + Duration::from_millis(1);
        assert!(
            throttle.admit(end, total, total),
            "the final byte is reported even inside the interval"
        );
        assert!(
            !throttle.admit(end + Duration::from_secs(1), total, total),
            "and only once"
        );
    }
}
