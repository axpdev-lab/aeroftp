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
use secrecy::SecretString;
use sha2::{Digest, Sha256};
use ssh2::{MethodType, Session};
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

/// Canonical host-key algorithm preference list, shared between the libssh2
/// leg (`ssh_transport.rs`) and the russh leg (`russh_session_transport.rs`).
///
/// Why this exists (Z.1.4 host-key asymmetry fix, 2026-05-12): pinning the
/// SHA-256 fingerprint of *the host key the server selected at handshake*
/// only works if both SSH libraries negotiate the same algorithm. ssh2 (the
/// classic SFTP leg) and russh (the native probe leg) had different default
/// preferences, so a server exposing both `ssh-ed25519` and `rsa-sha2-512`
/// would yield two different fingerprints depending on which library opened
/// the channel — fingerprint pinning rejected the second one. Enforcing the
/// same priority order on both legs makes the selection deterministic and
/// the fingerprint pinnable across reconnects.
///
/// Order rationale: Ed25519 first (smallest, fastest, modern), then ECDSA
/// (widely supported), then RSA SHA-2 variants. Legacy `ssh-rsa` with SHA-1
/// is deliberately omitted.
pub const AERORSYNC_HOST_KEY_ALGS: &str =
    "ssh-ed25519,ecdsa-sha2-nistp256,ecdsa-sha2-nistp384,ecdsa-sha2-nistp521,rsa-sha2-512,rsa-sha2-256";

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
    /// Z.4.5 R1 password transport: optional SSH password used by the
    /// russh leg (`RusshSessionTransport`) when `private_key_path` is not
    /// usable for the target host. The libssh2 leg
    /// (`SshRemoteShellTransport`) ignores this field; it is always
    /// pubkey-only by design.
    ///
    /// Wrapped in [`SecretString`] so:
    /// 1. drop zeroizes the in-memory bytes;
    /// 2. the `Debug` derive on this struct prints `[REDACTED ...]`
    ///    instead of the raw password (verified by
    ///    `russh_session_transport::tests::password_does_not_leak_in_debug`).
    ///
    /// `None` means "no password material configured": `RusshSessionTransport`
    /// then falls back to the pubkey path. An empty `SecretString` is treated
    /// the same as `None` by the connect path so callers cannot accidentally
    /// auth as the empty user.
    pub auth_password: Option<SecretString>,
    /// When true, the russh leg delegates authentication to a running SSH
    /// agent (`SSH_AUTH_SOCK`) instead of loading `private_key_path`. The
    /// libssh2 single-shot leg ignores this field (pubkey-file-only by
    /// design); an agent profile is therefore always routed through the
    /// russh leg via [`SshTransportConfig::prefers_russh_leg`]. Password
    /// auth still takes precedence when both are configured.
    pub auth_agent: bool,
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
            auth_password: None,
            auth_agent: false,
        }
    }

    /// Z.4.5 R1: presence/non-empty check for the optional password
    /// transport. Returns `Some(&SecretString)` only when the password
    /// is actually usable; an empty `SecretString` is treated as no
    /// password at all so the russh leg never sends a zero-length
    /// authentication payload.
    pub fn usable_password(&self) -> Option<&SecretString> {
        use secrecy::ExposeSecret;
        match &self.auth_password {
            Some(secret) if !secret.expose_secret().is_empty() => Some(secret),
            _ => None,
        }
    }

    /// Whether this profile must be driven through the russh leg rather
    /// than the libssh2 single-shot leg. True for password auth (libssh2
    /// leg is pubkey-only) and for agent auth (libssh2 leg cannot reach
    /// `SSH_AUTH_SOCK` in this codebase). Pubkey-file profiles return
    /// false and keep using the historical libssh2 single-shot path for
    /// non-batch transfers. Probe and single-shot upload/download share
    /// this predicate so leg selection is consistent across the call
    /// sites in `delta_transport_impl.rs`.
    pub fn prefers_russh_leg(&self) -> bool {
        self.usable_password().is_some() || self.auth_agent
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

/// Cloned control handles of the active session (worker command sender +
/// shared TCP stream for the forced fd shutdown), or `None` when no
/// session is in flight.
type ActiveSessionSnapshot = Option<(mpsc::Sender<WorkerCommand>, Arc<TcpStream>)>;

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

    /// Store the freshly opened session in the `active` slot.
    ///
    /// Y-RSC.2: a poisoned mutex means another thread panicked while
    /// holding this lock. Propagate a typed transport error instead of
    /// panicking in the caller's async context.
    fn store_active(&self, session: ActiveSession) -> Result<(), AerorsyncError> {
        let mut guard = self.active.lock().map_err(|_| {
            AerorsyncError::transport(
                "ssh transport session mutex poisoned while storing the active session",
            )
        })?;
        *guard = Some(session);
        Ok(())
    }

    /// Snapshot the worker sender + TCP handle of the active session, if
    /// any, without holding the lock across the subsequent I/O.
    ///
    /// Y-RSC.2: mutex poison surfaces as a typed transport error. The
    /// caller (`cancel`) has already stored the cancel flag by then, so
    /// cooperative cancellation still engages even when the forced fd
    /// shutdown cannot be reached.
    fn snapshot_active(&self) -> Result<ActiveSessionSnapshot, AerorsyncError> {
        let guard = self.active.lock().map_err(|_| {
            AerorsyncError::transport(
                "ssh transport session mutex poisoned while snapshotting the active session",
            )
        })?;
        Ok(guard.as_ref().map(|s| (s.sender.clone(), s.tcp.clone())))
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

        self.store_active(ActiveSession { sender, tcp })?;

        Ok(SshProtoStream {
            sender: stream_sender,
            cancel_flag: self.cancel_flag.clone(),
        })
    }

    async fn cancel(&self) -> Result<(), AerorsyncError> {
        // Flag first: even if the session snapshot below fails on a
        // poisoned mutex, every in-flight and future operation observes
        // the cooperative cancel.
        self.cancel_flag.store(true, Ordering::SeqCst);
        let snapshot = self.snapshot_active()?;
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

    // Z.1.4: align host-key algorithm preference with the russh leg so both
    // SSH libraries select the same host key on servers exposing multiple
    // algorithms. Without this, pinned-fingerprint policy would reject the
    // second library to reconnect.
    session
        .method_pref(MethodType::HostKey, AERORSYNC_HOST_KEY_ALGS)
        .map_err(|e| {
            AerorsyncError::transport(format!(
                "ssh method_pref HostKey '{AERORSYNC_HOST_KEY_ALGS}': {e}"
            ))
        })?;

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
// S8i / A2.1: Raw byte-stream SSH transport (for the native rsync driver).
//
// The legacy `SshProtoStream` above uses u32-BE length-prefixed frames
// (RSNP). The real-wire rsync driver needs raw bytes without any framing
//: the framing is done by `MuxHeader` inside the stream. We add a second
// stream type `SshRawStream` that shares the connect+auth code path via
// `connect_and_auth` but spawns its own worker with raw read/write.
// =============================================================================

/// Raw-stream worker command. Parallel to `WorkerCommand` but without the
/// length-prefix on the wire.
enum RawWorkerCommand {
    Write(Vec<u8>, oneshot::Sender<Result<(), RawWorkerError>>),
    Read(usize, oneshot::Sender<Result<Vec<u8>, RawWorkerError>>),
    Shutdown(oneshot::Sender<Result<(), RawWorkerError>>),
    #[allow(dead_code)] // reserved hard-stop path for worker teardown
    Terminate,
}

/// Error payload the raw worker thread sends back over its reply
/// channels.
///
/// Y-RSC.2: the worker knows structurally whether an EOF was a clean
/// remote close (exit status 0), so it stamps that classification here
/// instead of encoding it in the message text. The async side maps it to
/// [`AerorsyncError::transport_clean_eof`] without ever inspecting the
/// wording.
enum RawWorkerError {
    /// The remote closed the channel cleanly (exit status 0) while a
    /// read was pending.
    CleanEof(String),
    /// Any other worker-side failure.
    Other(String),
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

    fn map_worker_error(&self, err: RawWorkerError) -> AerorsyncError {
        let (clean_eof, detail) = match err {
            RawWorkerError::CleanEof(detail) => (true, detail),
            RawWorkerError::Other(detail) => (false, detail),
        };
        // Cancel takes precedence: a worker failure observed after the
        // cancel flag flipped is reported as the cancellation the caller
        // asked for, exactly as before Y-RSC.2.
        if self.cancel_flag.load(Ordering::SeqCst) {
            AerorsyncError::cancelled(detail)
        } else if clean_eof {
            AerorsyncError::transport_clean_eof(detail)
        } else {
            AerorsyncError::transport(detail)
        }
    }
}

/// Append exact native-rsync channel bytes when the wire-dump diagnostic is
/// enabled. Called by the blocking worker at the point bytes have actually
/// crossed the libssh2 channel, so the transcript also localises a stalled
/// read/write phase.
fn wire_dump_raw_append(file: &str, bytes: &[u8]) {
    let dir = match std::env::var("AEROFTP_WIRE_DUMP_DIR") {
        Ok(dir) if !dir.is_empty() => dir,
        _ => return,
    };
    use std::io::Write as _;
    let path = std::path::Path::new(&dir).join(file);
    if let Ok(mut output) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = output.write_all(bytes);
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
        // `cancel()`'s "WorkerCommand::Terminate" broadcast: the TCP fd
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
    let command = request.full_command_line();
    channel
        .exec(&command)
        .map_err(|e| AerorsyncError::transport(format!("exec {command}: {e}")))?;
    wire_dump_raw_append("remote_command.txt", format!("{command}\n").as_bytes());

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
                    // Do NOT call `channel.flush()` after write.
                    //
                    // `ssh2::Channel::flush` maps to `libssh2_channel_flush_ex`,
                    // which flushes the *read* buffer: it discards unread
                    // CHANNEL_DATA already queued for this stream (see
                    // libssh2 docs: "Flush the read buffer for a given
                    // channel instance"). It does not push the send queue
                    // to TCP.
                    //
                    // Race (known issue #3 / InvalidProtocolVersion
                    // 2015297409): stock rsync often replies with its
                    // 4-byte protocol version as soon as it has read ours.
                    // That reply can land in the libssh2 packet queue
                    // during `write_all`. A following `flush()` then
                    // drops those 4 bytes, so the next `read` starts at
                    // offset +4 of the server preamble (`81 ff 1e 78` =
                    // compat_flags varint + algo-list length + first 'x'
                    // of "xxh128..."), which decodes as version
                    // 2015297409. Intermittent on contended CI; green
                    // when the server response arrives after flush.
                    //
                    // Blocking `write_all` is enough: libssh2 ships the
                    // CHANNEL_DATA packet before returning.
                    let result = channel
                        .write_all(&bytes)
                        .map_err(|e| RawWorkerError::Other(format!("write_bytes: {e}")));
                    if result.is_ok() {
                        wire_dump_raw_append("raw_client_to_server.bin", &bytes);
                    }
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
                                let message =
                                    format!("read_bytes: remote closed (exit {status}): {stderr}");
                                // Y-RSC.2: exit status 0 is a clean close;
                                // classify structurally instead of leaving
                                // the driver to parse the message text.
                                Err(if status == 0 {
                                    RawWorkerError::CleanEof(message)
                                } else {
                                    RawWorkerError::Other(message)
                                })
                            } else {
                                wire_dump_raw_append("raw_server_to_client.bin", &buf);
                                Ok(buf)
                            }
                        }
                        Err(e) => Err(RawWorkerError::Other(format!("read_bytes: {e}"))),
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
                        .and_then(|_| channel.wait_close().map_err(|e| format!("wait_close: {e}")))
                        .map_err(RawWorkerError::Other);
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
    fn prefers_russh_leg_true_for_agent_and_password_false_for_pubkey() {
        use super::SshTransportConfig;
        use secrecy::SecretString;
        use std::path::PathBuf;

        let base = SshTransportConfig::localhost_test(PathBuf::from("/dev/null"), 1 << 20);

        // Pure pubkey-file profile: stays on the libssh2 single-shot leg.
        assert!(!base.prefers_russh_leg());

        // Agent profile: must route through russh (libssh2 leg cannot
        // reach SSH_AUTH_SOCK in this codebase).
        let agent = SshTransportConfig {
            auth_agent: true,
            ..SshTransportConfig::localhost_test(PathBuf::from("/dev/null"), 1 << 20)
        };
        assert!(agent.prefers_russh_leg());

        // Password profile: russh leg (unchanged behaviour).
        let pw = SshTransportConfig {
            auth_password: Some(SecretString::from("pw".to_string())),
            ..SshTransportConfig::localhost_test(PathBuf::from("/dev/null"), 1 << 20)
        };
        assert!(pw.prefers_russh_leg());

        // Empty password is treated as no password; with no agent flag
        // the profile stays on the pubkey leg.
        let empty_pw = SshTransportConfig {
            auth_password: Some(SecretString::from(String::new())),
            ..SshTransportConfig::localhost_test(PathBuf::from("/dev/null"), 1 << 20)
        };
        assert!(!empty_pw.prefers_russh_leg());
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

    /// Verifies the core forced-termination fast path used by `cancel()` on Unix:
    /// a cloned `TcpStream` shares the same fd as the owned one, and
    /// `shutdown(Shutdown::Both)` from any thread immediately unblocks a blocking
    /// read on the other handle, so `cancel()` breaks a libssh2 read stuck inside
    /// the worker without waiting for a timeout.
    ///
    /// Unix-only: Winsock `shutdown()` does not reliably wake a concurrent blocking
    /// `recv()` on another thread, so this instant-unblock property does not hold on
    /// Windows. There, cancellation still works but is bounded by the worker's socket
    /// read timeout (`set_read_timeout(io_timeout_ms)`): the blocked read returns on
    /// timeout and the loop then observes the cancel flag. A prompt Windows unblock
    /// would need `CancelIoEx` / closing the socket; that is a responsiveness
    /// enhancement, not a correctness gap, so this fast-path test is gated to Unix.
    #[cfg(unix)]
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
        // Either EOF (Ok(0)) or an I/O error: both prove the read was
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

    // ---- Y-RSC.2: structured clean-EOF classification --------------------

    fn raw_stream_with_flag(cancelled: bool) -> super::SshRawStream {
        use std::sync::atomic::AtomicBool;
        let (sender, _receiver) = std::sync::mpsc::channel::<super::RawWorkerCommand>();
        // The receiver is dropped: these tests only exercise the error
        // mapping, never a live worker round-trip.
        super::SshRawStream {
            sender,
            cancel_flag: Arc::new(AtomicBool::new(cancelled)),
        }
    }

    #[test]
    fn raw_worker_clean_eof_maps_to_structured_clean_transport_eof() {
        let stream = raw_stream_with_flag(false);
        let err = stream.map_worker_error(super::RawWorkerError::CleanEof(
            "read_bytes: remote closed (exit 0): ".to_string(),
        ));
        assert!(err.is_clean_transport_eof());
        assert_eq!(
            err.kind,
            crate::aerorsync::types::AerorsyncErrorKind::TransportFailure
        );
        assert_eq!(err.detail, "read_bytes: remote closed (exit 0): ");
    }

    #[test]
    fn raw_worker_other_error_is_plain_transport_failure() {
        let stream = raw_stream_with_flag(false);
        let err = stream.map_worker_error(super::RawWorkerError::Other(
            "read_bytes: remote closed (exit 12): boom".to_string(),
        ));
        assert!(!err.is_clean_transport_eof());
        assert_eq!(
            err.kind,
            crate::aerorsync::types::AerorsyncErrorKind::TransportFailure
        );
    }

    #[test]
    fn raw_worker_error_after_cancel_maps_to_cancelled_even_for_clean_eof() {
        // Cancel precedence is pre-Y-RSC.2 behaviour: preserve it.
        let stream = raw_stream_with_flag(true);
        let err = stream.map_worker_error(super::RawWorkerError::CleanEof(
            "read_bytes: remote closed (exit 0): ".to_string(),
        ));
        assert_eq!(
            err.kind,
            crate::aerorsync::types::AerorsyncErrorKind::Cancelled
        );
        assert!(!err.is_clean_transport_eof());
    }

    // ---- Y-RSC.2: typed mutex-poison propagation -------------------------

    fn poisoned_transport() -> super::SshRemoteShellTransport {
        use super::SshTransportConfig;
        use std::path::PathBuf;
        let transport = super::SshRemoteShellTransport::new(SshTransportConfig::localhost_test(
            PathBuf::from("/dev/null"),
            1 << 20,
        ));
        let active = transport.active.clone();
        let _ = thread::spawn(move || {
            let _guard = active.lock().unwrap();
            panic!("poison the active-session mutex on purpose");
        })
        .join();
        assert!(transport.active.lock().is_err(), "mutex must be poisoned");
        transport
    }

    #[tokio::test]
    async fn cancel_on_poisoned_mutex_returns_typed_error_and_still_flags() {
        use super::RemoteShellTransport;
        use std::sync::atomic::Ordering;

        let transport = poisoned_transport();
        let err = transport
            .cancel()
            .await
            .expect_err("poisoned mutex must surface as a typed error, not a panic");
        assert_eq!(
            err.kind,
            crate::aerorsync::types::AerorsyncErrorKind::TransportFailure
        );
        assert!(err.detail.contains("poisoned"), "detail: {}", err.detail);
        // The cooperative cancel must have engaged before the failure.
        assert!(transport.cancel_flag.load(Ordering::SeqCst));
    }

    #[test]
    fn store_and_snapshot_active_return_typed_error_on_poisoned_mutex() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let tcp = TcpStream::connect(addr).unwrap();

        let transport = poisoned_transport();
        let (sender, _receiver) = std::sync::mpsc::channel();
        let store_err = transport
            .store_active(super::ActiveSession {
                sender,
                tcp: Arc::new(tcp),
            })
            .expect_err("store_active must not panic on poison");
        assert_eq!(
            store_err.kind,
            crate::aerorsync::types::AerorsyncErrorKind::TransportFailure
        );
        assert!(store_err.detail.contains("poisoned"));

        let snapshot_err = transport
            .snapshot_active()
            .expect_err("snapshot_active must not panic on poison");
        assert_eq!(
            snapshot_err.kind,
            crate::aerorsync::types::AerorsyncErrorKind::TransportFailure
        );
        assert!(snapshot_err.detail.contains("poisoned"));
    }

    #[test]
    fn snapshot_active_on_healthy_transport_is_none() {
        use super::SshTransportConfig;
        use std::path::PathBuf;
        let transport = super::SshRemoteShellTransport::new(SshTransportConfig::localhost_test(
            PathBuf::from("/dev/null"),
            1 << 20,
        ));
        let snapshot = transport
            .snapshot_active()
            .expect("healthy mutex snapshots fine");
        assert!(snapshot.is_none(), "no session was ever stored");
    }
}
