// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Dependency-free process resource sampler (DAG-P2-07, block D).
//!
//! Captures a point-in-time snapshot of this process' CPU and memory
//! footprint so a later wiring slice can bracket transfer jobs with
//! start/end samples and report honest per-job resource cost.
//!
//! Platform behavior:
//! - Linux: reads `/proc/self/stat` (CPU ticks), `/proc/self/status`
//!   (`VmRSS`), and counts `/proc/self/fd` entries.
//! - non-Linux: [`ProcessResourceSample::capture`] is compiled to a stub
//!   returning `None` (compile-time `cfg`, not runtime detection).
//!
//! Degradation rules (documented contract):
//! - `capture()` returns `Some` only when utime, stime AND VmRSS parsed.
//! - `fd_count` is best-effort: if `/proc/self/fd` cannot be read the
//!   sample still succeeds with `fd_count == 0`. A live process always
//!   holds at least one fd (the directory read itself), so `0`
//!   unambiguously means "unknown" rather than a real count.

use serde::{Deserialize, Serialize};

/// Point-in-time resource snapshot of the current process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessResourceSample {
    /// User-mode CPU time consumed so far, in nanoseconds.
    pub cpu_user_nanos: u64,
    /// Kernel-mode CPU time consumed so far, in nanoseconds.
    pub cpu_system_nanos: u64,
    /// Current resident set size in bytes (`VmRSS` at capture time).
    pub rss_bytes: u64,
    /// Number of open file descriptors; `0` means "unknown" (see module docs).
    pub fd_count: u32,
}

/// Resource cost of a bracketed job: end minus start.
///
/// Peaks are `Option` because this slice has no periodic sampler; a later
/// slice that samples mid-job can attach observed peaks without changing
/// the struct shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessResourceDelta {
    /// User-mode CPU consumed between start and end, in nanoseconds.
    pub cpu_user_nanos: u64,
    /// Kernel-mode CPU consumed between start and end, in nanoseconds.
    pub cpu_system_nanos: u64,
    /// RSS at job start, in bytes.
    pub rss_start_bytes: u64,
    /// RSS at job end, in bytes.
    pub rss_end_bytes: u64,
    /// Highest RSS observed mid-job, if a periodic sampler was attached.
    pub rss_peak_observed: Option<u64>,
    /// Highest fd count observed mid-job, if a periodic sampler was attached.
    pub fd_peak_observed: Option<u32>,
    /// Open fds at job start.
    pub fd_start_count: u32,
    /// Open fds at job end; growth past start hints at an fd leak.
    pub fd_end_count: u32,
}

impl ProcessResourceDelta {
    /// Computes the delta between two samples. Counters use saturating
    /// subtraction so a non-monotonic source never underflows.
    pub fn between(start: &ProcessResourceSample, end: &ProcessResourceSample) -> Self {
        Self {
            cpu_user_nanos: end.cpu_user_nanos.saturating_sub(start.cpu_user_nanos),
            cpu_system_nanos: end.cpu_system_nanos.saturating_sub(start.cpu_system_nanos),
            rss_start_bytes: start.rss_bytes,
            rss_end_bytes: end.rss_bytes,
            rss_peak_observed: None,
            fd_peak_observed: None,
            fd_start_count: start.fd_count,
            fd_end_count: end.fd_count,
        }
    }
}

/// Bracket handle: capture with [`ResourceSampleGuard::begin`], run the job,
/// close with [`ResourceSampleGuard::finish`]. No background thread; peaks
/// stay `None` until a later slice attaches a periodic sampler.
#[derive(Debug, Clone, Copy)]
pub struct ResourceSampleGuard {
    start: ProcessResourceSample,
}

impl ResourceSampleGuard {
    /// Captures the start sample. Returns `None` where sampling is
    /// unsupported or `/proc` parsing failed (same rules as `capture()`).
    pub fn begin() -> Option<Self> {
        ProcessResourceSample::capture().map(|start| Self { start })
    }

    /// Captures the end sample and computes the delta. Returns `None` if
    /// the end capture fails.
    pub fn finish(self) -> Option<ProcessResourceDelta> {
        ProcessResourceSample::capture().map(|end| ProcessResourceDelta::between(&self.start, &end))
    }
}

