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
        let mut exit_sent: Option<oneshot::Sender<u32>> = Some(exit_tx);
        'outer: while let Some(msg) = read_half.wait().await {
            match msg {
                ChannelMsg::Data { data } if stdout_tx.send(data.to_vec()).is_err() => {
                    break 'outer;
                }
                ChannelMsg::Data { .. } => {}
                // SSH_EXTENDED_DATA_STDERR = 1 per RFC 4254 §5.2.
                // Other ext streams are silently discarded.
                ChannelMsg::ExtendedData { data, ext: 1 }
                    if stderr_tx.send(data.to_vec()).is_err() =>
                {
                    break 'outer;
                }
                ChannelMsg::ExtendedData { .. } => {}
                ChannelMsg::ExitStatus { exit_status } => {
                    if let Some(tx) = exit_sent.take() {
                        let _ = tx.send(exit_status);
                    }
                }
                ChannelMsg::ExitSignal { .. } => {
                    // Signaled termination: treat as abnormal if no status was sent.
                    if let Some(tx) = exit_sent.take() {
                        let _ = tx.send(EXIT_ABNORMAL);
                    }
                }
                ChannelMsg::Eof | ChannelMsg::Close => {
                    break;
                }
                _ => {}
            }
        }
        // Channel closed without an ExitStatus: synthesize abnormal exit.
        if let Some(tx) = exit_sent.take() {
            let _ = tx.send(EXIT_ABNORMAL);
        }
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
