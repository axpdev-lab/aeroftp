//! SSH remote exec primitive.
//!
//! Opens a new SSH session channel on an existing, authenticated [`russh::client::Handle`]
//! and requests execution of an arbitrary command (RFC 4254 §6.5). Returns an [`ExecSession`]
//! exposing raw stdin / stdout / stderr streams plus a oneshot receiver for the remote exit
//! status.
//!
//! Unlike `ssh_shell.rs`, this module does **not** allocate a PTY. Streams are byte-pipes,
//! safe for binary protocols such as `rsync --server`.
//!
//! # Design
//! - `handle` is shared via `Arc<TokioMutex<Handle<H>>>` (see `providers::sftp::SharedSshHandle`)
//!   so that an active SFTP session can spin up additional exec channels without
//!   re-authenticating.
//! - The channel is split into read/write halves. A detached reader task drains
//!   `ChannelMsg::Data` into an mpsc for stdout and `ChannelMsg::ExtendedData { ext: 1 }`
//!   into an mpsc for stderr. Exit status is delivered over a oneshot.
//! - stdin is a `'static` `AsyncWrite` obtained from the write half (russh clones the
//!   underlying mpsc sender, so detached use is safe).
//! - When [`ExecSession`] is dropped the reader task is aborted; remote process receives
//!   EOF and, if it respects it, terminates.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Foundations module for Fase 1 delta sync. Public API is consumed by
// `rsync_over_ssh::probe_rsync` and future AI agent tools (server_exec).
// Items appear "never used" until T1.5 Part B wires the delta-sync branch
// in `sync.rs`: remove this allow when that integration lands.
#![allow(dead_code)]

use russh::client::{Handle, Handler};
use russh::ChannelMsg;
use std::sync::Arc;
use tokio::io::AsyncWrite;
use tokio::sync::{mpsc, oneshot, Mutex as TokioMutex};

/// Handle to a running remote command.
///
/// Drop to tear down: the inner reader task is aborted and the write half is released,
/// which (combined with EOF propagation) will typically signal the remote process to exit.
pub struct ExecSession {
    pub stdin: Box<dyn AsyncWrite + Send + Unpin>,
    pub stdout_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    pub stderr_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    pub exit_rx: oneshot::Receiver<u32>,
    _reader: ReaderGuard,
}

/// RAII guard that aborts the reader task when the session is dropped.
struct ReaderGuard(tokio::task::JoinHandle<()>);

impl Drop for ReaderGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Sentinel value used when the remote exits without sending an ExitStatus message.
/// Chosen to match the de-facto convention used by OpenSSH clients when the channel
/// is torn down abnormally.
pub const EXIT_ABNORMAL: u32 = 255;

/// Loop-control decision derived from a channel message.
///
/// Factored out of the async reader so the `Eof` / `Close` /
/// late-`ExitStatus` ordering (the part that must never regress) is
/// deterministically unit-testable without a live SSH channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReaderControl {
    /// Keep reading with the normal unbounded wait.
    Continue,
    /// `Eof` observed: keep reading (a late `ExitStatus` may still arrive
    /// before `Close`) but bound subsequent waits.
    Drain,
    /// Channel definitively closed: stop the reader.
    Stop,
}

/// Tracks the authoritative remote exit across the reader's lifetime.
///
/// The first `ExitStatus` wins; `ExitSignal` maps to [`EXIT_ABNORMAL`]; if
/// the channel ends without either, [`resolve`](Self::resolve) yields
/// [`EXIT_ABNORMAL`] (the genuine-abnormal contract). This mirrors the
/// previous take-once `oneshot` semantics, decoupled from the I/O loop.
#[derive(Debug, Default)]
struct ExitTracker {
    code: Option<u32>,
}

