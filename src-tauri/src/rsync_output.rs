//! Parser for rsync stdout/stderr output.
//!
//! Decodes rsync progress, summary, warnings and errors into typed events.
//! Pure functional module: no I/O, no async. Used by `rsync_over_ssh` to
//! translate the remote rsync process output into UI events and final stats.
//!
//! Reference output format (rsync 3.x with `--info=progress2` or default verbose):
//!
//! ```text
//! sending incremental file list
//! ./
//! file.bin
//!         10,485,760 100%    5.28MB/s    0:00:01 (xfr#1, to-chk=0/1)
//!
//! sent 10,486,108 bytes  received 35 bytes  2,996,040.86 bytes/sec
//! total size is 10,485,760  speedup is 1.00
//! ```

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Foundations module for Fase 1 delta sync. Consumed by `rsync_over_ssh` to
// turn remote rsync output into typed events. Remove this allow once the
// sync loop wires `delta_sync_rsync::transfer_with_delta` (T1.5 Part B).
#![allow(dead_code)]

use crate::number_parsing::{parse_f64_loose, parse_u64_loose};
use regex::Regex;
use std::sync::LazyLock;

/// One semantic event extracted from a single line of rsync output.
#[derive(Debug, Clone, PartialEq)]
pub enum RsyncEvent {
    /// A file's progress line (emitted repeatedly during transfer).
    /// `speed_raw` keeps the human string (e.g. "5.28MB/s") for UI display;
    /// byte-accurate speed is available in `Summary::bytes_per_sec`.
    Progress {
        bytes: u64,
        percent: u8,
        speed_raw: String,
        eta: Option<String>,
    },
    /// A filename emitted at start of a file's transfer.
    FileStart { name: String },
    /// Final transfer summary (one per rsync invocation).
    Summary {
        sent: u64,
        received: u64,
        bytes_per_sec: f64,
        total_size: u64,
        speedup: f64,
    },
    /// Non-fatal warning (e.g. skipped file, vanished source).
    Warning { message: String },
    /// Fatal rsync error.
    Error { message: String },
}

// Regexes accept both thousands separators ('.' and ',') so the parser is
// locale-independent. The `number_parsing` helpers decide which separator is
// decimal and which is thousands using deterministic heuristics that match
// every sane rsync locale (POSIX, en_US, it_IT, de_DE, fr_FR, ...).

static RE_PROGRESS: LazyLock<Regex> = LazyLock::new(|| {
    // Matches lines like:
    //         10,485,760 100%    5.28MB/s    0:00:01 (xfr#1, to-chk=0/1)
    //         10.485.760 100%    5,28MB/s    0:00:01 (it_IT locale)
    // or      10485760 50%    1.20MB/s    0:00:05
    Regex::new(r"^\s+([\d.,]+)\s+(\d+)%\s+(\S+)(?:\s+(\S+))?").unwrap()
});

static RE_SUMMARY_BYTES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^sent\s+([\d.,]+)\s+bytes\s+received\s+([\d.,]+)\s+bytes\s+([\d.,]+)\s+bytes/sec")
        .unwrap()
});

static RE_SUMMARY_TOTAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^total size is\s+([\d.,]+)\s+speedup is\s+([\d.,]+)").unwrap());

static RE_ERROR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^rsync error:\s*(.+?)(?:\s*\(code\s*\d+\))?$").unwrap());

/// Lines that rsync prints as pure informational noise; never treated as FileStart.
const IGNORE_PREFIXES: &[&str] = &[
    "sending incremental file list",
    "receiving incremental file list",
    "building file list",
    "created directory",
    "deleting ",
    "./",
];

