// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Cross-platform graceful shutdown signal.
//!
//! Prior to this helper each CLI subcommand hand-rolled its own
//! `tokio::signal::ctrl_c()` await, which covered only SIGINT. systemd,
//! Docker, and any well-behaved supervisor default to SIGTERM, and those
//! deliveries were silently dropped: the process only died on SIGKILL,
//! skipping provider disconnects, SQLite WAL checkpoints, FUSE unmount,
//! and axum graceful drain.
//!
//! `shutdown_signal()` returns a future that completes on the first of:
//! * SIGINT (Ctrl+C on all platforms),
//! * SIGTERM (Unix supervisors),
//! * Ctrl+Break on Windows (console).
//!
//! Combine with axum's `with_graceful_shutdown(...)`, `tokio::select!`, or
//! a `CancellationToken` to trigger coordinated teardown.

use std::future::Future;
use std::io;

/// Which signal ended up triggering shutdown. Exposed so callers can log
/// it for postmortem use; most call sites just need the future to resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownSignal {
    /// Ctrl+C / SIGINT.
    Interrupt,
    /// SIGTERM (Unix) or Ctrl+Break (Windows console).
    Terminate,
}

/// The first shutdown signal, listened to from the moment this is called:
/// a signal that arrives before the returned future is first awaited still
/// ends it. Returns which signal fired.
///
/// Call it before telling anyone the process is ready to be stopped (a
/// "Press Ctrl+C" banner, a mount, a daemon's runtime files): it listened
/// only once first polled, and a SIGTERM sent in between killed the process
/// without its teardown. It must be called inside a Tokio runtime.
///
/// Calling this more than once is supported but each call installs its own
/// listeners; CLI binaries should take it exactly once per serve loop and
/// propagate the resolution into a `CancellationToken`.
pub fn shutdown_signal() -> impl Future<Output = io::Result<ShutdownSignal>> + Send + 'static {
    #[cfg(unix)]
    let listening = {
        use tokio::signal::unix::{signal, SignalKind};
        signal(SignalKind::interrupt())
            .and_then(|interrupt| Ok((interrupt, signal(SignalKind::terminate())?)))
    };

    #[cfg(windows)]
    let listening = {
        use tokio::signal::windows::{ctrl_break, ctrl_c};
        ctrl_c().and_then(|interrupt| Ok((interrupt, ctrl_break()?)))
    };

    async move {
        #[cfg(any(unix, windows))]
        {
            let (mut interrupt, mut terminate) = listening?;
            tokio::select! {
                _ = interrupt.recv() => Ok(ShutdownSignal::Interrupt),
                _ = terminate.recv() => Ok(ShutdownSignal::Terminate),
            }
        }

        #[cfg(not(any(unix, windows)))]
        {
            // Fallback for hypothetical targets: SIGINT only.
            tokio::signal::ctrl_c().await?;
            Ok(ShutdownSignal::Interrupt)
        }
    }
}