impl ExitTracker {
    /// Fold one channel message; returns what the reader loop should do.
    /// Payload (stdout/stderr) forwarding stays in the async loop so this
    /// is pure and side-effect free.
    fn observe(&mut self, msg: &ChannelMsg) -> ReaderControl {
        match msg {
            ChannelMsg::ExitStatus { exit_status } => {
                self.code.get_or_insert(*exit_status);
                ReaderControl::Continue
            }
            ChannelMsg::ExitSignal { .. } => {
                self.code.get_or_insert(EXIT_ABNORMAL);
                ReaderControl::Continue
            }
            ChannelMsg::Eof => ReaderControl::Drain,
            ChannelMsg::Close => ReaderControl::Stop,
            // Data / ExtendedData / window adjustments / etc. are inert for
            // exit accounting (their payloads are handled by the loop).
            _ => ReaderControl::Continue,
        }
    }

    /// Whether an authoritative status/signal has already been observed.
    fn is_resolved(&self) -> bool {
        self.code.is_some()
    }

    /// Exit code to deliver once the channel terminates definitively.
    fn resolve(&self) -> u32 {
        self.code.unwrap_or(EXIT_ABNORMAL)
    }
}

/// Open a remote exec channel on `handle_shared` and run `command`.
///
/// Returns immediately after the exec request has been acknowledged; caller pumps I/O
/// by reading `stdout_rx` / `stderr_rx` and writing to `stdin`. Remote exit status is
/// delivered on `exit_rx` (lossy if the process exits abnormally: `EXIT_ABNORMAL`).
///
/// The handle mutex is held only for the duration of `channel_open_session()` + `exec()`
/// (typically microseconds); concurrent SFTP operations on the same handle are not
/// measurably impacted.
pub async fn ssh_exec<H>(
    handle_shared: Arc<TokioMutex<Handle<H>>>,
    command: &str,
) -> Result<ExecSession, String>
where
    H: Handler<Error = russh::Error> + Send + Sync + 'static,
{
    let channel = {
        let guard = handle_shared.lock().await;
        guard
            .channel_open_session()
            .await
            .map_err(|e| format!("ssh exec: open channel: {}", e))?
    };

    channel
        .exec(true, command.as_bytes())
        .await
        .map_err(|e| format!("ssh exec: request: {}", e))?;

    let (mut read_half, write_half) = channel.split();
    let stdin: Box<dyn AsyncWrite + Send + Unpin> = Box::new(write_half.make_writer());

    let (stdout_tx, stdout_rx) = mpsc::unbounded_channel();
    let (stderr_tx, stderr_rx) = mpsc::unbounded_channel();
    let (exit_tx, exit_rx) = oneshot::channel::<u32>();

    let reader_task = tokio::spawn(async move {
        let mut tracker = ExitTracker::default();
        // After `Eof` a well-behaved server still owes us `exit-status`
        // and then `Close` (RFC 4254 §6.10 permits `exit-status` to follow
        // `Eof`). Breaking on `Eof` would drop a late `exit-status` and
        // wrongly synthesize `EXIT_ABNORMAL`. So we keep draining past
        // `Eof` until `Close`, but bound that post-`Eof` wait so a server
        // that never sends `Close` cannot wedge the reader task.
        const EOF_GRACE: std::time::Duration = std::time::Duration::from_secs(5);
        let mut draining = false;
        'outer: loop {
            let msg = if draining {
                match tokio::time::timeout(EOF_GRACE, read_half.wait()).await {
                    Ok(Some(m)) => m,
                    // Stream ended, or the grace elapsed without `Close`:
                    // stop (do not hang). `tracker.resolve()` preserves the
                    // genuine-abnormal sentinel when no status was seen.
                    Ok(None) | Err(_) => break 'outer,
                }
            } else {
                match read_half.wait().await {
                    Some(m) => m,
                    None => break 'outer,
                }
            };
            // Forward payload before interpreting control so the server's
            // stdout/stderr ordering is preserved. The send happens in the
            // guard; a closed consumer (send error) tears the reader down.
            match &msg {
                ChannelMsg::Data { data } if stdout_tx.send(data.to_vec()).is_err() => {
                    break 'outer;
                }
                // SSH_EXTENDED_DATA_STDERR = 1 per RFC 4254 §5.2.
                // Other ext streams are silently discarded.
                ChannelMsg::ExtendedData { data, ext: 1 }
                    if stderr_tx.send(data.to_vec()).is_err() =>
                {
                    break 'outer;
                }
                _ => {}
            }
            match tracker.observe(&msg) {
                ReaderControl::Continue => {}
                ReaderControl::Drain => {
                    // Already have an authoritative status: nothing useful
                    // can still arrive, so stop now instead of paying the
                    // grace wait for a `Close` we no longer need.
                    if tracker.is_resolved() {
                        break 'outer;
                    }
                    draining = true;
                }
                ReaderControl::Stop => break 'outer,
            }
        }
        // Delivered once, at definitive channel termination.
        let _ = exit_tx.send(tracker.resolve());
    });

    Ok(ExecSession {
        stdin,
        stdout_rx,
        stderr_rx,
        exit_rx,
        _reader: ReaderGuard(reader_task),
    })
}

