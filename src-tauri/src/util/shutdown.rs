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

/// Await the first shutdown signal. Returns which signal fired.
///
/// Calling this more than once is supported but each call installs its own
/// handler; CLI binaries should await it exactly once at the top of their
/// serve loop and propagate the resolution into a `CancellationToken`.
pub async fn shutdown_signal() -> io::Result<ShutdownSignal> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut sigint = signal(SignalKind::interrupt())?;
        let mut sigterm = signal(SignalKind::terminate())?;

        tokio::select! {
            _ = sigint.recv() => Ok(ShutdownSignal::Interrupt),
            _ = sigterm.recv() => Ok(ShutdownSignal::Terminate),
        }
    }

    #[cfg(windows)]
    {
        use tokio::signal::windows::{ctrl_break, ctrl_c};

        let mut c_c = ctrl_c()?;
        let mut c_break = ctrl_break()?;

        tokio::select! {
            _ = c_c.recv() => Ok(ShutdownSignal::Interrupt),
            _ = c_break.recv() => Ok(ShutdownSignal::Terminate),
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        // Fallback for hypothetical targets: SIGINT only.
        tokio::signal::ctrl_c().await?;
        Ok(ShutdownSignal::Interrupt)
    }
}
