// AeroSync Scheduler Module
// Interval-based sync scheduling with time window and day-of-week filtering

use chrono::{DateTime, Datelike, Local, Timelike, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::LazyLock;
use tracing::{info, warn};

/// Write lock to prevent TOCTOU race between frontend saves and background worker updates
pub static SCHEDULE_WRITE_LOCK: LazyLock<std::sync::Mutex<()>> =
    LazyLock::new(|| std::sync::Mutex::new(()));

// ---------------------------------------------------------------------------
// Weekday
// ---------------------------------------------------------------------------

/// Day of week for schedule filtering
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl Weekday {
    /// Convert from `chrono::Weekday` to our `Weekday` enum.
    pub fn from_chrono(day: chrono::Weekday) -> Self {
        match day {
            chrono::Weekday::Mon => Self::Mon,
            chrono::Weekday::Tue => Self::Tue,
            chrono::Weekday::Wed => Self::Wed,
            chrono::Weekday::Thu => Self::Thu,
            chrono::Weekday::Fri => Self::Fri,
            chrono::Weekday::Sat => Self::Sat,
            chrono::Weekday::Sun => Self::Sun,
        }
    }

    /// Return the previous day of the week.
    pub fn prev(self) -> Self {
        match self {
            Self::Mon => Self::Sun,
            Self::Tue => Self::Mon,
            Self::Wed => Self::Tue,
            Self::Thu => Self::Wed,
            Self::Fri => Self::Thu,
            Self::Sat => Self::Fri,
            Self::Sun => Self::Sat,
        }
    }
}

// ---------------------------------------------------------------------------
// TimeWindow
// ---------------------------------------------------------------------------

/// Time window during which sync is allowed.
///
/// Supports overnight windows (e.g., 22:00-06:00) by detecting when
/// `start` is later than `end`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeWindow {
    /// Start hour (0-23)
    pub start_hour: u8,
    /// Start minute (0-59)
    pub start_minute: u8,
    /// End hour (0-23)
    pub end_hour: u8,
    /// End minute (0-59)
    pub end_minute: u8,
    /// Allowed days of the week. Empty means every day.
    #[serde(default)]
    pub days: Vec<Weekday>,
}

impl TimeWindow {
    /// Validate that hour/minute values are within valid ranges.
    pub fn validate(&self) -> Result<(), String> {
        if self.start_hour > 23 {
            return Err(format!("start_hour {} out of range 0-23", self.start_hour));
        }
        if self.end_hour > 23 {
            return Err(format!("end_hour {} out of range 0-23", self.end_hour));
        }
        if self.start_minute > 59 {
            return Err(format!("start_minute {} out of range 0-59", self.start_minute));
        }
        if self.end_minute > 59 {
            return Err(format!("end_minute {} out of range 0-59", self.end_minute));
        }
        Ok(())
    }

    /// Check whether the given `(hour, minute)` falls inside this window.
    ///
    /// Handles overnight windows transparently: if `start > end` the window
    /// wraps around midnight (e.g., 22:00-06:00 means "from 22:00 to 06:00
    /// the next day").
    pub fn contains_time(&self, hour: u8, minute: u8) -> bool {
        let start = self.start_hour as u16 * 60 + self.start_minute as u16;
        let end = self.end_hour as u16 * 60 + self.end_minute as u16;
        let now = hour as u16 * 60 + minute as u16;

        if start <= end {
            // Normal window (e.g. 09:00 - 17:00)
            now >= start && now < end
        } else {
            // Overnight window (e.g. 22:00 - 06:00)
            now >= start || now < end
        }
    }

    /// Check whether the given day is allowed by this window.
    ///
    /// An empty `days` vec means every day is allowed.
    pub fn contains_day(&self, day: &Weekday) -> bool {
        if self.days.is_empty() {
            return true;
        }
        self.days.contains(day)
    }