impl ProcessResourceSample {
    /// Captures a snapshot of the current process. See module docs for the
    /// degradation contract. Cost: three small `/proc` reads plus one
    /// directory scan; no allocation-heavy work, no blocking I/O beyond
    /// the kernel's pseudo-filesystem.
    #[cfg(target_os = "linux")]
    pub fn capture() -> Option<Self> {
        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
        let (utime_ticks, stime_ticks) = parse_stat_cpu_ticks(&stat)?;
        let hz = clock_ticks_per_sec();
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let rss_bytes = parse_status_vm_rss(&status)?;
        Some(Self {
            cpu_user_nanos: ticks_to_nanos(utime_ticks, hz),
            cpu_system_nanos: ticks_to_nanos(stime_ticks, hz),
            rss_bytes,
            fd_count: count_fds().unwrap_or(0),
        })
    }

    /// Non-Linux stub: always `None` (compile-time platform gate).
    #[cfg(not(target_os = "linux"))]
    pub fn capture() -> Option<Self> {
        None
    }
}

/// Parses utime/stime (fields 14/15, 1-based) from a `/proc/self/stat` line.
///
/// `comm` (field 2) may contain spaces and parentheses, so naive whitespace
/// splitting misaligns every later field. Anchor on the LAST ')' instead:
/// everything after it is whitespace-separated starting at field 3.
fn parse_stat_cpu_ticks(stat: &str) -> Option<(u64, u64)> {
    let after_comm = stat.get(stat.rfind(')')? + 1..)?;
    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    // fields[0] is field 3 (state), so field n sits at index n - 3.
    let utime = fields.get(14 - 3)?.parse::<u64>().ok()?;
    let stime = fields.get(15 - 3)?.parse::<u64>().ok()?;
    Some((utime, stime))
}

/// Parses `VmRSS` from `/proc/self/status`, returning bytes.
///
/// Chosen over `/proc/self/statm` because VmRSS is reported directly in kB,
/// while statm's resident count is in pages and would need a second
/// page-size lookup to convert. VmRSS also matches what standard tools
/// (top, ps) report as RSS.
fn parse_status_vm_rss(status: &str) -> Option<u64> {
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest
                .trim()
                .strip_suffix("kB")
                .unwrap_or_else(|| rest.trim())
                .trim()
                .parse()
                .ok()?;
            return Some(kb.saturating_mul(1024));
        }
    }
    None
}

/// Counts entries of `/proc/self/fd`. `None` means the directory could not
/// be read at all; callers treat that as "unknown", not as zero fds.
fn count_fds() -> Option<u32> {
    let entries = std::fs::read_dir("/proc/self/fd").ok()?;
    let mut count: u32 = 0;
    for entry in entries {
        if entry.is_ok() {
            count = count.saturating_add(1);
        }
    }
    Some(count)
}

/// Clock ticks per second (CLK_TCK) for tick-to-nanos conversion.
///
/// `libc` is already in the dependency tree for unix targets, so ask the
/// kernel via `sysconf(_SC_CLK_TCK)`. Falls back to 100 Hz (the value on
/// virtually every Linux build) only if sysconf itself fails.
#[cfg(target_os = "linux")]
fn clock_ticks_per_sec() -> u64 {
    // SAFETY: `_SC_CLK_TCK` is a valid sysconf name on every Linux libc;
    // the call has no side effects and cannot touch memory.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if hz > 0 {
        hz as u64
    } else {
        // LOUD FALLBACK: 100 Hz is the near-universal default USER_HZ, but a
        // failed sysconf means we are guessing; CPU nanos may be off by the
        // ratio between the real CLK_TCK and 100.
        100
    }
}

