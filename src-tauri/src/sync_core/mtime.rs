//! One rule for "the same modification time" on every sync surface: the GUI
//! compare, the CLI `sync` (one way and `both`), and the MCP / AeroAgent
//! `sync_tree`.
//!
//! Until this module each surface kept its own window (the GUI 30 s, the CLI
//! 2 s one way and 0 s in `both`, `sync_tree` 0 s), so the same pair of files
//! could be identical in one and a conflict in another. The rule now:
//!
//! - the window is 2 s ([`DEFAULT_MODIFY_WINDOW`]), or what the caller asks
//!   for (`--modify-window`), raised to the coarser precision of the two sides
//!   when a backend keeps its times more coarsely than that;
//! - when either side keeps no comparable time (precision `None`: an FTP
//!   listing read through `LIST`, whose dates are server-local with no zone),
//!   there is no window at all: the files are compared by size only, and the
//!   caller says so.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::time::Duration;

/// The narrowest window: FAT keeps 2 s, FTP `MDTM` rounds to the second, and
/// the clocks of two machines never agree exactly.
pub const DEFAULT_MODIFY_WINDOW: Duration = Duration::from_secs(2);

/// A local filesystem mtime as the sync reads it: whole seconds. FAT's 2 s is
/// covered by the default window.
pub const LOCAL_MTIME_PRECISION: Option<Duration> = Some(Duration::from_secs(1));

/// How the modification times of a pair of stores compare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModifyWindow {
    /// Two times this many seconds apart, or closer, are the same instant.
    Seconds { secs: u64 },
    /// One side keeps no comparable time: the files are compared by size only.
    SizeOnly { reason: SizeOnlyReason },
}

/// Why a pair of stores is compared by size only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SizeOnlyReason {
    /// An FTP server read through `LIST` (no MLSD): its dates are the
    /// server's local time with no zone.
    FtpListDates,
    /// The backend reports no modification time that can be compared.
    NoComparableTime,
}

impl SizeOnlyReason {
    /// The reason to state for a backend whose precision is unknown.
    pub fn for_provider(provider: crate::providers::ProviderType) -> Self {
        match provider {
            crate::providers::ProviderType::Ftp | crate::providers::ProviderType::Ftps => {
                Self::FtpListDates
            }
            _ => Self::NoComparableTime,
        }
    }
}

impl Default for ModifyWindow {
    fn default() -> Self {
        Self::Seconds {
            secs: DEFAULT_MODIFY_WINDOW.as_secs(),
        }
    }
}

impl ModifyWindow {
    /// The window for two stores: the requested one (2 s when none), raised
    /// to the coarser precision of the two, or size only (for `unknown`) when
    /// either precision is unknown. A requested window never makes an unknown
    /// precision comparable.
    pub fn resolve(
        requested: Option<Duration>,
        a: Option<Duration>,
        b: Option<Duration>,
        unknown: SizeOnlyReason,
    ) -> Self {
        let (Some(a), Some(b)) = (a, b) else {
            return Self::SizeOnly { reason: unknown };
        };
        let widest = requested.unwrap_or(DEFAULT_MODIFY_WINDOW).max(a).max(b);
        // A sub-second precision rounds up to the whole second it lives in.
        let secs = widest.as_secs() + u64::from(widest.subsec_nanos() > 0);
        Self::Seconds { secs }
    }

    /// The window between the local filesystem and `provider`.
    pub fn against_provider<P: crate::providers::StorageProvider + ?Sized>(
        requested: Option<Duration>,
        provider: &P,
    ) -> Self {
        Self::resolve(
            requested,
            LOCAL_MTIME_PRECISION,
            provider.mtime_precision(),
            SizeOnlyReason::for_provider(provider.provider_type()),
        )
    }

    /// The legacy FTP session (`crate::ftp::FtpManager`) lists with `LIST`
    /// only, so its dates are never comparable.
    pub const LEGACY_FTP: Self = Self::SizeOnly {
        reason: SizeOnlyReason::FtpListDates,
    };

    /// Whether the times are compared at all.
    pub fn compares_times(self) -> bool {
        matches!(self, Self::Seconds { .. })
    }

    /// Order two Unix times: `Equal` inside the window, `None` when the window
    /// is size only or either time is unknown.
    pub fn order(self, a: Option<i64>, b: Option<i64>) -> Option<Ordering> {
        let Self::Seconds { secs } = self else {
            return None;
        };
        let (a, b) = (a?, b?);
        let window = i64::try_from(secs).unwrap_or(i64::MAX);
        if a.abs_diff(b) <= window.unsigned_abs() {
            Some(Ordering::Equal)
        } else {
            Some(a.cmp(&b))
        }
    }

