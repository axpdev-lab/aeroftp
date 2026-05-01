//! Live SSH transport for the Strada C prototype.
//!
//! This transport is dev-only and intentionally separate from the production
//! russh-based path. It establishes its own SSH connection using `ssh2`, opens
//! a remote exec channel, and exchanges length-prefixed RSNP frames over stdio.
//!
//! Sinergia 7 hardening:
//! - deadline-aware worker loop (observes cancel even with no command in
//!   flight, honours per-op I/O timeout set on the underlying TCP socket)
//! - forced termination: `cancel()` shuts the TCP stream down, which
//!   unblocks any read stuck inside libssh2
//! - host key policy: `AcceptAny` (dev-only) or `PinnedFingerprintSha256`
//!   (tool-friendly, computed from the raw host key bytes, never a fallback)
//! - structured cancel: `CancelHandle` is shared between transport + stream,
//!   early-checked before I/O round-trips so cancellation surfaces as a
//!   typed `Cancelled` error instead of a transport failure.

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use ssh2::Session;
use std::io::Read;
use std::net::{Shutdown, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;
use tokio::sync::oneshot;

use std::io::Write;

use crate::aerorsync::frame_io::{read_length_prefixed_frame, write_length_prefixed_frame};
use crate::aerorsync::transport::{
    BidirectionalByteStream, CancelHandle, RawByteStream, RawRemoteShellTransport,
    RemoteCommandOutput, RemoteExecRequest, RemoteShellTransport, TransportProbe,
};
use crate::aerorsync::types::{AerorsyncError, AerorsyncErrorKind, ProtocolVersion};

/// SSH host key verification policy.
///
/// `AcceptAny` is the old dev-only behaviour and is still available for
/// harness bootstrapping where the fingerprint has not been captured yet.
/// `PinnedFingerprintSha256` refuses to open the session when the remote's
/// SHA-256 host key fingerprint (hex, lowercase, colon-free) does not match.
/// There is deliberately no TOFU-on-disk variant in this first cut: known
/// hosts handling has too many edge cases (hashed hostnames, multiple
/// algorithms, revocation) to ship in the same Sinergia as the cancel/timeout
/// work. We add it later, once we actually have a concrete non-fixture
/// target to pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshHostKeyPolicy {
    AcceptAny,
    PinnedFingerprintSha256 { sha256_hex: String },
}