    /// Check whether the given `(hour, minute, weekday)` falls inside this
    /// window, correctly handling overnight carry-over for day filters.
    ///
    /// For overnight windows (e.g. 22:00-06:00) with day filter `[Mon]`:
    /// - Monday 23:00 is inside (start portion, check Monday)
    /// - Tuesday 02:00 is inside (after-midnight portion of Monday's window,
    ///   check Monday = yesterday)
    /// - Tuesday 23:00 is outside (start portion, but Tuesday is not allowed)
    pub fn contains_time_and_day(&self, hour: u8, minute: u8, today: &Weekday) -> bool {
        if !self.contains_time(hour, minute) {
            return false;
        }

        let start = self.start_hour as u16 * 60 + self.start_minute as u16;
        let end = self.end_hour as u16 * 60 + self.end_minute as u16;
        let now = hour as u16 * 60 + minute as u16;

        // For overnight windows, if we're in the after-midnight portion,
        // the relevant day is yesterday (when the window started)
        let check_day = if start > end && now < end {
            today.prev()
        } else {
            *today
        };

        self.contains_day(&check_day)
    }
}

// ---------------------------------------------------------------------------
// SyncSchedule
// ---------------------------------------------------------------------------

/// Sync schedule configuration.
///
/// Persisted to `~/.config/aeroftp/sync_schedule.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncSchedule {
    /// Master toggle — when `false` the scheduler never fires.
    pub enabled: bool,
    /// Interval between sync runs in seconds.
    /// `0` disables interval-based sync; minimum effective value is 60.
    pub interval_secs: u64,
    /// Optional time window restriction.
    pub time_window: Option<TimeWindow>,
    /// Temporary pause (user-toggled, does not change `enabled`).
    pub paused: bool,
    /// Last successful sync timestamp (UTC).
    pub last_sync: Option<DateTime<Utc>>,
}

impl Default for SyncSchedule {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_secs: 86400, // 24h — watcher handles real-time, scheduler is safety net
            time_window: None,
            paused: false,
            last_sync: None,
        }
    }
}

impl SyncSchedule {
    /// Check whether a sync should fire right now.
    ///
    /// Returns `true` when **all** of the following hold:
    /// 1. `enabled` is `true`
    /// 2. `paused` is `false`
    /// 3. `interval_secs` >= 60
    /// 4. Current local time is inside the time window (if configured)
    /// 5. Current day of week is allowed (if the window has day filters)
    /// 6. Enough time has elapsed since `last_sync`
    pub fn should_sync_now(&self) -> bool {
        if !self.enabled || self.paused {
            return false;
        }

        if self.interval_secs < 60 {
            return false;
        }

        if !self.is_in_time_window() {
            return false;
        }

        // Check elapsed time since last sync
        if let Some(last) = self.last_sync {
            let elapsed = Utc::now().signed_duration_since(last);
            if elapsed.num_seconds() < self.interval_secs as i64 {
                return false;
            }
        }

        true
    }

    /// Seconds until the next sync opportunity, or `None` if the schedule
    /// is disabled / paused / has a zero interval.
    ///
    /// When inside the time window, this returns the remaining seconds of
    /// the current interval. When outside the window, it returns the seconds
    /// until the window opens next plus any remaining interval.
    pub fn next_sync_in(&self) -> Option<u64> {
        if !self.enabled || self.paused || self.interval_secs < 60 {
            return None;
        }

        let now_utc = Utc::now();
        let now_local = Local::now();

        // Seconds remaining in the current interval
        let interval_remaining = if let Some(last) = self.last_sync {
            let elapsed = now_utc.signed_duration_since(last).num_seconds().max(0) as u64;
            if elapsed >= self.interval_secs {
                0u64
            } else {
                self.interval_secs - elapsed
            }
        } else {
            // Never synced — ready immediately (interval-wise)
            0u64
        };

        // If no time window, it's purely interval-based
        let window = match &self.time_window {
            Some(w) => w,
            None => return Some(interval_remaining),
        };

        let hour = now_local.hour() as u8;
        let minute = now_local.minute() as u8;
        let today = Weekday::from_chrono(now_local.weekday());

        let in_window = window.contains_time_and_day(hour, minute, &today);

        if in_window {
            return Some(interval_remaining);
        }

        // Outside the window — compute seconds until window opens
        let secs_until_window = seconds_until_window_opens(window, &now_local);
        Some(secs_until_window + interval_remaining)
    }