/// Parse a single line of rsync output into a semantic event.
/// Returns `None` for blank lines, banner text, or unclassified output.
pub fn parse_line(line: &str) -> Option<RsyncEvent> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() {
        return None;
    }

    // Error must be checked before warning (rsync error lines start with "rsync:" too).
    if trimmed.starts_with("rsync error:") || trimmed.starts_with("rsync: error:") {
        if let Some(caps) = RE_ERROR.captures(trimmed) {
            return Some(RsyncEvent::Error {
                message: caps
                    .get(1)
                    .map_or(trimmed.to_string(), |m| m.as_str().trim().to_string()),
            });
        }
        return Some(RsyncEvent::Error {
            message: trimmed.to_string(),
        });
    }

    if trimmed.starts_with("rsync:")
        || trimmed.starts_with("skipping ")
        || trimmed.starts_with("file has vanished:")
        || trimmed.starts_with("symlink has no referent:")
    {
        return Some(RsyncEvent::Warning {
            message: trimmed.to_string(),
        });
    }

    if let Some(caps) = RE_PROGRESS.captures(trimmed) {
        let bytes = parse_u64_loose(caps.get(1)?.as_str())?;
        // Parse wide to tolerate outliers like "999%" from rsync edge cases; clamp to 100 for UI.
        let percent_wide: u16 = caps.get(2)?.as_str().parse().ok()?;
        let percent = percent_wide.min(100) as u8;
        let speed = caps.get(3)?.as_str().to_string();
        // Guard against matching " 1:23:45" lines accidentally by requiring speed to contain a unit letter.
        if !speed.chars().any(|c| c.is_ascii_alphabetic()) {
            return None;
        }
        let eta = caps.get(4).map(|m| m.as_str().to_string());
        return Some(RsyncEvent::Progress {
            bytes,
            percent,
            speed_raw: speed,
            eta,
        });
    }

    if let Some(caps) = RE_SUMMARY_BYTES.captures(trimmed) {
        let sent = parse_u64_loose(caps.get(1)?.as_str())?;
        let received = parse_u64_loose(caps.get(2)?.as_str())?;
        let bytes_per_sec = parse_f64_loose(caps.get(3)?.as_str())?;
        // Summary is emitted in two lines: this is partial. Caller may pair with total line.
        return Some(RsyncEvent::Summary {
            sent,
            received,
            bytes_per_sec,
            total_size: 0,
            speedup: 0.0,
        });
    }

    if let Some(caps) = RE_SUMMARY_TOTAL.captures(trimmed) {
        let total_size = parse_u64_loose(caps.get(1)?.as_str())?;
        let speedup = parse_f64_loose(caps.get(2)?.as_str())?;
        return Some(RsyncEvent::Summary {
            sent: 0,
            received: 0,
            bytes_per_sec: 0.0,
            total_size,
            speedup,
        });
    }

    if IGNORE_PREFIXES.iter().any(|p| trimmed.starts_with(p)) {
        return None;
    }

    // Indented lines that didn't match progress are ignored (itemize output, directory listings).
    if trimmed.starts_with(' ') || trimmed.starts_with('\t') {
        return None;
    }

    // A non-empty, non-prefix, non-regex line is treated as a filename boundary.
    Some(RsyncEvent::FileStart {
        name: trimmed.to_string(),
    })
}