/// Converts clock ticks to nanoseconds via a u128 intermediate so large
/// uptimes cannot overflow the multiply.
fn ticks_to_nanos(ticks: u64, hz: u64) -> u64 {
    if hz == 0 {
        return 0;
    }
    (u128::from(ticks) * 1_000_000_000 / u128::from(hz)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    // Realistic /proc/self/stat shape with the classic edge case: comm
    // contains both spaces AND parentheses, including a ')' inside.
    const STAT_TRICKY: &str = "12345 (weird (proc) name) S 1234 12345 12345 \
        34816 12400 4194304 1000 5000 0 0 40 20 20 0 1 0 500 12345678 2500 \
        18446744073709551615 4194304 4300000 140700000000000 0 0 0 0 0 0 0 0 0 17 1 0 0 0 0 0";

    #[test]
    fn parse_stat_handles_comm_with_spaces_and_parens() {
        // utime field 14 = 40 ticks, stime field 15 = 20 ticks; anything
        // shifted by the tricky comm would land on 4194304/1000 instead.
        let (utime, stime) = parse_stat_cpu_ticks(STAT_TRICKY).expect("tricky stat parses");
        assert_eq!(utime, 40);
        assert_eq!(stime, 20);
    }

    #[test]
    fn parse_stat_rejects_truncated_line() {
        assert_eq!(parse_stat_cpu_ticks("1 (x) S 2 3"), None);
    }

    #[test]
    fn parse_status_extracts_vm_rss() {
        let status = "Name:\taeroftp\nState:\tS (sleeping)\nVmPeak:\t  999999 kB\n\
                      VmRSS:\t   12345 kB\nVmHWM:\t   20000 kB\nThreads:\t4\n";
        assert_eq!(parse_status_vm_rss(status), Some(12345 * 1024));
    }

    #[test]
    fn parse_status_rejects_missing_vm_rss() {
        assert_eq!(parse_status_vm_rss("Name:\tx\nThreads:\t1\n"), None);
    }

    #[test]
    fn ticks_to_nanos_converts_and_survives_zero_hz() {
        assert_eq!(ticks_to_nanos(150, 100), 1_500_000_000);
        assert_eq!(ticks_to_nanos(0, 100), 0);
        assert_eq!(ticks_to_nanos(10, 0), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn capture_returns_populated_sample_on_linux() {
        let s = ProcessResourceSample::capture().expect("capture works on Linux");
        assert!(s.rss_bytes > 0, "a live process has non-zero RSS");
        assert!(
            s.fd_count >= 3,
            "stdin/stdout/stderr guarantee >= 3 fds, got {}",
            s.fd_count
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cpu_user_is_monotonic_across_busy_work() {
        let before = ProcessResourceSample::capture().expect("first capture");
        // Brief busy loop: burns user CPU without sleeping.
        let mut acc: u64 = 0;
        for i in 0..1_000_000u64 {
            acc = acc.wrapping_add(i.wrapping_mul(31));
        }
        std::hint::black_box(acc);
        let after = ProcessResourceSample::capture().expect("second capture");
        assert!(
            after.cpu_user_nanos >= before.cpu_user_nanos,
            "user CPU must not regress: {} < {}",
            after.cpu_user_nanos,
            before.cpu_user_nanos
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn guard_delta_brackets_touched_allocation() {
        let guard = ResourceSampleGuard::begin().expect("begin captures");
        // Allocate and TOUCH a few MiB so the pages are genuinely resident
        // before the end sample; keep the buffer alive across finish().
        let mut buf = vec![0u8; 8 * 1024 * 1024];
        for chunk in buf.chunks_mut(4096) {
            chunk[0] = 1;
        }
        let delta = guard.finish().expect("finish captures");
        std::hint::black_box(&buf);
        // RSS growth from touched pages is reliable, but allocators may
        // already own the heap; assert the contract (populated fields,
        // end not below start) rather than an exact byte count.
        assert!(delta.rss_start_bytes > 0);
        assert!(delta.rss_end_bytes >= delta.rss_start_bytes);
        // fd counts are process-global while `cargo test` runs sibling tests
        // on parallel threads that open/close their own fds, so an ordering
        // assertion between start and end would be racy. Assert population
        // instead: a live test process always holds the three std fds.
        assert!(delta.fd_start_count >= 3);
        assert!(delta.fd_end_count >= 3);
        assert!(delta.rss_peak_observed.is_none(), "no periodic sampler yet");
        assert!(delta.fd_peak_observed.is_none());
    }

    #[test]
    fn delta_between_saturates_on_non_monotonic_counters() {
        let start = ProcessResourceSample {
            cpu_user_nanos: 100,
            cpu_system_nanos: 50,
            rss_bytes: 10,
            fd_count: 4,
        };
        let end = ProcessResourceSample {
            cpu_user_nanos: 90,
            cpu_system_nanos: 70,
            rss_bytes: 12,
            fd_count: 6,
        };
        let d = ProcessResourceDelta::between(&start, &end);
        assert_eq!(d.cpu_user_nanos, 0, "regression saturates to zero");
        assert_eq!(d.cpu_system_nanos, 20);
        assert_eq!(d.rss_start_bytes, 10);
        assert_eq!(d.rss_end_bytes, 12);
        assert_eq!(d.fd_start_count, 4);
        assert_eq!(d.fd_end_count, 6);
    }

    // Non-Linux path: `capture()` compiles to a `None` stub behind
    // `#[cfg(not(target_os = "linux"))]`. It cannot execute on this host;
    // its compilation is covered by cross-checking a non-Linux target
    // (out of scope for this wave's gates).
}