    /// [`Self::order`] on two provider-reported times, read with the one
    /// parser every comparison uses ([`crate::parse_remote_mtime`]).
    pub fn order_text(self, a: Option<&str>, b: Option<&str>) -> Option<Ordering> {
        self.order(
            a.and_then(crate::parse_remote_mtime),
            b.and_then(crate::parse_remote_mtime),
        )
    }

    /// A short statement of the rule, for logs and CLI notices.
    pub fn describe(self) -> String {
        match self {
            Self::Seconds { secs } => format!("modification times compared within {secs} s"),
            Self::SizeOnly {
                reason: SizeOnlyReason::FtpListDates,
            } => "the FTP server lists without MLSD, so its dates are server-local with no zone: files compared by size only".to_string(),
            Self::SizeOnly {
                reason: SizeOnlyReason::NoComparableTime,
            } => "the backend reports no comparable modification time: files compared by size only".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: fn(u64) -> Option<Duration> = |s| Some(Duration::from_secs(s));
    const R: SizeOnlyReason = SizeOnlyReason::NoComparableTime;

    /// The field the GUI reads from `CompareReport.modify_window`: its JSON is
    /// a contract with the frontend.
    #[test]
    fn the_report_field_serializes_as_the_frontend_reads_it() {
        assert_eq!(
            serde_json::to_value(ModifyWindow::default()).unwrap(),
            serde_json::json!({"kind": "seconds", "secs": 2})
        );
        assert_eq!(
            serde_json::to_value(ModifyWindow::SizeOnly {
                reason: SizeOnlyReason::FtpListDates
            })
            .unwrap(),
            serde_json::json!({"kind": "size_only", "reason": "ftp_list_dates"})
        );
        assert_eq!(
            serde_json::to_value(ModifyWindow::SizeOnly {
                reason: SizeOnlyReason::NoComparableTime
            })
            .unwrap(),
            serde_json::json!({"kind": "size_only", "reason": "no_comparable_time"})
        );
    }

    #[test]
    fn the_window_is_two_seconds_raised_to_the_coarser_side() {
        assert_eq!(
            ModifyWindow::resolve(None, S(1), S(1), R),
            ModifyWindow::Seconds { secs: 2 }
        );
        assert_eq!(
            ModifyWindow::resolve(None, S(1), S(60), R),
            ModifyWindow::Seconds { secs: 60 }
        );
        assert_eq!(
            ModifyWindow::resolve(Some(Duration::from_secs(10)), S(1), S(1), R),
            ModifyWindow::Seconds { secs: 10 }
        );
        // A requested window narrower than a side's precision is raised.
        assert_eq!(
            ModifyWindow::resolve(Some(Duration::ZERO), S(1), S(1), R),
            ModifyWindow::Seconds { secs: 1 }
        );
        assert_eq!(
            ModifyWindow::resolve(Some(Duration::from_millis(2500)), S(1), S(1), R),
            ModifyWindow::Seconds { secs: 3 }
        );
    }

    #[test]
    fn an_unknown_precision_is_size_only_whatever_is_requested() {
        let size_only = ModifyWindow::SizeOnly { reason: R };
        assert_eq!(ModifyWindow::resolve(None, S(1), None, R), size_only);
        assert_eq!(
            ModifyWindow::resolve(Some(Duration::from_secs(3600)), None, S(1), R),
            size_only
        );
        assert_eq!(size_only.order(Some(1), Some(1000)), None);
    }

    #[test]
    fn order_is_equal_inside_the_window_and_unknown_without_a_time() {
        let w = ModifyWindow::Seconds { secs: 2 };
        assert_eq!(w.order(Some(100), Some(102)), Some(Ordering::Equal));
        assert_eq!(w.order(Some(102), Some(100)), Some(Ordering::Equal));
        assert_eq!(w.order(Some(103), Some(100)), Some(Ordering::Greater));
        assert_eq!(w.order(Some(100), Some(103)), Some(Ordering::Less));
        assert_eq!(w.order(None, Some(100)), None);
        assert_eq!(
            w.order_text(
                Some("2026-09-24 19:41:46"),
                Some("Thu, 24 Sep 2026 19:41:47 GMT")
            ),
            Some(Ordering::Equal)
        );
        // An FTP `LIST` date is not an instant, so it orders as unknown.
        assert_eq!(
            w.order_text(Some("2026-09-24 19:41"), Some("2026-09-24 19:41:00")),
            None
        );
    }
}