    /// Check whether the current local time is inside the configured time
    /// window. Returns `true` when no window is set (always allowed).
    pub fn is_in_time_window(&self) -> bool {
        let window = match &self.time_window {
            Some(w) => w,
            None => return true,
        };

        let now = Local::now();
        let hour = now.hour() as u8;
        let minute = now.minute() as u8;
        let today = Weekday::from_chrono(now.weekday());

        window.contains_time_and_day(hour, minute, &today)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Estimate the number of seconds from `now` until the given `TimeWindow`
/// opens next. Accounts for day-of-week filters (scans up to 7 days ahead).
fn seconds_until_window_opens(window: &TimeWindow, now: &DateTime<Local>) -> u64 {
    let start_minutes = window.start_hour as u32 * 60 + window.start_minute as u32;
    let now_minutes = now.hour() * 60 + now.minute();

    // Iterate up to 8 days to find the next allowed slot
    for day_offset in 0u32..8 {
        let candidate = *now + chrono::Duration::days(day_offset as i64);
        let candidate_weekday = Weekday::from_chrono(candidate.weekday());

        if !window.contains_day(&candidate_weekday) {
            continue;
        }

        if day_offset == 0 {
            // Same day — only valid if the window start is still ahead
            if now_minutes < start_minutes {
                return ((start_minutes - now_minutes) * 60 - now.second()) as u64;
            }
            // For overnight windows starting today, we are past the start
            // but not yet in the window (we're in the gap), so we need to
            // wait until tomorrow's perspective or next allowed day.
            continue;
        }

        // Future day — seconds until midnight + seconds from midnight to start
        let secs_until_midnight =
            ((23 - now.hour()) * 3600 + (59 - now.minute()) * 60 + (60 - now.second())) as u64;
        let secs_from_midnight = start_minutes as u64 * 60;
        let full_days = (day_offset - 1) as u64 * 86400;

        return secs_until_midnight + full_days + secs_from_midnight;
    }

    // Fallback (should not happen with <= 7-day scan)
    86400
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

/// Resolve the path to `~/.config/aeroftp/sync_schedule.json`.
fn schedule_path() -> Result<PathBuf, String> {
    let base = dirs::config_dir()
        .ok_or_else(|| "Cannot determine config directory".to_string())?;
    Ok(base.join("aeroftp").join("sync_schedule.json"))
}

/// Load the sync schedule from persistent config.
///
/// Returns `SyncSchedule::default()` when the file does not exist or cannot
/// be parsed.
pub fn load_sync_schedule() -> SyncSchedule {
    let _lock = SCHEDULE_WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = match schedule_path() {
        Ok(p) => p,
        Err(e) => {
            warn!("Cannot resolve schedule path: {}. Using defaults.", e);
            return SyncSchedule::default();
        }
    };

    match std::fs::read_to_string(&path) {
        Ok(contents) => match serde_json::from_str::<SyncSchedule>(&contents) {
            Ok(schedule) => {
                info!("Sync schedule loaded from {}", path.display());
                schedule
            }
            Err(e) => {
                warn!(
                    "Failed to parse sync schedule at {}: {}. Using defaults.",
                    path.display(),
                    e
                );
                SyncSchedule::default()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            info!(
                "No sync schedule found at {}. Using defaults.",
                path.display()
            );
            SyncSchedule::default()
        }
        Err(e) => {
            warn!(
                "Failed to read sync schedule at {}: {}. Using defaults.",
                path.display(),
                e
            );
            SyncSchedule::default()
        }
    }
}

/// Save the sync schedule to persistent config.
///
/// Creates the parent directory if it does not exist.
pub fn save_sync_schedule(schedule: &SyncSchedule) -> Result<(), String> {
    // Validate time window if present
    if let Some(ref tw) = schedule.time_window {
        tw.validate()?;
    }

    let _lock = SCHEDULE_WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = schedule_path()?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            format!(
                "Failed to create config directory {}: {}",
                parent.display(),
                e
            )
        })?;
    }

    let json = serde_json::to_string_pretty(schedule)
        .map_err(|e| format!("Failed to serialize sync schedule: {}", e))?;

    // Atomic write: temp file + rename
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &json)
        .map_err(|e| format!("Failed to write temp schedule to {}: {}", tmp_path.display(), e))?;
    std::fs::rename(&tmp_path, &path)
        .map_err(|e| format!("Failed to rename schedule file: {}", e))?;