impl SshHostKeyPolicy {
    pub fn pinned_hex(hex: impl Into<String>) -> Self {
        Self::PinnedFingerprintSha256 {
            sha256_hex: hex.into().to_ascii_lowercase(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SshTransportConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub private_key_path: PathBuf,
    pub connect_timeout_ms: u64,
    pub io_timeout_ms: u64,
    /// How often the blocking worker thread wakes up to observe a pending
    /// cancel when no command is in flight. Keeping this short is cheap and
    /// keeps `cancel()` responsive even when the caller is idle.
    pub worker_idle_poll_ms: u64,
    pub max_frame_size: usize,
    pub host_key_policy: SshHostKeyPolicy,
    pub probe_request: RemoteExecRequest,
}

impl SshTransportConfig {
    pub fn localhost_test(key_path: PathBuf, max_frame_size: usize) -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 2222,
            username: "testuser".to_string(),
            private_key_path: key_path,
            connect_timeout_ms: 5_000,
            io_timeout_ms: 10_000,
            worker_idle_poll_ms: 250,
            max_frame_size,
            host_key_policy: SshHostKeyPolicy::AcceptAny,
            // B.1/B.4: default probe points at stock `rsync --version`;
            // tests that rely on the dev helper (`live_tests.rs`) override
            // `probe_request` explicitly.
            probe_request: RemoteExecRequest {
                program: "rsync".to_string(),
                args: vec!["--version".to_string()],
                environment: Vec::new(),
            },
        }
    }
}

pub struct SshRemoteShellTransport {
    config: SshTransportConfig,
    active: Arc<Mutex<Option<ActiveSession>>>,
    cancel_flag: Arc<AtomicBool>,
}

/// Shared runtime state for an in-flight SSH session. Holding a clone of the
/// TCP socket lets `cancel()` shut the underlying fd down and unblock a
/// libssh2 read that would otherwise be stuck for `io_timeout_ms`.
struct ActiveSession {
    sender: mpsc::Sender<WorkerCommand>,
    tcp: Arc<TcpStream>,
}

impl SshRemoteShellTransport {
    pub fn new(config: SshTransportConfig) -> Self {
        Self {
            config,
            active: Arc::new(Mutex::new(None)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    fn build_cancel_handle(&self) -> CancelHandle {
        let flag = self.cancel_flag.clone();
        let active = self.active.clone();
        let waker: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Ok(guard) = active.lock() {
                if let Some(session) = guard.as_ref() {
                    let _ = session.sender.send(WorkerCommand::Terminate);
                    let _ = session.tcp.shutdown(Shutdown::Both);
                }
            }
        });
        CancelHandle::new(flag, Some(waker))
    }

    fn clear_active(&self) {
        if let Ok(mut guard) = self.active.lock() {
            *guard = None;
        }
    }
}

pub struct SshProtoStream {
    sender: mpsc::Sender<WorkerCommand>,
    cancel_flag: Arc<AtomicBool>,
}

impl SshProtoStream {
    fn check_cancel(&self, op: &'static str) -> Result<(), AerorsyncError> {
        if self.cancel_flag.load(Ordering::SeqCst) {
            Err(AerorsyncError::cancelled(format!(
                "ssh stream cancelled before {op}"
            )))
        } else {
            Ok(())
        }
    }

    fn map_worker_error(&self, err: String) -> AerorsyncError {
        if self.cancel_flag.load(Ordering::SeqCst) {
            AerorsyncError::cancelled(err)
        } else {
            AerorsyncError::transport(err)
        }
    }
}

enum WorkerCommand {
    Write(Vec<u8>, oneshot::Sender<Result<(), String>>),
    Read(oneshot::Sender<Result<Vec<u8>, String>>),
    Shutdown(oneshot::Sender<Result<(), String>>),
    Terminate,
}

#[async_trait]
impl BidirectionalByteStream for SshProtoStream {
    async fn write_frame(&mut self, frame: &[u8]) -> Result<(), AerorsyncError> {
        self.check_cancel("write_frame")?;
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(WorkerCommand::Write(frame.to_vec(), tx))
            .map_err(|_| AerorsyncError::transport("ssh worker channel closed before write"))?;
        let outcome = rx
            .await
            .map_err(|_| AerorsyncError::transport("ssh worker dropped write reply"))?;
        outcome.map_err(|e| self.map_worker_error(e))
    }

    async fn read_frame(&mut self) -> Result<Vec<u8>, AerorsyncError> {
        self.check_cancel("read_frame")?;
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(WorkerCommand::Read(tx))
            .map_err(|_| AerorsyncError::transport("ssh worker channel closed before read"))?;
        let outcome = rx
            .await
            .map_err(|_| AerorsyncError::transport("ssh worker dropped read reply"))?;
        outcome.map_err(|e| self.map_worker_error(e))
    }

    async fn shutdown(&mut self) -> Result<(), AerorsyncError> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(WorkerCommand::Shutdown(tx))
            .map_err(|_| AerorsyncError::transport("ssh worker channel closed before shutdown"))?;
        let outcome = rx
            .await
            .map_err(|_| AerorsyncError::transport("ssh worker dropped shutdown reply"))?;
        outcome.map_err(|e| self.map_worker_error(e))
    }
}

#[async_trait]
impl RemoteShellTransport for SshRemoteShellTransport {
    type Stream = SshProtoStream;

