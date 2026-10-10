// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! One pacing rule for progress frames that go straight to the frontend,
//! outside the GUI transfer governor (`progress_governor`): local archive and
//! vault frames, the app update download, the P2P send dialog and the AI
//! transfer tool.
//!
//! A frame is due when [`MIN_INTERVAL`] has passed since the last one or the
//! percentage has moved [`MIN_PCT_STEP`] points, whichever comes first; the
//! final frame is always due, once. The time branch is what keeps a long
//! operation moving: a percentage step alone leaves a 7.5 GB job silent for
//! 75 MB at a time, about 19 s at 4 MB/s, and the frontend, which derives the
//! current speed from these frames (`src/utils/frameSpeed.ts`), shows a speed
//! that stands still for as long.
//!
//! The aerorsync driver paces its own sink with the same interval
//! (`aerorsync::progress::ProgressThrottle`): that module does not depend on
//! the application, so it cannot use this one.

use std::time::{Duration, Instant};

/// Longest a frame waits while the operation moves.
pub const MIN_INTERVAL: Duration = Duration::from_millis(150);

/// Percentage points that make a frame due before [`MIN_INTERVAL`].
pub const MIN_PCT_STEP: i64 = 2;

/// Pacing state of one operation's progress frames.
#[derive(Debug)]
pub struct ProgressCadence {
    last_at: Instant,
    last_pct: i64,
    final_sent: bool,
}

impl Default for ProgressCadence {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgressCadence {
    /// A cadence whose interval starts now, with no frame emitted yet.
    pub fn new() -> Self {
        Self::starting_at(Instant::now())
    }

    fn starting_at(now: Instant) -> Self {
        Self {
            last_at: now,
            last_pct: -1,
            final_sent: false,
        }
    }

    /// Whether the frame at `pct` should be emitted now. `is_final` marks the
    /// terminal frame, which is due once whatever the interval. Records the
    /// frame when it is due.
    pub fn admit(&mut self, pct: i64, is_final: bool) -> bool {
        self.admit_at(Instant::now(), pct, is_final)
    }

    fn admit_at(&mut self, now: Instant, pct: i64, is_final: bool) -> bool {
        let due = if is_final {
            !self.final_sent
        } else {
            now.saturating_duration_since(self.last_at) >= MIN_INTERVAL
                || pct.saturating_sub(self.last_pct) >= MIN_PCT_STEP
        };
        if due {
            self.last_at = now;
            self.last_pct = pct;
            self.final_sent |= is_final;
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slow_operation_emits_at_the_interval_while_the_percentage_stands_still() {
        // 7.5 GB at 4 MB/s moves 1 % every ~19 s: a callback every 50 ms for
        // 3 s never changes the integer percentage, and a frame is still due
        // every interval, never two within one.
        let t0 = Instant::now();
        let mut cadence = ProgressCadence::starting_at(t0);
        let tick = Duration::from_millis(50);
        let frames: Vec<Duration> = (1..=60u32)
            .map(|i| tick * i)
            .filter(|&at| cadence.admit_at(t0 + at, 0, false))
            .collect();
        assert_eq!(
            frames.len(),
            20,
            "one frame per 150 ms over 3 s: {frames:?}"
        );
        for pair in frames.windows(2) {
            assert_eq!(pair[1] - pair[0], MIN_INTERVAL, "{frames:?}");
        }
    }

    #[test]
    fn a_fast_operation_emits_every_two_points() {
        // 100 % in 100 ms, one callback per point: the step makes a frame due
        // every 2 points without waiting for the interval.
        let t0 = Instant::now();
        let mut cadence = ProgressCadence::starting_at(t0);
        let pcts: Vec<i64> = (0..=99i64)
            .filter(|&pct| cadence.admit_at(t0 + Duration::from_millis(pct as u64), pct, false))
            .collect();
        assert_eq!(pcts.first(), Some(&1));
        assert!(
            pcts.windows(2).all(|w| w[1] - w[0] == MIN_PCT_STEP),
            "{pcts:?}"
        );
    }

    #[test]
    fn the_final_frame_is_due_once_inside_the_interval() {
        let t0 = Instant::now();
        let mut cadence = ProgressCadence::starting_at(t0);
        assert!(cadence.admit_at(t0 + Duration::from_millis(1), 99, false));
        assert!(
            cadence.admit_at(t0 + Duration::from_millis(2), 100, true),
            "the terminal frame is never held back"
        );
        assert!(
            !cadence.admit_at(t0 + Duration::from_millis(3), 100, true),
            "and it is emitted once"
        );
    }
}