    info!("Sync schedule saved to {}", path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_schedule() {
        let s = SyncSchedule::default();
        assert!(!s.enabled);
        assert_eq!(s.interval_secs, 86400);
        assert!(s.time_window.is_none());
        assert!(!s.paused);
        assert!(s.last_sync.is_none());
    }

    #[test]
    fn test_should_sync_disabled() {
        let s = SyncSchedule {
            enabled: false,
            interval_secs: 300,
            time_window: None,
            paused: false,
            last_sync: None,
        };
        assert!(!s.should_sync_now());
    }

    #[test]
    fn test_should_sync_paused() {
        let s = SyncSchedule {
            enabled: true,
            interval_secs: 300,
            time_window: None,
            paused: true,
            last_sync: None,
        };
        assert!(!s.should_sync_now());
    }

    #[test]
    fn test_time_window_normal() {
        // 09:00 - 17:00
        let w = TimeWindow {
            start_hour: 9,
            start_minute: 0,
            end_hour: 17,
            end_minute: 0,
            days: vec![],
        };

        // 12:00 is inside
        assert!(w.contains_time(12, 0));
        // 09:00 exactly is inside (inclusive start)
        assert!(w.contains_time(9, 0));
        // 16:59 is inside
        assert!(w.contains_time(16, 59));
        // 17:00 is outside (exclusive end)
        assert!(!w.contains_time(17, 0));
        // 20:00 is outside
        assert!(!w.contains_time(20, 0));
        // 08:59 is outside
        assert!(!w.contains_time(8, 59));
    }

    #[test]
    fn test_time_window_overnight() {
        // 22:00 - 06:00 (overnight)
        let w = TimeWindow {
            start_hour: 22,
            start_minute: 0,
            end_hour: 6,
            end_minute: 0,
            days: vec![],
        };

        // 23:00 is inside (after start, before midnight)
        assert!(w.contains_time(23, 0));
        // 03:00 is inside (after midnight, before end)
        assert!(w.contains_time(3, 0));
        // 22:00 exactly is inside (inclusive start)
        assert!(w.contains_time(22, 0));
        // 05:59 is inside
        assert!(w.contains_time(5, 59));
        // 06:00 is outside (exclusive end)
        assert!(!w.contains_time(6, 0));
        // 12:00 is outside (daytime gap)
        assert!(!w.contains_time(12, 0));
        // 21:59 is outside (just before start)
        assert!(!w.contains_time(21, 59));
    }

    #[test]
    fn test_time_window_day_filter() {
        let w = TimeWindow {
            start_hour: 9,
            start_minute: 0,
            end_hour: 17,
            end_minute: 0,
            days: vec![
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
            ],
        };

        // Weekdays are allowed
        assert!(w.contains_day(&Weekday::Mon));
        assert!(w.contains_day(&Weekday::Fri));

        // Weekend is blocked
        assert!(!w.contains_day(&Weekday::Sat));
        assert!(!w.contains_day(&Weekday::Sun));
    }

    #[test]
    fn test_time_window_empty_days() {
        let w = TimeWindow {
            start_hour: 0,
            start_minute: 0,
            end_hour: 23,
            end_minute: 59,
            days: vec![], // empty = every day
        };

        assert!(w.contains_day(&Weekday::Mon));
        assert!(w.contains_day(&Weekday::Sat));
        assert!(w.contains_day(&Weekday::Sun));
    }

    #[test]
    fn test_next_sync_in() {
        // Never synced, no window, enabled — should be 0 (ready now)
        let s = SyncSchedule {
            enabled: true,
            interval_secs: 300,
            time_window: None,
            paused: false,
            last_sync: None,
        };
        assert_eq!(s.next_sync_in(), Some(0));

        // Recently synced — should return remaining interval seconds
        let s2 = SyncSchedule {
            enabled: true,
            interval_secs: 300,
            time_window: None,
            paused: false,
            last_sync: Some(Utc::now() - chrono::Duration::seconds(100)),
        };
        let remaining = s2.next_sync_in().unwrap();
        // Should be approximately 200 seconds (allow 2s tolerance for test execution time)
        assert!(remaining >= 198 && remaining <= 202, "remaining={}", remaining);

        // Fully elapsed interval — 0 seconds remaining
        let s3 = SyncSchedule {
            enabled: true,
            interval_secs: 60,
            time_window: None,
            paused: false,
            last_sync: Some(Utc::now() - chrono::Duration::seconds(120)),
        };
        assert_eq!(s3.next_sync_in(), Some(0));

        // Disabled — None
        let s4 = SyncSchedule {
            enabled: false,
            interval_secs: 300,
            time_window: None,
            paused: false,
            last_sync: None,
        };
        assert_eq!(s4.next_sync_in(), None);

        // Paused — None
        let s5 = SyncSchedule {
            enabled: true,
            interval_secs: 300,
            time_window: None,
            paused: true,
            last_sync: None,
        };
        assert_eq!(s5.next_sync_in(), None);
    }

    #[test]
    fn test_schedule_serde_roundtrip() {
        let schedule = SyncSchedule {
            enabled: true,
            interval_secs: 600,
            time_window: Some(TimeWindow {
                start_hour: 22,
                start_minute: 30,
                end_hour: 6,
                end_minute: 15,
                days: vec![Weekday::Mon, Weekday::Wed, Weekday::Fri],
            }),
            paused: false,
            last_sync: Some(Utc::now()),
        };

        let json = serde_json::to_string_pretty(&schedule).expect("serialize");
        let parsed: SyncSchedule = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(parsed.enabled, schedule.enabled);
        assert_eq!(parsed.interval_secs, schedule.interval_secs);
        assert_eq!(parsed.paused, schedule.paused);
        assert!(parsed.last_sync.is_some());
        assert!(parsed.time_window.is_some());

        let tw = parsed.time_window.unwrap();
        assert_eq!(tw.start_hour, 22);
        assert_eq!(tw.start_minute, 30);
        assert_eq!(tw.end_hour, 6);
        assert_eq!(tw.end_minute, 15);
        assert_eq!(tw.days.len(), 3);
        assert_eq!(tw.days[0], Weekday::Mon);
        assert_eq!(tw.days[1], Weekday::Wed);
        assert_eq!(tw.days[2], Weekday::Fri);
    }

    #[test]
    fn test_time_window_validation() {
        // Valid window
        let w = TimeWindow {
            start_hour: 9, start_minute: 0,
            end_hour: 17, end_minute: 30,
            days: vec![],
        };
        assert!(w.validate().is_ok());

        // Invalid start_hour
        let w2 = TimeWindow {
            start_hour: 25, start_minute: 0,
            end_hour: 17, end_minute: 0,
            days: vec![],
        };
        assert!(w2.validate().is_err());

        // Invalid end_minute
        let w3 = TimeWindow {
            start_hour: 9, start_minute: 0,
            end_hour: 17, end_minute: 60,
            days: vec![],
        };
        assert!(w3.validate().is_err());
    }

    #[test]
    fn test_weekday_from_chrono() {
        assert_eq!(Weekday::from_chrono(chrono::Weekday::Mon), Weekday::Mon);
        assert_eq!(Weekday::from_chrono(chrono::Weekday::Tue), Weekday::Tue);
        assert_eq!(Weekday::from_chrono(chrono::Weekday::Wed), Weekday::Wed);
        assert_eq!(Weekday::from_chrono(chrono::Weekday::Thu), Weekday::Thu);
        assert_eq!(Weekday::from_chrono(chrono::Weekday::Fri), Weekday::Fri);
        assert_eq!(Weekday::from_chrono(chrono::Weekday::Sat), Weekday::Sat);
        assert_eq!(Weekday::from_chrono(chrono::Weekday::Sun), Weekday::Sun);
    }

    #[test]
    fn test_should_sync_interval_too_low() {
        // interval_secs < 60 should prevent sync
        let s = SyncSchedule {
            enabled: true,
            interval_secs: 30,
            time_window: None,
            paused: false,
            last_sync: None,
        };
        assert!(!s.should_sync_now());

        // interval_secs = 0 should prevent sync
        let s2 = SyncSchedule {
            enabled: true,
            interval_secs: 0,
            time_window: None,
            paused: false,
            last_sync: None,
        };
        assert!(!s2.should_sync_now());
    }

    #[test]
    fn test_weekday_prev() {
        assert_eq!(Weekday::Mon.prev(), Weekday::Sun);
        assert_eq!(Weekday::Tue.prev(), Weekday::Mon);
        assert_eq!(Weekday::Wed.prev(), Weekday::Tue);
        assert_eq!(Weekday::Thu.prev(), Weekday::Wed);
        assert_eq!(Weekday::Fri.prev(), Weekday::Thu);
        assert_eq!(Weekday::Sat.prev(), Weekday::Fri);
        assert_eq!(Weekday::Sun.prev(), Weekday::Sat);
    }

    #[test]
    fn test_overnight_carry_over_day() {
        // Monday 22:00-06:00 window, current time: Tuesday 02:00
        // Should be IN window because Tuesday 02:00 is part of Monday's overnight window
        let w = TimeWindow {
            start_hour: 22,
            start_minute: 0,
            end_hour: 6,
            end_minute: 0,
            days: vec![Weekday::Mon],
        };

        // Tuesday 02:00 — after-midnight portion of Monday's overnight window
        // The relevant day is Monday (yesterday), which IS in the days filter
        assert!(w.contains_time_and_day(2, 0, &Weekday::Tue));

        // Monday 23:00 — start portion of Monday's overnight window
        assert!(w.contains_time_and_day(23, 0, &Weekday::Mon));

        // Tuesday 23:00 — start portion, but Tuesday is NOT in the days filter
        assert!(!w.contains_time_and_day(23, 0, &Weekday::Tue));

        // Wednesday 02:00 — after-midnight portion, yesterday is Tuesday, NOT in filter
        assert!(!w.contains_time_and_day(2, 0, &Weekday::Wed));

        // Monday 12:00 — outside the time window entirely
        assert!(!w.contains_time_and_day(12, 0, &Weekday::Mon));
    }

    #[test]
    fn test_overnight_carry_over_day_multiple_days() {
        // Window: Fri+Sat 21:00-03:00
        let w = TimeWindow {
            start_hour: 21,
            start_minute: 0,
            end_hour: 3,
            end_minute: 0,
            days: vec![Weekday::Fri, Weekday::Sat],
        };

        // Friday 22:00 — start portion, Friday IS in filter
        assert!(w.contains_time_and_day(22, 0, &Weekday::Fri));

        // Saturday 01:00 — after-midnight portion, yesterday=Friday IS in filter
        assert!(w.contains_time_and_day(1, 0, &Weekday::Sat));

        // Saturday 22:00 — start portion, Saturday IS in filter
        assert!(w.contains_time_and_day(22, 0, &Weekday::Sat));

        // Sunday 01:00 — after-midnight portion, yesterday=Saturday IS in filter
        assert!(w.contains_time_and_day(1, 0, &Weekday::Sun));

        // Monday 01:00 — after-midnight portion, yesterday=Sunday NOT in filter
        assert!(!w.contains_time_and_day(1, 0, &Weekday::Mon));

        // Thursday 22:00 — start portion, Thursday NOT in filter
        assert!(!w.contains_time_and_day(22, 0, &Weekday::Thu));
    }

    #[test]
    fn test_contains_time_and_day_normal_window() {
        // Normal (non-overnight) window should use today's day as-is
        let w = TimeWindow {
            start_hour: 9,
            start_minute: 0,
            end_hour: 17,
            end_minute: 0,
            days: vec![Weekday::Mon, Weekday::Wed],
        };

        // Monday 12:00 — inside window, Monday allowed
        assert!(w.contains_time_and_day(12, 0, &Weekday::Mon));

        // Wednesday 10:00 — inside window, Wednesday allowed
        assert!(w.contains_time_and_day(10, 0, &Weekday::Wed));

        // Tuesday 12:00 — inside window time, but Tuesday not allowed
        assert!(!w.contains_time_and_day(12, 0, &Weekday::Tue));

        // Monday 20:00 — outside window time
        assert!(!w.contains_time_and_day(20, 0, &Weekday::Mon));
    }

    #[test]
    fn test_contains_time_and_day_empty_days() {
        // Empty days means all days allowed — overnight carry-over irrelevant
        let w = TimeWindow {
            start_hour: 22,
            start_minute: 0,
            end_hour: 6,
            end_minute: 0,
            days: vec![],
        };

        assert!(w.contains_time_and_day(23, 0, &Weekday::Mon));
        assert!(w.contains_time_and_day(2, 0, &Weekday::Tue));
        assert!(w.contains_time_and_day(2, 0, &Weekday::Sun));
        assert!(!w.contains_time_and_day(12, 0, &Weekday::Mon));
    }
}
