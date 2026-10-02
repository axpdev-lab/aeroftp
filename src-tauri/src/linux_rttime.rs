// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Give WebKit's real-time threads a warning before the kernel kills them.
//!
//! WebKitGTK promotes a few threads to `SCHED_RR` through RealtimeKit (the
//! network process `Storage` thread, the web process `EventDispatcher`). A
//! real-time thread may run for `RLIMIT_RTTIME` microseconds without blocking:
//! at the soft limit the kernel sends `SIGXCPU`, at the hard limit `SIGKILL`.
//! WebKit handles `SIGXCPU` by demoting its threads, and it lowers the soft
//! limit to 80% of RealtimeKit's maximum so that warning always comes first,
//! but only when the inherited hard limit is above that maximum.
//!
//! A GNOME session hands every application the limits of gnome-shell, which
//! are exactly RealtimeKit's maximum for both values (200000/200000 us on
//! Ubuntu 26.04). WebKit then changes nothing, the soft limit equals the hard
//! one, and a busy real-time thread gets `SIGKILL` with no warning and no log
//! line. When that thread belongs to the network process, every page loses its
//! in-flight loads and IPC replies: the main window reloads (the chat turn
//! waiting for an approval is gone) or stays blank after a cold start. It
//! happened whenever a second webview started loading next to the main one
//! (the splash, the AeroAgent approval window).
//!
//! Lowering only the soft limit, as WebKit itself would, restores the warning.
//! The WebKit processes inherit the limit, so it has to be set before the
//! first webview exists.

use std::sync::OnceLock;

use libc::rlim_t;
use log::info;

/// What was done at startup, held until a logger exists to receive it.
static DECISION: OnceLock<String> = OnceLock::new();

/// The soft limit to set for these limits, or `None` to leave them alone.
///
/// Mirrors WebKit's own adjustment (80% of the hard limit). Nothing to do when
/// the hard limit is unlimited (WebKit adjusts it itself), zero, or already
/// above the soft one.
pub(crate) fn lowered_soft_limit(soft: rlim_t, hard: rlim_t, infinity: rlim_t) -> Option<rlim_t> {
    if hard == infinity || hard == 0 || soft < hard {
        return None;
    }
    Some(hard - hard / 5)
}

/// Lowers the soft `RLIMIT_RTTIME` of the calling process when it equals the
/// hard one. Returns the new `(soft, hard)` pair when it changed anything.
///
/// Only system calls and arithmetic, so it is also safe between `fork` and
/// `exec`.
fn lower_soft_limit() -> std::io::Result<Option<(rlim_t, rlim_t)>> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a valid, writable rlimit for the call's duration.
    if unsafe { libc::getrlimit(libc::RLIMIT_RTTIME, &mut limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let Some(soft) = lowered_soft_limit(limit.rlim_cur, limit.rlim_max, libc::RLIM_INFINITY) else {
        return Ok(None);
    };
    limit.rlim_cur = soft;
    // SAFETY: `limit` is a valid rlimit; lowering the soft value below the
    // hard one never needs a privilege.
    if unsafe { libc::setrlimit(libc::RLIMIT_RTTIME, &limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(Some((limit.rlim_cur, limit.rlim_max)))
}

/// Applies [`lower_soft_limit`] to this process. Call before any GTK/WebKit
/// initialization; [`log_decision`] reports the outcome once logging is up.
pub fn configure() {
    let message = match lower_soft_limit() {
        Ok(Some((soft, hard))) => format!(
            "RLIMIT_RTTIME soft limit lowered to {soft} us (hard {hard} us) so WebKit real-time threads get SIGXCPU before SIGKILL"
        ),
        Ok(None) => "RLIMIT_RTTIME left as inherited".to_string(),
        Err(e) => format!("RLIMIT_RTTIME could not be adjusted: {e}"),
    };
    let _ = DECISION.set(message);
}

/// Replays the startup decision into the log.
pub fn log_decision() {
    if let Some(message) = DECISION.get() {
        info!("{message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;

    const INF: rlim_t = libc::RLIM_INFINITY;

    #[test]
    fn a_soft_limit_equal_to_the_hard_one_drops_to_eighty_percent() {
        assert_eq!(lowered_soft_limit(200_000, 200_000, INF), Some(160_000));
    }

    #[test]
    fn limits_that_already_warn_or_cannot_be_reached_are_left_alone() {
        assert_eq!(lowered_soft_limit(160_000, 200_000, INF), None);
        assert_eq!(lowered_soft_limit(INF, INF, INF), None);
        assert_eq!(lowered_soft_limit(0, 0, INF), None);
    }

    /// The GNOME case end to end, in a child so the test runner keeps its own
    /// limits: inherit equal soft and hard limits (200000 us, or the runner's
    /// hard limit when lower, since an unprivileged child cannot raise it),
    /// adjust, and read what the next exec'd process (a WebKit helper, in the
    /// app) inherits.
    #[test]
    fn a_process_started_after_the_adjustment_inherits_the_lowered_soft_limit() {
        let mut inherited = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: `inherited` is a valid, writable rlimit.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_RTTIME, &mut inherited) },
            0
        );
        let gnome = inherited.rlim_max.min(200_000);
        if gnome == 0 {
            // A zero hard limit is left alone; nothing to observe.
            return;
        }
        let mut command = std::process::Command::new("cat");
        command.arg("/proc/self/limits");
        // SAFETY: the closure only makes system calls (see `lower_soft_limit`).
        unsafe {
            command.pre_exec(move || {
                let gnome = libc::rlimit {
                    rlim_cur: gnome,
                    rlim_max: gnome,
                };
                if libc::setrlimit(libc::RLIMIT_RTTIME, &gnome) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                lower_soft_limit().map(|_| ())
            });
        }
        let output = command.output().expect("run cat in the adjusted child");
        assert!(output.status.success(), "{output:?}");
        let limits = String::from_utf8_lossy(&output.stdout);
        let line = limits
            .lines()
            .find(|line| line.starts_with("Max realtime timeout"))
            .expect("the kernel reports the real-time limit");
        let values: Vec<&str> = line.split_whitespace().skip(3).take(2).collect();
        let expected = [(gnome - gnome / 5).to_string(), gnome.to_string()];
        assert_eq!(values, expected, "{line}");
    }
}