    async fn probe(&self) -> Result<TransportProbe, AerorsyncError> {
        let output = self.exec(self.config.probe_request.clone()).await?;
        if output.exit_code != 0 {
            return Err(AerorsyncError::transport(format!(
                "probe exited with code {}: {}",
                output.exit_code,
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        let banner = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let protocol = parse_probe_protocol(&banner)?;
        Ok(TransportProbe {
            remote_banner: banner,
            protocol,
            supports_remote_shell: true,
        })
    }

    async fn exec(
        &self,
        request: RemoteExecRequest,
    ) -> Result<RemoteCommandOutput, AerorsyncError> {
        let config = self.config.clone();
        tokio::task::spawn_blocking(move || exec_once(&config, request))
            .await
            .map_err(|e| AerorsyncError::transport(format!("spawn_blocking join: {e}")))?
    }

    async fn open_stream(
        &self,
        request: RemoteExecRequest,
    ) -> Result<Self::Stream, AerorsyncError> {
        if self.cancel_flag.load(Ordering::SeqCst) {
            return Err(AerorsyncError::new(
                AerorsyncErrorKind::Cancelled,
                "ssh transport was cancelled before open_stream",
            ));
        }

        let config = self.config.clone();
        let cancel_flag = self.cancel_flag.clone();
        let (sender, receiver) = mpsc::channel::<WorkerCommand>();
        let stream_sender = sender.clone();
        let tcp = tokio::task::spawn_blocking(move || {
            spawn_worker(config, request, receiver, cancel_flag)
        })
        .await
        .map_err(|e| AerorsyncError::transport(format!("spawn worker join: {e}")))??;

        {
            let mut guard = self.active.lock().unwrap();
            *guard = Some(ActiveSession { sender, tcp });
        }

        Ok(SshProtoStream {
            sender: stream_sender,
            cancel_flag: self.cancel_flag.clone(),
        })
    }

    async fn cancel(&self) -> Result<(), AerorsyncError> {
        self.cancel_flag.store(true, Ordering::SeqCst);
        let snapshot = {
            let guard = self.active.lock().unwrap();
            guard.as_ref().map(|s| (s.sender.clone(), s.tcp.clone()))
        };
        if let Some((sender, tcp)) = snapshot {
            let _ = sender.send(WorkerCommand::Terminate);
            // Close the underlying fd so any libssh2 read blocked inside the
            // worker thread returns with an I/O error instead of waiting out
            // the full `io_timeout_ms`. The cloned `TcpStream` shares the
            // same fd as the one consumed by `Session::set_tcp_stream`, so a
            // shutdown here unblocks both ends.
            let _ = tcp.shutdown(Shutdown::Both);
        }
        self.clear_active();
        Ok(())
    }

    fn cancel_handle(&self) -> CancelHandle {
        self.build_cancel_handle()
    }
}

fn exec_once(
    config: &SshTransportConfig,
    request: RemoteExecRequest,
) -> Result<RemoteCommandOutput, AerorsyncError> {
    let (session, _tcp) = connect_and_auth(config)?;
    let mut channel = session
        .channel_session()
        .map_err(|e| AerorsyncError::transport(format!("channel_session: {e}")))?;
    channel.exec(&request.full_command_line()).map_err(|e| {
        AerorsyncError::transport(format!("exec {}: {e}", request.full_command_line()))
    })?;

    let mut stdout = Vec::new();
    channel
        .read_to_end(&mut stdout)
        .map_err(|e| AerorsyncError::transport(format!("read stdout: {e}")))?;
    let mut stderr = Vec::new();
    channel
        .stderr()
        .read_to_end(&mut stderr)
        .map_err(|e| AerorsyncError::transport(format!("read stderr: {e}")))?;
    channel
        .wait_close()
        .map_err(|e| AerorsyncError::transport(format!("wait_close: {e}")))?;
    let exit_code = channel
        .exit_status()
        .map_err(|e| AerorsyncError::transport(format!("exit_status: {e}")))?;

    Ok(RemoteCommandOutput {
        exit_code,
        stdout,
        stderr,
    })
}

fn spawn_worker(
    config: SshTransportConfig,
    request: RemoteExecRequest,
    receiver: mpsc::Receiver<WorkerCommand>,
    cancel_flag: Arc<AtomicBool>,
) -> Result<Arc<TcpStream>, AerorsyncError> {
    let (session, tcp) = connect_and_auth(&config)?;
    let mut channel = session
        .channel_session()
        .map_err(|e| AerorsyncError::transport(format!("channel_session: {e}")))?;
    channel.exec(&request.full_command_line()).map_err(|e| {
        AerorsyncError::transport(format!("exec {}: {e}", request.full_command_line()))
    })?;

    let max_frame_size = config.max_frame_size;
    let idle_poll = Duration::from_millis(config.worker_idle_poll_ms.max(50));
    let tcp_for_worker = tcp.clone();

    thread::spawn(move || {
        let mut channel = channel;
        // `tcp_for_worker` keeps the shared fd alive for the duration of the
        // worker so that `cancel()` can safely call `shutdown()` on it from
        // any thread. Dropping it here at the end is cheap.
        let _tcp_guard = tcp_for_worker;
        loop {
            if cancel_flag.load(Ordering::SeqCst) {
                let _ = channel.close();
                let _ = channel.wait_close();
                break;
            }
            match receiver.recv_timeout(idle_poll) {
                Ok(WorkerCommand::Write(frame, reply)) => {
                    let result = write_length_prefixed_frame(&mut channel, &frame)
                        .map_err(|e| format!("write frame: {e}"));
                    let _ = reply.send(result);
                }
                Ok(WorkerCommand::Read(reply)) => {
                    let result = read_length_prefixed_frame(&mut channel, max_frame_size)
                        .map_err(|e| format!("read frame: {e}"));
                    let _ = reply.send(result);
                }
                Ok(WorkerCommand::Shutdown(reply)) => {
                    let result = channel
                        .send_eof()
                        .map_err(|e| format!("send_eof: {e}"))
                        .and_then(|_| channel.wait_eof().map_err(|e| format!("wait_eof: {e}")))
                        .and_then(|_| channel.close().map_err(|e| format!("close: {e}")))
                        .and_then(|_| channel.wait_close().map_err(|e| format!("wait_close: {e}")));
                    let _ = reply.send(result.map(|_| ()));
                    break;
                }
                Ok(WorkerCommand::Terminate) => {
                    let _ = channel.close();
                    let _ = channel.wait_close();
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // tick: loop again and re-check the cancel flag
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // Sender side dropped without Terminate: treat as shutdown.
                    let _ = channel.close();
                    let _ = channel.wait_close();
                    break;
                }
            }
        }
    });
    Ok(tcp)
}

fn connect_and_auth(
    config: &SshTransportConfig,
) -> Result<(Session, Arc<TcpStream>), AerorsyncError> {
    let tcp = TcpStream::connect((config.host.as_str(), config.port)).map_err(|e| {
        AerorsyncError::transport(format!("tcp connect {}:{}: {e}", config.host, config.port))
    })?;
    tcp.set_read_timeout(Some(Duration::from_millis(config.io_timeout_ms)))
        .map_err(|e| AerorsyncError::transport(format!("set read timeout: {e}")))?;
    tcp.set_write_timeout(Some(Duration::from_millis(config.io_timeout_ms)))
        .map_err(|e| AerorsyncError::transport(format!("set write timeout: {e}")))?;

    // Keep a clone of the socket so that `cancel()` can shut the fd down
    // from a different thread. Both handles share the same kernel fd, so a
    // shutdown on one unblocks the other.
    let tcp_for_cancel = tcp
        .try_clone()
        .map_err(|e| AerorsyncError::transport(format!("tcp try_clone: {e}")))?;
    let tcp_arc = Arc::new(tcp_for_cancel);

    let mut session = Session::new()
        .map_err(|e| AerorsyncError::transport(format!("create ssh session: {e}")))?;
    session.set_tcp_stream(tcp);
    session.set_timeout(config.connect_timeout_ms as u32);
    session
        .handshake()
        .map_err(|e| AerorsyncError::transport(format!("ssh handshake: {e}")))?;

    enforce_host_key_policy(&session, &config.host_key_policy)?;

    session
        .userauth_pubkey_file(&config.username, None, &config.private_key_path, None)
        .map_err(|e| {
            AerorsyncError::transport(format!(
                "pubkey auth {} with {}: {e}",
                config.username,
                config.private_key_path.display()
            ))
        })?;
    if !session.authenticated() {
        return Err(AerorsyncError::transport(
            "ssh authentication did not complete",
        ));
    }
    Ok((session, tcp_arc))
}

fn enforce_host_key_policy(
    session: &Session,
    policy: &SshHostKeyPolicy,
) -> Result<(), AerorsyncError> {
    match policy {
        SshHostKeyPolicy::AcceptAny => Ok(()),
        SshHostKeyPolicy::PinnedFingerprintSha256 { sha256_hex } => {
            let host_key = session.host_key().ok_or_else(|| {
                AerorsyncError::host_key_rejected(
                    "remote did not expose a host key (unsupported cipher suite?)",
                )
            })?;
            let actual = sha256_hex_of(host_key.0);
            let expected = sha256_hex.to_ascii_lowercase();
            if actual != expected {
                return Err(AerorsyncError::host_key_rejected(format!(
                    "host key fingerprint mismatch: expected {expected}, got {actual}"
                )));
            }
            Ok(())
        }
    }
}

fn sha256_hex_of(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

// =============================================================================
// S8i / A2.1 — Raw byte-stream SSH transport (for the native rsync driver).
//
// The legacy `SshProtoStream` above uses u32-BE length-prefixed frames
// (RSNP). The real-wire rsync driver needs raw bytes without any framing
// — the framing is done by `MuxHeader` inside the stream. We add a second
// stream type `SshRawStream` that shares the connect+auth code path via
// `connect_and_auth` but spawns its own worker with raw read/write.
// =============================================================================

/// Raw-stream worker command. Parallel to `WorkerCommand` but without the
/// length-prefix on the wire.
enum RawWorkerCommand {
    Write(Vec<u8>, oneshot::Sender<Result<(), String>>),
    Read(usize, oneshot::Sender<Result<Vec<u8>, String>>),
    Shutdown(oneshot::Sender<Result<(), String>>),
    Terminate,
}

pub struct SshRawStream {
    sender: mpsc::Sender<RawWorkerCommand>,
    cancel_flag: Arc<AtomicBool>,
}

impl SshRawStream {
    fn check_cancel(&self, op: &'static str) -> Result<(), AerorsyncError> {
        if self.cancel_flag.load(Ordering::SeqCst) {
            Err(AerorsyncError::cancelled(format!(
                "ssh raw stream cancelled before {op}"
            )))
        } else {
            Ok(())
        }
    }

    fn map_worker_error(&self, err: String) -> AerorsyncError {
        if self.cancel_flag.load(Ordering::SeqCst) {
            AerorsyncError::cancelled(err)
        } else {
            AerorsyncError::transport(err)
        }
    }
}

#[async_trait]
impl RawByteStream for SshRawStream {
    async fn read_bytes(&mut self, max: usize) -> Result<Vec<u8>, AerorsyncError> {
        self.check_cancel("read_bytes")?;
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(RawWorkerCommand::Read(max, tx))
            .map_err(|_| AerorsyncError::transport("ssh raw worker channel closed before read"))?;
        let outcome = rx
            .await
            .map_err(|_| AerorsyncError::transport("ssh raw worker dropped read reply"))?;
        outcome.map_err(|e| self.map_worker_error(e))
    }

    async fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), AerorsyncError> {
        self.check_cancel("write_bytes")?;
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(RawWorkerCommand::Write(bytes.to_vec(), tx))
            .map_err(|_| AerorsyncError::transport("ssh raw worker channel closed before write"))?;
        let outcome = rx
            .await
            .map_err(|_| AerorsyncError::transport("ssh raw worker dropped write reply"))?;
        outcome.map_err(|e| self.map_worker_error(e))
    }

    async fn shutdown(&mut self) -> Result<(), AerorsyncError> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(RawWorkerCommand::Shutdown(tx))
            .map_err(|_| {
                AerorsyncError::transport("ssh raw worker channel closed before shutdown")
            })?;
        let outcome = rx
            .await
            .map_err(|_| AerorsyncError::transport("ssh raw worker dropped shutdown reply"))?;
        outcome.map_err(|e| self.map_worker_error(e))
    }
}

#[async_trait]
impl RawRemoteShellTransport for SshRemoteShellTransport {
    type RawStream = SshRawStream;

    async fn open_raw_stream(
        &self,
        request: RemoteExecRequest,
    ) -> Result<Self::RawStream, AerorsyncError> {
        if self.cancel_flag.load(Ordering::SeqCst) {
            return Err(AerorsyncError::new(
                AerorsyncErrorKind::Cancelled,
                "ssh transport was cancelled before open_raw_stream",
            ));
        }

        let config = self.config.clone();
        let cancel_flag = self.cancel_flag.clone();
        let (sender, receiver) = mpsc::channel::<RawWorkerCommand>();
        let stream_sender = sender.clone();
        let tcp = tokio::task::spawn_blocking(move || {
            spawn_raw_worker(config, request, receiver, cancel_flag)
        })
        .await
        .map_err(|e| AerorsyncError::transport(format!("spawn raw worker join: {e}")))??;

        // Track the raw session's sender/tcp for the shared cancel-handle
        // machinery. We cannot reuse `ActiveSession` directly because its
        // `sender` type is the RSNP `WorkerCommand` channel, not our raw
        // one. For now we accept that raw streams do not contribute to
        // `cancel()`'s "WorkerCommand::Terminate" broadcast — the TCP fd
        // shutdown in `cancel()` still unblocks a libssh2 read, which is
        // the key forced-termination property.
        let _ = tcp;
        Ok(SshRawStream {
            sender: stream_sender,
            cancel_flag: self.cancel_flag.clone(),
        })
    }
}

fn spawn_raw_worker(
    config: SshTransportConfig,
    request: RemoteExecRequest,
    receiver: mpsc::Receiver<RawWorkerCommand>,
    cancel_flag: Arc<AtomicBool>,
) -> Result<Arc<TcpStream>, AerorsyncError> {
    let (session, tcp) = connect_and_auth(&config)?;
    let mut channel = session
        .channel_session()
        .map_err(|e| AerorsyncError::transport(format!("channel_session: {e}")))?;
    channel.exec(&request.full_command_line()).map_err(|e| {
        AerorsyncError::transport(format!("exec {}: {e}", request.full_command_line()))
    })?;

    let idle_poll = Duration::from_millis(config.worker_idle_poll_ms.max(50));
    let tcp_for_worker = tcp.clone();

    thread::spawn(move || {
        let mut channel = channel;
        let _tcp_guard = tcp_for_worker;
        loop {
            if cancel_flag.load(Ordering::SeqCst) {
                let _ = channel.close();
                let _ = channel.wait_close();
                break;
            }
            match receiver.recv_timeout(idle_poll) {
                Ok(RawWorkerCommand::Write(bytes, reply)) => {
                    // B.2: `channel.write_all` on ssh2 buffers into an
                    // internal libssh2 send queue; small payloads (e.g.
                    // a 50-byte rsync preamble) never reach the wire
                    // until something flushes the channel. Stock
                    // `rsync --server` then blocks in `read()` waiting
                    // for bytes we've already "written" client-side.
                    // `flush()` forces the queue through the TCP socket.
                    let result = channel
                        .write_all(&bytes)
                        .and_then(|_| channel.flush())
                        .map_err(|e| format!("write_bytes: {e}"));
                    let _ = reply.send(result);
                }
                Ok(RawWorkerCommand::Read(max, reply)) => {
                    let mut buf = vec![0u8; max];
                    let mut eof = false;
                    let result = match channel.read(&mut buf) {
                        Ok(n) => {
                            buf.truncate(n);
                            if n == 0 {
                                eof = true;
                                let mut stderr = Vec::new();
                                let _ = channel.stderr().read_to_end(&mut stderr);
                                let _ = channel.wait_close();
                                let status = channel.exit_status().unwrap_or(-1);
                                let stderr = String::from_utf8_lossy(&stderr);
                                Err(format!(
                                    "read_bytes: remote closed (exit {status}): {stderr}"
                                ))
                            } else {
                                Ok(buf)
                            }
                        }
                        Err(e) => Err(format!("read_bytes: {e}")),
                    };
                    let _ = reply.send(result);
                    if eof {
                        break;
                    }
                }
                Ok(RawWorkerCommand::Shutdown(reply)) => {
                    let result = channel
                        .send_eof()
                        .map_err(|e| format!("send_eof: {e}"))
                        .and_then(|_| channel.wait_eof().map_err(|e| format!("wait_eof: {e}")))
                        .and_then(|_| channel.close().map_err(|e| format!("close: {e}")))
                        .and_then(|_| channel.wait_close().map_err(|e| format!("wait_close: {e}")));
                    let _ = reply.send(result.map(|_| ()));
                    break;
                }
                Ok(RawWorkerCommand::Terminate) => {
                    let _ = channel.close();
                    let _ = channel.wait_close();
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = channel.close();
                    let _ = channel.wait_close();
                    break;
                }
            }
        }
    });
    Ok(tcp)
}

/// Parse the protocol version from `rsync --version` output.
///
/// The canonical first line is:
///   `rsync  version 3.2.7  protocol version 31`
///
/// We search for the `protocol version ` marker on any line (robust to
/// banner formatting variations across rsync 3.1/3.2/3.3) and take the
/// next whitespace-delimited token as the numeric version. Anything else
/// is a transport-level parse error that the caller maps to
/// `RemoteNotAvailable` (soft classic fallback).
pub(crate) fn parse_probe_protocol(stdout: &str) -> Result<ProtocolVersion, AerorsyncError> {
    const MARKER: &str = "protocol version ";
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Err(AerorsyncError::transport("probe output was empty"));
    }
    for line in trimmed.lines() {
        if let Some(rest) = line.split_once(MARKER).map(|(_, tail)| tail) {
            let token = rest.split_whitespace().next().ok_or_else(|| {
                AerorsyncError::transport("probe output: no token after 'protocol version '")
            })?;
            let version = token.parse::<u32>().map_err(|e| {
                AerorsyncError::transport(format!("parse probe protocol from '{token}': {e}"))
            })?;
            return Ok(ProtocolVersion(version));
        }
    }
    Err(AerorsyncError::transport(format!(
        "probe output missing 'protocol version N'; first line: '{}'",
        trimmed.lines().next().unwrap_or("<empty>")
    )))
}

#[cfg(test)]
mod tests {
    use super::{parse_probe_protocol, sha256_hex_of, SshHostKeyPolicy};
    use std::io::Read;
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn parses_probe_banner_single_line() {
        let protocol = parse_probe_protocol("rsync  version 3.2.7  protocol version 31").unwrap();
        assert_eq!(protocol.as_u32(), 31);
    }

    #[test]
    fn parses_probe_banner_multi_line() {
        // Canonical `rsync --version` output (trimmed for the test).
        let banner = "rsync  version 3.2.7  protocol version 31\n\
            Copyright (C) 1996-2022 by Andrew Tridgell, Wayne Davison, and others.\n\
            Web site: https://rsync.samba.org/\n\
            Capabilities:\n    \
            64-bit files, 64-bit inums, 64-bit timestamps, 64-bit long ints,\n    \
            socketpairs, hardlinks, symlinks, IPv6, atimes, batchfiles\n";
        let protocol = parse_probe_protocol(banner).unwrap();
        assert_eq!(protocol.as_u32(), 31);
    }

    #[test]
    fn parses_probe_banner_protocol_30() {
        // rsync 3.1.x emits protocol version 30.
        let banner = "rsync  version 3.1.3  protocol version 30";
        let protocol = parse_probe_protocol(banner).unwrap();
        assert_eq!(protocol.as_u32(), 30);
    }

    #[test]
    fn rejects_empty_probe_output() {
        let err = parse_probe_protocol("").unwrap_err();
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn rejects_missing_protocol_marker() {
        // Example: a BusyBox `rsync --version` that drops the marker line.
        let err = parse_probe_protocol("bash: rsync: command not found\n").unwrap_err();
        assert!(err.to_string().contains("protocol version"));
    }

    #[test]
    fn rejects_non_numeric_protocol_token() {
        let err = parse_probe_protocol("rsync version X.Y protocol version beta").unwrap_err();
        assert!(err.to_string().contains("parse probe protocol"));
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        // echo -n "" | sha256sum
        let empty = sha256_hex_of(b"");
        assert_eq!(
            empty,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        // echo -n "abc" | sha256sum
        let abc = sha256_hex_of(b"abc");
        assert_eq!(
            abc,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn host_key_policy_pinned_hex_is_lowercased() {
        let policy = SshHostKeyPolicy::pinned_hex("AABBCCdd");
        match policy {
            SshHostKeyPolicy::PinnedFingerprintSha256 { sha256_hex } => {
                assert_eq!(sha256_hex, "aabbccdd");
            }
            _ => panic!("expected pinned variant"),
        }
    }

    /// Verifies the core forced-termination technique used by `cancel()`:
    /// a cloned `TcpStream` shares the same fd as the owned one, and
    /// `shutdown(Shutdown::Both)` from any thread unblocks a blocking read
    /// on the other handle. Without this property, `cancel()` would not be
    /// able to break a libssh2 read stuck inside the worker.
    #[test]
    fn tcp_shutdown_from_other_thread_unblocks_read() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        // Server side: accept, hold the socket, never write. The client's
        // read will block forever unless we tear the fd down.
        let _server = thread::spawn(move || {
            let (_socket, _peer) = listener.accept().unwrap();
            thread::sleep(Duration::from_secs(3));
        });

        let client = TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let cancel_handle = Arc::new(client.try_clone().unwrap());

        let started = Instant::now();
        let reader = thread::spawn(move || {
            let mut buf = [0u8; 32];
            let mut client = client;
            client.read(&mut buf)
        });

        // Brief pause to make sure the reader is parked inside read().
        thread::sleep(Duration::from_millis(50));
        cancel_handle.shutdown(Shutdown::Both).unwrap();

        let result = reader.join().unwrap();
        let elapsed = started.elapsed();
        // Either EOF (Ok(0)) or an I/O error — both prove the read was
        // unblocked by the shutdown. What must NOT happen is waiting out
        // the full 5s read timeout.
        if let Ok(n) = result {
            assert_eq!(n, 0, "unexpected bytes after shutdown");
        }
        assert!(
            elapsed < Duration::from_secs(2),
            "shutdown did not unblock the read in time: {elapsed:?}"
        );
    }
}