/// Convenience helper: execute `command`, collect stdout to end, discard stderr, return exit.
///
/// Useful for one-shot probes (e.g. `command -v rsync`, `rsync --version`) where streaming
/// is unnecessary. Returns `(stdout_bytes, exit_code)`. Stderr is captured separately.
pub async fn ssh_exec_collect<H>(
    handle_shared: Arc<TokioMutex<Handle<H>>>,
    command: &str,
    max_output_bytes: usize,
) -> Result<(Vec<u8>, Vec<u8>, u32), String>
where
    H: Handler<Error = russh::Error> + Send + Sync + 'static,
{
    let mut session = ssh_exec(handle_shared, command).await?;

    let mut stdout = Vec::<u8>::new();
    let mut stderr = Vec::<u8>::new();

    loop {
        tokio::select! {
            biased;
            msg = session.stdout_rx.recv() => match msg {
                Some(chunk) => {
                    if stdout.len() + chunk.len() > max_output_bytes {
                        let remaining = max_output_bytes.saturating_sub(stdout.len());
                        stdout.extend_from_slice(&chunk[..remaining]);
                    } else {
                        stdout.extend_from_slice(&chunk);
                    }
                }
                None => break,
            },
            msg = session.stderr_rx.recv() => {
                if let Some(chunk) = msg {
                    if stderr.len() + chunk.len() <= max_output_bytes {
                        stderr.extend_from_slice(&chunk);
                    }
                }
            },
        }
    }

    // Drain any remaining stderr.
    while let Ok(chunk) = session.stderr_rx.try_recv() {
        if stderr.len() + chunk.len() <= max_output_bytes {
            stderr.extend_from_slice(&chunk);
        }
    }

    let exit = session.exit_rx.await.unwrap_or(EXIT_ABNORMAL);
    Ok((stdout, stderr, exit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::ChannelMsg;

    /// Faithful, synchronous re-enactment of the reader task's control
    /// flow (the only thing dropped is the async I/O and the post-`Eof`
    /// `tokio::time::timeout`; an exhausted message slice models exactly
    /// the two non-`Close` terminations: stream end and grace expiry).
    /// Production and this driver share `ExitTracker::observe` /
    /// `is_resolved` / `resolve`, so the regression is locked at its
    /// source: if `Eof` ever maps back to `Stop`, the late-`ExitStatus`
    /// cases below flip to 255 and fail.
    fn drive(msgs: Vec<ChannelMsg>) -> u32 {
        let mut tracker = ExitTracker::default();
        let mut draining = false;
        for msg in msgs {
            match tracker.observe(&msg) {
                ReaderControl::Continue => {}
                ReaderControl::Drain => {
                    if tracker.is_resolved() {
                        break;
                    }
                    draining = true;
                }
                ReaderControl::Stop => break,
            }
        }
        let _ = draining; // mirrors the loop's bounded-wait latch
        tracker.resolve()
    }

    fn exit_signal() -> ChannelMsg {
        ChannelMsg::ExitSignal {
            signal_name: russh::Sig::TERM,
            core_dumped: false,
            error_message: String::new(),
            lang_tag: String::new(),
        }
    }

    // `Data`/`ExtendedData` need a `bytes::Bytes` (not a direct dep); for
    // exit accounting they are inert and classify exactly like any other
    // payload-bearing message via the `_` arm. `Success` is a
    // construct-without-bytes stand-in for "a non-control message arrived
    // before EOF" and proves it does not perturb the late-exit capture.
    fn data_like() -> ChannelMsg {
        ChannelMsg::Success
    }

    #[test]
    fn late_exit_status_between_eof_and_close_is_captured() {
        // The headline regression: server sends data, EOF, THEN the real
        // exit-status, THEN Close. The old `break` on `Eof` dropped the
        // status and synthesized 255; we must report the true 0.
        let exit = drive(vec![
            data_like(),
            ChannelMsg::Eof,
            ChannelMsg::ExitStatus { exit_status: 0 },
            ChannelMsg::Close,
        ]);
        assert_eq!(exit, 0, "late exit-status after EOF must win, not 255");
    }

    #[test]
    fn late_nonzero_exit_status_after_eof_is_preserved() {
        let exit = drive(vec![
            data_like(),
            ChannelMsg::Eof,
            ChannelMsg::ExitStatus { exit_status: 3 },
            ChannelMsg::Close,
        ]);
        assert_eq!(exit, 3);
    }

    #[test]
    fn close_without_any_status_stays_abnormal() {
        let exit = drive(vec![data_like(), ChannelMsg::Eof, ChannelMsg::Close]);
        assert_eq!(exit, EXIT_ABNORMAL);
    }

    #[test]
    fn stream_end_after_eof_without_close_does_not_hang_and_is_abnormal() {
        // No `Close` ever: in production the post-EOF grace elapses; here
        // the slice simply ends. Both terminate (no hang) and, with no
        // status seen, report the genuine-abnormal sentinel.
        let exit = drive(vec![data_like(), ChannelMsg::Eof]);
        assert_eq!(exit, EXIT_ABNORMAL);
    }

    #[test]
    fn exit_signal_maps_to_abnormal() {
        let exit = drive(vec![exit_signal(), ChannelMsg::Close]);
        assert_eq!(exit, EXIT_ABNORMAL);
    }

    #[test]
    fn normal_order_exit_status_before_eof_is_zero() {
        // Common case: exit-status precedes EOF. The fast path stops at
        // EOF (already resolved) and reports the real code.
        let exit = drive(vec![
            ChannelMsg::ExitStatus { exit_status: 0 },
            ChannelMsg::Eof,
            ChannelMsg::Close,
        ]);
        assert_eq!(exit, 0);
    }

    #[test]
    fn first_status_wins_over_a_later_signal() {
        // Take-once semantics preserved from the previous oneshot design.
        let exit = drive(vec![
            ChannelMsg::ExitStatus { exit_status: 0 },
            exit_signal(),
            ChannelMsg::Close,
        ]);
        assert_eq!(exit, 0);
    }

    #[test]
    fn control_classification_is_stable() {
        let mut t = ExitTracker::default();
        assert_eq!(t.observe(&data_like()), ReaderControl::Continue);
        assert_eq!(t.observe(&ChannelMsg::Eof), ReaderControl::Drain);
        assert_eq!(
            t.observe(&ChannelMsg::ExitStatus { exit_status: 0 }),
            ReaderControl::Continue
        );
        assert!(t.is_resolved());
        assert_eq!(t.observe(&ChannelMsg::Close), ReaderControl::Stop);
        assert_eq!(exit_signal_control(), ReaderControl::Continue);
    }

    fn exit_signal_control() -> ReaderControl {
        ExitTracker::default().observe(&exit_signal())
    }
}