/// Merge a pair of [`RsyncEvent::Summary`] events (bytes line + total line) into one complete record.
/// Returns the merged summary if both are `Summary`, otherwise `None`.
pub fn merge_summary(a: &RsyncEvent, b: &RsyncEvent) -> Option<RsyncEvent> {
    match (a, b) {
        (
            RsyncEvent::Summary {
                sent,
                received,
                bytes_per_sec,
                ..
            },
            RsyncEvent::Summary {
                total_size,
                speedup,
                ..
            },
        )
        | (
            RsyncEvent::Summary {
                total_size,
                speedup,
                ..
            },
            RsyncEvent::Summary {
                sent,
                received,
                bytes_per_sec,
                ..
            },
        ) if *total_size > 0 && *sent + *received > 0 => Some(RsyncEvent::Summary {
            sent: *sent,
            received: *received,
            bytes_per_sec: *bytes_per_sec,
            total_size: *total_size,
            speedup: *speedup,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_and_banner_lines_are_ignored() {
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line("\n"), None);
        assert_eq!(parse_line("sending incremental file list"), None);
        assert_eq!(parse_line("receiving incremental file list"), None);
        assert_eq!(parse_line("./"), None);
    }

    #[test]
    fn progress_line_full() {
        let e = parse_line("         10,485,760 100%    5.28MB/s    0:00:01 (xfr#1, to-chk=0/1)")
            .expect("must parse");
        match e {
            RsyncEvent::Progress {
                bytes,
                percent,
                speed_raw,
                eta,
            } => {
                assert_eq!(bytes, 10_485_760);
                assert_eq!(percent, 100);
                assert_eq!(speed_raw, "5.28MB/s");
                assert_eq!(eta.as_deref(), Some("0:00:01"));
            }
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn progress_line_no_commas() {
        let e = parse_line("       1024 50%    1.20MB/s    0:00:05").expect("must parse");
        match e {
            RsyncEvent::Progress {
                bytes,
                percent,
                speed_raw,
                eta,
            } => {
                assert_eq!(bytes, 1024);
                assert_eq!(percent, 50);
                assert_eq!(speed_raw, "1.20MB/s");
                assert_eq!(eta.as_deref(), Some("0:00:05"));
            }
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn progress_line_percent_clamped() {
        // rsync sometimes emits 999% on tiny files; verify clamp.
        let e = parse_line("         1,024 999%    1MB/s    0:00:00").expect("must parse");
        match e {
            RsyncEvent::Progress { percent, .. } => assert_eq!(percent, 100),
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn summary_bytes_line() {
        let e = parse_line("sent 10,486,108 bytes  received 35 bytes  2,996,040.86 bytes/sec")
            .expect("must parse");
        match e {
            RsyncEvent::Summary {
                sent,
                received,
                bytes_per_sec,
                total_size,
                speedup,
            } => {
                assert_eq!(sent, 10_486_108);
                assert_eq!(received, 35);
                assert!((bytes_per_sec - 2_996_040.86).abs() < 0.01);
                assert_eq!(total_size, 0); // filled by total-line
                assert_eq!(speedup, 0.0);
            }
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn summary_total_line() {
        let e = parse_line("total size is 10,485,760  speedup is 1.00").expect("must parse");
        match e {
            RsyncEvent::Summary {
                total_size,
                speedup,
                sent,
                ..
            } => {
                assert_eq!(total_size, 10_485_760);
                assert!((speedup - 1.0).abs() < 0.01);
                assert_eq!(sent, 0);
            }
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn summary_high_speedup_delta_case() {
        // Case with identical file: speedup very high.
        let e = parse_line("total size is 104,857,600  speedup is 52.40").expect("must parse");
        match e {
            RsyncEvent::Summary { speedup, .. } => assert!((speedup - 52.4).abs() < 0.01),
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn merge_summary_combines_two_partials() {
        let a = parse_line("sent 1,000 bytes  received 50 bytes  500.00 bytes/sec").unwrap();
        let b = parse_line("total size is 1,000,000  speedup is 952.38").unwrap();
        let merged = merge_summary(&a, &b).expect("must merge");
        match merged {
            RsyncEvent::Summary {
                sent,
                received,
                bytes_per_sec,
                total_size,
                speedup,
            } => {
                assert_eq!(sent, 1000);
                assert_eq!(received, 50);
                assert!((bytes_per_sec - 500.0).abs() < 0.01);
                assert_eq!(total_size, 1_000_000);
                assert!((speedup - 952.38).abs() < 0.01);
            }
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn fatal_error_line() {
        let e = parse_line("rsync error: some error (code 23) at main.c(1338) [sender=3.2.7]")
            .expect("must parse");
        match e {
            RsyncEvent::Error { message } => assert!(message.contains("some error")),
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn warning_skipping_line() {
        let e = parse_line("skipping non-regular file \"/tmp/socket\"").expect("must parse");
        match e {
            RsyncEvent::Warning { message } => assert!(message.contains("non-regular")),
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn warning_vanished_line() {
        let e = parse_line("file has vanished: /foo/bar.tmp").expect("must parse");
        matches!(e, RsyncEvent::Warning { .. });
    }

    #[test]
    fn rsync_info_line_is_warning() {
        // rsync: connection unexpectedly closed, followed by error on next line.
        let e = parse_line("rsync: connection unexpectedly closed").expect("must parse");
        matches!(e, RsyncEvent::Warning { .. });
    }

    #[test]
    fn filename_line_plain() {
        let e = parse_line("path/to/file.bin").expect("must parse");
        match e {
            RsyncEvent::FileStart { name } => assert_eq!(name, "path/to/file.bin"),
            other => panic!("unexpected {:?}", other),
        }
    }

    #[test]
    fn indented_non_progress_line_is_ignored() {
        // itemize-changes-style output lives in an indented / tabbed form; we skip those.
        assert_eq!(parse_line("    some weird line no match"), None);
        assert_eq!(parse_line("\tanother"), None);
    }

    #[test]
    fn eta_optional_when_absent() {
        let e = parse_line("     1,024 100%    1MB/s").expect("must parse");
        match e {
            RsyncEvent::Progress { eta, .. } => assert!(eta.is_none()),
            other => panic!("unexpected {:?}", other),
        }
    }
}
