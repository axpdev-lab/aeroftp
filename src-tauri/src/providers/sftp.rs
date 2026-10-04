//! SFTP Provider Implementation
//!
//! This module provides SFTP (SSH File Transfer Protocol) support using the russh crate.
//! Supports both password and SSH key-based authentication.
//!
//! Status: v1.3.0

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use super::checksum_matrix;
use super::types::is_session_closed_error_message;
use super::{
    ChecksumCapability, ProviderError, ProviderTransferExecutorKind, ProviderType, RemoteEntry,
    SftpConfig, StorageProvider,
};
use crate::ssh_exec::ssh_exec_collect;
use async_trait::async_trait;
use russh::client::AuthResult;
use russh::client::{self, Config, Handle, Handler};
use russh::keys::{
    self, known_hosts, Algorithm, HashAlg, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate,
};
use russh::{compression, Preferred};
use russh_sftp::client::{RawSftpSession, SftpSession};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex as TokioMutex;
use tokio_util::sync::CancellationToken;

use super::multi_thread::{
    aerotmp_path_for, parallel_refused, run_concurrent_range_download, share_progress,
    source_changed, ConcurrentRangeConfig, ConcurrentRangeOutcome, RangeSourceFingerprint,
    SegmentedRun, SegmentedTemp, AFTER_TRANSFER_READ_RETRY,
};

/// Apply the native transport's production metadata policy in one testable
/// choke point. Xattrs are a Unix capability; POSIX access ACL preservation
/// is intentionally Linux-only because the ACL backend hard-rejects other
/// platforms. Directory default ACL awaits recursive entries.
#[cfg(feature = "aerorsync")]
fn configure_aerorsync_metadata(
    transport: crate::aerorsync::delta_transport_impl::AerorsyncDeltaTransport,
) -> crate::aerorsync::delta_transport_impl::AerorsyncDeltaTransport {
    #[cfg(unix)]
    let transport = transport.with_xattrs(true);
    #[cfg(target_os = "linux")]
    let transport = transport.with_acls(true);
    transport
}

/// Hard cap on intra-file SFTP range streams (PD-SFTP-2), mirroring the S3
/// `MULTI_THREAD_MAX_STREAMS`. Each stream is a full independent SSH
/// connection from the pool, so the cap stays conservative; the live
/// benchmark in master 9.6.2 says where it pays.
const SFTP_MULTI_THREAD_MAX_STREAMS: usize = 16;
/// Independent SSH connections a multi-file job may hold (see
/// `transfer_executor_max_sessions`).
const SFTP_POOL_MAX_SESSIONS: u16 = 16;

/// Job-wide guardrails for read-ahead. The byte budget includes one chunk per
/// reader, one channel window, and the writer's current chunk. The handle cap
/// additionally bounds server-side state when small chunks would otherwise
/// permit a very large numeric window.
const SFTP_READAHEAD_JOB_BUFFER_BUDGET: u64 = 128 * 1024 * 1024;
const SFTP_READAHEAD_JOB_MAX_HANDLES: usize = 256;
/// Read-ahead window used when nothing else asks for one. Measured on the
/// Hetzner lab SFTP over a 53 ms link with a 300 MiB file (2026-09-07):
/// serial reads 118 to 129 s, window 16: 38.8 s, window 32: 34.5 s, window
/// 64: 35.6 s, rclone 30 s. 32 is where the curve flattens; a bandwidth cap
/// still takes the serial loop, which owns the precise throttle.
const SFTP_READAHEAD_DEFAULT_WINDOW: usize = 32;
const SFTP_READAHEAD_MAX_WINDOW: usize = 1024;
/// The measured default window of one connection in bytes: 32 READs of the
/// 256 KiB default chunk.
const SFTP_READAHEAD_DEFAULT_BYTES: u64 = SFTP_READAHEAD_DEFAULT_WINDOW as u64 * 256 * 1024;
/// What the connections of one download share when nothing was asked: twice
/// the one-connection default, so one or two connections keep that default
/// and more connections split it.
const SFTP_READAHEAD_SHARED_BUDGET: u64 = 2 * SFTP_READAHEAD_DEFAULT_BYTES;
/// The least one connection of a segmented download keeps in flight when the
/// connections share the default budget: four 256 KiB READs.
const SFTP_READAHEAD_MIN_SHARED_BYTES: u64 = 4 * 256 * 1024;
/// The smallest chunk a read-ahead takes from the length of a server's first
/// reply (never above the configured chunk).
const SFTP_READAHEAD_MIN_LEARNED_CHUNK: u64 = 32 * 1024;

/// The default window as a budget for one download instead of a value for
/// each of its connections, counted in chunks of `chunk` bytes. Every READ in
/// flight holds a chunk-sized buffer here until its reply has been written
/// out, so the window per connection is what a segmented download keeps in
/// memory, times its connections. Each connection used to take the whole
/// default: four connections held 128 READs of 256 KiB, and a 300 MiB download
/// peaked at 142 to 151 MB of RSS, against 117 to 120 MB with 16 READs per
/// connection, as fast or faster in two rounds of three, and 102 to 106 MB with
/// 8, which was slower in most rounds (lab, 47 ms link, 2026-10-04). The budget
/// is in bytes, so a smaller chunk gets more READs for the same depth; never
/// fewer than two, below which there is no read-ahead at all.
fn shared_default_readahead_window(connections: usize, chunk: usize) -> usize {
    let per_connection = (SFTP_READAHEAD_SHARED_BUDGET / connections.max(1) as u64).clamp(
        SFTP_READAHEAD_MIN_SHARED_BYTES,
        SFTP_READAHEAD_DEFAULT_BYTES,
    );
    usize::try_from(per_connection / chunk.max(4096) as u64)
        .unwrap_or(usize::MAX)
        .max(2)
}

/// Default intra-file cutoff: below this a single SFTP stream is faster
/// than paying N SSH handshakes. Matches the S3 default (250 MiB) so the
/// `--multi-thread-cutoff` CLI flag behaves identically across backends.
const SFTP_MULTI_THREAD_CUTOFF_DEFAULT: u64 = 250 * 1024 * 1024;
/// Lower bound `set_multi_thread_download` enforces on the cutoff, also
/// exposed through `multi_thread_cutoff_floor` so the batch executor
/// applies the same bound as the single-file path.
const SFTP_MULTI_THREAD_CUTOFF_FLOOR: u64 = 1024 * 1024;
/// Map a russh / russh-sftp / io error onto a [`ProviderError`].
///
/// The russh family does not type-tag transport-level failures (broken
/// pipe, channel torn down, EOF after server idle reaper); they all
/// surface as opaque `Display` strings nested inside the operation
/// error. We string-match those patterns and route them into
/// [`ProviderError::ConnectionLost`] so the command layer can attempt
/// a silent reconnect+replay. Anything else falls through to the
/// caller-supplied fallback variant (NotFound / TransferFailed /
/// ServerError / ...) preserving the previous behavior.
fn classify_russh_err(
    e: impl std::fmt::Display,
    fallback: impl FnOnce(String) -> ProviderError,
) -> ProviderError {
    let s = e.to_string();
    if is_session_closed_error_message(&s) {
        ProviderError::ConnectionLost(s)
    } else if is_request_timeout(&s) {
        ProviderError::Timeout
    } else {
        fallback(s)
    }
}

/// Map an error of a request that creates, opens for writing, writes or
/// closes a remote file, or looks at one to resume it. A request timeout
/// there is not a limit reached with nothing failed (exit 8): the request may
/// have reached the server, and a 0-byte or partial file may be left at the
/// remote path, so the transfer failed (exit 4, still retried). The message
/// keeps the word "timeout", which the CLI's worker batches read as a lost
/// session. Every other error is classified as [`classify_russh_err`] does,
/// a transfer failure where nothing more specific applies.
fn classify_russh_write_err(e: impl std::fmt::Display, context: &str) -> ProviderError {
    let fallback = |s: String| ProviderError::TransferFailed(format!("{context}: {s}"));
    match classify_russh_err(e, fallback) {
        ProviderError::Timeout => fallback(
            "the server did not answer in time (timeout); the remote file may be incomplete"
                .to_string(),
        ),
        other => other,
    }
}

/// The provider's SFTP session, with the signal that its transport ended.
///
/// russh-sftp 2.4 does not wake a request that waits for its reply when the
/// transport goes away: its reader stops, but the senders the replies would
/// have gone through stay registered in a map the session still owns, so
/// nothing answers them. Most requests carry the session's 10 s timeout. The
/// acknowledgement of a pipelined WRITE carries none, and an upload waits on
/// the oldest one before it sends more, so an upload in flight when the
/// server died or restarted waited forever (live: `docker restart` of the
/// server during `put -r`, killed by the timeout). The transfer loops race
/// their waits against `ended` instead ([`until_sftp_ends`]).
struct SftpChannel {
    session: SftpSession,
    ended: CancellationToken,
}

impl SftpChannel {
    /// The SFTP session over `stream`, watched ([`WatchedSftpStream`]).
    async fn open<S>(stream: S) -> Result<Self, russh_sftp::client::error::Error>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let ended = CancellationToken::new();
        let stream = WatchedSftpStream {
            inner: stream,
            ended: ended.clone(),
        };
        let session = SftpSession::new(stream).await?;
        Ok(Self { session, ended })
    }
}

impl std::ops::Deref for SftpChannel {
    type Target = SftpSession;

    fn deref(&self) -> &SftpSession {
        &self.session
    }
}

impl std::ops::DerefMut for SftpChannel {
    fn deref_mut(&mut self) -> &mut SftpSession {
        &mut self.session
    }
}

/// The stream under an [`SftpChannel`]: the first read that finds it ended or
/// failed, and the first write or flush that fails, cancel `ended`.
struct WatchedSftpStream<S> {
    inner: S,
    ended: CancellationToken,
}

impl<S: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for WatchedSftpStream<S> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let room = buf.remaining();
        let before = buf.filled().len();
        let poll = std::pin::Pin::new(&mut this.inner).poll_read(cx, buf);
        let ended = match &poll {
            std::task::Poll::Ready(Ok(())) => room > 0 && buf.filled().len() == before,
            std::task::Poll::Ready(Err(_)) => true,
            std::task::Poll::Pending => false,
        };
        if ended {
            this.ended.cancel();
        }
        poll
    }
}

impl<S: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for WatchedSftpStream<S> {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let poll = std::pin::Pin::new(&mut this.inner).poll_write(cx, buf);
        if matches!(poll, std::task::Poll::Ready(Err(_))) {
            this.ended.cancel();
        }
        poll
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let poll = std::pin::Pin::new(&mut this.inner).poll_flush(cx);
        if matches!(poll, std::task::Poll::Ready(Err(_))) {
            this.ended.cancel();
        }
        poll
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// What a transfer wait cut short by the end of the transport reports. The
/// wording is one of `SESSION_CLOSED_NEEDLES`, so the command layer reads it
/// as a lost connection it may retry.
const SFTP_TRANSPORT_ENDED: &str = "SFTP session closed: the connection to the server ended";

/// One wait on the SFTP session that russh-sftp would not end if the
/// transport went away (see [`SftpChannel`]), ended as soon as it does.
async fn until_sftp_ends<T, E: From<std::io::Error>>(
    ended: &CancellationToken,
    wait: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, E> {
    tokio::select! {
        biased;
        result = wait => result,
        () = ended.cancelled() => Err(E::from(std::io::Error::new(
            std::io::ErrorKind::ConnectionAborted,
            SFTP_TRANSPORT_ENDED,
        ))),
    }
}

/// Closes a read handle without waiting on a transport that ended. The
/// `ended` token fires in russh-sftp's reader task before its writer drops the
/// request channel, so a CLOSE sent in that window registers a reply nobody
/// will send and waits out the session's 10 s; racing it ends it at once.
async fn close_sftp_file(file: russh_sftp::client::fs::File, ended: &CancellationToken) {
    let _ = until_sftp_ends(ended, file.close()).await;
}

/// How long an upload waits for the server to acknowledge its writes before
/// it gives the session up. It covers the case the end of the transport does
/// not: an SSH connection that still answers its keepalives while the SFTP
/// server behind it stopped answering. It leans toward patience. Writes go
/// out one WRITE at a time ([`SftpWriteBuffer`]) and russh-sftp keeps at most
/// 8 in flight, so a write waits for at most one acknowledgement and the close
/// for the 8 still in flight, about 2 MiB with OpenSSH's writes of just under
/// 256 KiB: only a link that cannot move that in five minutes (about 7 KB/s)
/// trips it, whatever `--buffer-size` is.
const SFTP_WRITE_ACK_BOUND: std::time::Duration = std::time::Duration::from_secs(300);

/// The buffer between an upload's local file and its remote handle.
///
/// russh-sftp cuts what it is handed into WRITEs of at most the server's
/// write length (OpenSSH announces 261120 bytes, 1 KiB under 256 KiB, through
/// `limits@openssh.com`) and keeps at most 8 in flight on a handle. Handed a
/// whole buffer at a time, it sent the end of each buffer as a short WRITE:
/// the 256 KiB default went out as 261120 bytes plus 1024, half of the 8
/// slots carried 1 KiB, and an upload kept about 0.8 MB on the wire where
/// 2 MB fit. A 300 MiB upload to the lab over a 47 ms link took 18 to 24 s,
/// and 10 to 11.5 s with a buffer of exactly one WRITE (2026-10-03). So the
/// buffer hands over one WRITE at a time, learns the session's write length
/// from the first WRITE that comes back short, and reads more before what is
/// left is less than a whole WRITE: only the last WRITE of a file is short,
/// whatever the server's limit and the buffer size.
struct SftpWriteBuffer {
    buf: Vec<u8>,
    start: usize,
    end: usize,
    eof: bool,
    /// The session's write length, once a WRITE carried less than it was
    /// offered.
    max_write: Option<usize>,
}

impl SftpWriteBuffer {
    fn new(size: usize) -> Self {
        Self {
            buf: vec![0u8; size.max(1)],
            start: 0,
            end: 0,
            eof: false,
            max_write: None,
        }
    }

    /// Reads from `local` until the buffer is full or the file ends, when
    /// what is left would not make a whole WRITE (while the write length is
    /// not known, when nothing is left). Returns the bytes read.
    async fn refill<R>(&mut self, local: &mut R) -> std::io::Result<usize>
    where
        R: tokio::io::AsyncRead + Unpin,
    {
        use tokio::io::AsyncReadExt;
        let whole_write = self.max_write.unwrap_or(self.buf.len());
        if self.eof || self.end - self.start >= whole_write {
            return Ok(0);
        }
        self.buf.copy_within(self.start..self.end, 0);
        self.end -= self.start;
        self.start = 0;
        let mut read = 0;
        while self.end < self.buf.len() {
            let n = local.read(&mut self.buf[self.end..]).await?;
            if n == 0 {
                self.eof = true;
                break;
            }
            self.end += n;
            read += n;
        }
        Ok(read)
    }

    /// Whether every byte of the file went into a WRITE.
    fn is_done(&self) -> bool {
        self.eof && self.start == self.end
    }

    /// Hands the next WRITE to `file`, under [`until_sftp_acks`], and returns
    /// the bytes it carried.
    async fn write_next<W>(
        &mut self,
        file: &mut W,
        ended: &CancellationToken,
    ) -> std::io::Result<usize>
    where
        W: tokio::io::AsyncWrite + Unpin,
    {
        use tokio::io::AsyncWriteExt;
        let offered = self.end - self.start;
        let n = until_sftp_acks(ended, file.write(&self.buf[self.start..self.end])).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "the SFTP session took no bytes of a WRITE",
            ));
        }
        if n < offered {
            self.max_write = Some(self.max_write.map_or(n, |m| m.max(n)));
        }
        self.start += n;
        Ok(n)
    }
}

/// A wait on write acknowledgements (a write that fills the pipeline, the
/// close that drains it): ended by the end of the transport or, failing
/// that, by [`SFTP_WRITE_ACK_BOUND`]. A session that ran into the bound
/// cannot be trusted any more, so it counts as ended.
async fn until_sftp_acks<T>(
    ended: &CancellationToken,
    wait: impl std::future::Future<Output = std::io::Result<T>>,
) -> std::io::Result<T> {
    match tokio::time::timeout(SFTP_WRITE_ACK_BOUND, until_sftp_ends(ended, wait)).await {
        Ok(result) => result,
        Err(_) => {
            ended.cancel();
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "SFTP server acknowledged no write in {} s: operation timed out",
                    SFTP_WRITE_ACK_BOUND.as_secs()
                ),
            ))
        }
    }
}

/// Whether an SFTP error is russh-sftp's own request timeout (its display is
/// exactly `Timeout`): a reply that did not come within 10 s. It is a timeout,
/// not a lost connection: a live but slow server (a large directory listing, a
/// stat on a network filesystem) answers late and the session is still good,
/// while a transport that really ended is seen by `is_connected` and redialled
/// on the next call. `list` and `stat` used to report it as not found (exit 2),
/// `mkdir` and `delete` as a server error; as a timeout it is exit 8, retried.
/// Only the exact text counts: a server message that contains the word (a path
/// such as `/x/timeout.log`) is not one.
fn is_request_timeout(message: &str) -> bool {
    message.trim() == "Timeout"
}

/// Map `SftpSession::try_exists` onto [`StorageProvider::exists`].
///
/// russh-sftp converts `SSH_FX_NO_SUCH_FILE` into `Ok(false)` and returns
/// every other failure as `Err`. Absence stays `Ok(false)`; permission,
/// session and I/O failures stay `Err`, so a root under an unreadable
/// parent is a gap rather than a missing source.
fn map_sftp_try_exists(
    result: Result<bool, russh_sftp::client::error::Error>,
) -> Result<bool, ProviderError> {
    match result {
        Ok(exists) => Ok(exists),
        Err(russh_sftp::client::error::Error::Status(status))
            if status.status_code == russh_sftp::protocol::StatusCode::NoSuchFile =>
        {
            Ok(false)
        }
        Err(e) => Err(classify_sftp_exists_err(e)),
    }
}

fn classify_sftp_exists_err(e: russh_sftp::client::error::Error) -> ProviderError {
    if let russh_sftp::client::error::Error::Status(status) = &e {
        match status.status_code {
            russh_sftp::protocol::StatusCode::PermissionDenied => {
                let message = if status.error_message.is_empty() {
                    status.status_code.to_string()
                } else {
                    status.error_message.clone()
                };
                return ProviderError::PermissionDenied(message);
            }
            russh_sftp::protocol::StatusCode::ConnectionLost => {
                return ProviderError::ConnectionLost(status.error_message.clone());
            }
            russh_sftp::protocol::StatusCode::NoConnection => {
                return ProviderError::NotConnected;
            }
            _ => {}
        }
    }
    classify_russh_err(e, |s| {
        ProviderError::ServerError(format!("Failed to check existence: {s}"))
    })
}

/// Shared, lock-protected handle to the underlying russh SSH session.
/// Used by sibling modules (e.g. rsync-over-SSH) to open additional channels
/// (exec, direct-tcpip) without re-authenticating.
pub type SharedSshHandle = Arc<TokioMutex<Handle<SshHandler>>>;

/// POSIX single-quote a string for safe interpolation into a remote shell
/// command. The whole value is wrapped in `'...'` (everything literal inside
/// single quotes) and every embedded `'` is emitted as `'\''` (close quote,
/// escaped literal quote, reopen quote). This neutralises `$()`, backticks,
/// `;`, `&&`, newlines and spaces: there is no shell metacharacter that
/// survives single-quoting. Used by [`SftpProvider::checksum`] before passing
/// a listing-derived path to `sha256sum` over an exec channel.
fn shell_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// SSH Client Handler for server key verification.
///
/// Exposed as `pub` because [`SharedSshHandle`] (a public type alias in the same
/// module) names it, and clippy's `exported_private_dependencies` lint requires the
/// visibility levels to match. Callers outside this module don't construct or
/// manipulate it: they only hold the handle and pass it back through APIs that
/// expect `SharedSshHandle`.
pub struct SshHandler {
    /// The host being connected to (for known_hosts lookup)
    host: String,
    /// The port being connected to
    port: u16,
    /// CLI mode: auto-accept unknown hosts and save to known_hosts
    trust_unknown_hosts: bool,
    /// Shared slot populated on successful verification with the
    /// SHA-256 hex fingerprint (lowercase, colon-free) of the server
    /// host key's SSH-wire-encoded bytes. The native rsync path
    /// (`providers::sftp::delta_transport`) consumes this to pin its
    /// second SSH connection: U-02 closes the MITM hole that
    /// `SshHostKeyPolicy::AcceptAny` left open on the native leg.
    host_key_sha256_hex: Arc<std::sync::OnceLock<String>>,
}

impl SshHandler {
    fn with_trust_and_slot(
        host: &str,
        port: u16,
        trust: bool,
        slot: Arc<std::sync::OnceLock<String>>,
    ) -> Self {
        Self {
            host: host.to_string(),
            port,
            trust_unknown_hosts: trust,
            host_key_sha256_hex: slot,
        }
    }

    /// Compute the SHA-256 hex digest of the SSH-wire-encoded public
    /// key bytes, matching the layout that libssh2's
    /// `session.host_key()` returns on the other side of the native
    /// rsync connection. Returns `None` if the russh key encoding fails
    ///: in that case the native path will refuse to enable because
    /// the slot stays empty (secure default).
    fn compute_host_key_fingerprint_hex(key: &PublicKey) -> Option<String> {
        use sha2::{Digest, Sha256};
        let wire = key.to_bytes().ok()?;
        let digest = Sha256::digest(&wire);
        let mut hex = String::with_capacity(digest.len() * 2);
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
        }
        Some(hex)
    }
}

impl Handler for SshHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        // russh 0.63 presents either a bare key or a host CERTIFICATE here.
        // The certificate arm is unreachable as this client is configured:
        // `Preferred::host_key_certificates` defaults to an empty list, so no
        // certificate algorithm is ever offered and a server cannot present
        // one. It is refused rather than left to a wildcard so that the answer
        // is a decision instead of an accident.
        //
        // The default holding is the second half of that claim, and it was
        // checked rather than assumed: this tree builds a `Preferred` of its
        // own in exactly two places, `providers/sftp.rs` (compression only) and
        // `aerorsync/russh_session_transport.rs` (host-key algorithms only),
        // and both take `host_key_certificates` from the default. The one that
        // does override `key` lists Ed25519, three ECDSA curves and two RSA
        // hashes, every one of them a plain-key algorithm, so it does not
        // reopen the door either. The arm goes live if any `Preferred` fills
        // that list, or if a `key` list gains a `*-cert-v01@openssh.com`
        // algorithm. Both of those edits are named by path on purpose, because
        // this paragraph is repeated in three handlers and "not in this file"
        // would be true in one of them and reassuring in exactly the file that
        // owns the dangerous line: today they are `providers/sftp.rs` and
        // `aerorsync/russh_session_transport.rs`, the two that build a
        // `Preferred`, the second holding the only `key` override, plus
        // wherever a new `Preferred` is added.
        //
        // WHOEVER ENABLES HOST CERTIFICATES MUST COME BACK TO ALL FOUR
        // HANDLERS FIRST. Filling that list makes this arm live, and accepting
        // a certificate is a trust
        // policy nobody has discussed: it would mean trusting a CA to vouch for
        // hosts, which is a different model from the known-hosts and pinned
        // fingerprint checks below. Refusing keeps today's behaviour exactly,
        // because today the case cannot arise.
        let server_public_key = match server_public_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key,
            PublicKeyOrCertificate::Certificate(_) => {
                tracing::warn!(
                    "SSH: refusing a host certificate; certificate algorithms are not offered \
                     by this client, so this should be unreachable"
                );
                return Ok(false);
            }
        };
        // Use russh's built-in known_hosts verification
        match known_hosts::check_known_hosts(&self.host, self.port, server_public_key) {
            Ok(true) => {
                tracing::info!("SFTP: Host key verified for {}", self.host);
                // U-02 slot populate: native rsync path pins against
                // this fingerprint.
                if let Some(hex) = Self::compute_host_key_fingerprint_hex(server_public_key) {
                    let _ = self.host_key_sha256_hex.set(hex);
                }
                Ok(true)
            }
            Ok(false) => {
                if self.trust_unknown_hosts {
                    // CLI --trust-host-key mode: accept and learn
                    tracing::info!(
                        "SFTP: Auto-accepting host key for {} (--trust-host-key)",
                        self.host
                    );
                    if let Err(e) =
                        known_hosts::learn_known_hosts(&self.host, self.port, server_public_key)
                    {
                        tracing::warn!("SFTP: Failed to save host key to known_hosts: {}", e);
                    }
                    if let Some(hex) = Self::compute_host_key_fingerprint_hex(server_public_key) {
                        let _ = self.host_key_sha256_hex.set(hex);
                    }
                    Ok(true)
                } else {
                    // SEC-P1-06: Host not in known_hosts: reject here.
                    // Frontend must call sftp_check_host_key + sftp_accept_host_key first.
                    tracing::warn!(
                        "SFTP: Host key for {} not pre-approved via TOFU dialog: rejecting",
                        self.host
                    );
                    Ok(false)
                }
            }
            Err(keys::Error::KeyChanged { line }) => {
                tracing::error!(
                    "SFTP: REJECTING connection to {} - host key changed at known_hosts line {} (possible MITM attack)",
                    self.host,
                    line
                );
                Ok(false)
            }
            Err(e) => {
                // SEC: Reject on unknown errors: do not silently accept.
                // Only TOFU (Ok(false)) should auto-accept; other errors may indicate
                // corrupted known_hosts or key format issues.
                tracing::error!(
                    "SFTP: REJECTING connection to {} - known_hosts verification error: {}",
                    self.host,
                    e
                );
                Ok(false)
            }
        }
    }
}

/// Secure connection spec retained after `connect()` so the shared
/// transfer engine can re-dial N **independent** SSH+SFTP connections for
/// file-level parallelism (PD-SFTP-1).
///
/// This mirrors `FtpConnectionSpec` / `FtpManager::connection_spec()`:
/// `provider_connect` zeroizes the outer config password after the first
/// connect, so the provider must retain its own `SecretString` copy.
/// Holding credentials for the provider's lifetime is the exact security
/// posture FTP already ships. Secrets are only exposed (`ExposeSecret`)
/// at dial time, never on IPC or in logs.
#[derive(Clone)]
pub struct SftpConnectionSpec {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: Option<secrecy::SecretString>,
    pub private_key_path: Option<String>,
    pub key_passphrase: Option<secrecy::SecretString>,
    pub initial_path: Option<String>,
    pub timeout_secs: u64,
    /// SHA-256 hex of the host key accepted by the first connect. Pool
    /// re-dials verify the new connection's key against this (defense in
    /// depth on top of `known_hosts`), same posture as the U-02 rsync pin.
    pub pinned_host_key_sha256: Option<String>,
}

impl SftpConnectionSpec {
    /// Rebuild an `SftpConfig` for an independent worker. `trust_unknown_hosts`
    /// is forced to `false`: a pooled re-dial must never TOFU, the host key
    /// is already in `known_hosts` from the first connect.
    fn to_config(&self) -> SftpConfig {
        SftpConfig {
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            password: self.password.clone(),
            private_key_path: self.private_key_path.clone(),
            key_passphrase: self.key_passphrase.clone(),
            initial_path: self.initial_path.clone(),
            timeout_secs: self.timeout_secs,
            trust_unknown_hosts: false,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum SftpReadaheadSetting {
    #[default]
    LegacyEnvironment,
    Disabled,
    Window(usize),
}

impl SftpReadaheadSetting {
    fn from_explicit(window: Option<usize>) -> Self {
        match window.and_then(normalize_sftp_readahead_window) {
            Some(window) => Self::Window(window),
            None => Self::Disabled,
        }
    }

    fn requested_window_from(self, legacy_raw: Option<&str>) -> Option<usize> {
        match self {
            // Nothing asked: read ahead by default. An explicit value in the
            // environment still wins, including an explicit "off"; the
            // single-stream SFTP download was one 256 KiB read per round trip
            // without it (see `SFTP_READAHEAD_DEFAULT_WINDOW`).
            Self::LegacyEnvironment => match legacy_raw {
                None => Some(SFTP_READAHEAD_DEFAULT_WINDOW),
                Some(raw) => parse_sftp_readahead_window(Some(raw)),
            },
            Self::Disabled => None,
            Self::Window(window) => Some(window),
        }
    }

    fn requested_window(self) -> Option<usize> {
        match self {
            Self::LegacyEnvironment => {
                self.requested_window_from(std::env::var("AEROFTP_SFTP_READAHEAD").ok().as_deref())
            }
            Self::Disabled | Self::Window(_) => self.requested_window_from(None),
        }
    }

    /// The window, in chunks of `chunk` bytes, each of the `connections` of
    /// one segmented download asks for. Nothing asked: the measured default
    /// is a budget for the whole download, which its connections share (see
    /// [`shared_default_readahead_window`]). Anything asked (CLI, preset, GUI
    /// or the environment) is a per-connection value and stays as asked.
    fn requested_window_per_connection_from(
        self,
        legacy_raw: Option<&str>,
        connections: usize,
        chunk: usize,
    ) -> Option<usize> {
        match (self, legacy_raw) {
            (Self::LegacyEnvironment, None) => {
                Some(shared_default_readahead_window(connections, chunk))
            }
            _ => self.requested_window_from(legacy_raw),
        }
    }

    fn requested_window_per_connection(self, connections: usize, chunk: usize) -> Option<usize> {
        match self {
            Self::LegacyEnvironment => self.requested_window_per_connection_from(
                std::env::var("AEROFTP_SFTP_READAHEAD").ok().as_deref(),
                connections,
                chunk,
            ),
            Self::Disabled | Self::Window(_) => {
                self.requested_window_per_connection_from(None, connections, chunk)
            }
        }
    }

    /// Whether a download reads ahead with the shared default rather than a
    /// window that was asked for.
    fn uses_shared_default_from(self, legacy_raw: Option<&str>) -> bool {
        matches!((self, legacy_raw), (Self::LegacyEnvironment, None))
    }

    fn uses_shared_default(self) -> bool {
        match self {
            Self::LegacyEnvironment => self
                .uses_shared_default_from(std::env::var("AEROFTP_SFTP_READAHEAD").ok().as_deref()),
            Self::Disabled | Self::Window(_) => false,
        }
    }
}

/// SFTP Provider
///
/// Provides secure file transfer over SSH using the SFTP protocol.
/// The OpenSSH extension that gives SFTP the replace semantics POSIX has and
/// plain SFTP does not.
///
/// `SSH_FXP_RENAME` in SFTP protocol 3 is specified to FAIL when the
/// destination already exists, and a server answers it with a bare
/// `SSH_FX_FAILURE` that says nothing else. So every caller that publishes a
/// staged temporary over a live file failed, on every server: the CLI and MCP
/// `edit`, the AeroCrypt marker publish, the crypt configuration writes. G119.
/// The control that settled it: the same overwrite, on the same server in the
/// same minute, succeeds through OpenSSH's own `sftp` client, because that
/// client sends this extension.
const POSIX_RENAME_EXTENSION: &str = "posix-rename@openssh.com";

/// The `posix-rename@openssh.com` payload: two SSH strings, old then new.
///
/// Wire-identical to `hardlink@openssh.com`, whose `HardlinkExtension` in
/// russh-sftp would encode it just as well. A named struct is used instead so
/// the packet that goes out says what it is.
#[derive(serde::Serialize)]
struct PosixRenamePayload {
    oldpath: String,
    newpath: String,
}

/// What one connection knows about [`POSIX_RENAME_EXTENSION`].
///
/// `SftpSession` keeps both the advertised extension map and `extended()` to
/// itself, so the answer has to come from a `RawSftpSession` of our own, on a
/// second channel. It is opened lazily, on the first rename that finds its
/// destination occupied, so a connection that never replaces a file never
/// pays for it, and the answer is remembered so a server without the
/// extension does not cost a channel per attempt.
enum PosixRenameSupport {
    /// Not asked yet on this connection.
    Unasked,
    /// Advertised, and this session performs it.
    Available(Box<RawSftpSession>),
    /// Not advertised by this server.
    Absent,
}

pub struct SftpProvider {
    config: SftpConfig,
    /// SSH connection handle (shared so rsync-over-SSH can open exec channels on the same session).
    ssh_handle: Option<SharedSshHandle>,
    /// SFTP session for file operations
    sftp: Option<SftpChannel>,
    /// Current working directory
    current_dir: String,
    /// Home directory (resolved on connect)
    home_dir: String,
    /// Download speed limit in bytes/sec (0 = unlimited)
    download_limit_bps: u64,
    /// Upload speed limit in bytes/sec (0 = unlimited)
    upload_limit_bps: u64,
    /// SSH compression enabled (zlib@openssh.com)
    compression_enabled: bool,
    /// Buffer size for download/upload (default: 32 KB)
    buffer_size: usize,
    /// Shared slot populated by [`SshHandler`] during `check_server_key`
    /// with the SHA-256 hex fingerprint of the accepted host key. The
    /// native rsync transport reuses this fingerprint to pin its own
    /// SSH connection (U-02) so the fresh TCP socket it opens for
    /// `aerorsync_serve` does not skip host-key verification.
    host_key_sha256_hex: Arc<std::sync::OnceLock<String>>,
    /// Secure connection spec for re-dialling independent pool sessions.
    /// `Some` once captured: either at the end of a successful `connect()`
    /// (before `provider_connect` zeroizes the outer config) or when this
    /// provider was produced by `clone_for_transfer()` as a not-yet-
    /// connected pool worker.
    connection_spec: Option<SftpConnectionSpec>,
    /// Intra-file parallel streams (PD-SFTP-2). `1` (default) disables it:
    /// the single-stream path stays the only behaviour. `>= 2` enables a
    /// chunked range download over N independent SSH connections for files
    /// at/above `multi_thread_cutoff`. Set via `set_multi_thread_download`
    /// (CLI `--multi-thread-streams`).
    multi_thread_streams: usize,
    /// File size at/above which intra-file parallelism engages.
    multi_thread_cutoff: u64,
    /// Per-provider read-ahead selection. The legacy environment is consulted
    /// only while this remains unspecified; GUI/CLI configuration replaces it
    /// with an isolated explicit value.
    sftp_readahead: SftpReadaheadSetting,
    /// What this connection knows about `posix-rename@openssh.com`; see
    /// [`PosixRenameSupport`]. Deliberately NOT carried over by
    /// `clone_for_transfer`: a pool worker dials its own connection, so it
    /// must ask its own server rather than inherit an answer about another.
    posix_rename: PosixRenameSupport,
    /// Test-only hook. Production never cancels this token; read-ahead used
    /// to watch a local `CancellationToken::new()` that no caller cancelled.
    /// Each download replaces the token in the slot, so two concurrent
    /// downloads on the same instance leave the slot watching only the later
    /// one. Arc+Mutex so a test can cancel without holding `&mut self`.
    transfer_cancel: Arc<std::sync::Mutex<CancellationToken>>,
    /// Test-only hook. When true, the next read-ahead writer iteration fails
    /// after the remote opens, so a test can count CLOSE on that exit.
    /// Production never sets it. Per-instance so parallel tests do not share
    /// one process-wide flag.
    fail_readahead_write: Arc<AtomicBool>,
    /// Set by `resume_download` for the download it runs, and taken by that
    /// download as it starts, so it never outlives the call: in place, an
    /// explicit resume goes on from the destination, where a plain download
    /// starts from zero (see `ResumableFile::open`).
    resume_in_place: bool,
}

impl SftpProvider {
    pub fn new(config: SftpConfig) -> Self {
        Self {
            config,
            ssh_handle: None,
            sftp: None,
            current_dir: "/".to_string(),
            home_dir: "/".to_string(),
            download_limit_bps: 0,
            upload_limit_bps: 0,
            compression_enabled: false,
            // 256 KiB is the sweet spot for SFTP throughput on modern links:
            // 32 KiB caps loopback at ~35 MB/s, 256 KiB reaches ~65 MB/s,
            // and 1 MiB only adds another ~5 MB/s while wasting RAM. OpenSSH
            // (>=8) and russh-sftp both negotiate packet sizes well above 32K
            // in practice. Override per-call with --chunk-size / --buffer-size.
            buffer_size: 256 * 1024,
            host_key_sha256_hex: Arc::new(std::sync::OnceLock::new()),
            connection_spec: None,
            multi_thread_streams: 1,
            multi_thread_cutoff: SFTP_MULTI_THREAD_CUTOFF_DEFAULT,
            sftp_readahead: SftpReadaheadSetting::LegacyEnvironment,
            posix_rename: PosixRenameSupport::Unasked,
            transfer_cancel: Arc::new(std::sync::Mutex::new(CancellationToken::new())),
            fail_readahead_write: Arc::new(AtomicBool::new(false)),
            resume_in_place: false,
        }
    }

    /// Shared slot for the in-flight download token. Test-only hook; see
    /// `transfer_cancel`.
    #[doc(hidden)]
    pub fn transfer_cancel_slot(&self) -> Arc<std::sync::Mutex<CancellationToken>> {
        Arc::clone(&self.transfer_cancel)
    }

    /// Arm the next read-ahead writer iteration to fail. Test-only hook; see
    /// `fail_readahead_write`.
    #[doc(hidden)]
    pub fn set_fail_readahead_write(&self, fail: bool) {
        self.fail_readahead_write.store(fail, Ordering::SeqCst);
    }

    /// Return the SHA-256 hex fingerprint of the host key that
    /// [`SshHandler::check_server_key`] accepted during the current
    /// SFTP session, or `None` before a successful handshake.
    ///
    /// Used by [`SftpProvider::delta_transport`] (U-02) to pin the
    /// native rsync path's independent SSH connection against the same
    /// fingerprint the classic SFTP verification already cleared.
    pub fn accepted_host_key_sha256_hex(&self) -> Option<String> {
        self.host_key_sha256_hex.get().cloned()
    }

    /// Secure connection spec retained after a successful `connect()`.
    /// Mirrors `FtpManager::connection_spec()`. `None` until connected (or
    /// until set by `clone_for_transfer()` on a pool worker).
    pub fn connection_spec(&self) -> Option<SftpConnectionSpec> {
        self.connection_spec.clone()
    }

    /// Ensure this provider has its own independent, authenticated SSH+SFTP
    /// connection (PD-SFTP-1). A `clone_for_transfer()` worker starts
    /// unconnected and carries only the secure spec; the first transfer
    /// dials a **separate** SSH connection (separate TCP socket, separate
    /// auth) so N files run truly in parallel, exactly like the FTP pool.
    ///
    /// Host-key safety on the re-dial: `connect()` still goes through
    /// `SshHandler` -> `known_hosts` with `trust_unknown_hosts = false`
    /// (never TOFU on a pooled dial; the key is already known from the
    /// first connect, `KeyChanged` is rejected). Defense in depth: the
    /// freshly accepted fingerprint is compared against the pin captured
    /// at the first connect and a mismatch aborts the worker.
    /// Close the SSH connection this provider holds, without waiting on it for
    /// long. After a stalled session it may still be up (an SFTP server that
    /// stopped answering behind a live sshd, keepalives still answered), and
    /// replacing the handle alone would leave it open for the life of the
    /// process: the SFTP reader task keeps a clone of its sender.
    async fn retire_ssh_connection(&mut self) {
        if let Some(handle) = self.ssh_handle.take() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), async move {
                let guard = handle.lock().await;
                let _ = guard
                    .disconnect(russh::Disconnect::ByApplication, "", "en")
                    .await;
            })
            .await;
        }
    }

    async fn ensure_connected(&mut self) -> Result<(), ProviderError> {
        if self.is_connected() {
            return Ok(());
        }
        if self.sftp.take().is_some() {
            // A session whose transport ended answers nothing: dial again, as
            // for a worker that never dialled.
            self.retire_ssh_connection().await;
        }
        let spec = self
            .connection_spec
            .clone()
            .ok_or(ProviderError::NotConnected)?;
        self.config = spec.to_config();
        // Fresh per-connection slot so the comparison reflects this dial.
        self.host_key_sha256_hex = Arc::new(std::sync::OnceLock::new());
        // A clone worker carries the primary's working directory so a relative
        // path resolves where the primary would resolve it; `connect` resets it
        // to the initial path or the home. Keep what the clone was given across
        // this first, lazy dial (scan and transfer workers never `cd` on their
        // own).
        let carried_current_dir = self.current_dir.clone();
        self.connect().await?;
        self.current_dir = carried_current_dir;
        if let Some(pinned) = spec.pinned_host_key_sha256.as_deref() {
            match self.accepted_host_key_sha256_hex().as_deref() {
                Some(seen) if seen == pinned => {}
                other => {
                    let _ = self.disconnect().await;
                    return Err(ProviderError::ConnectionFailed(format!(
                        "SFTP pool re-dial host key mismatch (expected {}, got {:?}): aborting worker",
                        pinned, other
                    )));
                }
            }
        }
        Ok(())
    }

    /// The object as this session sees it now, read on the connection that is
    /// already open.
    ///
    /// Going through the trait's `stat` would mean a session of its own, and
    /// on SFTP a session is a full handshake: about 1.3 seconds on the lab
    /// link, twice per segmented download, whether or not anything changed.
    async fn range_source_reading(
        sftp: &SftpChannel,
        full_path: &str,
    ) -> Result<RangeSourceFingerprint, String> {
        let metadata = until_sftp_ends(&sftp.ended, sftp.metadata(full_path))
            .await
            .map_err(|e| format!("it could not be read ({e})"))?;
        let entry = Self::metadata_to_entry(String::new(), full_path.to_string(), &metadata);
        Ok(RangeSourceFingerprint::of(&entry))
    }

    /// Run the parallel download and publish it only if the object did not
    /// move while the windows were reading it.
    ///
    /// `Ok(true)` it is published, `Ok(false)` it was refused and the caller
    /// takes its single-stream path, `Err` a real failure. Both readings of
    /// the object go through this session, which is open and idle while the
    /// windows run on their own connections. Opening a session for them would
    /// cost a full SFTP handshake twice on every segmented download, about
    /// 1.3 seconds each on the lab link, and that cost is fixed: it weighs
    /// most exactly where the transfer is fastest.
    async fn parallel_download_if_unchanged(
        &self,
        remote_path: &str,
        full_path: &str,
        local_path: &str,
        total_size: u64,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<bool, ProviderError> {
        let sftp = self.get_sftp()?;
        let before = match Self::range_source_reading(sftp, full_path).await {
            Ok(reading) => match reading.matches_planned_size(total_size) {
                Ok(()) => reading,
                Err(why) => {
                    tracing::warn!("{}", parallel_refused("SFTP intra-file", remote_path, &why));
                    return Ok(false);
                }
            },
            Err(why) => {
                tracing::warn!("{}", parallel_refused("SFTP intra-file", remote_path, &why));
                return Ok(false);
            }
        };

        let temp = self
            .download_intra_file_pooled(remote_path, local_path, total_size, on_progress)
            .await?;

        // One retry, the same tolerance the shared comparison gives the other
        // providers: this reading decides whether bytes already on disk are
        // kept, and a hiccup is not proof that the object moved.
        let changed = match Self::range_source_reading(sftp, full_path).await {
            Ok(after) => before.differs_from(&after),
            Err(first) => {
                tracing::warn!(
                    "SFTP intra-file: {} could not be read after the transfer ({}), reading once more",
                    full_path,
                    first
                );
                tokio::time::sleep(AFTER_TRANSFER_READ_RETRY).await;
                match Self::range_source_reading(sftp, full_path).await {
                    Ok(after) => before.differs_from(&after),
                    Err(why) => Some(format!(
                        "it could not be read again after the transfer: {why}"
                    )),
                }
            }
        };
        // The temporary stays claimed through the reading above and the rename.
        if let Some(what) = changed {
            temp.discard();
            tracing::warn!("{}", source_changed("SFTP intra-file", remote_path, &what));
            return Ok(false);
        }
        match temp.publish(Path::new(local_path)).await {
            Ok(()) => {
                tracing::info!("SFTP: intra-file download complete: {}", remote_path);
                Ok(true)
            }
            Err(e) => Err(ProviderError::IoError(e)),
        }
    }

    /// PD-SFTP-2 intra-file download: split a large file into N gap-free
    /// windows, each streamed over its **own independent SSH connection**
    /// (the exact connection model of the file-level pool: spec re-dial with
    /// host-key pin, no shared SSH handle/channel), assembled into a
    /// pre-allocated `.aerosegtmp`, which the caller publishes once it has read
    /// the object again. Reuses the shared
    /// [`run_concurrent_range_download`] orchestrator (plan / temp / RAII
    /// cleanup / bounded concurrency / progress / cancel) so HTTP and SFTP
    /// share one engine, not a fifth implementation.
    ///
    /// Strict gate (the SFTP equivalent of HTTP `206` + `Content-Range`):
    /// every window must yield exactly `end - start + 1` bytes; a premature
    /// EOF is a hard error, never a silent short read. SFTP has no
    /// `ServerIgnoredRange` analogue (`seek`+`read` cannot ignore a range),
    /// so that orchestrator arm is unreachable here and fails loud if hit.
    ///
    /// Single-session READ pipelining (rclone's `--sftp-concurrency`) is
    /// deliberately **not** implemented: per the rev-3 honesty rule it is an
    /// optional, separately-measured tier, never a closure promise. N
    /// independent connections is the mechanism, exactly like PD-SFTP-1.
    async fn download_intra_file_pooled(
        &self,
        remote_path: &str,
        local_path: &str,
        total_size: u64,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<SegmentedTemp, ProviderError> {
        let spec = self
            .connection_spec
            .clone()
            .ok_or(ProviderError::NotConnected)?;
        // R21: window count from the shared planner via the provider gate
        // method (the caller's gate already ran it and got >= 2); never below
        // 2 on this path.
        let streams = self.planned_download_segments(total_size).max(2);
        let buffer_size = self.buffer_size.max(4096);
        // Split any bandwidth cap across the N connections so the aggregate
        // stays near the user's limit (same intent as the single-stream
        // throttle; no new semantics vs the PD-SFTP-1 file-level pool, which
        // also runs N connections).
        let per_stream_limit_bps = if self.download_limit_bps > 0 {
            (self.download_limit_bps / streams as u64).max(1)
        } else {
            0
        };
        let remote_path_owned = remote_path.to_string();
        let current_dir = self.current_dir.clone();
        let home_dir = self.home_dir.clone();
        let compression_enabled = self.compression_enabled;
        let requested_readahead_window = self
            .sftp_readahead
            .requested_window_per_connection(streams, buffer_size);
        // On a server that sends less per READ than the chunk, the shared
        // default may grow back to the handles the release opened on each
        // connection, never past them; a window that was asked for stays as
        // asked (see `sftp_readahead_range_into`).
        let readahead_grow_to = self.sftp_readahead.uses_shared_default().then(|| {
            SFTP_READAHEAD_DEFAULT_WINDOW.min(
                SFTP_READAHEAD_JOB_MAX_HANDLES / streams.clamp(1, SFTP_MULTI_THREAD_MAX_STREAMS),
            )
        });

        let cfg = ConcurrentRangeConfig {
            final_path: PathBuf::from(local_path),
            provider_type: ProviderType::Sftp,
            endpoint_identity: self.endpoint_identity(),
            total_size,
            streams,
            max_streams: SFTP_MULTI_THREAD_MAX_STREAMS,
            max_parallel: streams,
        };

        tracing::info!(
            "SFTP: intra-file download {} ({} bytes) over {} independent connections",
            remote_path_owned,
            total_size,
            streams
        );

        let write_one_range = move |start: u64,
                                    end: u64,
                                    temp_path: PathBuf,
                                    aggregate: Arc<AtomicU64>,
                                    cancel: CancellationToken| {
            let spec = spec.clone();
            let remote_path = remote_path_owned.clone();
            let current_dir = current_dir.clone();
            let home_dir = home_dir.clone();
            async move {
                sftp_download_one_range(
                    spec,
                    remote_path,
                    current_dir,
                    home_dir,
                    buffer_size,
                    per_stream_limit_bps,
                    compression_enabled,
                    streams,
                    requested_readahead_window,
                    readahead_grow_to,
                    start,
                    end,
                    temp_path,
                    aggregate,
                    cancel,
                )
                .await
            }
        };

        let outcome = run_concurrent_range_download(
            cfg,
            write_one_range,
            CancellationToken::new(),
            on_progress,
        )
        .await;

        match outcome {
            // The windows are in `<local>.aerosegtmp` and the file is not
            // published here: the caller reads the object again through the
            // session it already holds and publishes only if it did not move.
            Ok(SegmentedRun::Completed(temp)) => Ok(temp),
            Ok(SegmentedRun::ServerIgnoredRange) => {
                // Unreachable for SFTP: seek+read cannot "ignore" a range.
                // Never silently re-download (it would double the bytes). The
                // engine already removed its temporary; one found at that name
                // now belongs to another download.
                Err(ProviderError::TransferFailed(
                    "SFTP intra-file: unexpected range-ignored outcome".to_string(),
                ))
            }
            Err(e) => Err(e),
        }
    }

    /// Return a cloneable handle to the underlying SSH session, if connected.
    ///
    /// Exposed to let sibling modules (rsync-over-SSH, port forwarding, ...)
    /// open additional channels on the same authenticated session. The handle
    /// is protected by a Tokio [`Mutex`](TokioMutex): callers should hold the
    /// guard for the minimal time required to send a message, since concurrent
    /// SFTP operations go through the same inner mpsc sender.
    pub fn handle_shared(&self) -> Option<SharedSshHandle> {
        self.ssh_handle.clone()
    }

    /// Build a [`DeltaTransport`](crate::delta_transport::DeltaTransport) ready to
    /// run against this provider's SSH session, or `None` if this provider is not
    /// currently eligible for delta sync.
    ///
    /// Eligibility conditions (all must hold):
    /// - Provider is connected (shared handle present)
    /// - SSH authentication has either a private key path on disk or a
    ///   non-empty password saved in the profile.
    ///
    /// This method is the single choke point where an `SftpProvider` becomes a
    /// `dyn DeltaTransport`. The adapter layer (`delta_sync_rsync`) never reaches
    /// into provider internals, preserving the forward compatibility promise for
    /// the strada C native transport.
    ///
    /// ## Cross-OS (PR-T11)
    ///
    /// - **Unix + any build**: returns `RsyncBinaryTransport` as the classic
    ///   fallback when the native feature is off or refuses.
    /// - **Unix + `aerorsync`**: attempts `AerorsyncDeltaTransport`
    ///   first (if the runtime toggle and host-key pinning allow), otherwise
    ///   falls back to `RsyncBinaryTransport`.
    /// - **Windows + `aerorsync`**: uses the native transport only.
    ///   Without the feature compiled in, this method returns `None` so the
    ///   consumer transparently drops to classic SFTP (same shape the adapter
    ///   already accepts for non-SFTP providers).
    pub fn delta_transport(&self) -> Option<Box<dyn crate::delta_transport::DeltaTransport>> {
        let handle = self.ssh_handle.clone()?;
        let known_hosts_path = dirs::home_dir().map(|h| h.join(".ssh").join("known_hosts"));
        let rsync_config = self.rsync_config_for_delta(known_hosts_path)?;

        #[cfg(feature = "aerorsync")]
        {
            // Runtime toggle - read from settings. When on, attempt
            // AerorsyncDeltaTransport and fall through to classic
            // binary on any construction error.
            //
            // U-02 security gate: the native path opens its own SSH
            // connection (separate TCP socket, separate libssh2 session)
            // and must not weaken the host-key posture of the parent
            // SFTP session. We only enable the native leg when the
            // classic SFTP flow has already captured the accepted host
            // key's SHA-256 fingerprint. Without a fingerprint we refuse
            // to enable native: the fresh SSH connection would otherwise
            // ride `AcceptAny`, which is a MITM window on a second
            // independent socket.
            let native_mode = crate::settings::load_native_rsync_mode();
            if !matches!(native_mode, crate::settings::NativeRsyncMode::Classic) {
                use crate::aerorsync::ssh_transport::SshHostKeyPolicy;

                let host_key_policy = match self.accepted_host_key_sha256_hex() {
                    Some(hex) => SshHostKeyPolicy::pinned_hex(hex),
                    None => {
                        tracing::warn!(
                            "providers::sftp: native rsync disabled for this session: parent \
                             SFTP handshake did not capture a host key fingerprint (possible \
                             incomplete handshake or early error); falling back to classic"
                        );
                        if matches!(native_mode, crate::settings::NativeRsyncMode::Native) {
                            tracing::warn!(
                                "providers::sftp: native-only rsync mode selected; skipping classic binary fallback"
                            );
                            return None;
                        }
                        return classic_binary_fallback(rsync_config, handle);
                    }
                };

                match crate::aerorsync_adapter::config::transport_from_rsync_config(
                    &rsync_config,
                    host_key_policy,
                ) {
                    Ok(transport) => {
                        // B4 / ACL B4: production metadata opt-ins follow live
                        // stock-rsync acceptance. fail_on_metadata_loss remains
                        // off, so destination ENOTSUP stays soft by default.
                        let transport = configure_aerorsync_metadata(transport);
                        tracing::info!(
                            "providers::sftp: using native rsync delta transport (host key pinned)"
                        );
                        return Some(Box::new(transport));
                    }
                    Err(error) => {
                        tracing::warn!(
                            "providers::sftp: native rsync transport construction failed ({error}); falling back to classic"
                        );
                        if matches!(native_mode, crate::settings::NativeRsyncMode::Native) {
                            tracing::warn!(
                                "providers::sftp: native-only rsync mode selected; skipping classic binary fallback"
                            );
                            return None;
                        }
                    }
                }
            }
        }

        classic_binary_fallback(rsync_config, handle)
    }

    fn expand_home_path(path: &str) -> String {
        if let Some(stripped) = path.strip_prefix("~/") {
            if let Some(home) = dirs::home_dir() {
                return home.join(stripped).to_string_lossy().to_string();
            }
        }

        path.to_string()
    }

    fn rsync_config_for_delta(
        &self,
        known_hosts_path: Option<std::path::PathBuf>,
    ) -> Option<crate::rsync_over_ssh::RsyncConfig> {
        use crate::rsync_over_ssh::AuthMethod;
        use secrecy::ExposeSecret;

        let (ssh_key_path, ssh_password, auth_method) =
            if let Some(key_path_str) = self.config.private_key_path.as_ref() {
                (
                    Some(std::path::PathBuf::from(Self::expand_home_path(
                        key_path_str,
                    ))),
                    None,
                    AuthMethod::SshKey,
                )
            } else {
                let password = self
                    .config
                    .password
                    .as_ref()
                    .filter(|secret| !secret.expose_secret().is_empty())?;
                (None, Some(password.clone()), AuthMethod::Password)
            };

        Some(crate::rsync_over_ssh::RsyncConfig {
            compress: true,
            preserve_times: true,
            progress: true,
            min_file_size: crate::rsync_over_ssh::DEFAULT_MIN_FILE_SIZE,
            ssh_key_path,
            ssh_password,
            auth_method,
            ssh_port: Some(self.config.port),
            ssh_user: self.config.username.clone(),
            ssh_host: self.config.host.clone(),
            // Classic SFTP flow already verified the host key via
            // `SshHandler::check_server_key`; rsync's SSH transport can
            // trust that verification for the same session.
            strict_host_key_check: "accept-new".to_string(),
            known_hosts_path,
        })
    }
}

/// PR-T11 cross-OS helper. On Unix this constructs the classic
/// `RsyncBinaryTransport` that drives the system `rsync` binary; on Windows
/// the binary is not available, so we silently return `None` and let the
/// consumer fall through to standard SFTP (identical shape to the
/// "non-SFTP provider" branch already handled upstream).
fn classic_binary_fallback(
    rsync_config: crate::rsync_over_ssh::RsyncConfig,
    handle: SharedSshHandle,
) -> Option<Box<dyn crate::delta_transport::DeltaTransport>> {
    #[cfg(unix)]
    {
        Some(Box::new(crate::delta_transport::RsyncBinaryTransport::new(
            rsync_config,
            Some(handle),
        )))
    }
    #[cfg(not(unix))]
    {
        let _ = (rsync_config, handle);
        tracing::debug!(
            "providers::sftp: no binary rsync on this platform; classic fallback returns None \
             (caller transparently drops to plain SFTP)"
        );
        None
    }
}

impl SftpProvider {
    /// Normalize path (ensure absolute)
    fn normalize_path(&self, path: &str) -> String {
        if path.starts_with('/') {
            path.to_string()
        } else if path.is_empty() || path == "." {
            self.current_dir.clone()
        } else if path == ".." {
            let parent = Path::new(&self.current_dir)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| "/".to_string());
            if parent.is_empty() {
                "/".to_string()
            } else {
                parent
            }
        } else if path == "~" {
            self.home_dir.clone()
        } else if let Some(stripped) = path.strip_prefix("~/") {
            format!("{}/{}", self.home_dir.trim_end_matches('/'), stripped)
        } else {
            format!("{}/{}", self.current_dir.trim_end_matches('/'), path)
        }
    }

    /// Get SFTP session or error if not connected
    /// Open, once per connection, the raw SFTP session used for
    /// `posix-rename@openssh.com`, and report whether this server offers it.
    ///
    /// `Ok(None)` means the server does not advertise the extension. That is a
    /// fact about the server and not a failure, so the caller turns it into a
    /// refusal the user can act on rather than into a retry.
    ///
    /// The channel is a second one on the same SSH connection, which is why
    /// the ordering holds: the client does not send the rename until the
    /// upload's `SSH_FXP_CLOSE` has been answered on the first channel, so the
    /// second `sftp-server` never sees a half-written temporary.
    async fn posix_rename_session(&mut self) -> Result<Option<&RawSftpSession>, ProviderError> {
        if matches!(self.posix_rename, PosixRenameSupport::Unasked) {
            let handle = self.ssh_handle.clone().ok_or(ProviderError::NotConnected)?;
            let channel = {
                let guard = handle.lock().await;
                guard.channel_open_session().await.map_err(|e| {
                    classify_russh_err(e, |s| {
                        ProviderError::ServerError(format!(
                            "Failed to open a channel to ask for {POSIX_RENAME_EXTENSION}: {s}"
                        ))
                    })
                })?
            };
            channel.request_subsystem(true, "sftp").await.map_err(|e| {
                classify_russh_err(e, |s| {
                    ProviderError::ServerError(format!(
                        "Failed to request the SFTP subsystem for {POSIX_RENAME_EXTENSION}: {s}"
                    ))
                })
            })?;
            let session = RawSftpSession::new(channel.into_stream());
            let version = session.init().await.map_err(|e| {
                classify_russh_err(e, |s| {
                    ProviderError::ServerError(format!(
                        "Failed to negotiate the SFTP session for {POSIX_RENAME_EXTENSION}: {s}"
                    ))
                })
            })?;
            self.posix_rename = if version.extensions.contains_key(POSIX_RENAME_EXTENSION) {
                PosixRenameSupport::Available(Box::new(session))
            } else {
                tracing::info!(
                    "SFTP: {} does not advertise {}; replacing a file in place is not available",
                    self.config.host,
                    POSIX_RENAME_EXTENSION
                );
                PosixRenameSupport::Absent
            };
        }
        Ok(match &self.posix_rename {
            PosixRenameSupport::Available(session) => Some(session),
            _ => None,
        })
    }

    fn get_sftp(&self) -> Result<&SftpChannel, ProviderError> {
        self.sftp.as_ref().ok_or(ProviderError::NotConnected)
    }

    /// Get mutable SFTP session or error if not connected
    #[allow(dead_code)]
    fn get_sftp_mut(&mut self) -> Result<&mut SftpChannel, ProviderError> {
        self.sftp.as_mut().ok_or(ProviderError::NotConnected)
    }

    /// Convert russh-sftp metadata to RemoteEntry.
    ///
    /// Free of `&self` so `list` can call it from inside the concurrent
    /// per-entry futures without capturing the provider.
    fn metadata_to_entry(
        name: String,
        path: String,
        metadata: &russh_sftp::protocol::FileAttributes,
    ) -> RemoteEntry {
        let is_dir = metadata
            .permissions
            .map(|p| (p & 0o40000) != 0)
            .unwrap_or(false);

        let permissions = metadata.permissions.map(|p| format_permissions(p, is_dir));

        let modified = metadata.mtime.map(|t| {
            chrono::DateTime::from_timestamp(t as i64, 0)
                .map(|dt| dt.format("%Y-%m-%d %H:%M:%SZ").to_string())
                .unwrap_or_default()
        });

        RemoteEntry {
            name,
            path,
            is_dir,
            size: metadata.size.unwrap_or(0),
            modified,
            permissions,
            owner: metadata.uid.map(|u| u.to_string()),
            group: metadata.gid.map(|g| g.to_string()),
            is_symlink: false, // Will be set separately for symlinks
            link_target: None,
            mime_type: None,
            metadata: Default::default(),
        }
    }

    /// Authenticate using SSH private key
    async fn authenticate_with_key(
        &self,
        handle: &mut Handle<SshHandler>,
    ) -> Result<bool, ProviderError> {
        let key_path = self.config.private_key_path.as_ref().ok_or_else(|| {
            ProviderError::AuthenticationFailed("No private key path specified".to_string())
        })?;

        let expanded_path = Self::expand_home_path(key_path);

        tracing::info!("SFTP: Loading private key from {}", expanded_path);

        // Load and parse the key using russh's built-in key loading
        use secrecy::ExposeSecret;
        let passphrase_str = self
            .config
            .key_passphrase
            .as_ref()
            .map(|s| s.expose_secret().to_string());
        let key_pair =
            keys::load_secret_key(&expanded_path, passphrase_str.as_deref()).map_err(|e| {
                ProviderError::AuthenticationFailed(format!("Failed to load key: {}", e))
            })?;

        // A1 finding: RSA keys authenticated with `None` (= ssh-rsa /
        // SHA-1) are rejected by OpenSSH 8.8+ because RSA-SHA1 is
        // disabled by default. We have to negotiate rsa-sha2-512 or
        // rsa-sha2-256 depending on the key type. For non-RSA keys
        // (ed25519, ecdsa) the hash is baked into the algorithm name so
        // `None` is correct and required.
        //
        // Strategy: try SHA-512 first (RFC 8332 preference), fall back
        // to SHA-256 on auth failure, then fall back to no-hash (ssh-rsa
        // SHA-1) for ancient servers that still accept it. Non-RSA
        // keys take the `None` path directly.
        let key_pair = Arc::new(key_pair);
        let is_rsa = matches!(key_pair.algorithm(), Algorithm::Rsa { .. });

        let attempts: Vec<Option<HashAlg>> = if is_rsa {
            vec![Some(HashAlg::Sha512), Some(HashAlg::Sha256), None]
        } else {
            vec![None]
        };

        let mut last_auth_error: Option<String> = None;
        for hash in attempts {
            let key_with_hash = PrivateKeyWithHashAlg::new(key_pair.clone(), hash);
            match handle
                .authenticate_publickey(&self.config.username, key_with_hash)
                .await
            {
                Ok(AuthResult::Success) => return Ok(true),
                Ok(AuthResult::Failure { .. }) => {
                    // Next hash algorithm; OpenSSH returns this for
                    // "publickey accepted but signature algo rejected".
                    continue;
                }
                Err(e) => {
                    last_auth_error = Some(e.to_string());
                    continue;
                }
            }
        }

        if let Some(err) = last_auth_error {
            return Err(ProviderError::AuthenticationFailed(format!(
                "Key authentication failed after RSA SHA-512/256/1 negotiation attempts: {err}"
            )));
        }
        Ok(false)
    }

    async fn verify_remote_upload_size(
        &self,
        sftp: &SftpChannel,
        remote_path: &str,
        expected_size: u64,
    ) -> Result<(), ProviderError> {
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(3);
        let mut last_observation = format!("expected {} bytes, got no metadata yet", expected_size);

        loop {
            match until_sftp_ends(&sftp.ended, sftp.metadata(remote_path)).await {
                Ok(metadata) => {
                    let actual_size = metadata.size.unwrap_or(0);
                    if actual_size == expected_size {
                        return Ok(());
                    }
                    last_observation = format!(
                        "expected {} bytes, got {} bytes",
                        expected_size, actual_size
                    );
                }
                // No answer can come any more: a lost connection, not a
                // verification to keep trying for its 3 s.
                Err(error) if sftp.ended.is_cancelled() => {
                    return Err(classify_russh_write_err(
                        error,
                        &format!("Upload verification failed for {remote_path}"),
                    ));
                }
                Err(error) => {
                    last_observation = error.to_string();
                }
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(ProviderError::TransferFailed(format!(
                    "Upload verification failed for {}: {}",
                    remote_path, last_observation,
                )));
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }
    }

    /// Align the remote file's mtime/atime with the local source so repeated
    /// sync scans don't re-upload unchanged files just because the server
    /// stamped the upload time. Best-effort: failures are logged, not fatal.
    /// Shared by `upload` and `resume_upload`.
    ///
    /// `local_modified` is the source's time as the upload started. Read from
    /// the path once the upload is over, it was the time of a save made
    /// meanwhile, lent to bytes that are not that save's: a sync then read
    /// the pair as identical and the save never went up.
    async fn preserve_remote_mtime(
        &self,
        sftp: &SftpChannel,
        remote_path: &str,
        local_modified: Option<std::time::SystemTime>,
    ) {
        let Some(modified) = local_modified else {
            tracing::warn!(
                "SFTP: No local time to preserve on {}: the source's metadata could not be read",
                remote_path
            );
            return;
        };
        if let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH) {
            match u32::try_from(duration.as_secs()) {
                Ok(epoch_secs) => {
                    let mut attrs = russh_sftp::protocol::FileAttributes::empty();
                    // SFTP's ACMODTIME attribute serializes both fields together;
                    // reuse the source mtime for atime to avoid sending a zero atime.
                    attrs.atime = Some(epoch_secs);
                    attrs.mtime = Some(epoch_secs);
                    if let Err(error) =
                        until_sftp_ends(&sftp.ended, sftp.set_metadata(remote_path, attrs)).await
                    {
                        tracing::warn!(
                            "SFTP: Failed to preserve remote mtime for {}: {}",
                            remote_path,
                            error
                        );
                    }
                }
                Err(_) => tracing::warn!(
                    "SFTP: Skipping mtime preservation for {} because source mtime is out of range",
                    remote_path
                ),
            }
        }
    }
}

/// How an interrupted upload should be resumed.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ResumeUploadPlan {
    /// Nothing usable on the remote (empty / stale offset): full upload from 0.
    FullUpload,
    /// The remote already holds at least the whole local file: nothing to send.
    AlreadyComplete,
    /// Append the local tail starting at this byte offset.
    Append(u64),
}

/// Decide how to resume an upload from the caller's requested offset, the
/// actual remote size, and the local file size. The offset is clamped to what
/// really landed (`remote_size`) so a stale caller offset can never make us
/// append past a short remote file, which would corrupt it.
pub(crate) fn plan_resume_upload(
    caller_offset: u64,
    remote_size: u64,
    local_size: u64,
) -> ResumeUploadPlan {
    let start = caller_offset.min(remote_size);
    if start == 0 {
        ResumeUploadPlan::FullUpload
    } else if start >= local_size {
        ResumeUploadPlan::AlreadyComplete
    } else {
        ResumeUploadPlan::Append(start)
    }
}

/// File-type mask and the symlink type of a POSIX mode word.
const S_IFMT: u32 = 0o170000;
const S_IFLNK: u32 = 0o120000;

/// Test `S_IFLNK` on a raw SFTP mode word.
///
/// `SSH_FXP_READDIR` replies carry each entry's own attributes with `lstat`
/// semantics, so the symlink bit is already in hand and the listing needs no
/// extra `SSH_FXP_LSTAT` per entry. This is the same assumption rclone's sftp
/// backend makes.
///
/// Returns `None` when the mode carries no file-type bits at all. Some
/// embedded firmware sends permission bits only, and an unknown type must be
/// probed rather than silently read as "not a symlink": `list` would then
/// hand a symlink-to-directory to callers as a real directory, and every
/// recursive walk would follow it (`GAP-A02`).
fn symlink_bit(mode: u32) -> Option<bool> {
    match mode & S_IFMT {
        0 => None,
        file_type => Some(file_type == S_IFLNK),
    }
}

/// Format Unix permissions as rwx string
fn format_permissions(mode: u32, is_dir: bool) -> String {
    let user = format!(
        "{}{}{}",
        if mode & 0o400 != 0 { 'r' } else { '-' },
        if mode & 0o200 != 0 { 'w' } else { '-' },
        if mode & 0o100 != 0 { 'x' } else { '-' }
    );
    let group = format!(
        "{}{}{}",
        if mode & 0o040 != 0 { 'r' } else { '-' },
        if mode & 0o020 != 0 { 'w' } else { '-' },
        if mode & 0o010 != 0 { 'x' } else { '-' }
    );
    let other = format!(
        "{}{}{}",
        if mode & 0o004 != 0 { 'r' } else { '-' },
        if mode & 0o002 != 0 { 'w' } else { '-' },
        if mode & 0o001 != 0 { 'x' } else { '-' }
    );
    format!(
        "{}{}{}{}",
        if is_dir { 'd' } else { '-' },
        user,
        group,
        other
    )
}

#[async_trait]
impl StorageProvider for SftpProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Sftp
    }

    fn display_name(&self) -> String {
        format!("{}@{}", self.config.username, self.config.host)
    }

    fn endpoint_identity(&self) -> crate::transfer_dag::EndpointIdentity {
        crate::transfer_dag::EndpointIdentity::new("sftp", &self.config.host, &self.config.username)
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        tracing::info!(
            "SFTP: Connecting to {}:{}",
            self.config.host,
            self.config.port
        );
        // The posix-rename answer and its session belong to the connection
        // that was asked: after a dropped transport and a new dial it would be
        // a dead session, and every replace would fail on it.
        self.posix_rename = PosixRenameSupport::Unasked;
        // A reconnect over a connection still held (the GUI's silent
        // reconnect after a stalled session) closes it first instead of
        // overwriting the handle.
        self.sftp = None;
        self.retire_ssh_connection().await;

        // Create SSH config with keepalive to prevent server from closing connection
        let preferred = if self.compression_enabled {
            tracing::info!("SFTP: SSH compression enabled (zlib@openssh.com)");
            Preferred {
                compression: std::borrow::Cow::Borrowed(&[
                    compression::ZLIB_LEGACY,
                    compression::ZLIB,
                    compression::NONE,
                ]),
                ..Default::default()
            }
        } else {
            Preferred::default()
        };
        let config = Config {
            inactivity_timeout: Some(std::time::Duration::from_secs(self.config.timeout_secs * 2)),
            keepalive_interval: Some(std::time::Duration::from_secs(15)), // Send keepalive every 15s
            keepalive_max: 3, // Allow 3 missed keepalives before disconnect
            preferred,
            ..Default::default()
        };

        // Connect to SSH server
        let addr = format!("{}:{}", self.config.host, self.config.port);
        let mut handle = client::connect(
            Arc::new(config),
            &addr,
            SshHandler::with_trust_and_slot(
                &self.config.host,
                self.config.port,
                self.config.trust_unknown_hosts,
                self.host_key_sha256_hex.clone(),
            ),
        )
        .await
        .map_err(|e| ProviderError::ConnectionFailed(format!("ssh: {e}")))?;

        tracing::info!("SFTP: SSH connection established, authenticating...");

        // Authenticate
        let authenticated = if self.config.private_key_path.is_some() {
            // Try key-based authentication
            self.authenticate_with_key(&mut handle).await?
        } else if let Some(password) = &self.config.password {
            // Try password authentication first, then keyboard-interactive as fallback
            use russh::client::KeyboardInteractiveAuthResponse;
            use secrecy::ExposeSecret;
            let pw = password.expose_secret().to_string();
            let result = handle
                .authenticate_password(&self.config.username, &pw)
                .await
                .map_err(|e| {
                    ProviderError::AuthenticationFailed(format!("Password auth failed: {}", e))
                })?;
            if matches!(result, AuthResult::Success) {
                true
            } else {
                // Fallback: keyboard-interactive (many servers like SourceForge require this)
                tracing::info!("SFTP: Password auth not accepted, trying keyboard-interactive...");
                let ki_result = handle
                    .authenticate_keyboard_interactive_start(&self.config.username, None::<String>)
                    .await
                    .map_err(|e| {
                        ProviderError::AuthenticationFailed(format!(
                            "Keyboard-interactive auth failed: {}",
                            e
                        ))
                    })?;
                match ki_result {
                    KeyboardInteractiveAuthResponse::Success => true,
                    KeyboardInteractiveAuthResponse::Failure { .. } => false,
                    KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => {
                        // Server asks for responses - send password for each prompt
                        let responses: Vec<String> = prompts.iter().map(|_| pw.clone()).collect();
                        let resp = handle
                            .authenticate_keyboard_interactive_respond(responses)
                            .await
                            .map_err(|e| {
                                ProviderError::AuthenticationFailed(format!(
                                    "Keyboard-interactive respond failed: {}",
                                    e
                                ))
                            })?;
                        matches!(resp, KeyboardInteractiveAuthResponse::Success)
                    }
                }
            }
        } else {
            return Err(ProviderError::AuthenticationFailed(
                "No authentication method provided (need password or private key)".to_string(),
            ));
        };

        if !authenticated {
            return Err(ProviderError::AuthenticationFailed(
                "Authentication rejected by server".to_string(),
            ));
        }

        tracing::info!("SFTP: Authenticated successfully, opening SFTP channel...");

        // Open SFTP subsystem channel
        let channel = handle.channel_open_session().await.map_err(|e| {
            ProviderError::ConnectionFailed(format!("Failed to open session channel: {}", e))
        })?;

        channel.request_subsystem(true, "sftp").await.map_err(|e| {
            ProviderError::ConnectionFailed(format!("Failed to request SFTP subsystem: {}", e))
        })?;

        // Create SFTP session from channel
        let sftp = SftpChannel::open(channel.into_stream())
            .await
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!("Failed to create SFTP session: {}", e))
            })?;

        // Get home directory (canonicalize ".")
        let home = until_sftp_ends(&sftp.ended, sftp.canonicalize("."))
            .await
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!("Failed to get home directory: {}", e))
            })?;

        self.home_dir = home;

        // Set initial directory
        if let Some(initial) = &self.config.initial_path {
            self.current_dir = self.normalize_path(initial);
        } else {
            self.current_dir = self.home_dir.clone();
        }

        self.ssh_handle = Some(Arc::new(TokioMutex::new(handle)));
        self.sftp = Some(sftp);

        // PD-SFTP-1: capture a secure connection spec now, while
        // `self.config` still holds the secrets (`provider_connect`
        // zeroizes the outer config only after this returns). The pinned
        // host-key fingerprint was populated by `SshHandler` during the
        // handshake; preserve an earlier pin if this dial reused one.
        let prior_pin = self
            .connection_spec
            .as_ref()
            .and_then(|s| s.pinned_host_key_sha256.clone());
        self.connection_spec = Some(SftpConnectionSpec {
            host: self.config.host.clone(),
            port: self.config.port,
            username: self.config.username.clone(),
            password: self.config.password.clone(),
            private_key_path: self.config.private_key_path.clone(),
            key_passphrase: self.config.key_passphrase.clone(),
            initial_path: self.config.initial_path.clone(),
            timeout_secs: self.config.timeout_secs,
            pinned_host_key_sha256: self.host_key_sha256_hex.get().cloned().or(prior_pin),
        });

        tracing::info!(
            "SFTP: Connected successfully to {} (home: {})",
            self.config.host,
            self.home_dir
        );
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        tracing::info!("SFTP: Disconnecting from {}", self.config.host);

        // Close SFTP session
        if let Some(sftp) = self.sftp.take() {
            let _ = sftp.close().await;
        }

        // The posix-rename answer belongs to the connection that was asked,
        // not to this struct: the next `connect()` may reach a different
        // server, and an inherited "Absent" would refuse a replace the new
        // server can do.
        self.posix_rename = PosixRenameSupport::Unasked;

        // Close SSH handle. Arc<Mutex<_>> means other clones (e.g. rsync-over-SSH borrowers)
        // may still hold references; the disconnect message is sent through the shared sender,
        // which is exactly what we want: the session is tore down once for everyone.
        if let Some(handle) = self.ssh_handle.take() {
            let guard = handle.lock().await;
            let _ = guard
                .disconnect(russh::Disconnect::ByApplication, "", "en")
                .await;
        }

        self.current_dir = "/".to_string();
        self.home_dir = "/".to_string();

        tracing::info!("SFTP: Disconnected");
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.sftp
            .as_ref()
            .is_some_and(|sftp| !sftp.ended.is_cancelled())
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        tracing::debug!("SFTP: Listing directory: {}", full_path);

        let entries = until_sftp_ends(&sftp.ended, sftp.read_dir(&full_path))
            .await
            .map_err(|e| {
                classify_russh_err(e, |s| {
                    ProviderError::NotFound(format!("Failed to list directory: {}", s))
                })
            })?;

        // Build the work list from the READDIR reply without any further I/O.
        // Every entry's own attributes are already in hand; a follow-up request
        // is needed only for the attr-less-server recovery and for real
        // symlinks. Collect them first, then resolve those follow-ups
        // concurrently over the one SFTP channel below instead of awaiting one
        // entry at a time (lever 2). `entry.metadata()` returns owned, Copy
        // attributes, so nothing borrows the directory reader past this loop.
        let mut pending = Vec::new();
        for entry in entries {
            let name = entry.file_name();

            // Skip . and ..
            if name == "." || name == ".." {
                continue;
            }

            // Tolerate malformed directory entries instead of letting one
            // bad name break the whole listing (FileZilla fzssh 1.2.1 class).
            // russh-sftp already decodes names with from_utf8_lossy, so a
            // non-UTF8 name survives as replacement chars; an empty name is
            // the only remaining unusable case and is skipped with a log.
            if name.is_empty() {
                tracing::warn!(
                    "SFTP: skipping directory entry with empty name in {}",
                    full_path
                );
                continue;
            }

            let entry_path = if full_path == "/" {
                format!("/{}", name)
            } else {
                format!("{}/{}", full_path.trim_end_matches('/'), name)
            };

            pending.push((name, entry_path, entry.metadata()));
        }

        // Max metadata follow-ups in flight over the single SFTP channel.
        // russh-sftp tags each request with an id from an atomic counter and
        // demultiplexes replies by id, and every SftpSession method takes
        // &self, so these pipeline on one connection with no extra sockets and
        // no trait change. 48 matches rclone's sftp backend default. After
        // lever 1 a well-behaved server issues no follow-up at all for a plain
        // file or directory, so this only bites for symlink-heavy trees and
        // for capability-poor servers, which is exactly where the serial walk
        // used to stall.
        const LIST_FOLLOWUP_CONCURRENCY: usize = 48;

        use futures_util::stream::StreamExt;
        let mut result: Vec<RemoteEntry> = futures_util::stream::iter(pending)
            .map(|(name, entry_path, readdir_attrs)| async move {
                let mut remote_entry =
                    Self::metadata_to_entry(name.clone(), entry_path.clone(), &readdir_attrs);

                // Minimal/embedded SFTP servers (some NAS firmware) omit file
                // attributes in READDIR replies. Without permission bits neither
                // our code nor russh-sftp's file_type() can tell a directory
                // from a file, so metadata_to_entry reports it as a file and the
                // directory becomes unenterable. Recover with an explicit STAT,
                // which these servers answer with full attributes (FileZilla
                // fzssh 1.2.1 / rclone sftp behaviour for capability-poor
                // servers). Bounded to the attr-less case so well-behaved
                // servers pay no extra round-trip.
                if remote_entry.permissions.is_none() {
                    if let Ok(stat) = until_sftp_ends(&sftp.ended, sftp.metadata(&entry_path)).await
                    {
                        remote_entry =
                            Self::metadata_to_entry(name.clone(), entry_path.clone(), &stat);
                    }
                }

                // Check if it's a symlink. The READDIR attributes already carry
                // the entry's own mode (lstat semantics), so a well-behaved
                // server answers this for free. Only when the server sent no
                // file-type bits do we spend an SSH_FXP_LSTAT: note that
                // `remote_entry` may by then hold the recovered STAT attributes,
                // which follow the link and so can never show S_IFLNK.
                let is_symlink = match readdir_attrs.permissions.and_then(symlink_bit) {
                    Some(flag) => flag,
                    None => until_sftp_ends(&sftp.ended, sftp.symlink_metadata(&entry_path))
                        .await
                        .ok()
                        .and_then(|link_meta| link_meta.permissions)
                        .and_then(symlink_bit)
                        .unwrap_or(false),
                };

                if is_symlink {
                    remote_entry.is_symlink = true;
                    if let Ok(target) =
                        until_sftp_ends(&sftp.ended, sftp.read_link(&entry_path)).await
                    {
                        remote_entry.link_target = Some(target);
                    }
                    // Follow the symlink to determine the real type (file vs directory)
                    // metadata() follows symlinks, unlike symlink_metadata()
                    if let Ok(target_meta) =
                        until_sftp_ends(&sftp.ended, sftp.metadata(&entry_path)).await
                    {
                        if let Some(target_perms) = target_meta.permissions {
                            remote_entry.is_dir = (target_perms & 0o40000) != 0;
                        }
                        // Update size from target if available
                        if let Some(target_size) = target_meta.size {
                            remote_entry.size = target_size;
                        }
                    }
                }

                remote_entry
            })
            .buffer_unordered(LIST_FOLLOWUP_CONCURRENCY)
            .collect()
            .await;

        // The follow-ups drop their errors on purpose (a server that refuses
        // one STAT still lists), so one cut by the end of the transport left
        // its entry half read, as a file with no mode. That is a lost
        // connection, which the caller reconnects for, not a listing.
        if sftp.ended.is_cancelled() {
            return Err(ProviderError::ConnectionLost(
                SFTP_TRANSPORT_ENDED.to_string(),
            ));
        }

        // Sort: directories first, then by name. buffer_unordered yields in
        // completion order, so the tiebreak on the exact name (not only the
        // lowercased one) keeps the output fully deterministic no matter which
        // follow-up finished first.
        result.sort_by(|a, b| match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a
                .name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.name.cmp(&b.name)),
        });

        tracing::debug!("SFTP: Listed {} entries", result.len());
        Ok(result)
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_dir.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        // Verify the directory exists. Transport-level failures (server
        // idle reaper, broken pipe) are routed to ConnectionLost so the
        // command layer can reconnect+replay instead of misclassifying
        // them as a missing path.
        let metadata = until_sftp_ends(&sftp.ended, sftp.metadata(&full_path))
            .await
            .map_err(|e| classify_russh_err(e, ProviderError::NotFound))?;

        if let Some(perms) = metadata.permissions {
            if (perms & 0o40000) == 0 {
                return Err(ProviderError::InvalidPath(format!(
                    "{} is not a directory",
                    full_path
                )));
            }
        }

        self.current_dir = full_path;
        tracing::debug!("SFTP: Changed directory to {}", self.current_dir);
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.cd("..").await
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        self.download_with_size_hint(remote_path, local_path, None, on_progress)
            .await
    }

    async fn download_with_size_hint(
        &mut self,
        remote_path: &str,
        local_path: &str,
        size_hint: Option<u64>,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let resume_in_place = std::mem::take(&mut self.resume_in_place);
        self.ensure_connected().await?;
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(remote_path);

        tracing::info!("SFTP: Downloading {} to {}", full_path, local_path);

        // Speculate on the serial path for a hint of at most one buffer:
        // STAT and OPEN can share a round trip on the multiplexed session.
        // The hint selects only this optimization; the fresh STAT still drives
        // path selection, termination, throttling, resume and progress.
        // A stale hint or an explicit fast-path setting can select a different
        // downloader below, which closes the speculative handle first.
        let small_enough_for_serial =
            matches!(size_hint, Some(hint) if hint <= self.buffer_size as u64);
        let (metadata, preopened) = if small_enough_for_serial {
            let (metadata, opened) = tokio::join!(
                until_sftp_ends(&sftp.ended, sftp.metadata(&full_path)),
                until_sftp_ends(&sftp.ended, sftp.open(&full_path)),
            );
            (metadata, Some(opened))
        } else {
            (
                until_sftp_ends(&sftp.ended, sftp.metadata(&full_path)).await,
                None,
            )
        };
        let metadata = match metadata {
            Ok(metadata) => metadata,
            Err(error) => {
                // OPEN may have succeeded independently of STAT. Finish its
                // CLOSE before returning, including on servers that deny STAT.
                if let Some(Ok(file)) = preopened {
                    close_sftp_file(file, &sftp.ended).await;
                }
                return Err(classify_russh_err(error, ProviderError::NotFound));
            }
        };
        let mut total_size = metadata.size.unwrap_or(0);
        // The OPEN above is speculation, fired next to the STAT to overlap the
        // two round trips. Its failure is not the download's failure: the fresh
        // STAT can still select the pooled, read-ahead or pipelined path, and
        // each of those opens handles of its own. Aborting here would turn a
        // transient refusal, a warm worker hitting the server's handle limit
        // being the realistic one, into a failed download that used to work.
        //
        // Degrading to "no speculative handle" hides nothing: the serial path
        // opens again below and classifies the same error the same way, so a
        // genuine permission or missing-file failure still surfaces, once.
        let mut preopened = match preopened {
            Some(Ok(file)) => Some(file),
            Some(Err(e)) => {
                tracing::debug!(
                    "SFTP: speculative OPEN of {} failed ({}); continuing without it",
                    full_path,
                    e
                );
                None
            }
            None => None,
        };

        // A fast path engaging despite the hint (a stale one or an
        // explicit fast-path setting) cannot use the pre-opened handle: it runs its
        // own reads. Close it awaited, not dropped: a queued close_nowait is
        // the handle-leak shape the awaited close was added for.
        macro_rules! close_preopened {
            () => {
                if let Some(file) = preopened.take() {
                    close_sftp_file(file, &sftp.ended).await;
                }
            };
        }

        // PD-SFTP-2: intra-file parallelism. Engaged only when the user opted
        // in (`set_multi_thread_download(streams >= 2, ...)`), the file is
        // at/above the cutoff, and a real connection spec exists so we can
        // re-dial N independent SSH connections (the SftpConnectionPool kind).
        // Without all three this is a no-op and the single-stream path below
        // is unchanged: honest non-regression, no protocol overclaim.
        let mut on_progress = on_progress;
        // Set when the parallel path refused to publish: the object is moving,
        // so the fallback must read it on one handle. The read-ahead and
        // pipelined paths below open several handles to the same path, a few
        // milliseconds apart, and a replacement inside that burst would put
        // the fallback back in the hole the refusal just avoided.
        let mut source_is_moving = false;
        // R21: the provider gate method (shared planner: explicit cutoff +
        // provider floor via the setter, 16 clamp, 1 MiB minimum window), so
        // the single-file path agrees with batch and pget.
        let planned_mt = self.planned_download_segments(total_size);
        if planned_mt >= 2 && self.connection_spec.is_some() {
            close_preopened!();
            // Half of the callback goes with the attempt and half stays, so
            // the path a refusal falls back to keeps reporting.
            let (attempt_progress, fallback_progress) = share_progress(on_progress.take());
            on_progress = fallback_progress;
            match self
                .parallel_download_if_unchanged(
                    remote_path,
                    &full_path,
                    local_path,
                    total_size,
                    attempt_progress,
                )
                .await
            {
                Ok(true) => return Ok(()),
                // Refused, not failed: the object is not the one the windows
                // were planned for, or it could not be read again to prove it
                // stayed put. One stream reads one consistent view, which is
                // exactly what the parallel path could not promise, so take
                // the path below instead of failing a download that has a
                // correct way to finish.
                Ok(false) => source_is_moving = true,
                Err(e) => return Err(e),
            }
        }

        // Sliding-window read-ahead downloader (our own, no crate fork). Takes
        // precedence over PD-PIPE-1 when provider state selects it. Same
        // guardrails: known size, no active
        // bandwidth cap (the serial loop owns precise throttling).
        if let Some(requested_window) = self.sftp_readahead.requested_window() {
            if !source_is_moving
                && total_size > 0
                && self.download_limit_bps == 0
                && crate::transfer_dag::governor::global()
                    .bandwidth()
                    .is_unlimited()
                && sftp_readahead_local_path_is_eligible(local_path)
            {
                if let Some(window) = effective_sftp_readahead_window(
                    requested_window,
                    self.buffer_size,
                    total_size,
                    1,
                ) {
                    close_preopened!();
                    let sftp = self.get_sftp()?;
                    let cancel = {
                        let mut slot = self
                            .transfer_cancel
                            .lock()
                            .expect("transfer cancel mutex poisoned");
                        *slot = CancellationToken::new();
                        slot.clone()
                    };
                    let (attempt_progress, fallback_progress) = share_progress(on_progress.take());
                    on_progress = fallback_progress;
                    match sftp_readahead_download(
                        sftp,
                        &full_path,
                        total_size,
                        local_path,
                        self.buffer_size,
                        window,
                        attempt_progress,
                        &cancel,
                        Arc::clone(&self.fail_readahead_write),
                    )
                    .await
                    {
                        Ok(()) => return Ok(()),
                        Err(e @ ProviderError::ParallelRefused(_)) => {
                            // Read on several handles and the object moved
                            // between the opens, or it is not the object the
                            // transfer was planned for: the serial path below
                            // reads on one handle.
                            tracing::warn!("{}; downloading on a single handle", e);
                            source_is_moving = true;
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
        }

        // PD-PIPE-1: opt-in pipelined single-stream read on the *one
        // existing* SFTP session (no new connection, no pool). Off by
        // default = the serial loop below, byte-identical. Skipped when the
        // size is unknown/zero or a bandwidth limit is active (the serial
        // loop owns the exact throttling, including the process-global cap);
        // the SHA-256 live gate guards it.
        if let Some(window) = sftp_read_pipeline_window() {
            if !source_is_moving
                && total_size > 0
                && self.download_limit_bps == 0
                && crate::transfer_dag::governor::global()
                    .bandwidth()
                    .is_unlimited()
            {
                close_preopened!();
                let sftp = self.get_sftp()?;
                let mut atomic = super::atomic_write::AtomicFile::new(local_path)
                    .await
                    .map_err(|e| {
                        ProviderError::TransferFailed(format!("Failed to create local file: {}", e))
                    })?;
                let (attempt_progress, fallback_progress) = share_progress(on_progress.take());
                on_progress = fallback_progress;
                match sftp_pipelined_download(
                    sftp,
                    &full_path,
                    total_size,
                    &mut atomic,
                    self.buffer_size,
                    window,
                    attempt_progress,
                )
                .await
                {
                    Ok(()) => {
                        atomic.commit().await.map_err(|e| {
                            ProviderError::TransferFailed(format!(
                                "Failed to finalize download: {}",
                                e
                            ))
                        })?;
                        tracing::info!(
                            "SFTP: Download complete (pipelined, window={}): {} bytes",
                            window,
                            total_size
                        );
                        return Ok(());
                    }
                    // Read on several handles and the object moved under
                    // them: the staged file goes with `atomic`, and the
                    // serial path below reads on one handle.
                    Err(e @ ProviderError::ParallelRefused(_)) => {
                        tracing::warn!("{}; downloading on a single handle", e);
                        source_is_moving = true;
                    }
                    Err(e) => return Err(e),
                }
            }
        }

        if source_is_moving {
            // Everything below is bounded by `total_size`, which was read
            // before the object moved: keeping it would publish a file cut to
            // a length that is no longer the object's.
            if let Ok(sftp) = self.get_sftp() {
                if let Ok(fresh) = Self::range_source_reading(sftp, &full_path).await {
                    if fresh.size() != total_size {
                        tracing::warn!(
                            "SFTP: {} is {} bytes now and was {}, the single-handle download uses the new size",
                            full_path,
                            fresh.size(),
                            total_size
                        );
                        total_size = fresh.size();
                    }
                }
            }
        }

        // Open remote file
        // The handle may already be open from the STAT/OPEN overlap above.
        let mut remote_file = match preopened.take() {
            Some(file) => file,
            None => until_sftp_ends(&sftp.ended, sftp.open(&full_path))
                .await
                .map_err(|e| {
                    classify_russh_err(e, |s| {
                        ProviderError::TransferFailed(format!("Failed to open remote file: {}", s))
                    })
                })?,
        };

        // Everything that can fail once the handle is open runs inside this
        // block, so the handle is closed, awaited, on every exit and not only
        // on the one that reaches the end. Dropping it only queues a close
        // (`close_nowait`), and a worker that now lives across thousands of
        // files (warm reuse) outran the server's per-session handle limit that
        // way ("Limit exceeded: handle limit reached" on the 5000-file review
        // cell). The awaited close used to sit on the success path alone, so a
        // failed local create, seek, read or write, and the early return for a
        // partial that already holds the whole file, all left it to Drop.
        let ended = sftp.ended.clone();
        let streamed: Result<(super::atomic_write::ResumableFile, u64, bool), ProviderError> = {
            // Moved in, not borrowed: a borrowed `on_progress` would make this
            // future require `Sync` from a callback that is only `Send`. The
            // remote handle is lent for the duration and closed right after.
            let remote_file = &mut remote_file;
            let ended = &ended;
            let buffer_size = self.buffer_size;
            let download_limit_bps = self.download_limit_bps;
            async move {
                // Resumable local file: writes to `.aerotmp`, KEEPS the partial on
                // cancel/error (drop) so a later re-download resumes, renames on commit.
                let opened = if resume_in_place {
                    super::atomic_write::ResumableFile::open_resume(local_path).await
                } else {
                    super::atomic_write::ResumableFile::open(local_path).await
                };
                let mut resumable = opened.map_err(|e| {
                    ProviderError::TransferFailed(format!("Failed to create local file: {}", e))
                })?;

                let mut resume_offset = resumable.offset();
                // A partial larger than the current remote file is stale (the remote
                // changed): discard it and start fresh rather than appending to bad data.
                if total_size > 0 && resume_offset > total_size {
                    resumable.discard().await.ok();
                    resumable = super::atomic_write::ResumableFile::open_fresh(local_path)
                        .await
                        .map_err(|e| {
                            ProviderError::TransferFailed(format!(
                                "Failed to create local file: {}",
                                e
                            ))
                        })?;
                    resume_offset = 0;
                }

                // The partial already holds the whole file: nothing to read.
                if total_size > 0 && resume_offset == total_size {
                    if let Some(ref progress) = on_progress {
                        progress(total_size, total_size);
                    }
                    return Ok((resumable, total_size, true));
                }

                // Resume: seek the remote read to the partial's end so we append the
                // correct bytes instead of re-fetching from zero.
                if resume_offset > 0 {
                    use tokio::io::AsyncSeekExt;
                    remote_file
                        .seek(std::io::SeekFrom::Start(resume_offset))
                        .await
                        .map_err(|e| {
                            ProviderError::TransferFailed(format!(
                                "Failed to seek for resume: {}",
                                e
                            ))
                        })?;
                    tracing::info!("SFTP: Resuming download from offset {}", resume_offset);
                }

                // Read and write in chunks with optional rate limiting
                let mut buffer = vec![0u8; buffer_size];
                let mut transferred: u64 = resume_offset;
                if let Some(ref progress) = on_progress {
                    progress(transferred, total_size);
                }
                let start = std::time::Instant::now();
                // DAG-P2-01: shared process-global bandwidth bucket (no-op when unset).
                let global_bw = crate::transfer_dag::governor::global();

                loop {
                    if total_size > 0 && transferred >= total_size {
                        break;
                    }
                    // Reserve global tokens before the remote read so concurrent jobs
                    // cannot put bytes on the wire before the shared cap admits them.
                    // A short final read can over-reserve only the unused tail of one
                    // buffer, which is conservative and never lets the cap burst.
                    let remaining = total_size.saturating_sub(transferred);
                    let allowance = if remaining == 0 {
                        buffer.len() as u64
                    } else {
                        remaining.min(buffer.len() as u64)
                    };
                    global_bw
                        .charge(
                            crate::transfer_dag::governor::TransferDirection::Download,
                            allowance,
                        )
                        .await;
                    let bytes_read = until_sftp_ends(ended, remote_file.read(&mut buffer))
                        .await
                        .map_err(|e| {
                            classify_russh_err(e, |s| {
                                ProviderError::TransferFailed(format!("Read error: {}", s))
                            })
                        })?;

                    if bytes_read == 0 {
                        break;
                    }

                    resumable
                        .write_all(&buffer[..bytes_read])
                        .await
                        .map_err(|e| {
                            ProviderError::TransferFailed(format!("Write error: {}", e))
                        })?;

                    transferred += bytes_read as u64;

                    if let Some(ref progress) = on_progress {
                        progress(transferred, total_size);
                    }

                    // Apply bandwidth throttling on bytes moved THIS session, so a
                    // resume does not over-sleep for already-downloaded data.
                    if download_limit_bps > 0 {
                        let session_bytes = transferred - resume_offset;
                        let expected = std::time::Duration::from_secs_f64(
                            session_bytes as f64 / download_limit_bps as f64,
                        );
                        let elapsed = start.elapsed();
                        if expected > elapsed {
                            tokio::time::sleep(expected - elapsed).await;
                        }
                    }
                }
                Ok((resumable, transferred, false))
            }
        }
        .await;

        close_sftp_file(remote_file, &ended).await;
        let (resumable, transferred, from_partial) = streamed?;
        resumable.commit().await.map_err(|e| {
            ProviderError::TransferFailed(format!("Failed to finalize download: {}", e))
        })?;

        if from_partial {
            tracing::info!(
                "SFTP: Download already complete from partial: {} bytes",
                transferred
            );
        } else {
            tracing::info!("SFTP: Download complete: {} bytes", transferred);
        }
        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(remote_path);
        let limit = super::MAX_DOWNLOAD_TO_BYTES;

        tracing::debug!("SFTP: Reading file to bytes: {}", full_path);

        // H2: Check file size before reading to prevent OOM
        if let Ok(metadata) = until_sftp_ends(&sftp.ended, sftp.metadata(&full_path)).await {
            if metadata.size.unwrap_or(0) > limit {
                return Err(ProviderError::TransferFailed(format!(
                    "File too large for in-memory download ({:.1} MB). Use streaming download for files over {:.0} MB.",
                    metadata.size.unwrap_or(0) as f64 / 1_048_576.0,
                    limit as f64 / 1_048_576.0,
                )));
            }
        }

        let data = until_sftp_ends(&sftp.ended, sftp.read(&full_path))
            .await
            .map_err(|e| {
                classify_russh_err(e, |s| {
                    ProviderError::TransferFailed(format!("Failed to read file: {}", s))
                })
            })?;

        if data.len() as u64 > limit {
            return Err(ProviderError::TransferFailed(format!(
                "Download exceeded {:.0} MB size limit. Use streaming download for large files.",
                limit as f64 / 1_048_576.0,
            )));
        }

        Ok(data)
    }

    async fn download_to_bytes_capped(
        &mut self,
        remote_path: &str,
        max_bytes: u64,
    ) -> Result<Vec<u8>, ProviderError> {
        use tokio::io::AsyncReadExt;

        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(remote_path);

        tracing::debug!(
            "SFTP: Reading file to bytes (cap {} B): {}",
            max_bytes,
            full_path
        );

        // Honest servers get rejected up front. A server that under-reports
        // `size` here is NOT trusted: the streaming loop below refuses before
        // the buffer crosses `max_bytes`, so the lying body is never fully read.
        if let Ok(metadata) = until_sftp_ends(&sftp.ended, sftp.metadata(&full_path)).await {
            if metadata.size.unwrap_or(0) > max_bytes {
                return Err(ProviderError::TransferFailed(format!(
                    "File too large for in-memory download ({:.1} MB). Cap is {:.0} MB.",
                    metadata.size.unwrap_or(0) as f64 / 1_048_576.0,
                    max_bytes as f64 / 1_048_576.0,
                )));
            }
        }

        let mut remote_file = until_sftp_ends(&sftp.ended, sftp.open(&full_path))
            .await
            .map_err(|e| {
                classify_russh_err(e, |s| {
                    ProviderError::TransferFailed(format!("Failed to open remote file: {}", s))
                })
            })?;

        // Bound the accumulator to `max_bytes`: refuse a chunk that would push
        // it over the cap instead of `read`-ing the whole file into memory.
        let buffer_size = self.buffer_size;
        let ended = sftp.ended.clone();
        let streamed: Result<Vec<u8>, ProviderError> = {
            let remote_file = &mut remote_file;
            let ended = &ended;
            async move {
                let mut data: Vec<u8> = Vec::new();
                let mut buffer = vec![0u8; buffer_size.max(4096)];
                loop {
                    let bytes_read = until_sftp_ends(ended, remote_file.read(&mut buffer))
                        .await
                        .map_err(|e| {
                        classify_russh_err(e, |s| {
                            ProviderError::TransferFailed(format!("Read error: {}", s))
                        })
                    })?;
                    if bytes_read == 0 {
                        break;
                    }
                    if data.len() as u64 + bytes_read as u64 > max_bytes {
                        return Err(ProviderError::TransferFailed(format!(
                            "Download exceeded the {:.0} MB cap (server under-reported size). Use streaming download for larger files.",
                            max_bytes as f64 / 1_048_576.0,
                        )));
                    }
                    data.extend_from_slice(&buffer[..bytes_read]);
                }
                Ok(data)
            }
        }
        .await;
        close_sftp_file(remote_file, &ended).await;
        streamed
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        self.ensure_connected().await?;
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(remote_path);

        tracing::info!("SFTP: Uploading {} to {}", local_path, full_path);

        // Open local file
        let mut local_file = tokio::fs::File::open(local_path).await.map_err(|e| {
            ProviderError::TransferFailed(format!("Failed to open local file: {}", e))
        })?;

        // Its size for progress reporting, and its time as the upload starts,
        // which the remote is stamped with at the end, read from the open file.
        let local_meta = local_file.metadata().await.ok();
        let total_size = local_meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let local_modified = local_meta.and_then(|m| m.modified().ok());

        tracing::info!("SFTP: Upload local file size: {} bytes", total_size);

        // Create remote file via russh_sftp (uses existing SSH session, no second connection)
        let mut remote_file = until_sftp_ends(&sftp.ended, sftp.create(&full_path))
            .await
            .map_err(|e| classify_russh_write_err(e, "Failed to create remote file"))?;

        let buffer_size = self.buffer_size;
        let upload_limit_bps = self.upload_limit_bps;
        let ended = sftp.ended.clone();
        let streamed: Result<u64, ProviderError> = {
            let remote_file = &mut remote_file;
            let ended = &ended;
            async move {
                let mut buffer = SftpWriteBuffer::new(buffer_size);
                let mut transferred: u64 = 0;
                let start = std::time::Instant::now();
                let global_bw = crate::transfer_dag::governor::global();

                loop {
                    let bytes_read = buffer.refill(&mut local_file).await.map_err(|e| {
                        ProviderError::TransferFailed(format!("Local read error: {}", e))
                    })?;

                    if buffer.is_done() {
                        break;
                    }

                    if bytes_read > 0 {
                        global_bw
                            .charge(
                                crate::transfer_dag::governor::TransferDirection::Upload,
                                bytes_read as u64,
                            )
                            .await;
                    }
                    let written = buffer
                        .write_next(remote_file, ended)
                        .await
                        .map_err(|e| classify_russh_write_err(e, "Remote write error"))?;

                    transferred += written as u64;

                    if let Some(ref progress) = on_progress {
                        progress(transferred, total_size);
                    }

                    if upload_limit_bps > 0 {
                        let expected = std::time::Duration::from_secs_f64(
                            transferred as f64 / upload_limit_bps as f64,
                        );
                        let elapsed = start.elapsed();
                        if expected > elapsed {
                            tokio::time::sleep(expected - elapsed).await;
                        }
                    }
                }
                Ok(transferred)
            }
        }
        .await;
        // shutdown() is the awaited close for a write handle (russh-sftp
        // File::close is equivalent). Run it on every exit, then propagate
        // the copy error if any, then a flush failure on the success path.
        let shutdown_res = shutdown_sftp_file(&mut remote_file, &ended).await;
        let transferred = streamed?;
        shutdown_res.map_err(|e| classify_russh_write_err(e, "Failed to flush remote file"))?;

        self.verify_remote_upload_size(sftp, &full_path, total_size)
            .await?;

        // Keep remote mtime aligned with the local source so repeated sync
        // scans don't re-upload unchanged files just because the server stamped
        // the file with upload time.
        self.preserve_remote_mtime(sftp, &full_path, local_modified)
            .await;

        tracing::info!(
            "SFTP: Upload complete via russh_sftp: {} bytes",
            transferred
        );
        Ok(())
    }

    /// SFTP resumes an interrupted upload by appending from a byte offset, so
    /// the answer here is yes. It was left at the trait default of `false`
    /// while [`Self::resume_upload`] below was implemented and
    /// [`Self::supports_resume_upload_append`] answered true, which made the
    /// capability real and unreachable: the GUI resumed an interrupted SFTP
    /// upload and the CLI's `--partial` did not, because it consults THIS
    /// method and this one alone.
    ///
    /// Deliberately NOT unified with the two neighbours. `supports_resume_upload_append`
    /// is kept separate from the DAG hints on purpose, and that intent is
    /// documented on it; the three answer related questions to different
    /// callers, and only this one was wrong.
    fn supports_resume(&self) -> bool {
        true
    }

    /// Resume an interrupted download.
    ///
    /// `download` already does the whole job: it opens the `.aerotmp` through
    /// `ResumableFile`, reads the offset OFF THAT FILE, discards a partial
    /// larger than the current remote (a changed remote must not be appended
    /// to), finalizes without reading when the partial is already complete,
    /// and seeks the remote read to the partial's end. So this method
    /// delegates rather than reimplementing, and the `offset` argument is
    /// deliberately not used: the caller measured it by stat'ing the same
    /// `.aerotmp` that `ResumableFile` opens, so re-deriving it there keeps
    /// one source of truth instead of two that can disagree.
    ///
    /// It exists because [`Self::supports_resume`] is a SHARED gate. The CLI
    /// reads it for `--partial` on upload, and `provider_transfer_executor`
    /// reads it on a download RETRY, where a non-zero partial routes to this
    /// method. Answering true without this override would have sent that retry
    /// into the trait default, which returns `NotSupported`, so a retry that
    /// used to restart the download would have failed instead. Implementing
    /// the capability on one side and advertising it on both is the defect
    /// this pairing exists to prevent.
    async fn resume_download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        _offset: u64,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        self.resume_in_place = true;
        self.download(remote_path, local_path, on_progress).await
    }

    /// SFTP can append the tail of an interrupted upload from a byte offset
    /// (the GUI "Resume" action), so the remote partial is not re-sent.
    fn supports_resume_upload_append(&self) -> bool {
        true
    }

    /// Resume an interrupted upload: append the local file's tail onto the
    /// remote partial instead of re-sending from zero. The caller offset is
    /// clamped to the real remote size (re-stat) so a stale offset can never
    /// append past a short remote file and corrupt it. Falls back to a full
    /// `upload` when there is nothing usable to resume from.
    async fn resume_upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        offset: u64,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        use russh_sftp::protocol::OpenFlags;
        use tokio::io::AsyncSeekExt;

        self.ensure_connected().await?;

        let total_size = tokio::fs::metadata(local_path).await.map_err(|e| {
            ProviderError::TransferFailed(format!("Failed to stat local file: {}", e))
        })?;
        // The source's time as the resume starts, stamped on the remote at the end.
        let local_modified = total_size.modified().ok();
        let total_size = total_size.len();

        // Re-stat the remote so the resume offset reflects what actually landed.
        // Only "no such file" means there is nothing to resume from. Any other
        // failure (a connection lost in that very window) is reported: read as
        // 0 it turned the resume into a full upload that truncated the remote.
        let remote_size = {
            let sftp = self.get_sftp()?;
            let full_path = self.normalize_path(remote_path);
            match until_sftp_ends(&sftp.ended, sftp.metadata(&full_path)).await {
                Ok(metadata) => metadata.size.unwrap_or(0),
                Err(russh_sftp::client::error::Error::Status(status))
                    if status.status_code == russh_sftp::protocol::StatusCode::NoSuchFile =>
                {
                    0
                }
                Err(e) => {
                    return Err(classify_russh_write_err(
                        e,
                        "Failed to stat remote for resume",
                    ))
                }
            }
        };

        match plan_resume_upload(offset, remote_size, total_size) {
            ResumeUploadPlan::FullUpload => {
                // No usable partial on the remote: do a normal full upload.
                return self.upload(local_path, remote_path, on_progress).await;
            }
            ResumeUploadPlan::AlreadyComplete => {
                if let Some(ref progress) = on_progress {
                    progress(total_size, total_size);
                }
                tracing::info!(
                    "SFTP: Resume upload no-op, remote already has {} bytes",
                    remote_size
                );
                return Ok(());
            }
            ResumeUploadPlan::Append(start_offset) => {
                let sftp = self.get_sftp()?;
                let full_path = self.normalize_path(remote_path);
                tracing::info!(
                    "SFTP: Resuming upload of {} from offset {} (local {} bytes)",
                    full_path,
                    start_offset,
                    total_size
                );

                // Open WRITE|CREATE (no TRUNCATE) and seek to the partial's end.
                // We seek explicitly instead of using APPEND: some servers ignore
                // the seek when APPEND is set and always write at EOF.
                let mut remote_file = until_sftp_ends(
                    &sftp.ended,
                    sftp.open_with_flags(&full_path, OpenFlags::WRITE | OpenFlags::CREATE),
                )
                .await
                .map_err(|e| classify_russh_write_err(e, "Failed to open remote for resume"))?;
                let buffer_size = self.buffer_size;
                let upload_limit_bps = self.upload_limit_bps;
                let local_path_owned = local_path.to_string();
                let ended = sftp.ended.clone();
                let streamed: Result<u64, ProviderError> = {
                    let remote_file = &mut remote_file;
                    let ended = &ended;
                    async move {
                        remote_file
                            .seek(std::io::SeekFrom::Start(start_offset))
                            .await
                            .map_err(|e| {
                                ProviderError::TransferFailed(format!(
                                    "Failed to seek remote for resume: {}",
                                    e
                                ))
                            })?;

                        let mut local_file = tokio::fs::File::open(&local_path_owned)
                            .await
                            .map_err(|e| {
                                ProviderError::TransferFailed(format!(
                                    "Failed to open local file: {}",
                                    e
                                ))
                            })?;
                        local_file
                            .seek(std::io::SeekFrom::Start(start_offset))
                            .await
                            .map_err(|e| {
                                ProviderError::TransferFailed(format!(
                                    "Failed to seek local for resume: {}",
                                    e
                                ))
                            })?;

                        let mut buffer = SftpWriteBuffer::new(buffer_size);
                        let mut transferred: u64 = start_offset;
                        if let Some(ref progress) = on_progress {
                            progress(transferred, total_size);
                        }
                        let start = std::time::Instant::now();
                        let global_bw = crate::transfer_dag::governor::global();

                        loop {
                            let bytes_read = buffer.refill(&mut local_file).await.map_err(|e| {
                                ProviderError::TransferFailed(format!("Local read error: {}", e))
                            })?;
                            if buffer.is_done() {
                                break;
                            }
                            if bytes_read > 0 {
                                global_bw
                                    .charge(
                                        crate::transfer_dag::governor::TransferDirection::Upload,
                                        bytes_read as u64,
                                    )
                                    .await;
                            }
                            let written = buffer
                                .write_next(remote_file, ended)
                                .await
                                .map_err(|e| classify_russh_write_err(e, "Remote write error"))?;
                            transferred += written as u64;
                            if let Some(ref progress) = on_progress {
                                progress(transferred, total_size);
                            }
                            if upload_limit_bps > 0 {
                                let session_bytes = transferred - start_offset;
                                let expected = std::time::Duration::from_secs_f64(
                                    session_bytes as f64 / upload_limit_bps as f64,
                                );
                                let elapsed = start.elapsed();
                                if expected > elapsed {
                                    tokio::time::sleep(expected - elapsed).await;
                                }
                            }
                        }
                        Ok(transferred)
                    }
                }
                .await;
                let shutdown_res = shutdown_sftp_file(&mut remote_file, &ended).await;
                let transferred = streamed?;
                shutdown_res
                    .map_err(|e| classify_russh_write_err(e, "Failed to flush remote file"))?;

                self.verify_remote_upload_size(sftp, &full_path, total_size)
                    .await?;

                self.preserve_remote_mtime(sftp, &full_path, local_modified)
                    .await;

                tracing::info!("SFTP: Resume upload complete: {} bytes total", transferred);
                Ok(())
            }
        }
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        tracing::info!("SFTP: Creating directory: {}", full_path);

        until_sftp_ends(&sftp.ended, sftp.create_dir(&full_path))
            .await
            .map_err(|e| {
                classify_russh_err(e, |s| {
                    ProviderError::ServerError(format!("Failed to create directory: {}", s))
                })
            })?;

        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        tracing::info!("SFTP: Deleting file: {}", full_path);

        until_sftp_ends(&sftp.ended, sftp.remove_file(&full_path))
            .await
            .map_err(|e| {
                classify_russh_err(e, |s| {
                    ProviderError::ServerError(format!("Failed to delete file: {}", s))
                })
            })?;

        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        tracing::info!("SFTP: Removing directory: {}", full_path);

        // `SSH_FXP_RMDIR` refuses a directory that is not empty on the server.
        // A reply that says so (status SSH_FX_DIR_NOT_EMPTY on v6 servers) is
        // `DirectoryNotEmpty`. OpenSSH answers a bare "Failure" that says no
        // more, so the directory is looked into: one that still holds entries
        // is `DirectoryNotEmpty` too, and only a refusal of an empty or
        // unreadable directory stays a server error (permissions, a lock).
        let refused = until_sftp_ends(&sftp.ended, sftp.remove_dir(&full_path))
            .await
            .map_err(|e| {
                classify_russh_err(e, |s| {
                    if super::ftp::reply_names_not_empty(&s) {
                        ProviderError::DirectoryNotEmpty(format!(
                            "Failed to remove directory: {}",
                            s
                        ))
                    } else {
                        ProviderError::ServerError(format!("Failed to remove directory: {}", s))
                    }
                })
            });
        match refused {
            Ok(()) => Ok(()),
            Err(ProviderError::ServerError(text)) => match self.list(&full_path).await {
                Ok(entries) if !entries.is_empty() => Err(ProviderError::DirectoryNotEmpty(
                    format!("{text} ({full_path} holds {} entries)", entries.len()),
                )),
                _ => Err(ProviderError::ServerError(text)),
            },
            Err(other) => Err(other),
        }
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        let full_path = self.normalize_path(path);

        tracing::info!("SFTP: Recursively removing directory: {}", full_path);

        // List all entries
        let entries = self.list(&full_path).await?;

        // Delete all entries recursively (GAP-A02: skip symlinks to prevent following into target dirs)
        for entry in entries {
            if entry.is_symlink {
                self.delete(&entry.path).await?;
            } else if entry.is_walkable_dir() {
                // Use Box::pin to avoid infinite recursion type issues
                Box::pin(self.rmdir_recursive(&entry.path)).await?;
            } else {
                self.delete(&entry.path).await?;
            }
        }

        // Now remove the empty directory
        self.rmdir(&full_path).await
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let sftp = self.get_sftp()?;
        let from_path = self.normalize_path(from);
        let to_path = self.normalize_path(to);

        if from_path == to_path {
            // OpenSSH answers a rename onto itself with an error; everywhere
            // else it is a no-op.
            return Ok(());
        }

        tracing::info!("SFTP: Renaming {} to {}", from_path, to_path);

        let refusal = match until_sftp_ends(&sftp.ended, sftp.rename(&from_path, &to_path)).await {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };
        // SFTP v3 refuses a taken destination with the bare SSH_FX_FAILURE,
        // which says nothing more. The destination being there is what makes
        // it AlreadyExists (the CLI's exit 9); anything else stays the
        // server's refusal.
        let failure = matches!(
            &refusal,
            russh_sftp::client::error::Error::Status(status)
                if status.status_code == russh_sftp::protocol::StatusCode::Failure
        );
        let error = classify_russh_err(refusal, |s| {
            ProviderError::ServerError(format!("Failed to rename: {}", s))
        });
        // A rename that only changes the letter case needs its own look: a
        // case-insensitive server finds the source itself at `to`. There the
        // parent listing of `to` decides, since it names each entry as
        // stored: an entry spelled exactly like `to` is another item. When
        // only the name changes case in one folder (byte-identical parents),
        // one entry is the source whichever spelling it is stored under, so
        // the name is taken only when entries spelled like `to` and like
        // `from` are both there (a case-sensitive server holding both). A
        // listing that cannot be read leaves the server's refusal as it came.
        let taken = if !failure {
            false
        } else if from_path.to_lowercase() == to_path.to_lowercase() {
            let split = |path: &str| -> (String, String) {
                match path.rsplit_once('/') {
                    Some(("", name)) => ("/".to_string(), name.to_string()),
                    Some((parent, name)) => (parent.to_string(), name.to_string()),
                    None => (".".to_string(), path.to_string()),
                }
            };
            let (to_parent, to_name) = split(&to_path);
            let (from_parent, from_name) = split(&from_path);
            let one_folder_two_spellings = from_parent == to_parent && from_name != to_name;
            match until_sftp_ends(&sftp.ended, sftp.read_dir(&to_parent)).await {
                Ok(entries) => {
                    let names: Vec<String> =
                        entries.into_iter().map(|entry| entry.file_name()).collect();
                    names.contains(&to_name)
                        && (!one_folder_two_spellings || names.contains(&from_name))
                }
                Err(_) => false,
            }
        } else {
            map_sftp_try_exists(until_sftp_ends(&sftp.ended, sftp.try_exists(&to_path)).await)
                .unwrap_or(false)
        };
        if taken {
            return Err(ProviderError::AlreadyExists(to_path));
        }
        Err(error)
    }

    async fn supports_atomic_replace(&mut self) -> Result<bool, ProviderError> {
        Ok(self.posix_rename_session().await?.is_some())
    }

    async fn replace(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        use russh_sftp::protocol::{Packet, StatusCode};

        let from_path = self.normalize_path(from);
        let to_path = self.normalize_path(to);

        tracing::info!("SFTP: Replacing {} with {}", to_path, from_path);

        // Encoded before the session is borrowed, so the borrow lives only as
        // long as the request itself.
        let payload = russh_sftp::ser::to_bytes(&PosixRenamePayload {
            oldpath: from_path.clone(),
            newpath: to_path.clone(),
        })
        .map(|bytes| bytes.to_vec())
        .map_err(|e| {
            ProviderError::ServerError(format!("Failed to encode {POSIX_RENAME_EXTENSION}: {e}"))
        })?;

        // The rename goes over a second channel of the same SSH connection,
        // so the end of the main session's transport is its end too.
        let ended = self.get_sftp()?.ended.clone();
        let Some(session) = self.posix_rename_session().await? else {
            return Err(ProviderError::NotSupported(format!(
                "cannot replace `{to_path}` atomically: this SFTP server does not announce \
                 `{POSIX_RENAME_EXTENSION}`, and plain SFTP rename is specified to refuse a \
                 destination that already exists. `{to_path}` is unchanged. To overwrite it \
                 anyway, upload over it with `put`, which truncates and rewrites in place: \
                 that is not atomic either, but it is your choice and its bad moment is a \
                 partial file rather than no file."
            )));
        };

        match until_sftp_ends(&ended, session.extended(POSIX_RENAME_EXTENSION, payload)).await {
            Ok(Packet::Status(status)) if status.status_code == StatusCode::Ok => Ok(()),
            Ok(Packet::Status(status)) => Err(ProviderError::ServerError(format!(
                "Failed to replace: {} ({:?})",
                status.error_message, status.status_code
            ))),
            Ok(_) => Err(ProviderError::ServerError(format!(
                "Failed to replace: the server answered {POSIX_RENAME_EXTENSION} with a packet \
                 that is not a status"
            ))),
            Err(e) => Err(classify_russh_err(e, |s| {
                ProviderError::ServerError(format!("Failed to replace: {s}"))
            })),
        }
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        let metadata = until_sftp_ends(&sftp.ended, sftp.metadata(&full_path))
            .await
            .map_err(|e| classify_russh_err(e, ProviderError::NotFound))?;

        let name = Path::new(&full_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| full_path.clone());

        let mut entry = Self::metadata_to_entry(name, full_path.clone(), &metadata);

        // Check for symlink. The check is best effort, but not over a
        // transport that ended while it was asked: the entry would say "not a
        // link" for a question that got no answer.
        let link_meta = until_sftp_ends(&sftp.ended, sftp.symlink_metadata(&full_path)).await;
        if let Ok(link_meta) = link_meta {
            if let Some(perms) = link_meta.permissions {
                if (perms & 0o170000) == 0o120000 {
                    entry.is_symlink = true;
                    if let Ok(target) =
                        until_sftp_ends(&sftp.ended, sftp.read_link(&full_path)).await
                    {
                        entry.link_target = Some(target);
                    }
                }
            }
        }
        if sftp.ended.is_cancelled() {
            return Err(ProviderError::ConnectionLost(
                SFTP_TRANSPORT_ENDED.to_string(),
            ));
        }

        Ok(entry)
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        let metadata = until_sftp_ends(&sftp.ended, sftp.metadata(&full_path))
            .await
            .map_err(|e| classify_russh_err(e, ProviderError::NotFound))?;

        Ok(metadata.size.unwrap_or(0))
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);
        map_sftp_try_exists(until_sftp_ends(&sftp.ended, sftp.try_exists(&full_path)).await)
    }

    fn supports_checksum(&self) -> bool {
        // We can attempt a server-side hash whenever the SSH session is up.
        // `checksum()` degrades to an empty map (consumers then omit) if the
        // server has no `sha256sum`: honest, like rclone.
        self.ssh_handle.is_some()
    }

    /// Gated on the same live session as `supports_checksum`: without an SSH
    /// channel there is nobody to run `sha256sum`, so the surface should offer
    /// nothing rather than a button that cannot fire.
    fn checksum_capability(&self, _path: &str) -> ChecksumCapability {
        if self.ssh_handle.is_some() {
            checksum_matrix::capability(self.provider_type())
        } else {
            ChecksumCapability::default()
        }
    }

    /// Server-side SHA-256 computed by the remote host via an SSH exec
    /// channel (`sha256sum`). The file is read and hashed entirely on the
    /// server: no file content crosses the wire to us, unlike a download.
    ///
    /// Returns an empty map (not an error) when the server lacks
    /// `sha256sum`, the command exits non-zero, or the output is
    /// unparseable: callers then omit the hash, matching rclone's
    /// behaviour of silently skipping hashes a backend cannot provide.
    async fn checksum(
        &mut self,
        path: &str,
    ) -> Result<std::collections::HashMap<String, String>, ProviderError> {
        let handle = self.ssh_handle.clone().ok_or(ProviderError::NotConnected)?;
        let full_path = self.normalize_path(path);
        // `--` ends option parsing; the path is fully single-quoted so no
        // shell metacharacter (`$()`, backtick, `;`, space, newline) in a
        // listing-derived name can break out. See `shell_single_quote`.
        let cmd = format!("sha256sum -- {}", shell_single_quote(&full_path));

        let (stdout, _stderr, _exit) = match ssh_exec_collect(handle, &cmd, 4096).await {
            Ok(v) => v,
            // A transport/channel failure is reported as "no server hash"
            // so consumers gracefully omit rather than failing the listing.
            Err(_) => return Ok(std::collections::HashMap::new()),
        };

        // `sha256sum` prints the `<64-hex>  name` line to stdout ONLY on
        // success; on any error it writes to stderr and emits no digest.
        // A well-formed digest is therefore itself proof of success, so we
        // do not gate on the exec exit status: some SSH servers deliver
        // `exit-status` after `eof`/`close`, and `ssh_exec_collect` then
        // reports the EXIT_ABNORMAL sentinel even though stdout is complete
        // and correct (observed on the OpenSSH lab box with an SFTP
        // subsystem channel concurrently open on the same handle).
        let mut out = std::collections::HashMap::new();
        if let Some(token) = String::from_utf8_lossy(&stdout).split_whitespace().next() {
            let digest = token.to_ascii_lowercase();
            if digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()) {
                out.insert("sha256".to_string(), digest);
            }
        }
        Ok(out)
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        // SFTP over SSH is a persistent connection
        // Just check if we're still connected
        if self.sftp.is_none() {
            return Err(ProviderError::NotConnected);
        }

        // Optionally do a simple operation to verify connection.
        // canonicalize(".") is lightweight. A failure here means the
        // server idle reaper or NAT closed the channel: classify as
        // ConnectionLost so the caller can reconnect+replay rather than
        // treating it as a permanent disconnect.
        if let Some(sftp) = &self.sftp {
            until_sftp_ends(&sftp.ended, sftp.canonicalize("."))
                .await
                .map_err(|e| {
                    classify_russh_err(e, |s| {
                        ProviderError::ConnectionLost(format!("SFTP keepalive failed: {}", s))
                    })
                })?;
        }

        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok(format!(
            "SFTP Server: {}:{} (user: {}, home: {})",
            self.config.host, self.config.port, self.config.username, self.home_dir
        ))
    }

    fn supports_chmod(&self) -> bool {
        true // SFTP supports chmod
    }

    async fn chmod(&mut self, path: &str, mode: u32) -> Result<(), ProviderError> {
        let sftp = self.get_sftp()?;
        let full_path = self.normalize_path(path);

        tracing::info!("SFTP: chmod {} to {:o}", full_path, mode);

        let attrs = russh_sftp::protocol::FileAttributes {
            permissions: Some(mode),
            ..Default::default()
        };

        until_sftp_ends(&sftp.ended, sftp.set_metadata(&full_path, attrs))
            .await
            .map_err(|e| {
                classify_russh_err(e, |s| {
                    ProviderError::ServerError(format!("Failed to chmod: {}", s))
                })
            })?;

        Ok(())
    }

    fn supports_symlinks(&self) -> bool {
        true // SFTP supports symlinks
    }

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let sftp = self.get_sftp()?;
        let root = self.normalize_path(path);
        let mut results = Vec::new();
        let mut dirs_to_scan = vec![root];

        while let Some(dir) = dirs_to_scan.pop() {
            let entries = match until_sftp_ends(&sftp.ended, sftp.read_dir(&dir)).await {
                Ok(e) => e,
                // A folder that cannot be read is skipped; a connection that
                // ended is not a folder, and the rest of the walk could not
                // be read either.
                Err(e) => match classify_russh_err(e, ProviderError::ServerError) {
                    lost @ ProviderError::ConnectionLost(_) => return Err(lost),
                    _ => continue,
                },
            };

            for entry in entries {
                let name = entry.file_name();
                if name == "." || name == ".." {
                    continue;
                }

                let entry_path = if dir == "/" {
                    format!("/{}", name)
                } else {
                    format!("{}/{}", dir.trim_end_matches('/'), name)
                };

                let remote_entry =
                    Self::metadata_to_entry(name.clone(), entry_path.clone(), &entry.metadata());

                if remote_entry.is_walkable_dir() {
                    dirs_to_scan.push(entry_path.clone());
                }

                if super::matches_find_pattern(&name, pattern) {
                    results.push(remote_entry);
                    if results.len() >= 500 {
                        return Ok(results);
                    }
                }
            }
        }

        Ok(results)
    }

    async fn storage_info(&mut self) -> Result<super::StorageInfo, ProviderError> {
        let sftp = self.get_sftp()?;
        let path = self.normalize_path(".");

        let stat = until_sftp_ends(&sftp.ended, sftp.fs_info(path))
            .await
            .map_err(|e| {
                classify_russh_err(e, |s| {
                    ProviderError::ServerError(format!("statvfs failed: {}", s))
                })
            })?
            .ok_or_else(|| {
                ProviderError::NotSupported("Server does not support statvfs".to_string())
            })?;

        let total = stat.blocks * stat.fragment_size;
        let free = stat.blocks_avail * stat.fragment_size;
        let used = total.saturating_sub(free);

        Ok(super::StorageInfo {
            used,
            total,
            free,
            versioning_bytes: None,
        })
    }

    async fn set_speed_limit(
        &mut self,
        upload_kb: u64,
        download_kb: u64,
    ) -> Result<(), ProviderError> {
        self.upload_limit_bps = upload_kb * 1024;
        self.download_limit_bps = download_kb * 1024;
        tracing::info!(
            "SFTP: Speed limits set: download={}KB/s upload={}KB/s",
            download_kb,
            upload_kb
        );
        Ok(())
    }

    async fn get_speed_limit(&mut self) -> Result<(u64, u64), ProviderError> {
        Ok((self.upload_limit_bps / 1024, self.download_limit_bps / 1024))
    }

    fn transfer_optimization_hints(&self) -> super::TransferOptimizationHints {
        // Shaped-graph multipart trait (S3-T09): intentionally NotSupported
        // by design on SFTP.
        //
        // SFTP v3 (the version russh and the broader OpenSSH ecosystem
        // expose) writes to an open file handle with `SSH_FXP_WRITE` at
        // explicit offsets. In principle a single file could be written
        // by multiple concurrent `SSH_FXP_WRITE` packets at different
        // offsets over one channel, but in practice (a) most servers
        // serialise writes on the open file handle, and (b) `russh-sftp`
        // pipelines the WRITEs of one handle in order (8 in flight, which
        // already fill OpenSSH's 2 MiB channel window) and knows no parts;
        // `upload` above streams the file through one `sftp.create` handle. Real
        // file-level parallelism on SFTP comes from `SftpConnectionPool`
        // re-dialling independent SSH channels (see
        // `transfer_executor_kind` below).
        //
        // Wiring a per-part SFTP backend is tracked as T-DEBT-09
        // (`--sftp-concurrency` flag) for v4.x. The pool is not the
        // missing piece: it already hands out one lease per file, which
        // is the file-level concurrency described above. What is missing
        // is one level down, inside a single file: a per-part fan-out
        // and a writer that can address parts on the upload path. Until
        // then we leave `supports_multipart=false` and let the runner
        // pick the legacy single-stream path.
        super::TransferOptimizationHints {
            supports_resume_download: false,
            supports_resume_upload: false,
            supports_range_download: true,
            supports_compression: true,
            supports_delta_sync: true,
            ..Default::default()
        }
    }

    /// PD-SFTP-1: advertise real file-level parallelism only once a secure
    /// connection spec exists to re-dial independent SSH connections from.
    /// Without it (never connected, or credentials unavailable) SFTP stays
    /// a single locked lease: honest non-regression, no overclaim.
    /// Opt into warm-connection reuse across the files of one batch, like FTP.
    ///
    /// The batch executor mints a clone worker per file and, unless the
    /// provider opts in, drops it afterwards: on SFTP that was a full SSH
    /// handshake (key exchange plus authentication, several round trips) for
    /// every file. The review battery of 2026-09-05 measured it on 5000 small
    /// files over a 53 ms link: about 3 files per second with four leases.
    /// Reuse is safe here for the same reasons it is on FTP: `upload` and
    /// `download` short-circuit `ensure_connected` while the channel is open
    /// and address every path through `normalize_path`, so a recycled worker
    /// carries no per-file state; the executor parks a worker only after a
    /// SUCCESSFUL transfer, and a parked worker whose channel has since died
    /// fails its next file loudly rather than silently, exactly as a fresh
    /// dial that failed would.
    fn supports_transfer_worker_reuse(&self) -> bool {
        true
    }

    /// Parallel directory scans on N independent SSH connections. The kind's
    /// name says HTTP because that is where the clone-pool scanner was born;
    /// the contract it encodes ("`clone_for_list` mints an independent
    /// worker") holds for a re-dialled SSH connection just the same. Without a
    /// connection spec there is nothing to re-dial from, so the scan stays on
    /// the single locked session.
    fn list_executor_kind(&self) -> super::ProviderListExecutorKind {
        if self.connection_spec.is_some() {
            super::ProviderListExecutorKind::HttpClonePool
        } else {
            super::ProviderListExecutorKind::LockedSingle
        }
    }

    /// Same ceiling as the transfer pool: each lease is a full SSH connection.
    fn list_executor_max_sessions(&self) -> u16 {
        self.transfer_executor_max_sessions()
    }

    /// A scan worker is a transfer worker: an unconnected clone that dials its
    /// own connection on first use and, being reusable, is kept warm by the
    /// scanner across directories instead of re-dialled per directory.
    fn clone_for_list(&self) -> Result<Box<dyn StorageProvider>, ProviderError> {
        self.clone_for_transfer()
    }

    fn transfer_executor_kind(&self) -> ProviderTransferExecutorKind {
        if self.connection_spec.is_some() {
            ProviderTransferExecutorKind::SftpConnectionPool
        } else {
            ProviderTransferExecutorKind::LockedSingle
        }
    }

    /// Ceiling of independent SSH connections one job may hold. Each lease is
    /// a full connection. The former cap of 4 asked for "a live benchmark on
    /// the target server" before being raised; the DAG engine review battery
    /// on the Hetzner lab (wired gigabit, 5000 x 4 KiB files, 2026-09-08) is
    /// that benchmark: at --parallel 16 rclone gained 1.9x on upload and 3.6x
    /// on download over 4, while AeroFTP moved 1% because the cap silently
    /// bound the flag. 16 matches rclone's range and the clamp already used by
    /// --sftp-concurrency; the effective count stays min(ceiling, --parallel),
    /// so the default of 4 connections is unchanged.
    fn transfer_executor_max_sessions(&self) -> u16 {
        SFTP_POOL_MAX_SESSIONS
    }

    /// Produce an independent transfer worker. It is **not connected**:
    /// it carries only the secure `SftpConnectionSpec` and dials its own
    /// separate SSH connection lazily on the first transfer
    /// (`ensure_connected`). No SSH handle or channel is shared, so N
    /// workers are N independent connections, exactly like the FTP pool.
    fn clone_for_transfer(&self) -> Result<Box<dyn StorageProvider>, ProviderError> {
        let spec = self.connection_spec.clone().ok_or_else(|| {
            ProviderError::NotSupported(
                "SFTP clone_for_transfer requires a captured connection spec".to_string(),
            )
        })?;
        let mut worker = SftpProvider::new(spec.to_config());
        worker.current_dir = self.current_dir.clone();
        worker.home_dir = self.home_dir.clone();
        worker.download_limit_bps = self.download_limit_bps;
        worker.upload_limit_bps = self.upload_limit_bps;
        worker.compression_enabled = self.compression_enabled;
        worker.buffer_size = self.buffer_size;
        worker.connection_spec = Some(spec);
        worker.multi_thread_streams = self.multi_thread_streams;
        worker.multi_thread_cutoff = self.multi_thread_cutoff;
        worker.sftp_readahead = self.sftp_readahead;
        Ok(Box::new(worker))
    }

    fn set_chunk_sizes(&mut self, upload: Option<u64>, download: Option<u64>) {
        // Cap at 16 MB (larger buffers waste memory without improving throughput).
        // BUFFER-01: SFTP uses a single transfer buffer for both directions, so
        // when both --chunk-size and --buffer-size are given we apply the larger
        // value deterministically instead of silently letting the last writer win.
        let cap = 16 * 1024 * 1024;
        let requested = upload.into_iter().chain(download).max();
        if let Some(size) = requested {
            self.buffer_size = (size as usize).clamp(4096, cap);
        }
    }

    /// PD-SFTP-2: opt into intra-file parallelism. `streams <= 1` keeps the
    /// single-stream path (honest default). `cutoff` floors at 1 MiB so a
    /// degenerate value can never split a tiny file into N SSH handshakes.
    /// The intra-file path additionally requires a real connection spec
    /// (`SftpConnectionPool`) at `download()` time, so a not-connected /
    /// credential-less provider never overclaims.
    fn set_multi_thread_download(&mut self, streams: usize, cutoff_bytes: u64) {
        self.multi_thread_streams = streams.clamp(1, SFTP_MULTI_THREAD_MAX_STREAMS);
        self.multi_thread_cutoff = cutoff_bytes.max(SFTP_MULTI_THREAD_CUTOFF_FLOOR);
    }

    fn multi_thread_cutoff_floor(&self) -> u64 {
        SFTP_MULTI_THREAD_CUTOFF_FLOOR
    }

    fn planned_download_segments(&self, file_size: u64) -> usize {
        crate::provider_transfer_executor::plan_segment_count(
            file_size,
            self.multi_thread_streams,
            SFTP_MULTI_THREAD_MAX_STREAMS,
            crate::provider_transfer_executor::SegmentCutoff::Explicit(self.multi_thread_cutoff),
            SFTP_MULTI_THREAD_CUTOFF_FLOOR,
        )
    }

    fn set_sftp_readahead(&mut self, window: Option<usize>) {
        self.sftp_readahead = SftpReadaheadSetting::from_explicit(window);
    }

    fn supports_delta_sync(&self) -> bool {
        true
    }

    async fn read_range(
        &mut self,
        path: &str,
        offset: u64,
        len: u64,
    ) -> Result<Vec<u8>, ProviderError> {
        // PD-SFTP-1: clone-for-transfer workers start unconnected and
        // re-dial on first transfer. `download()` already does this; the
        // segmented engine (`run_provider_segmented_download`) calls
        // `read_range` directly on the pool worker, so the same self-dial
        // must happen here or every segmented download against a clone
        // pool fails with `Not connected`.
        self.ensure_connected().await?;
        let sftp = self
            .sftp
            .as_ref()
            .ok_or_else(|| ProviderError::NotConnected)?;
        let full_path = self.normalize_path(path);

        let mut file = until_sftp_ends(&sftp.ended, sftp.open(&full_path))
            .await
            .map_err(|e| {
                classify_russh_err(e, |s| {
                    ProviderError::ServerError(format!("Failed to open file for range read: {}", s))
                })
            })?;

        let ended = sftp.ended.clone();
        let streamed: Result<Vec<u8>, ProviderError> = {
            let file = &mut file;
            let ended = &ended;
            async move {
                use tokio::io::{AsyncReadExt, AsyncSeekExt};
                file.seek(std::io::SeekFrom::Start(offset))
                    .await
                    .map_err(|e| {
                        classify_russh_err(e, |s| {
                            ProviderError::ServerError(format!("Failed to seek: {}", s))
                        })
                    })?;

                // GAP-A03: Cap read_range allocation to prevent attacker-controlled OOM
                const MAX_READ_RANGE: u64 = 100 * 1024 * 1024; // 100 MB
                if len > MAX_READ_RANGE {
                    return Err(ProviderError::Other(format!(
                        "Read range size {} exceeds maximum {} bytes",
                        len, MAX_READ_RANGE
                    )));
                }

                let mut buf = vec![0u8; len as usize];
                let mut total_read = 0usize;
                while total_read < len as usize {
                    let n = until_sftp_ends(ended, file.read(&mut buf[total_read..]))
                        .await
                        .map_err(|e| {
                            classify_russh_err(e, |s| {
                                ProviderError::ServerError(format!("Failed to read range: {}", s))
                            })
                        })?;
                    if n == 0 {
                        break;
                    }
                    total_read += n;
                }
                buf.truncate(total_read);
                Ok(buf)
            }
        }
        .await;
        close_sftp_file(file, &ended).await;
        streamed
    }
}

/// PD-PIPE-1: parse `AEROFTP_SFTP_READ_PIPELINE` into a pipeline window.
///
/// The flag is **off by default**, so an unset/`0`/`1`/`false` value keeps
/// the exact serial `download()` loop (byte-identical, diff-0). A value
/// `>= 2` (or a truthy word) enables bounded read pipelining on the **single
/// existing** SFTP session and is the only thing that changes the read
/// scheduling. The window is capped so a hostile value cannot blow memory
/// (`window * buffer_size` is the worst-case in-flight footprint).
const SFTP_PIPELINE_MAX_WINDOW: usize = 64;
const SFTP_PIPELINE_DEFAULT_WINDOW: usize = 16;

/// Pure parser for the PD-PIPE-1 flag value (env read split out so it is
/// deterministically unit-testable without mutating process-global env).
fn parse_sftp_read_pipeline_window(raw: Option<&str>) -> Option<usize> {
    let v = raw?.trim().to_ascii_lowercase();
    match v.as_str() {
        "" | "0" | "1" | "false" | "off" | "no" => None,
        "true" | "on" | "yes" => Some(SFTP_PIPELINE_DEFAULT_WINDOW),
        _ => match v.parse::<usize>() {
            Ok(n) if n >= 2 => Some(n.min(SFTP_PIPELINE_MAX_WINDOW)),
            _ => None,
        },
    }
}

fn sftp_read_pipeline_window() -> Option<usize> {
    parse_sftp_read_pipeline_window(std::env::var("AEROFTP_SFTP_READ_PIPELINE").ok().as_deref())
}

/// EXPERIMENT (2026-07-22): sliding-window read-ahead flag. Unset / `0` / `1`
/// / falsey = off;
/// `on`/`yes`/`true` = the measured default window; numeric `>= 2` requests
/// that window. The effective value is further reduced by the job-wide byte
/// and handle budgets. Distinct env so it can be A/B'd against the batched
/// `sftp_pipelined_download`.
fn normalize_sftp_readahead_window(requested: usize) -> Option<usize> {
    (requested >= 2).then_some(requested.min(SFTP_READAHEAD_MAX_WINDOW))
}

fn parse_sftp_readahead_window(raw: Option<&str>) -> Option<usize> {
    let v = raw?.trim().to_ascii_lowercase();
    match v.as_str() {
        "" | "0" | "1" | "false" | "off" | "no" => None,
        "true" | "on" | "yes" => Some(SFTP_READAHEAD_DEFAULT_WINDOW),
        _ => v
            .parse::<usize>()
            .ok()
            .and_then(normalize_sftp_readahead_window),
    }
}

/// Apply a job-wide resource budget to a requested per-connection window.
/// Returns `None` when even a two-read window would exceed the budget, so the
/// caller can use its serial, rate-limited path instead.
fn effective_sftp_readahead_window(
    requested: usize,
    chunk: usize,
    expected: u64,
    parallel_connections: usize,
) -> Option<usize> {
    if expected == 0 {
        return None;
    }
    let chunk = chunk.max(4096) as u64;
    let n_chunks = usize::try_from(expected.div_ceil(chunk)).unwrap_or(usize::MAX);
    let connections = parallel_connections.clamp(1, SFTP_MULTI_THREAD_MAX_STREAMS);
    let per_connection_budget = SFTP_READAHEAD_JOB_BUFFER_BUDGET / connections as u64;
    let buffer_slots = per_connection_budget / chunk;
    let by_bytes = buffer_slots.saturating_sub(1) / 2;
    let by_handles = SFTP_READAHEAD_JOB_MAX_HANDLES / connections;
    let effective = requested
        .min(SFTP_READAHEAD_MAX_WINDOW)
        .min(n_chunks)
        .min(by_bytes as usize)
        .min(by_handles);
    (effective >= 2).then_some(effective)
}

/// Read-ahead writes out of order into a fresh temp, so it must not supersede
/// resumable or in-place semantics. A pre-existing `.aerotmp` is handled by the
/// serial path, which validates symlinks and resumes from its known offset.
fn sftp_readahead_local_path_is_eligible(local_path: &str) -> bool {
    let local_path = Path::new(local_path);
    !super::atomic_write::inplace_active()
        && !aerotmp_path_for(local_path).exists()
        && !super::atomic_write::readahead_temp_path_for(local_path).exists()
}

/// PD-PIPE-1: read exactly `want` bytes from `file` starting at the absolute
/// offset `abs_off`, looping on short protocol reads. A `read() == 0` before
/// `want` is **not** an error here: it means EOF (the remote file is shorter
/// than the metadata size); the caller treats a returned buffer shorter than
/// the requested window as the end of the transfer, exactly as the serial
/// loop stops on `read() == 0`.
async fn sftp_pipelined_read_window(
    file: &mut russh_sftp::client::fs::File,
    abs_off: u64,
    want: usize,
    ended: &CancellationToken,
) -> Result<Vec<u8>, ProviderError> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    file.seek(std::io::SeekFrom::Start(abs_off))
        .await
        .map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::TransferFailed(format!("Seek error (pipeline): {}", s))
            })
        })?;

    let mut buf = vec![0u8; want];
    let mut filled = 0usize;
    while filled < want {
        let n = until_sftp_ends(ended, file.read(&mut buf[filled..]))
            .await
            .map_err(|e| {
                classify_russh_err(e, |s| {
                    ProviderError::TransferFailed(format!("Read error (pipeline): {}", s))
                })
            })?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    buf.truncate(filled);
    Ok(buf)
}

/// Opens `n` read handles on `full_path` at once. A server with a tighter
/// handle limit gets fewer: when an OPEN fails, the ones that did open are
/// closed, awaited (Drop would only queue `close_nowait`), and half as many
/// are asked for, down to one; failing that one is the error. Cancellation
/// covers the OPEN fan-out as well.
async fn open_readahead_handles(
    sftp: &SftpChannel,
    full_path: &str,
    n: usize,
    cancel: &CancellationToken,
) -> Result<Vec<russh_sftp::client::fs::File>, ProviderError> {
    let mut n = n.max(1);
    loop {
        let open_fut = futures_util::future::join_all(
            (0..n).map(|_| until_sftp_ends(&sftp.ended, sftp.open(full_path))),
        );
        tokio::pin!(open_fut);
        let mut cancelled = false;
        let opened = tokio::select! {
            _ = cancel.cancelled() => {
                cancelled = true;
                // Stay in this function: wait for in-flight OPENs so their
                // File values can be closed with await. Beyond 2s the
                // remaining opens are dropped (close_nowait).
                match tokio::time::timeout(std::time::Duration::from_secs(2), &mut open_fut)
                    .await
                {
                    Ok(r) => r,
                    Err(_) => {
                        return Err(ProviderError::TransferFailed(
                            SFTP_TRANSFER_CANCELLED.to_string(),
                        ));
                    }
                }
            }
            result = &mut open_fut => result,
        };
        let mut ok = Vec::new();
        let mut err = None;
        for r in opened {
            match r {
                Ok(file) => ok.push(file),
                Err(e) => err = Some(e),
            }
        }
        if cancelled {
            close_sftp_files(ok, &sftp.ended).await;
            return Err(ProviderError::TransferFailed(
                SFTP_TRANSFER_CANCELLED.to_string(),
            ));
        }
        match err {
            None => return Ok(ok),
            Some(e) if n > 1 => {
                close_sftp_files(ok, &sftp.ended).await;
                let reduced = (n / 2).max(1);
                tracing::warn!(
                    "SFTP read-ahead: opening {} handles failed ({}); retrying with {}",
                    n,
                    e,
                    reduced
                );
                n = reduced;
            }
            Some(e) => {
                close_sftp_files(ok, &sftp.ended).await;
                return Err(classify_russh_err(e, |s| {
                    ProviderError::TransferFailed(format!(
                        "Failed to open remote file (readahead): {}",
                        s
                    ))
                }));
            }
        }
    }
}

/// One READ of at most `want` bytes at `abs_off`: what the server sends back
/// for a single request, which russh-sftp asks for no larger than the
/// session's read length. Empty at end of file.
async fn sftp_read_once(
    file: &mut russh_sftp::client::fs::File,
    abs_off: u64,
    want: usize,
    ended: &CancellationToken,
) -> Result<Vec<u8>, ProviderError> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    file.seek(std::io::SeekFrom::Start(abs_off))
        .await
        .map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::TransferFailed(format!("Seek error (readahead): {}", s))
            })
        })?;
    let mut buf = vec![0u8; want];
    let n = until_sftp_ends(ended, file.read(&mut buf))
        .await
        .map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::TransferFailed(format!("Read error (readahead): {}", s))
            })
        })?;
    buf.truncate(n);
    Ok(buf)
}

/// Removes a partial read-ahead download temp on drop unless disarmed after a
/// successful commit, so a mid-transfer error or a dropped future never leaves
/// an orphan `.aeroardtmp` behind.
#[derive(Debug)]
struct ReadaheadTempGuard {
    path: PathBuf,
    armed: bool,
}
impl ReadaheadTempGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }
    fn disarm(&mut self) {
        self.armed = false;
    }
}

/// Create and pre-size the read-ahead temp while retaining the same exclusive
/// file handle through all writes. `create_new` rejects symlinks and concurrent
/// writers; arming the guard before `set_len` covers allocation failures too.
async fn create_sftp_readahead_temp(
    local_path: &str,
    total_size: u64,
) -> Result<(tokio::fs::File, ReadaheadTempGuard), ProviderError> {
    let final_path = Path::new(local_path);
    if let Some(parent) = final_path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(ProviderError::IoError)?;
        }
    }

    let temp_path = super::atomic_write::readahead_temp_path_for(final_path);
    let file = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp_path)
        .await
        .map_err(|e| {
            ProviderError::TransferFailed(format!(
                "Failed to create exclusive local read-ahead temp {}: {}",
                temp_path.display(),
                super::atomic_write::temp_claim::name_too_long(e, &temp_path)
            ))
        })?;
    let guard = ReadaheadTempGuard::new(temp_path);
    if let Err(e) = file.set_len(total_size).await {
        // Windows cannot remove an open file. Close it before returning so the
        // subsequently-dropped guard can remove the failed allocation.
        drop(file);
        return Err(ProviderError::TransferFailed(format!(
            "Failed to size local temp: {}",
            e
        )));
    }
    Ok((file, guard))
}
impl Drop for ReadaheadTempGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Closes every handle of `files`, each one awaited (see [`close_sftp_file`]).
async fn close_sftp_files(files: Vec<russh_sftp::client::fs::File>, ended: &CancellationToken) {
    for file in files {
        close_sftp_file(file, ended).await;
    }
}

/// Awaited close of a write handle. `File::shutdown` drains pending WRITE
/// acks first; a rejected write returns before CLOSE is sent, so Drop would
/// only queue `close_nowait`. A second shutdown, with the ack queue empty,
/// awaits the CLOSE.
async fn shutdown_sftp_file(
    file: &mut russh_sftp::client::fs::File,
    ended: &CancellationToken,
) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    // The close waits for every write still in flight: see `until_sftp_acks`.
    let first = until_sftp_acks(ended, file.shutdown()).await;
    if first.is_err() && !ended.is_cancelled() {
        let _ = until_sftp_acks(ended, file.shutdown()).await;
    }
    first
}

/// Sliding-window read-ahead for the byte range `[start, start + expected)` on
/// ONE SFTP session, written positioned into the already-open `out` at each
/// chunk's absolute offset.
///
/// Why this exists: russh-sftp's `File` reads serially (one `SSH_FXP_READ`
/// awaited per `poll_read`; upstream AspectUnk/russh-sftp#70), so a single SFTP
/// connection downloads far below the link while its writes pipeline
/// (`write_nowait`) and upload runs ~3x faster on the same session. We cannot
/// reach the private `RawSftpSession` to add a symmetric `read_nowait`, but the
/// session multiplexes concurrent requests by id with no global lock, so we keep
/// `window` `SSH_FXP_READ` continuously in flight: open `window` cheap file
/// handles and let each free handle claim the next chunk. Unlike the batched
/// `sftp_pipelined_download` there is no per-window barrier: the readers feed a
/// single writer over a bounded channel, and every chunk is one READ (the first
/// READ sizes them), so the pipe never drains between batches. Composes with the
/// PD-SFTP-2 independent-connection pool (N connections x this read-ahead).
///
/// Strict: every chunk must return its full `want` (a short read before the
/// range end is a truncation / mid-transfer change -> hard error, never a silent
/// short write; the SHA-256 gate backs it). Cancellation is honored before and
/// during each read. `aggregate` always accumulates bytes written (the PD-SFTP-2
/// pool observes progress through it). `on_progress`, when set, is additionally
/// called from the single writer with the running total against
/// `total_for_progress`; it is taken by value (an owned `Box<dyn Fn + Send>` is
/// `Send`, so holding it across the writer's awaits keeps this future `Send`,
/// with no spawned ticker to leak).
#[allow(clippy::too_many_arguments)]
async fn sftp_readahead_range_into(
    sftp: &SftpChannel,
    full_path: &str,
    start: u64,
    expected: u64,
    out: &mut tokio::fs::File,
    chunk: usize,
    window: usize,
    grow_to: Option<usize>,
    aggregate: &Arc<AtomicU64>,
    cancel: &CancellationToken,
    total_for_progress: u64,
    on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    fail_write: Arc<AtomicBool>,
) -> Result<(), ProviderError> {
    use tokio::io::{AsyncSeekExt, AsyncWriteExt};
    if expected == 0 {
        return Ok(());
    }
    let chunk = (chunk.max(4096)) as u64;
    let n_chunks = expected.div_ceil(chunk).max(1);
    let eff_window = (window.clamp(1, SFTP_READAHEAD_MAX_WINDOW) as u64).min(n_chunks) as usize;
    let handles = open_readahead_handles(sftp, full_path, eff_window, cancel).await?;

    // One chunk, one READ. russh-sftp cuts a READ at the session's read length,
    // and OpenSSH's (261120 bytes) is 1 KiB under the 256 KiB default chunk:
    // every chunk took a second READ for its last KiB, sent only once the
    // first reply was in, and the server queued it behind every reply already
    // asked for. So the handles went round in step, and each round ended with
    // the server sending the tails and then nothing for a round trip, until
    // the next round of READs arrived (lab, 47 ms link, 2026-10-04: the
    // server's send queue ran empty every ~1.2 s). The first READ comes back
    // with what the server sends for one request, and that sizes every chunk
    // after it.
    let mut handles = handles;
    let first_want = chunk.min(expected) as usize;
    let Some(probe) = handles.first_mut() else {
        return Err(ProviderError::TransferFailed(
            "Failed to open remote file (readahead): no handle".to_string(),
        ));
    };
    let first = tokio::select! {
        _ = cancel.cancelled() => Err(ProviderError::TransferFailed(
            SFTP_TRANSFER_CANCELLED.to_string(),
        )),
        r = sftp_read_once(probe, start, first_want, &sftp.ended) => r,
    };
    let first = match first {
        Ok(first) if !first.is_empty() => first,
        Ok(_) => {
            close_sftp_files(handles, &sftp.ended).await;
            return Err(ProviderError::TransferFailed(format!(
                "Short read at offset {} (0 of {} bytes): remote file changed or truncated",
                start, first_want
            )));
        }
        Err(e) => {
            close_sftp_files(handles, &sftp.ended).await;
            return Err(e);
        }
    };
    // A first reply of only a few bytes does not size the rest below the floor:
    // the readers still loop on short replies, so the floor costs nothing in
    // correctness.
    let configured = chunk;
    let chunk = if first.len() < first_want {
        (first.len() as u64).max(SFTP_READAHEAD_MIN_LEARNED_CHUNK.min(configured))
    } else {
        configured
    };
    // What is left of the range after the first READ, in chunks of that size.
    let rest_start = start + first.len() as u64;
    let rest = expected - first.len() as u64;
    let n_chunks = rest.div_ceil(chunk);

    // Each handle keeps one READ in flight, so on a server that sends less
    // per READ than the configured chunk a window holds fewer bytes than it
    // was sized for. With `grow_to`, the window grows back toward those bytes,
    // but never past `grow_to` handles (the handles the release opened on a
    // connection): a server with a small read length gets no less depth than
    // before and no new load. Without it the window stays as asked. A server
    // that will not open more handles leaves the read-ahead with the ones it
    // already has; the bytes already read are kept either way.
    if let Some(grow_to) = grow_to.filter(|_| chunk < configured) {
        let target = usize::try_from(handles.len() as u64 * configured / chunk)
            .unwrap_or(usize::MAX)
            .min(grow_to)
            .min(SFTP_READAHEAD_MAX_WINDOW)
            .min(usize::try_from(n_chunks).unwrap_or(usize::MAX));
        if target > handles.len() {
            match open_readahead_handles(sftp, full_path, target - handles.len(), cancel).await {
                Ok(more) => handles.extend(more),
                Err(e) if is_transfer_cancellation(&e) => {
                    close_sftp_files(handles, &sftp.ended).await;
                    return Err(e);
                }
                Err(e) => tracing::warn!(
                    "SFTP read-ahead: no more handles ({}); reading with {}",
                    e,
                    handles.len()
                ),
            }
        }
    }
    let eff_window = handles.len();

    // `eff_window` readers -> one writer. The writer owns `out` (no cursor race)
    // and is the sole caller of `on_progress`.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(u64, Vec<u8>)>(eff_window.max(2));
    // Child token: an I/O error here must not cancel the caller's token.
    let work_cancel = cancel.child_token();

    let readers = {
        let work_cancel = work_cancel.clone();
        async move {
            let mut reader_tasks = Vec::with_capacity(eff_window);
            let next_chunk = Arc::new(AtomicU64::new(0));
            for mut file in handles {
                let tx = tx.clone();
                let work_cancel = work_cancel.clone();
                let next_chunk = next_chunk.clone();
                reader_tasks.push(async move {
                    let result: Result<(), ProviderError> = async {
                        loop {
                            if work_cancel.is_cancelled() {
                                return Err(ProviderError::TransferFailed(
                                    SFTP_TRANSFER_CANCELLED.to_string(),
                                ));
                            }
                            let j = next_chunk.fetch_add(1, Ordering::Relaxed);
                            if j >= n_chunks {
                                break;
                            }
                            let rel_off = j * chunk;
                            let abs_off = rest_start + rel_off;
                            let want = std::cmp::min(chunk, rest - rel_off) as usize;
                            let buf = tokio::select! {
                                _ = work_cancel.cancelled() => {
                                    return Err(ProviderError::TransferFailed(
                                        SFTP_TRANSFER_CANCELLED.to_string(),
                                    ));
                                }
                                r = sftp_pipelined_read_window(&mut file, abs_off, want, &sftp.ended) => r?,
                            };
                            if buf.len() != want {
                                return Err(ProviderError::TransferFailed(format!(
                                    "Short read at offset {} ({} of {} bytes): remote file changed or truncated",
                                    abs_off,
                                    buf.len(),
                                    want
                                )));
                            }
                            tokio::select! {
                                _ = work_cancel.cancelled() => {
                                    return Err(ProviderError::TransferFailed(
                                        SFTP_TRANSFER_CANCELLED.to_string(),
                                    ));
                                }
                                sent = tx.send((abs_off, buf)) => {
                                    if sent.is_err() {
                                        // Writer went away; its error propagates.
                                        break;
                                    }
                                }
                            }
                        }
                        Ok(())
                    }
                    .await;
                    close_sftp_file(file, &sftp.ended).await;
                    if result.is_err() {
                        work_cancel.cancel();
                    }
                    result
                });
            }
            // Drop the original sender so `rx` closes once every reader clone is
            // gone; otherwise the writer would wait forever.
            drop(tx);
            let results = futures_util::future::join_all(reader_tasks).await;
            first_reader_cause(results)
        }
    };

    // `async move` so `on_progress` is captured BY VALUE (owned `Box<dyn Fn +
    // Send>` is `Send`); capturing it by reference would need it to be `Sync`,
    // which a bare `dyn Fn + Send` is not, and would make this future `!Send`.
    let writer = {
        let work_cancel = work_cancel.clone();
        async move {
            // The first READ's bytes, then the readers' chunks.
            let mut first = Some((start, first));
            loop {
                let (abs_off, buf) = match first.take() {
                    Some(piece) => piece,
                    None => match rx.recv().await {
                        Some(piece) => piece,
                        None => break,
                    },
                };
                if work_cancel.is_cancelled() {
                    return Err(ProviderError::TransferFailed(
                        SFTP_TRANSFER_CANCELLED.to_string(),
                    ));
                }
                if fail_write.swap(false, Ordering::SeqCst) {
                    work_cancel.cancel();
                    return Err(ProviderError::IoError(std::io::Error::other(
                        "injected write fail",
                    )));
                }
                if let Err(e) = out.seek(std::io::SeekFrom::Start(abs_off)).await {
                    work_cancel.cancel();
                    return Err(ProviderError::IoError(e));
                }
                if let Err(e) = out.write_all(&buf).await {
                    work_cancel.cancel();
                    return Err(ProviderError::IoError(e));
                }
                let done =
                    aggregate.fetch_add(buf.len() as u64, Ordering::Relaxed) + buf.len() as u64;
                if let Some(ref cb) = on_progress {
                    cb(done, total_for_progress);
                }
            }
            Ok::<(), ProviderError>(())
        }
    };

    let (reader_res, writer_res) = tokio::join!(readers, writer);
    match (reader_res, writer_res) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(e)) | (Err(e), Ok(())) => Err(e),
        // The readers' error, unless it is a cancellation and the writer's
        // is not (the readers stopped because the writer failed).
        (Err(a), Err(b)) => first_reader_cause(vec![Err(a), Err(b)]),
    }
}

/// The error a group of read-ahead readers reports. When one reader fails
/// it cancels the others, and each of them then fails as cancelled; the first
/// error in reader order was often one of those, so a connection lost by one
/// reader surfaced as "Transfer cancelled by user", which nobody did and which
/// the command layer does not retry. The first error that is not such a
/// cancellation is the cause; a cancellation is reported only when every
/// failure was one (the caller's own cancel).
///
/// A cancellation is the error this module writes for one
/// ([`SFTP_TRANSFER_CANCELLED`]) or [`ProviderError::Cancelled`], nothing
/// else: a real error whose text merely contains the word (a path such as
/// `/data/cancelled/x.bin`) is a cause.
fn first_reader_cause(results: Vec<Result<(), ProviderError>>) -> Result<(), ProviderError> {
    let mut cancelled = None;
    for result in results {
        match result {
            Ok(()) => {}
            Err(e) if is_transfer_cancellation(&e) => {
                cancelled.get_or_insert(e);
            }
            Err(e) => return Err(e),
        }
    }
    cancelled.map_or(Ok(()), Err)
}

/// What a transfer of this module reports when it was cancelled, by the
/// caller or by a sibling reader that failed.
const SFTP_TRANSFER_CANCELLED: &str = "Transfer cancelled by user";

/// Whether `error` is a cancellation (see [`first_reader_cause`]).
fn is_transfer_cancellation(error: &ProviderError) -> bool {
    match error {
        ProviderError::Cancelled => true,
        ProviderError::TransferFailed(message) => message == SFTP_TRANSFER_CANCELLED,
        _ => false,
    }
}

/// Single-connection sliding-window read-ahead download of a whole file, over
/// the one existing SFTP session (no new connection, no crate fork). Selected
/// through provider state; see `sftp_readahead_range_into` for the mechanism
/// and the issue #70 rationale. Byte-identical to the serial loop for a static
/// file; SHA-256 gated.
#[allow(clippy::too_many_arguments)]
async fn sftp_readahead_download(
    sftp: &SftpChannel,
    full_path: &str,
    total_size: u64,
    local_path: &str,
    chunk: usize,
    window: usize,
    on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    cancel: &CancellationToken,
    fail_write: Arc<AtomicBool>,
) -> Result<(), ProviderError> {
    use tokio::io::AsyncWriteExt;

    // This path opens as many handles as the window, all at once, a few
    // milliseconds apart. A handle already open keeps reading the file it was
    // opened on, so the risk is not the transfer but that burst: an object
    // replaced inside it would be read as two versions. The window is far
    // narrower than a whole transfer, but the check costs one round trip on
    // the session that is already there.
    // The transfer is bounded by `total_size`, read by the caller earlier, so
    // a reading that does not match it means this download would publish a
    // file cut to a length the object no longer has. Refusing here costs
    // nothing: not a byte has been read.
    let before = match SftpProvider::range_source_reading(sftp, full_path).await {
        Ok(reading) => match reading.matches_planned_size(total_size) {
            Ok(()) => reading,
            Err(why) => {
                return Err(ProviderError::ParallelRefused(parallel_refused(
                    "SFTP readahead",
                    full_path,
                    &why,
                )))
            }
        },
        Err(why) => {
            return Err(ProviderError::ParallelRefused(parallel_refused(
                "SFTP readahead",
                full_path,
                &why,
            )))
        }
    };

    // Keep the exclusive handle returned by create_new through commit: no
    // symlink following and no create/reopen TOCTOU window.
    let (file, temp_guard) = create_sftp_readahead_temp(local_path, total_size).await?;
    // Keep `out` declared after `guard`: locals drop in reverse order, which
    // closes the file before the guard removes it on error or cancellation.
    let mut guard = temp_guard;
    let mut out = file;

    let aggregate = Arc::new(AtomicU64::new(0));

    // The writer owns `on_progress` and calls it directly (real, incremental);
    // an owned `Box<dyn Fn + Send>` stays `Send` across the writer's awaits, so
    // there is no spawned ticker to leak on a dropped/cancelled transfer.
    sftp_readahead_range_into(
        sftp,
        full_path,
        0,
        total_size,
        &mut out,
        chunk,
        window,
        None,
        &aggregate,
        cancel,
        total_size,
        on_progress,
        fail_write,
    )
    .await?;

    out.flush()
        .await
        .map_err(|e| ProviderError::TransferFailed(format!("Failed to flush download: {}", e)))?;
    out.sync_all()
        .await
        .map_err(|e| ProviderError::TransferFailed(format!("Failed to sync download: {}", e)))?;
    drop(out);

    {
        let changed = match SftpProvider::range_source_reading(sftp, full_path).await {
            Ok(after) => before.differs_from(&after),
            Err(first) => {
                tracing::warn!(
                    "SFTP readahead: {} could not be read after the transfer ({}), reading once more",
                    full_path,
                    first
                );
                tokio::time::sleep(AFTER_TRANSFER_READ_RETRY).await;
                match SftpProvider::range_source_reading(sftp, full_path).await {
                    Ok(after) => before.differs_from(&after),
                    Err(why) => Some(format!(
                        "it could not be read again after the transfer: {why}"
                    )),
                }
            }
        };
        if let Some(what) = changed {
            // The guard removes the staged file. The caller reads this as a
            // refusal and downloads on one handle instead.
            return Err(ProviderError::ParallelRefused(source_changed(
                "SFTP readahead",
                full_path,
                &what,
            )));
        }
    }

    tokio::fs::rename(&guard.path, local_path)
        .await
        .map_err(|e| {
            ProviderError::TransferFailed(format!("Failed to finalize download: {}", e))
        })?;
    guard.disarm();

    tracing::info!(
        "SFTP: Download complete (readahead, window={}): {} bytes",
        window,
        total_size
    );
    Ok(())
}

/// PD-PIPE-1: pipelined single-stream SFTP download over the **one existing**
/// SFTP session.
///
/// Root cause this addresses (PD-BENCH-1, master 9.6.4): the serial
/// `download()` loop issues a single outstanding `SSH_FXP_READ` and then
/// serially `write_all`s it, with no net/disk overlap, so one stream is far
/// below the link. russh-sftp's `SftpSession` multiplexes concurrent
/// requests by id over one channel (`RawSftpSession`, an internal `Arc`
/// shared by every `File` it opens), so opening `window` cheap file handles
/// to the same path and reading disjoint stripes concurrently keeps `window`
/// `SSH_FXP_READ` in flight on the same connection. **No new TCP connection,
/// no new pool, one SFTP session** (this is deliberately not the PD-SFTP-2
/// independent-connection pool).
///
/// Diff-0: the bytes written are `[0, min(total_size, EOF))` in strict
/// ascending order, identical to the serial loop for a static file (the
/// real, gated scenario). Chunks are produced in `window`-sized batches and
/// written in order; the first short read ends the transfer just like the
/// serial loop's `read() == 0`. Only the read scheduling differs. Trusting
/// the metadata size for windowing is the same accepted discipline as the
/// shipped PD-SFTP-2 range worker; every run is SHA-256 gated.
async fn sftp_pipelined_download(
    sftp: &SftpChannel,
    full_path: &str,
    total_size: u64,
    atomic: &mut super::atomic_write::AtomicFile,
    chunk: usize,
    window: usize,
    on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
) -> Result<(), ProviderError> {
    let chunk = chunk.max(4096) as u64;
    let window = window.clamp(2, 64);
    let chunks_needed = total_size.div_ceil(chunk).max(1) as usize;
    let eff_window = window.min(chunks_needed);

    // Several handles onto disjoint stripes of one path is the shape
    // `sftp_readahead_download` guards, and the guard is the same: the object
    // is read before the handles open and again once they are closed, and a
    // file that moved in between is refused rather than handed to the caller
    // to commit. Both readings cost one round trip on this session.
    let before = match SftpProvider::range_source_reading(sftp, full_path).await {
        Ok(reading) => match reading.matches_planned_size(total_size) {
            Ok(()) => reading,
            Err(why) => {
                return Err(ProviderError::ParallelRefused(parallel_refused(
                    "SFTP pipeline",
                    full_path,
                    &why,
                )))
            }
        },
        Err(why) => {
            return Err(ProviderError::ParallelRefused(parallel_refused(
                "SFTP pipeline",
                full_path,
                &why,
            )))
        }
    };

    // `eff_window` handles, all on the SAME session: one SSH channel, the
    // RawSftpSession multiplexes the concurrent reads by request id.
    let mut handles: Vec<russh_sftp::client::fs::File> = Vec::with_capacity(eff_window);
    for _ in 0..eff_window {
        match until_sftp_ends(&sftp.ended, sftp.open(full_path)).await {
            Ok(f) => handles.push(f),
            Err(e) => {
                close_sftp_files(handles, &sftp.ended).await;
                return Err(classify_russh_err(e, |s| {
                    ProviderError::TransferFailed(format!(
                        "Failed to open remote file (pipeline): {}",
                        s
                    ))
                }));
            }
        }
    }

    let streamed: Result<u64, ProviderError> = {
        let handles = &mut handles;
        async move {
            let mut transferred: u64 = 0;
            let mut offset: u64 = 0;
            'outer: while offset < total_size {
                // Plan up to `eff_window` consecutive chunks for this batch.
                let mut wants: Vec<usize> = Vec::with_capacity(eff_window);
                for _ in 0..eff_window {
                    if offset >= total_size {
                        break;
                    }
                    let want = std::cmp::min(chunk, total_size - offset) as usize;
                    wants.push(want);
                    offset += want as u64;
                }
                if wants.is_empty() {
                    break;
                }

                // Issue the batch concurrently: each future borrows one distinct
                // handle (disjoint &mut via split_at_mut), so `window` reads are in
                // flight on the single connection at once.
                let n = wants.len();
                let batch_base = offset - wants.iter().map(|w| *w as u64).sum::<u64>();
                let (used, _rest) = handles.split_at_mut(n);
                let mut futs = Vec::with_capacity(n);
                let mut abs = batch_base;
                for (f, &want) in used.iter_mut().zip(wants.iter()) {
                    futs.push(sftp_pipelined_read_window(f, abs, want, &sftp.ended));
                    abs += want as u64;
                }
                let results = futures_util::future::try_join_all(futs).await?;

                // Write in strict offset order; the first short read is EOF.
                for (buf, &want) in results.iter().zip(wants.iter()) {
                    atomic.write_all(buf).await.map_err(|e| {
                        ProviderError::TransferFailed(format!("Write error: {}", e))
                    })?;
                    transferred += buf.len() as u64;
                    if let Some(ref progress) = on_progress {
                        progress(transferred, total_size);
                    }
                    if buf.len() < want {
                        break 'outer;
                    }
                }
            }

            Ok(transferred)
        }
    }
    .await;
    close_sftp_files(handles, &sftp.ended).await;
    streamed?;

    let changed = match SftpProvider::range_source_reading(sftp, full_path).await {
        Ok(after) => before.differs_from(&after),
        Err(first) => {
            tracing::warn!(
                "SFTP pipeline: {} could not be read after the transfer ({}), reading once more",
                full_path,
                first
            );
            tokio::time::sleep(AFTER_TRANSFER_READ_RETRY).await;
            match SftpProvider::range_source_reading(sftp, full_path).await {
                Ok(after) => before.differs_from(&after),
                Err(why) => Some(format!(
                    "it could not be read again after the transfer: {why}"
                )),
            }
        }
    };
    if let Some(what) = changed {
        // The caller drops the staged file and downloads on one handle.
        return Err(ProviderError::ParallelRefused(source_changed(
            "SFTP pipeline",
            full_path,
            &what,
        )));
    }
    Ok(())
}

/// PD-PIPE-2 strict gate (pure, unit-tested): inside a range sub-window a
/// `read() == 0` before the requested length is a **hard error**, never a
/// silent short read. This is exactly the serial PD-SFTP-2 worker's `n == 0`
/// gate (same message shape: expected/at-offset/got), factored out so the
/// strict-short-read-before-length path is deterministically testable
/// without a live SFTP server.
fn sftp_strict_short_read_check(
    n: usize,
    filled: usize,
    want: usize,
    abs_off: u64,
) -> Result<(), ProviderError> {
    if n == 0 && filled < want {
        return Err(ProviderError::TransferFailed(format!(
            "SFTP range short read: expected {} bytes at offset {}, got {}",
            want, abs_off, filled
        )));
    }
    Ok(())
}

/// PD-PIPE-2: read **exactly** `want` bytes from `file` at the absolute
/// offset `abs_off`. Unlike the PD-PIPE-1 [`sftp_pipelined_read_window`]
/// (EOF-tolerant: a short read is the end of a single-stream `download()`),
/// this is the **strict** sub-window read for the PD-SFTP-2 range worker:
/// a `read() == 0` before `want` is a hard [`ProviderError::TransferFailed`]
/// via [`sftp_strict_short_read_check`], byte-for-byte the serial worker's
/// strict gate. Each window must yield its full length or fail loud.
async fn sftp_pipelined_range_read_strict(
    file: &mut russh_sftp::client::fs::File,
    abs_off: u64,
    want: usize,
    ended: &CancellationToken,
) -> Result<Vec<u8>, ProviderError> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    file.seek(std::io::SeekFrom::Start(abs_off))
        .await
        .map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::ServerError(format!("Failed to seek to range start: {}", s))
            })
        })?;

    let mut buf = vec![0u8; want];
    let mut filled = 0usize;
    while filled < want {
        let n = until_sftp_ends(ended, file.read(&mut buf[filled..]))
            .await
            .map_err(|e| {
                classify_russh_err(e, |s| {
                    ProviderError::TransferFailed(format!("Range read error: {}", s))
                })
            })?;
        sftp_strict_short_read_check(n, filled, want, abs_off)?;
        filled += n;
    }
    Ok(buf)
}

/// PD-PIPE-2: pipeline the PD-SFTP-2 range worker's read of
/// `[start, start + expected)` on the worker's **own single** SFTP session.
///
/// Compounds with PD-SFTP-2: that slice gives N independent connections
/// (one per range window); this gives K pipelined reads per connection over
/// distinct `File` handles on **that connection's one `SftpSession`**
/// (russh-sftp multiplexes concurrent requests by id over one channel; the
/// internal `Arc<RawSftpSession>` is shared by every `File`). **No new TCP
/// connection, no new pool, no second channel** (the same vehicle PD-PIPE-1
/// proved, applied inside the worker). Byte-identical to the serial worker
/// loop: the same bytes written at the same absolute offsets, the same
/// strict short-read hard error and the same per-batch cancellation; only
/// the read scheduling differs.
#[allow(clippy::too_many_arguments)]
async fn sftp_pipelined_range_into(
    sftp: &SftpChannel,
    full_path: &str,
    start: u64,
    expected: u64,
    out: &mut tokio::fs::File,
    chunk: usize,
    window: usize,
    aggregate: &Arc<AtomicU64>,
    cancel: &CancellationToken,
) -> Result<(), ProviderError> {
    use tokio::io::{AsyncSeekExt, AsyncWriteExt};

    let chunk = chunk.max(4096) as u64;
    let window = window.clamp(2, SFTP_PIPELINE_MAX_WINDOW);
    let chunks_needed = expected.div_ceil(chunk).max(1) as usize;
    let eff_window = window.min(chunks_needed);

    // `eff_window` handles, all on the worker's ONE existing session: one
    // SSH channel, the RawSftpSession multiplexes the concurrent reads by id.
    let mut handles: Vec<russh_sftp::client::fs::File> = Vec::with_capacity(eff_window);
    for _ in 0..eff_window {
        match until_sftp_ends(&sftp.ended, sftp.open(full_path)).await {
            Ok(f) => handles.push(f),
            Err(e) => {
                close_sftp_files(handles, &sftp.ended).await;
                return Err(classify_russh_err(e, |s| {
                    ProviderError::TransferFailed(format!(
                        "Failed to open remote file for range (pipeline): {}",
                        s
                    ))
                }));
            }
        }
    }

    let streamed: Result<(), ProviderError> = async {
        let stop = start + expected;
        let mut offset: u64 = start;
        while offset < stop {
            // Plan up to `eff_window` consecutive strict sub-stripes.
            let batch_base = offset;
            let mut wants: Vec<usize> = Vec::with_capacity(eff_window);
            for _ in 0..eff_window {
                if offset >= stop {
                    break;
                }
                let want = std::cmp::min(chunk, stop - offset) as usize;
                wants.push(want);
                offset += want as u64;
            }
            if wants.is_empty() {
                break;
            }

            // Issue the batch concurrently: each future borrows one distinct
            // handle (disjoint &mut via split_at_mut), so `window` strict reads
            // are in flight on the single connection at once.
            let n = wants.len();
            let (used, _rest) = handles.split_at_mut(n);
            let mut futs = Vec::with_capacity(n);
            let mut abs = batch_base;
            for (f, &want) in used.iter_mut().zip(wants.iter()) {
                futs.push(sftp_pipelined_range_read_strict(f, abs, want, &sftp.ended));
                abs += want as u64;
            }

            // Cancellation stays responsive per batch and returns the EXACT
            // serial-worker "Transfer cancelled by user" error.
            let results = tokio::select! {
                _ = cancel.cancelled() => {
                    return Err(ProviderError::TransferFailed(
                        SFTP_TRANSFER_CANCELLED.to_string(),
                    ));
                }
                r = futures_util::future::try_join_all(futs) => r?,
            };

            // Write each strict sub-stripe in strict offset order at its
            // absolute file offset; aggregate per chunk (the shared progress
            // counter, same as the serial worker).
            let mut abs = batch_base;
            for buf in results.iter() {
                out.seek(std::io::SeekFrom::Start(abs))
                    .await
                    .map_err(ProviderError::IoError)?;
                out.write_all(buf).await.map_err(ProviderError::IoError)?;
                aggregate.fetch_add(buf.len() as u64, Ordering::Relaxed);
                abs += buf.len() as u64;
            }
        }

        Ok(())
    }
    .await;
    close_sftp_files(handles, &sftp.ended).await;
    streamed
}

/// PD-SFTP-2 per-range worker: dial an **independent** SSH+SFTP connection
/// from `spec`, seek to `start`, and stream **exactly** `end - start + 1`
/// bytes into `temp_path` at absolute offset `start`. One call == one fresh
/// SSH connection (host-key pinned re-dial via `ensure_connected`), so N
/// ranges of one file = N independent connections, the same model as the
/// PD-SFTP-1 file-level pool and rclone's pooled SFTP.
///
/// Strict gate: a `read() == 0` before `expected` bytes is a hard
/// [`ProviderError`], never a silent short read. Writes are clamped to the
/// window so a remote file that grew mid-transfer cannot corrupt the
/// neighbouring range.
#[allow(clippy::too_many_arguments)]
async fn sftp_download_one_range(
    spec: SftpConnectionSpec,
    remote_path: String,
    current_dir: String,
    home_dir: String,
    buffer_size: usize,
    limit_bps: u64,
    compression_enabled: bool,
    parallel_connections: usize,
    requested_readahead_window: Option<usize>,
    readahead_grow_to: Option<usize>,
    start: u64,
    end: u64,
    temp_path: PathBuf,
    aggregate: Arc<AtomicU64>,
    cancel: CancellationToken,
) -> Result<ConcurrentRangeOutcome, ProviderError> {
    use tokio::io::{AsyncSeekExt, AsyncWriteExt};

    let expected = end - start + 1;

    // Independent worker: own socket + own auth, host-key pinned re-dial.
    let mut worker = SftpProvider::new(spec.to_config());
    worker.current_dir = current_dir;
    worker.home_dir = home_dir;
    worker.buffer_size = buffer_size;
    worker.compression_enabled = compression_enabled;
    worker.connection_spec = Some(spec);
    worker.ensure_connected().await?;

    let full_path = worker.normalize_path(&remote_path);
    let sftp = worker.get_sftp()?;
    let global_bw = crate::transfer_dag::governor::global();

    // READ-AHEAD (our own issue #70 workaround) takes precedence over PD-PIPE-2
    // when provider state requests it: keep `window` `SSH_FXP_READ` in flight on
    // THIS range worker's connection with no per-batch barrier. Composes with
    // PD-SFTP-2's N independent connections (N x this read-ahead). Same guardrail
    // as PD-PIPE-2: no active bandwidth cap (the serial loop owns throttling).
    if limit_bps == 0
        && crate::transfer_dag::throttle::is_unlimited(
            crate::transfer_dag::governor::TransferDirection::Download,
        )
    {
        if let Some(window) = requested_readahead_window.and_then(|requested| {
            effective_sftp_readahead_window(requested, buffer_size, expected, parallel_connections)
        }) {
            let mut out = tokio::fs::OpenOptions::new()
                .write(true)
                .open(&temp_path)
                .await
                .map_err(ProviderError::IoError)?;
            sftp_readahead_range_into(
                sftp,
                &full_path,
                start,
                expected,
                &mut out,
                buffer_size,
                window,
                readahead_grow_to,
                &aggregate,
                &cancel,
                expected,
                None,
                Arc::clone(&worker.fail_readahead_write),
            )
            .await?;
            out.flush().await.map_err(ProviderError::IoError)?;
            out.sync_all().await.map_err(ProviderError::IoError)?;
            let _ = worker.disconnect().await;
            return Ok(ConcurrentRangeOutcome::Completed);
        }
    }

    // PD-PIPE-2: when the opt-in flag yields a window and no bandwidth cap
    // is active, pipeline this range worker's read of
    // `[start, start + expected)` on its OWN single SFTP session. Compounds
    // with PD-SFTP-2's N independent connections; no new connection / pool /
    // channel. Default (flag unset) or an active cap falls through to the
    // exact serial loop below = diff-0 (the serial loop owns the precise
    // throttle, as in PD-PIPE-1).
    if limit_bps == 0
        && crate::transfer_dag::throttle::is_unlimited(
            crate::transfer_dag::governor::TransferDirection::Download,
        )
    {
        if let Some(window) = sftp_read_pipeline_window() {
            let mut out = tokio::fs::OpenOptions::new()
                .write(true)
                .open(&temp_path)
                .await
                .map_err(ProviderError::IoError)?;
            sftp_pipelined_range_into(
                sftp,
                &full_path,
                start,
                expected,
                &mut out,
                buffer_size,
                window,
                &aggregate,
                &cancel,
            )
            .await?;
            out.flush().await.map_err(ProviderError::IoError)?;
            out.sync_all().await.map_err(ProviderError::IoError)?;
            let _ = worker.disconnect().await;
            return Ok(ConcurrentRangeOutcome::Completed);
        }
    }

    let mut remote_file = until_sftp_ends(&sftp.ended, sftp.open(&full_path))
        .await
        .map_err(|e| {
            classify_russh_err(e, |s| {
                ProviderError::TransferFailed(format!(
                    "Failed to open remote file for range: {}",
                    s
                ))
            })
        })?;
    let streamed: Result<(), ProviderError> = async {
        remote_file
            .seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(|e| {
                classify_russh_err(e, |s| {
                    ProviderError::ServerError(format!("Failed to seek to range start: {}", s))
                })
            })?;

        let mut out = tokio::fs::OpenOptions::new()
            .write(true)
            .open(&temp_path)
            .await
            .map_err(ProviderError::IoError)?;
        out.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(ProviderError::IoError)?;

        let mut buf = vec![0u8; buffer_size];
        let mut written: u64 = 0;
        let started = std::time::Instant::now();
        while written < expected {
            let allowance = (expected - written).min(buf.len() as u64);
            tokio::select! {
                _ = cancel.cancelled() => {
                    return Err(ProviderError::TransferFailed(
                        SFTP_TRANSFER_CANCELLED.to_string(),
                    ));
                }
                _ = global_bw.charge(crate::transfer_dag::governor::TransferDirection::Download, allowance) => {}
            }
            tokio::select! {
                _ = cancel.cancelled() => {
                    return Err(ProviderError::TransferFailed(
                        SFTP_TRANSFER_CANCELLED.to_string(),
                    ));
                }
                read = until_sftp_ends(&sftp.ended, remote_file.read(&mut buf)) => {
                    let n = read.map_err(|e| {
                        classify_russh_err(e, |s| {
                            ProviderError::TransferFailed(format!("Range read error: {}", s))
                        })
                    })?;
                    if n == 0 {
                        return Err(ProviderError::TransferFailed(format!(
                            "SFTP range short read: expected {} bytes at offset {}, got {}",
                            expected, start, written
                        )));
                    }
                    let take = std::cmp::min(n as u64, expected - written) as usize;
                    out.write_all(&buf[..take])
                        .await
                        .map_err(ProviderError::IoError)?;
                    aggregate.fetch_add(take as u64, Ordering::Relaxed);
                    written += take as u64;

                    if limit_bps > 0 {
                        let expected_elapsed = std::time::Duration::from_secs_f64(
                            written as f64 / limit_bps as f64,
                        );
                        let elapsed = started.elapsed();
                        if expected_elapsed > elapsed {
                            tokio::time::sleep(expected_elapsed - elapsed).await;
                        }
                    }
                }
            }
        }

        out.flush().await.map_err(ProviderError::IoError)?;
        out.sync_all().await.map_err(ProviderError::IoError)?;
        Ok(())
    }
    .await;
    close_sftp_file(remote_file, &sftp.ended).await;
    streamed?;
    let _ = worker.disconnect().await;
    Ok(ConcurrentRangeOutcome::Completed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Characterisation: what makes the `find` walk safe today is not the
    /// contract, it is the shape of the data.
    ///
    /// `find` reads its entries from `read_dir`, whose attributes carry the
    /// entry's OWN mode (lstat semantics; the `list` path says so at the point
    /// where it spends an LSTAT only when the server omits the type bits). A
    /// symlink therefore arrives with `S_IFLNK`, `metadata_to_entry` derives
    /// `is_dir` from `S_IFDIR`, and the answer is false: the walk cannot
    /// descend into a link, and `is_walkable_dir()` there is exactly `is_dir`.
    ///
    /// This test passes today and proves nothing new. It exists for the day
    /// somebody makes `find` resolve the target the way `list` does: at that
    /// moment `is_dir` becomes true for a symlink to a directory, the guard
    /// stops being cosmetic and starts carrying weight, and this assertion
    /// goes red instead of leaving the change to be discovered by a user whose
    /// recursive search followed a link to an ancestor.
    #[test]
    fn a_symlink_from_readdir_is_not_a_directory() {
        use russh_sftp::protocol::FileAttributes;

        const S_IFLNK: u32 = 0o120000;
        const S_IFDIR: u32 = 0o040000;

        let link = FileAttributes {
            permissions: Some(S_IFLNK | 0o777),
            ..Default::default()
        };
        let entry = SftpProvider::metadata_to_entry("link".into(), "/link".into(), &link);
        assert!(
            !entry.is_dir,
            "readdir reports a symlink's own mode, so is_dir must be false"
        );
        assert!(
            !entry.is_walkable_dir(),
            "and the contract refuses the descent either way"
        );

        let dir = FileAttributes {
            permissions: Some(S_IFDIR | 0o755),
            ..Default::default()
        };
        let real = SftpProvider::metadata_to_entry("d".into(), "/d".into(), &dir);
        assert!(real.is_dir);
        assert!(
            real.is_walkable_dir(),
            "a real directory is still walked into"
        );
    }

    #[test]
    fn pd_pipe1_flag_is_off_by_default_and_capped() {
        // diff-0 default: unset / falsey / the degenerate "1" window all
        // mean "serial loop", i.e. None.
        assert_eq!(parse_sftp_read_pipeline_window(None), None);
        for off in ["", " ", "0", "1", "false", "off", "no", "FALSE", "Off"] {
            assert_eq!(parse_sftp_read_pipeline_window(Some(off)), None, "{off:?}");
        }
        // Truthy words enable the default window.
        for on in ["true", "on", "yes", "TRUE", " On "] {
            assert_eq!(
                parse_sftp_read_pipeline_window(Some(on)),
                Some(SFTP_PIPELINE_DEFAULT_WINDOW),
                "{on:?}"
            );
        }
        // Explicit numeric window, only >= 2 enables; capped at the max.
        assert_eq!(parse_sftp_read_pipeline_window(Some("2")), Some(2));
        assert_eq!(parse_sftp_read_pipeline_window(Some("16")), Some(16));
        assert_eq!(
            parse_sftp_read_pipeline_window(Some("9999")),
            Some(SFTP_PIPELINE_MAX_WINDOW)
        );
        // Junk is safe-off.
        for junk in ["abc", "-3", "2.5", "0x10"] {
            assert_eq!(
                parse_sftp_read_pipeline_window(Some(junk)),
                None,
                "{junk:?}"
            );
        }
    }

    #[test]
    fn readahead_flag_off_by_default_measured_default_and_generous_request_cap() {
        // Off by default and for the degenerate "1", mirroring the pipeline flag.
        assert_eq!(parse_sftp_readahead_window(None), None);
        for off in ["", " ", "0", "1", "false", "off", "no", "FALSE", "Off"] {
            assert_eq!(parse_sftp_readahead_window(Some(off)), None, "{off:?}");
        }
        // Truthy words enable the measured default window.
        for on in ["true", "on", "yes", "TRUE", " On "] {
            assert_eq!(
                parse_sftp_readahead_window(Some(on)),
                Some(SFTP_READAHEAD_DEFAULT_WINDOW),
                "{on:?}"
            );
        }
        // Explicit numeric window, only >= 2 enables; capped only by the
        // generous anti-footgun ceiling (no tight cap).
        assert_eq!(parse_sftp_readahead_window(Some("2")), Some(2));
        assert_eq!(parse_sftp_readahead_window(Some("64")), Some(64));
        assert_eq!(parse_sftp_readahead_window(Some("256")), Some(256));
        assert_eq!(
            parse_sftp_readahead_window(Some("100000")),
            Some(SFTP_READAHEAD_MAX_WINDOW)
        );
        for junk in ["abc", "-3", "2.5", "0x10"] {
            assert_eq!(parse_sftp_readahead_window(Some(junk)), None, "{junk:?}");
        }
    }

    #[test]
    fn readahead_provider_state_distinguishes_legacy_disabled_and_window() {
        let legacy = SftpReadaheadSetting::LegacyEnvironment;
        // Unset: the measured default window, not a serial read per round trip.
        assert_eq!(
            legacy.requested_window_from(None),
            Some(SFTP_READAHEAD_DEFAULT_WINDOW)
        );
        assert_eq!(legacy.requested_window_from(Some("on")), Some(32));
        assert_eq!(legacy.requested_window_from(Some("64")), Some(64));
        // An explicit "off" or "0" in the environment still disables it.
        assert_eq!(legacy.requested_window_from(Some("off")), None);
        assert_eq!(legacy.requested_window_from(Some("0")), None);

        let disabled = SftpReadaheadSetting::from_explicit(None);
        assert_eq!(disabled, SftpReadaheadSetting::Disabled);
        assert_eq!(disabled.requested_window_from(Some("64")), None);

        let explicit = SftpReadaheadSetting::from_explicit(Some(16));
        assert_eq!(explicit, SftpReadaheadSetting::Window(16));
        assert_eq!(explicit.requested_window_from(Some("off")), Some(16));
        assert_eq!(
            SftpReadaheadSetting::from_explicit(Some(100_000)),
            SftpReadaheadSetting::Window(SFTP_READAHEAD_MAX_WINDOW)
        );
        assert_eq!(
            SftpReadaheadSetting::from_explicit(Some(1)),
            SftpReadaheadSetting::Disabled
        );
    }

    #[test]
    fn the_default_readahead_window_is_shared_by_the_connections_of_one_download() {
        let legacy = SftpReadaheadSetting::LegacyEnvironment;
        let kib = 1024;
        // One connection keeps the measured default.
        assert_eq!(
            legacy.requested_window_per_connection_from(None, 1, 256 * kib),
            Some(SFTP_READAHEAD_DEFAULT_WINDOW)
        );
        // The CLI default splits a large file over four connections: each one
        // took the whole window, 128 READs of 256 KiB in flight for one file.
        assert_eq!(
            legacy.requested_window_per_connection_from(None, 4, 256 * kib),
            Some(16),
            "each of four connections must keep a quarter of the shared budget, not a whole window"
        );
        // The budget is bytes: a smaller chunk gets more READs, the same depth.
        assert_eq!(
            legacy.requested_window_per_connection_from(None, 4, 32 * kib),
            Some(128)
        );
        for chunk in [32 * kib, 64 * kib, 256 * kib, 1024 * kib] {
            for connections in 1..=SFTP_MULTI_THREAD_MAX_STREAMS {
                let each = legacy
                    .requested_window_per_connection_from(None, connections, chunk)
                    .unwrap();
                assert!(
                    each >= 2,
                    "{connections} connections of {chunk}-byte chunks: {each} READs is no read-ahead"
                );
                let bytes = (each * chunk * connections) as u64;
                let budget = SFTP_READAHEAD_SHARED_BUDGET
                    .max(SFTP_READAHEAD_MIN_SHARED_BYTES * connections as u64)
                    .max((2 * chunk * connections) as u64);
                assert!(
                    bytes <= budget,
                    "{connections} connections of {chunk}-byte chunks keep {bytes} bytes in flight for one download"
                );
            }
        }
        // Only the shared default may grow back on a server with a small read
        // length; whatever was asked stays as asked.
        assert!(legacy.uses_shared_default_from(None));
        assert!(!legacy.uses_shared_default_from(Some("64")));
        assert!(!SftpReadaheadSetting::from_explicit(Some(16)).uses_shared_default_from(None));
        assert!(!SftpReadaheadSetting::Disabled.uses_shared_default_from(None));
        // Whatever was asked stays a per-connection value.
        let explicit = SftpReadaheadSetting::from_explicit(Some(16));
        assert_eq!(
            explicit.requested_window_per_connection_from(None, 8, 256 * kib),
            Some(16)
        );
        assert_eq!(
            SftpReadaheadSetting::Disabled.requested_window_per_connection_from(None, 4, 256 * kib),
            None
        );
        assert_eq!(
            legacy.requested_window_per_connection_from(Some("64"), 4, 256 * kib),
            Some(64)
        );
        assert_eq!(
            legacy.requested_window_per_connection_from(Some("off"), 4, 256 * kib),
            None
        );
    }

    #[test]
    fn readahead_effective_window_preserves_tuned_case_and_caps_job_resources() {
        let gib = 1024 * 1024 * 1024;
        let requested = SftpReadaheadSetting::from_explicit(Some(16))
            .requested_window_from(Some("off"))
            .unwrap();
        assert_eq!(
            effective_sftp_readahead_window(requested, 256 * 1024, gib, 12),
            Some(16)
        );
        assert_eq!(
            effective_sftp_readahead_window(1024, 256 * 1024, gib, 16),
            Some(15)
        );
        assert_eq!(
            effective_sftp_readahead_window(1024, 16 * 1024 * 1024, gib, 16),
            None
        );
        assert_eq!(effective_sftp_readahead_window(16, 256 * 1024, 1, 1), None);
    }

    #[test]
    fn readahead_stale_sidecar_falls_back_without_deleting_it() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("file.bin");
        let temp = super::super::atomic_write::readahead_temp_path_for(&final_path);
        std::fs::write(&temp, b"possibly active").unwrap();

        assert!(!sftp_readahead_local_path_is_eligible(
            final_path.to_string_lossy().as_ref()
        ));
        assert_eq!(std::fs::read(&temp).unwrap(), b"possibly active");

        std::fs::remove_file(&temp).unwrap();
        assert!(sftp_readahead_local_path_is_eligible(
            final_path.to_string_lossy().as_ref()
        ));
    }

    #[tokio::test]
    async fn readahead_temp_creates_parent_and_cleans_up_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("missing").join("file.bin");
        let final_str = final_path.to_string_lossy().into_owned();
        let (file, guard) = create_sftp_readahead_temp(&final_str, 4096).await.unwrap();
        let temp = super::super::atomic_write::readahead_temp_path_for(&final_path);
        assert_eq!(tokio::fs::metadata(&temp).await.unwrap().len(), 4096);
        drop(file);
        drop(guard);
        assert!(!temp.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn readahead_temp_refuses_preplanted_symlink_without_clobbering_target() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("file.bin");
        let temp = super::super::atomic_write::readahead_temp_path_for(&final_path);
        let victim = dir.path().join("victim.txt");
        std::fs::write(&victim, b"must survive").unwrap();
        symlink(&victim, &temp).unwrap();

        let err = create_sftp_readahead_temp(&final_path.to_string_lossy(), 4096)
            .await
            .expect_err("create_new must reject the symlink");
        assert!(err.to_string().contains("exclusive local read-ahead temp"));
        assert_eq!(std::fs::read(&victim).unwrap(), b"must survive");
    }

    #[test]
    fn pd_pipe2_strict_short_read_before_length_is_hard_error() {
        // The strict-short-read-before-length hard-error path of the
        // PD-PIPE-2 assembler (deterministic, no live server). A protocol
        // `read() == 0` with fewer than `want` bytes filled is the exact
        // serial PD-SFTP-2 worker error: never a silent short read.
        let err = sftp_strict_short_read_check(0, 100, 4096, 1_048_576)
            .expect_err("zero read before length must be a hard error");
        match err {
            ProviderError::TransferFailed(m) => {
                assert!(m.contains("short read"), "message shape: {m}");
                assert!(m.contains("4096"), "want in message: {m}");
                assert!(m.contains("1048576"), "abs offset in message: {m}");
                assert!(m.contains("100"), "filled-so-far in message: {m}");
            }
            other => panic!("expected TransferFailed, got {other:?}"),
        }
        // A non-zero read is always progress (loop continues).
        assert!(sftp_strict_short_read_check(1, 0, 4096, 0).is_ok());
        assert!(sftp_strict_short_read_check(4096, 0, 4096, 0).is_ok());
        // Zero read exactly AT the requested length is not an error (the
        // while-loop would already have stopped: filled == want).
        assert!(sftp_strict_short_read_check(0, 4096, 4096, 0).is_ok());
        // Zero read past the length (defensive) is likewise not an error.
        assert!(sftp_strict_short_read_check(0, 5000, 4096, 0).is_ok());
    }

    #[test]
    fn shell_single_quote_neutralises_injection() {
        // Plain path: just wrapped.
        assert_eq!(shell_single_quote("/srv/file.txt"), "'/srv/file.txt'");
        // Spaces stay literal.
        assert_eq!(shell_single_quote("/a b/c"), "'/a b/c'");
        // Embedded single quote: close, escaped quote, reopen.
        assert_eq!(shell_single_quote("a'b"), "'a'\\''b'");
        // Command substitution / backticks are inert inside single quotes.
        assert_eq!(shell_single_quote("$(rm -rf /)"), "'$(rm -rf /)'");
        assert_eq!(shell_single_quote("`id`"), "'`id`'");
        // Separators and logical operators cannot break out.
        assert_eq!(shell_single_quote("x; rm -rf /"), "'x; rm -rf /'");
        assert_eq!(shell_single_quote("a && b"), "'a && b'");
        // Newline injection stays inside the quotes.
        assert_eq!(shell_single_quote("a\nrm -rf /"), "'a\nrm -rf /'");
        // The classic break-out attempt: '; rm -rf / ; echo '
        let evil = "'; rm -rf / ; echo '";
        let q = shell_single_quote(evil);
        assert!(q.starts_with('\'') && q.ends_with('\''));
        // Every original `'` became the 4-char `'\''` sequence; there is no
        // bare unescaped quote that could terminate the literal early.
        assert_eq!(q, "''\\''; rm -rf / ; echo '\\'''");
    }

    #[test]
    fn sftp_scans_in_parallel_only_once_a_connection_spec_exists() {
        let config = SftpConfig {
            host: "example.com".to_string(),
            port: 22,
            username: "testuser".to_string(),
            password: Some(secrecy::SecretString::from("testpass".to_string())),
            private_key_path: None,
            key_passphrase: None,
            initial_path: None,
            timeout_secs: 30,
            trust_unknown_hosts: false,
        };
        let mut provider = SftpProvider::new(config);
        // Never connected: nothing to re-dial from, the scan stays locked.
        assert_eq!(
            provider.list_executor_kind(),
            crate::providers::ProviderListExecutorKind::LockedSingle
        );
        assert!(provider.clone_for_list().is_err());
        // With the spec a connect captures, the scanner may fan out on N
        // independent connections, each an unconnected clone that dials
        // lazily and opts into warm reuse across directories.
        provider.connection_spec = Some(SftpConnectionSpec {
            host: "example.com".to_string(),
            port: 22,
            username: "testuser".to_string(),
            password: Some(secrecy::SecretString::from("testpass".to_string())),
            private_key_path: None,
            key_passphrase: None,
            initial_path: None,
            timeout_secs: 30,
            pinned_host_key_sha256: None,
        });
        assert_eq!(
            provider.list_executor_kind(),
            crate::providers::ProviderListExecutorKind::HttpClonePool
        );
        assert_eq!(
            provider.list_executor_max_sessions(),
            provider.transfer_executor_max_sessions()
        );
        let worker = provider.clone_for_list().expect("clone from spec");
        assert!(!worker.is_connected(), "a scan worker dials on first use");
        assert!(worker.supports_transfer_worker_reuse());
    }

    #[test]
    fn sftp_pool_ceiling_matches_the_intra_file_stream_range() {
        // --parallel is documented up to 32 and clamped per provider; the SFTP
        // ceiling follows the same 16 as --sftp-concurrency, so a request of
        // 16 is honoured and a request of 4 still yields 4 connections.
        let config = SftpConfig {
            host: "example.com".to_string(),
            port: 22,
            username: "testuser".to_string(),
            password: Some(secrecy::SecretString::from("testpass".to_string())),
            private_key_path: None,
            key_passphrase: None,
            initial_path: None,
            timeout_secs: 30,
            trust_unknown_hosts: false,
        };
        let provider = SftpProvider::new(config);
        assert_eq!(provider.transfer_executor_max_sessions(), 16);
        assert_eq!(
            provider.transfer_executor_max_sessions() as usize,
            SFTP_MULTI_THREAD_MAX_STREAMS
        );
    }

    #[test]
    fn sftp_multi_thread_cutoff_floor_matches_setter() {
        // R21: the trait floor is the same 1 MiB bound the setter applies,
        // so the batch executor mirrors the single-file path.
        let config = SftpConfig {
            host: "example.com".to_string(),
            port: 22,
            username: "testuser".to_string(),
            password: Some(secrecy::SecretString::from("testpass".to_string())),
            private_key_path: None,
            key_passphrase: None,
            initial_path: None,
            timeout_secs: 30,
            trust_unknown_hosts: false,
        };
        let mut provider = SftpProvider::new(config);
        assert_eq!(
            provider.multi_thread_cutoff_floor(),
            SFTP_MULTI_THREAD_CUTOFF_FLOOR
        );
        provider.set_multi_thread_download(4, 0);
        assert_eq!(provider.multi_thread_cutoff, 1024 * 1024);
        assert_eq!(
            provider.multi_thread_cutoff,
            provider.multi_thread_cutoff_floor()
        );
    }

    #[test]
    fn sftp_workers_opt_into_warm_reuse_like_ftp() {
        // PD-FTP-2 pool semantics: a worker is recycled only when the provider
        // opts in. SFTP now does, so a batch pays one SSH handshake per lease,
        // not one per file. Pinned here so a future "honest default false"
        // cannot come back without a measured reason.
        let config = SftpConfig {
            host: "example.com".to_string(),
            port: 22,
            username: "testuser".to_string(),
            password: Some(secrecy::SecretString::from("testpass".to_string())),
            private_key_path: None,
            key_passphrase: None,
            initial_path: None,
            timeout_secs: 30,
            trust_unknown_hosts: false,
        };
        let provider = SftpProvider::new(config);
        assert!(provider.supports_transfer_worker_reuse());
    }

    #[test]
    fn test_sftp_provider_creation() {
        let config = SftpConfig {
            host: "example.com".to_string(),
            port: 22,
            username: "testuser".to_string(),
            password: Some(secrecy::SecretString::from("testpass".to_string())),
            private_key_path: None,
            key_passphrase: None,
            initial_path: None,
            timeout_secs: 30,
            trust_unknown_hosts: false,
        };

        let provider = SftpProvider::new(config);
        assert_eq!(provider.provider_type(), ProviderType::Sftp);
        assert!(!provider.is_connected());
    }

    #[test]
    fn delta_rsync_config_accepts_password_only_profile() {
        use crate::rsync_over_ssh::AuthMethod;
        use secrecy::ExposeSecret;

        let config = SftpConfig {
            host: "nas.local".to_string(),
            port: 2222,
            username: "alice".to_string(),
            password: Some(secrecy::SecretString::from("secret".to_string())),
            private_key_path: None,
            key_passphrase: None,
            initial_path: None,
            timeout_secs: 30,
            trust_unknown_hosts: false,
        };

        let provider = SftpProvider::new(config);
        let cfg = provider
            .rsync_config_for_delta(Some(std::path::PathBuf::from("/tmp/known_hosts")))
            .expect("password-only profile should be delta-config eligible");

        assert_eq!(cfg.auth_method, AuthMethod::Password);
        assert!(cfg.ssh_key_path.is_none());
        assert_eq!(cfg.ssh_host, "nas.local");
        assert_eq!(cfg.ssh_port, Some(2222));
        assert_eq!(cfg.ssh_password.as_ref().unwrap().expose_secret(), "secret");
        assert!(cfg.validate_auth_material().is_ok());
    }

    #[test]
    fn delta_rsync_config_rejects_empty_password_without_key() {
        let config = SftpConfig {
            host: "nas.local".to_string(),
            port: 22,
            username: "alice".to_string(),
            password: Some(secrecy::SecretString::from(String::new())),
            private_key_path: None,
            key_passphrase: None,
            initial_path: None,
            timeout_secs: 30,
            trust_unknown_hosts: false,
        };

        let provider = SftpProvider::new(config);
        assert!(provider.rsync_config_for_delta(None).is_none());
    }

    #[cfg(feature = "aerorsync")]
    #[test]
    fn aerorsync_native_metadata_policy_is_linux_acl_unix_xattr_and_soft_loss() {
        let transport = crate::aerorsync::delta_transport_impl::AerorsyncDeltaTransport::new(
            crate::aerorsync::russh_session_transport::test_dummy_config(),
            1,
        );
        let configured = configure_aerorsync_metadata(transport);

        #[cfg(target_os = "linux")]
        assert_eq!(configured.metadata_policy(), (true, true, false));
        #[cfg(all(unix, not(target_os = "linux")))]
        assert_eq!(configured.metadata_policy(), (true, false, false));
        #[cfg(not(unix))]
        assert_eq!(configured.metadata_policy(), (false, false, false));
    }

    #[test]
    fn test_normalize_path() {
        let config = SftpConfig {
            host: "example.com".to_string(),
            port: 22,
            username: "testuser".to_string(),
            password: None,
            private_key_path: None,
            key_passphrase: None,
            initial_path: None,
            timeout_secs: 30,
            trust_unknown_hosts: false,
        };

        let mut provider = SftpProvider::new(config);
        provider.current_dir = "/home/user".to_string();
        provider.home_dir = "/home/user".to_string();

        assert_eq!(provider.normalize_path("/absolute"), "/absolute");
        assert_eq!(provider.normalize_path("relative"), "/home/user/relative");
        assert_eq!(provider.normalize_path(".."), "/home");
        assert_eq!(provider.normalize_path("."), "/home/user");
        assert_eq!(provider.normalize_path("~"), "/home/user");
        assert_eq!(
            provider.normalize_path("~/documents"),
            "/home/user/documents"
        );
    }

    #[test]
    fn test_format_permissions() {
        assert_eq!(format_permissions(0o755, true), "drwxr-xr-x");
        assert_eq!(format_permissions(0o644, false), "-rw-r--r--");
        assert_eq!(format_permissions(0o777, true), "drwxrwxrwx");
        assert_eq!(format_permissions(0o600, false), "-rw-------");
    }

    #[test]
    fn test_symlink_bit_reads_the_readdir_mode() {
        // A mode word carrying file-type bits answers on its own, which is
        // what lets `list` skip one SSH_FXP_LSTAT per directory entry.
        assert_eq!(symlink_bit(0o120777), Some(true)); // symlink
        assert_eq!(symlink_bit(0o100644), Some(false)); // regular file
        assert_eq!(symlink_bit(0o040755), Some(false)); // directory
        assert_eq!(symlink_bit(0o140755), Some(false)); // socket
        assert_eq!(symlink_bit(0o060660), Some(false)); // block device
    }

    #[test]
    fn test_symlink_bit_is_unknown_without_file_type_bits() {
        // Firmware that sends permission bits only must not be read as
        // "not a symlink": the caller has to fall back to an LSTAT probe,
        // otherwise a symlink-to-directory is walked into (GAP-A02).
        assert_eq!(symlink_bit(0o755), None);
        assert_eq!(symlink_bit(0o644), None);
        assert_eq!(symlink_bit(0), None);
    }

    #[test]
    fn test_plan_resume_upload() {
        // Happy path: remote holds a valid prefix, append the tail.
        assert_eq!(
            plan_resume_upload(15, 15, 100),
            ResumeUploadPlan::Append(15)
        );
        // No partial on the remote: full upload from zero.
        assert_eq!(plan_resume_upload(0, 0, 100), ResumeUploadPlan::FullUpload);
        // Caller offset present but remote is empty: clamp to 0 -> full upload
        // (never trust an offset the remote can't back).
        assert_eq!(plan_resume_upload(50, 0, 100), ResumeUploadPlan::FullUpload);
        // Stale caller offset larger than the real remote size: clamp down to
        // the remote size and append from there, never past it.
        assert_eq!(
            plan_resume_upload(90, 40, 100),
            ResumeUploadPlan::Append(40)
        );
        // Remote already has the whole file: nothing to send.
        assert_eq!(
            plan_resume_upload(100, 100, 100),
            ResumeUploadPlan::AlreadyComplete
        );
        // Remote somehow larger than local (stale/other file): treat as complete
        // rather than appending garbage.
        assert_eq!(
            plan_resume_upload(120, 120, 100),
            ResumeUploadPlan::AlreadyComplete
        );
    }

    fn sftp_status(
        code: russh_sftp::protocol::StatusCode,
        message: &str,
    ) -> russh_sftp::client::error::Error {
        russh_sftp::client::error::Error::Status(russh_sftp::protocol::Status {
            id: 1,
            status_code: code,
            error_message: message.to_string(),
            language_tag: "en".into(),
        })
    }

    #[test]
    fn exists_maps_a_present_path_to_true() {
        assert!(map_sftp_try_exists(Ok(true)).unwrap());
    }

    #[test]
    fn exists_maps_a_missing_path_to_false() {
        // russh-sftp::try_exists converts SSH_FX_NO_SUCH_FILE into Ok(false).
        assert!(!map_sftp_try_exists(Ok(false)).unwrap());
        // Defensive: if a future russh-sftp leaves that status as Err, absence
        // must still be Ok(false) so a sync into a directory not yet created
        // keeps scanning it as an empty tree.
        let err = sftp_status(russh_sftp::protocol::StatusCode::NoSuchFile, "No such file");
        assert!(!map_sftp_try_exists(Err(err)).unwrap());
    }

    #[test]
    fn exists_maps_permission_denied_to_err_not_absent() {
        let err = sftp_status(
            russh_sftp::protocol::StatusCode::PermissionDenied,
            "Permission denied",
        );
        match map_sftp_try_exists(Err(err)) {
            Err(ProviderError::PermissionDenied(message)) => {
                assert!(
                    message.to_ascii_lowercase().contains("permission"),
                    "got {message:?}"
                );
            }
            other => panic!("permission denied must stay an error, got {other:?}"),
        }
    }

    #[test]
    fn exists_maps_an_io_failure_to_err_not_absent() {
        let err = russh_sftp::client::error::Error::IO("broken pipe".into());
        assert!(
            map_sftp_try_exists(Err(err)).is_err(),
            "an I/O failure must stay an error, not Ok(false)"
        );
    }

    /// Where [`GoingAwayServer`] stops: from the n-th WRITE or the n-th READ
    /// it receives it answers nothing, and a moment later the transport
    /// between it and the client is cut, as when the server process dies or
    /// restarts mid-transfer. The moment lets the client fill its pipeline:
    /// every request in flight is then waiting at the cut, and none goes out
    /// after it (one would fail at once on the closed session and prove
    /// nothing). `Stall` stops answering at the n-th WRITE and keeps the
    /// transport, as an SFTP server stuck behind an SSH connection that still
    /// answers.
    #[derive(Clone, Copy)]
    enum GoAwayAt {
        Write(usize),
        Read(usize),
        Stall(usize),
        /// Goes away at the n-th STAT, LSTAT or FSTAT.
        Stat(usize),
        /// Goes away at the first OPENDIR.
        OpenDir,
        /// Stays, and answers every WRITE after this long.
        SlowWrites(std::time::Duration),
        /// Stays, and refuses every STAT, LSTAT and FSTAT.
        StatRefused,
        /// Goes away at the first OPEN (a create is an OPEN too).
        Open,
        /// Goes away at the first SETSTAT.
        SetStat,
        /// Goes away at the first REALPATH.
        RealPath,
        /// Stays, and never answers an OPEN.
        OpenUnanswered,
        /// Stays, and never answers a CLOSE.
        CloseUnanswered,
        /// Lists entries without attributes, so that a listing asks for each
        /// one, and goes away at the first of those STATs.
        ListFollowUp,
        /// Goes away at the n-th OPENDIR, after listing a folder `sub` and
        /// a file `b.txt` in every folder it opened before.
        OpenDirAt(usize),
    }

    /// An SFTP server on an in-memory transport that goes away in the middle
    /// of a transfer. It serves `source` to reads and answers stat with its
    /// size, so an upload, a resumed upload and a download all get going.
    struct GoingAwayServer {
        at: GoAwayAt,
        writes: usize,
        reads: usize,
        stats: usize,
        opendirs: usize,
        /// Directory handles whose entries were sent: the next READDIR of
        /// each ends the listing.
        listed: std::collections::HashSet<String>,
        source: Arc<Vec<u8>>,
        cut: Arc<tokio::sync::Notify>,
    }

    impl GoingAwayServer {
        fn ok(id: u32) -> russh_sftp::protocol::Status {
            russh_sftp::protocol::Status {
                id,
                status_code: russh_sftp::protocol::StatusCode::Ok,
                error_message: "Ok".to_string(),
                language_tag: "en-US".to_string(),
            }
        }

        fn attrs(&self, id: u32) -> russh_sftp::protocol::Attrs {
            russh_sftp::protocol::Attrs {
                id,
                attrs: russh_sftp::protocol::FileAttributes {
                    size: Some(self.source.len() as u64),
                    permissions: Some(0o100644),
                    ..Default::default()
                },
            }
        }

        /// The answer to a STAT, LSTAT or FSTAT.
        async fn stat_reply(
            &mut self,
            id: u32,
        ) -> Result<russh_sftp::protocol::Attrs, russh_sftp::protocol::StatusCode> {
            self.stats += 1;
            if matches!(self.at, GoAwayAt::Stat(n) if n == self.stats)
                || matches!(self.at, GoAwayAt::ListFollowUp)
            {
                self.go_away().await;
            }
            if matches!(self.at, GoAwayAt::StatRefused) {
                return Err(russh_sftp::protocol::StatusCode::PermissionDenied);
            }
            Ok(self.attrs(id))
        }

        /// Answer nothing more, and cut the transport a moment later.
        async fn go_away(&self) -> ! {
            let cut = self.cut.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                cut.notify_one();
            });
            std::future::pending::<()>().await;
            unreachable!()
        }
    }

    impl russh_sftp::server::Handler for GoingAwayServer {
        type Error = russh_sftp::protocol::StatusCode;

        fn unimplemented(&self) -> Self::Error {
            russh_sftp::protocol::StatusCode::OpUnsupported
        }

        async fn open(
            &mut self,
            id: u32,
            filename: String,
            _pflags: russh_sftp::protocol::OpenFlags,
            _attrs: russh_sftp::protocol::FileAttributes,
        ) -> Result<russh_sftp::protocol::Handle, Self::Error> {
            if matches!(self.at, GoAwayAt::Open) {
                self.go_away().await;
            }
            if matches!(self.at, GoAwayAt::OpenUnanswered) {
                std::future::pending::<()>().await;
            }
            Ok(russh_sftp::protocol::Handle {
                id,
                handle: filename,
            })
        }

        async fn close(
            &mut self,
            id: u32,
            _handle: String,
        ) -> Result<russh_sftp::protocol::Status, Self::Error> {
            if matches!(self.at, GoAwayAt::CloseUnanswered) {
                std::future::pending::<()>().await;
            }
            Ok(Self::ok(id))
        }

        async fn setstat(
            &mut self,
            id: u32,
            _path: String,
            _attrs: russh_sftp::protocol::FileAttributes,
        ) -> Result<russh_sftp::protocol::Status, Self::Error> {
            if matches!(self.at, GoAwayAt::SetStat) {
                self.go_away().await;
            }
            Ok(Self::ok(id))
        }

        async fn realpath(
            &mut self,
            id: u32,
            path: String,
        ) -> Result<russh_sftp::protocol::Name, Self::Error> {
            if matches!(self.at, GoAwayAt::RealPath) {
                self.go_away().await;
            }
            Ok(russh_sftp::protocol::Name {
                id,
                files: vec![russh_sftp::protocol::File::dummy(path)],
            })
        }

        async fn write(
            &mut self,
            id: u32,
            _handle: String,
            _offset: u64,
            _data: Vec<u8>,
        ) -> Result<russh_sftp::protocol::Status, Self::Error> {
            self.writes += 1;
            if matches!(self.at, GoAwayAt::Write(n) if n == self.writes) {
                self.go_away().await;
            }
            if matches!(self.at, GoAwayAt::Stall(n) if n == self.writes) {
                std::future::pending::<()>().await;
            }
            if let GoAwayAt::SlowWrites(delay) = self.at {
                tokio::time::sleep(delay).await;
            }
            Ok(Self::ok(id))
        }

        async fn read(
            &mut self,
            id: u32,
            _handle: String,
            offset: u64,
            len: u32,
        ) -> Result<russh_sftp::protocol::Data, Self::Error> {
            self.reads += 1;
            if matches!(self.at, GoAwayAt::Read(n) if n == self.reads) {
                self.go_away().await;
            }
            let start = offset as usize;
            if start >= self.source.len() {
                return Err(russh_sftp::protocol::StatusCode::Eof);
            }
            let end = (start + len as usize).min(self.source.len());
            Ok(russh_sftp::protocol::Data {
                id,
                data: self.source[start..end].to_vec(),
            })
        }

        async fn stat(
            &mut self,
            id: u32,
            _path: String,
        ) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
            self.stat_reply(id).await
        }

        async fn lstat(
            &mut self,
            id: u32,
            _path: String,
        ) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
            self.stat_reply(id).await
        }

        async fn fstat(
            &mut self,
            id: u32,
            _handle: String,
        ) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
            self.stat_reply(id).await
        }

        async fn opendir(
            &mut self,
            id: u32,
            path: String,
        ) -> Result<russh_sftp::protocol::Handle, Self::Error> {
            self.opendirs += 1;
            if matches!(self.at, GoAwayAt::OpenDir)
                || matches!(self.at, GoAwayAt::OpenDirAt(n) if n == self.opendirs)
            {
                self.go_away().await;
            }
            Ok(russh_sftp::protocol::Handle { id, handle: path })
        }

        async fn readdir(
            &mut self,
            id: u32,
            handle: String,
        ) -> Result<russh_sftp::protocol::Name, Self::Error> {
            if !self.listed.insert(handle) {
                return Err(russh_sftp::protocol::StatusCode::Eof);
            }
            let with_mode = |mode: Option<u32>| russh_sftp::protocol::FileAttributes {
                permissions: mode,
                ..Default::default()
            };
            let files = if matches!(self.at, GoAwayAt::ListFollowUp) {
                ["a.txt", "b.txt", "c.txt"]
                    .map(|name| russh_sftp::protocol::File::new(name, with_mode(None)))
                    .into()
            } else {
                vec![
                    russh_sftp::protocol::File::new("sub", with_mode(Some(0o40755))),
                    russh_sftp::protocol::File::new("b.txt", with_mode(Some(0o100644))),
                ]
            };
            Ok(russh_sftp::protocol::Name { id, files })
        }
    }

    /// A provider whose SFTP session runs over the transport of a
    /// [`GoingAwayServer`].
    async fn provider_on_a_server_that_goes_away(at: GoAwayAt, source: Vec<u8>) -> SftpProvider {
        let (client, proxy_client_side) = tokio::io::duplex(1 << 20);
        let (proxy_server_side, server) = tokio::io::duplex(1 << 20);
        let cut = Arc::new(tokio::sync::Notify::new());
        russh_sftp::server::run(
            server,
            GoingAwayServer {
                at,
                writes: 0,
                reads: 0,
                stats: 0,
                opendirs: 0,
                listed: Default::default(),
                source: Arc::new(source),
                cut: cut.clone(),
            },
        )
        .await;
        tokio::spawn(async move {
            let (mut a, mut b) = (proxy_client_side, proxy_server_side);
            tokio::select! {
                _ = tokio::io::copy_bidirectional(&mut a, &mut b) => {}
                _ = cut.notified() => {}
            }
            // Both ends drop here: the client reads end of stream.
        });
        let mut provider = provider_without_a_session();
        provider.sftp = Some(SftpChannel::open(client).await.expect("sftp init"));
        provider
    }

    /// A provider for a test to give an SFTP session over an in-memory
    /// transport: it never dials.
    fn provider_without_a_session() -> SftpProvider {
        SftpProvider::new(SftpConfig {
            host: "example.com".to_string(),
            port: 22,
            username: "testuser".to_string(),
            password: None,
            private_key_path: None,
            key_passphrase: None,
            initial_path: None,
            timeout_secs: 30,
            trust_unknown_hosts: false,
        })
    }

    /// Longer than any of these transfers takes against the in-memory server,
    /// so running into it means the transfer hung.
    const GONE_SERVER_BOUND: std::time::Duration = std::time::Duration::from_secs(15);

    fn local_file(dir: &tempfile::TempDir, len: usize) -> String {
        let path = dir.path().join("local.bin");
        std::fs::write(&path, vec![7u8; len]).expect("local file");
        path.to_string_lossy().into_owned()
    }

    /// An upload whose server goes away while writes are in flight must fail,
    /// not wait forever: russh-sftp 2.4 never completes the acknowledgement of
    /// a pipelined WRITE once the transport is gone, and the upload waits on
    /// the oldest one before it sends more (live: `docker restart` of the
    /// server during `put -r`, killed by the timeout).
    #[tokio::test]
    async fn an_upload_fails_when_the_server_goes_away_mid_transfer() {
        let mut provider =
            provider_on_a_server_that_goes_away(GoAwayAt::Write(3), Vec::new()).await;
        let dir = tempfile::tempdir().expect("tempdir");
        let local = local_file(&dir, 8 * 1024 * 1024);
        let outcome =
            tokio::time::timeout(GONE_SERVER_BOUND, provider.upload(&local, "/big.bin", None))
                .await
                .expect("the upload hung after the server went away");
        let err = outcome.expect_err("an upload cut in the middle cannot succeed");
        assert!(err.is_connection_lost(), "{err:?}");
        assert!(!provider.is_connected(), "the session is gone, and says so");
    }

    /// The same when every WRITE of the file is already sent and the upload
    /// is waiting for them in the close that ends it.
    #[tokio::test]
    async fn an_upload_fails_when_the_server_goes_away_before_the_close() {
        let mut provider =
            provider_on_a_server_that_goes_away(GoAwayAt::Write(2), Vec::new()).await;
        let dir = tempfile::tempdir().expect("tempdir");
        let local = local_file(&dir, 300 * 1024);
        let outcome = tokio::time::timeout(
            GONE_SERVER_BOUND,
            provider.upload(&local, "/small.bin", None),
        )
        .await
        .expect("the close hung after the server went away");
        let err = outcome.expect_err("an upload cut before its close cannot succeed");
        assert!(err.is_connection_lost(), "{err:?}");
    }

    /// A resumed upload writes through the same pipeline.
    #[tokio::test]
    async fn a_resumed_upload_fails_when_the_server_goes_away_mid_transfer() {
        let mut provider =
            provider_on_a_server_that_goes_away(GoAwayAt::Write(3), vec![7u8; 1024 * 1024]).await;
        let dir = tempfile::tempdir().expect("tempdir");
        let local = local_file(&dir, 8 * 1024 * 1024);
        let outcome = tokio::time::timeout(
            GONE_SERVER_BOUND,
            provider.resume_upload(&local, "/big.bin", 1024 * 1024, None),
        )
        .await
        .expect("the resumed upload hung after the server went away");
        let err = outcome.expect_err("a resumed upload cut in the middle cannot succeed");
        assert!(err.is_connection_lost(), "{err:?}");
    }

    /// Read-ahead readers report their cause, not the cancellation it caused
    /// in the others: a lost connection used to surface as "Transfer
    /// cancelled by user" whenever a cancelled reader came first.
    #[test]
    fn read_ahead_readers_report_the_cause_not_the_cancellations() {
        let cancelled = || {
            Err(ProviderError::TransferFailed(
                "Transfer cancelled by user".to_string(),
            ))
        };
        let lost = || Err(ProviderError::ConnectionLost("session closed".to_string()));
        let picked = first_reader_cause(vec![cancelled(), Ok(()), lost(), cancelled()]);
        assert!(
            matches!(picked, Err(ProviderError::ConnectionLost(_))),
            "{picked:?}"
        );
        let picked = first_reader_cause(vec![cancelled(), cancelled()]);
        assert!(
            picked
                .as_ref()
                .is_err_and(|e| e.to_string().contains("cancelled")),
            "a cancellation alone is still reported: {picked:?}"
        );
        assert!(first_reader_cause(vec![Ok(()), Ok(())]).is_ok());
    }

    /// A server whose SSH connection still answers while its SFTP server
    /// stopped acknowledging writes: the transport never ends, and the upload
    /// gives the session up after `SFTP_WRITE_ACK_BOUND` instead of waiting
    /// forever.
    #[tokio::test(start_paused = true)]
    async fn an_upload_gives_up_on_a_server_that_stops_acknowledging() {
        let mut provider =
            provider_on_a_server_that_goes_away(GoAwayAt::Stall(3), Vec::new()).await;
        let dir = tempfile::tempdir().expect("tempdir");
        let local = local_file(&dir, 8 * 1024 * 1024);
        let started = tokio::time::Instant::now();
        let outcome = tokio::time::timeout(
            SFTP_WRITE_ACK_BOUND * 2,
            provider.upload(&local, "/big.bin", None),
        )
        .await
        .expect("the upload waited past its bound");
        let err = outcome.expect_err("a stalled upload cannot succeed");
        assert!(err.is_connection_lost(), "{err:?}");
        assert!(started.elapsed() >= SFTP_WRITE_ACK_BOUND);
        assert!(!provider.is_connected(), "a stalled session is not reused");
    }

    /// A download's reads time out on their own after russh-sftp's 10 s, but
    /// once the transport is gone there is nothing to wait for: it fails at
    /// once, as a lost connection the command layer can retry.
    #[tokio::test]
    async fn a_download_fails_at_once_when_the_server_goes_away_mid_transfer() {
        let mut provider =
            provider_on_a_server_that_goes_away(GoAwayAt::Read(3), vec![7u8; 8 * 1024 * 1024])
                .await;
        let dir = tempfile::tempdir().expect("tempdir");
        let local = dir.path().join("down.bin");
        let started = std::time::Instant::now();
        let outcome = tokio::time::timeout(
            GONE_SERVER_BOUND,
            provider.download("/big.bin", &local.to_string_lossy(), None),
        )
        .await
        .expect("the download hung after the server went away");
        let err = outcome.expect_err("a download cut in the middle cannot succeed");
        assert!(err.is_connection_lost(), "{err:?}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "took {:?}: it waited for replies that could no longer come",
            started.elapsed()
        );
    }

    /// The other reads of a session race the end of its transport too: the
    /// serial download (a file of one buffer), `download_to_bytes`,
    /// `download_to_bytes_capped` and `read_range` each fail at once as a lost
    /// connection, where they waited out russh-sftp's 10 s.
    #[tokio::test]
    async fn the_other_reads_fail_at_once_when_the_server_goes_away() {
        let source = vec![7u8; 200 * 1024];
        for path in ["download", "to_bytes", "capped", "range"] {
            let mut provider =
                provider_on_a_server_that_goes_away(GoAwayAt::Read(1), source.clone()).await;
            let dir = tempfile::tempdir().expect("tempdir");
            let local = dir.path().join("down.bin").to_string_lossy().into_owned();
            let started = std::time::Instant::now();
            let outcome = tokio::time::timeout(GONE_SERVER_BOUND, async {
                match path {
                    "download" => provider.download("/small.bin", &local, None).await,
                    "to_bytes" => provider.download_to_bytes("/small.bin").await.map(|_| ()),
                    "capped" => provider
                        .download_to_bytes_capped("/small.bin", 1 << 20)
                        .await
                        .map(|_| ()),
                    _ => provider
                        .read_range("/small.bin", 0, 100 * 1024)
                        .await
                        .map(|_| ()),
                }
            })
            .await
            .unwrap_or_else(|_| panic!("{path}: hung after the server went away"));
            let err = outcome.expect_err(path);
            assert!(err.is_connection_lost(), "{path}: {err:?}");
            assert!(
                started.elapsed() < std::time::Duration::from_secs(5),
                "{path}: took {:?}",
                started.elapsed()
            );
        }
    }

    /// russh-sftp's own request timeout ("Timeout") is a timeout, not a lost
    /// connection: a slow but live server answers late and the session is
    /// still good. A server message that only contains the word (a path) keeps
    /// its own class, and a closed session stays a lost connection.
    #[test]
    fn only_russh_sftps_own_timeout_is_a_timeout() {
        assert!(matches!(
            classify_russh_err("Timeout", ProviderError::NotFound),
            ProviderError::Timeout
        ));
        assert!(matches!(
            classify_russh_err("No such file: open /x/timeout.log", ProviderError::NotFound),
            ProviderError::NotFound(_)
        ));
        assert!(
            classify_russh_err(SFTP_TRANSPORT_ENDED, ProviderError::NotFound).is_connection_lost()
        );
    }

    /// A listing in flight when the server goes away ends as a lost
    /// connection as soon as the transport ends (the request is raced with
    /// it), which the command layer retries. It waited out russh-sftp's 10 s
    /// and was reported as "not found" (exit 2).
    #[tokio::test(start_paused = true)]
    async fn a_listing_cut_by_the_server_going_away_is_a_lost_connection() {
        let mut provider = provider_on_a_server_that_goes_away(GoAwayAt::OpenDir, Vec::new()).await;
        let err = tokio::time::timeout(GONE_SERVER_BOUND * 4, provider.list("/"))
            .await
            .expect("the listing hung")
            .expect_err("a listing cut in the middle cannot succeed");
        assert!(err.is_connection_lost(), "{err:?}");
    }

    /// An upload whose size check meets a server that has gone away is a lost
    /// connection at once, where it kept asking for its 3 s and then reported
    /// a verification failure.
    #[tokio::test]
    async fn an_upload_verified_against_a_gone_server_is_a_lost_connection() {
        let mut provider = provider_on_a_server_that_goes_away(GoAwayAt::Stat(1), Vec::new()).await;
        let dir = tempfile::tempdir().expect("tempdir");
        let local = local_file(&dir, 100 * 1024);
        let started = std::time::Instant::now();
        let outcome = tokio::time::timeout(
            GONE_SERVER_BOUND,
            provider.upload(&local, "/small.bin", None),
        )
        .await
        .expect("the verification hung");
        let err = outcome.expect_err("an upload that cannot be verified cannot succeed");
        assert!(err.is_connection_lost(), "{err:?}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "took {:?}",
            started.elapsed()
        );
    }

    /// A resume whose look at the remote fails reports it. The failure was
    /// read as "nothing there", and the resume became a full upload that
    /// truncated the remote file.
    #[tokio::test]
    async fn a_resume_that_cannot_see_the_remote_is_not_run_as_a_full_upload() {
        let mut provider =
            provider_on_a_server_that_goes_away(GoAwayAt::StatRefused, Vec::new()).await;
        let dir = tempfile::tempdir().expect("tempdir");
        let local = local_file(&dir, 64 * 1024);
        let outcome = tokio::time::timeout(
            GONE_SERVER_BOUND,
            provider.resume_upload(&local, "/big.bin", 1024, None),
        )
        .await
        .expect("the resume hung");
        let err = outcome.expect_err("a resume that cannot see the remote cannot go on");
        assert!(
            err.to_string().contains("stat remote for resume"),
            "reported, not uploaded over: {err:?}"
        );
    }

    /// The write bound holds per acknowledgement, not per buffer: an upload
    /// with the largest buffer `--buffer-size` allows (16 MiB) to a server
    /// that takes 6 s over each write goes through, each WRITE acknowledged in
    /// time. The 16 MiB handed to russh-sftp in one write would wait for about
    /// 56 acknowledgements and give up at 300 s.
    #[tokio::test(start_paused = true)]
    async fn a_large_buffer_to_a_slow_server_stays_inside_the_ack_bound() {
        let size = 16 * 1024 * 1024;
        let mut provider = provider_on_a_server_that_goes_away(
            GoAwayAt::SlowWrites(std::time::Duration::from_secs(6)),
            vec![0u8; size],
        )
        .await;
        provider.set_chunk_sizes(Some(size as u64), None);
        let dir = tempfile::tempdir().expect("tempdir");
        let local = local_file(&dir, size);
        let uploaded = tokio::time::timeout(
            SFTP_WRITE_ACK_BOUND * 10,
            provider.upload(&local, "/big.bin", None),
        )
        .await
        .expect("the upload hung");
        uploaded.expect("every WRITE was acknowledged in time");
    }

    /// OpenSSH's write length, as its sftp-server announces it through
    /// `limits@openssh.com`: 1 KiB under its 256 KiB packet.
    const OPENSSH_MAX_WRITE: usize = 256 * 1024 - 1024;

    /// An SFTP server that announces OpenSSH's limits and keeps the offset
    /// and length of every WRITE. The remote file starts `remote_len` bytes
    /// long, for a resume to append to.
    struct OpenSshShapedServer {
        writes: Arc<std::sync::Mutex<Vec<(u64, usize)>>>,
        remote_len: u64,
    }

    impl russh_sftp::server::Handler for OpenSshShapedServer {
        type Error = russh_sftp::protocol::StatusCode;

        fn unimplemented(&self) -> Self::Error {
            russh_sftp::protocol::StatusCode::OpUnsupported
        }

        async fn init(
            &mut self,
            _version: u32,
            _extensions: std::collections::HashMap<String, String>,
        ) -> Result<russh_sftp::protocol::Version, Self::Error> {
            let mut version = russh_sftp::protocol::Version::new();
            version
                .extensions
                .insert(russh_sftp::extensions::LIMITS.to_string(), "1".to_string());
            Ok(version)
        }

        async fn extended(
            &mut self,
            id: u32,
            request: String,
            _data: Vec<u8>,
        ) -> Result<russh_sftp::protocol::Packet, Self::Error> {
            if request != russh_sftp::extensions::LIMITS {
                return Err(self.unimplemented());
            }
            let limits = russh_sftp::extensions::LimitsExtension {
                max_packet_len: 256 * 1024,
                max_read_len: OPENSSH_MAX_WRITE as u64,
                max_write_len: OPENSSH_MAX_WRITE as u64,
                max_open_handles: 0,
            };
            let data = russh_sftp::ser::to_bytes(&limits)
                .map_err(|_| russh_sftp::protocol::StatusCode::Failure)?
                .to_vec();
            Ok(russh_sftp::protocol::Packet::ExtendedReply(
                russh_sftp::protocol::ExtendedReply { id, data },
            ))
        }

        async fn open(
            &mut self,
            id: u32,
            filename: String,
            _pflags: russh_sftp::protocol::OpenFlags,
            _attrs: russh_sftp::protocol::FileAttributes,
        ) -> Result<russh_sftp::protocol::Handle, Self::Error> {
            Ok(russh_sftp::protocol::Handle {
                id,
                handle: filename,
            })
        }

        async fn close(
            &mut self,
            id: u32,
            _handle: String,
        ) -> Result<russh_sftp::protocol::Status, Self::Error> {
            Ok(GoingAwayServer::ok(id))
        }

        async fn write(
            &mut self,
            id: u32,
            _handle: String,
            offset: u64,
            data: Vec<u8>,
        ) -> Result<russh_sftp::protocol::Status, Self::Error> {
            self.writes
                .lock()
                .expect("write log")
                .push((offset, data.len()));
            self.remote_len = self.remote_len.max(offset + data.len() as u64);
            Ok(GoingAwayServer::ok(id))
        }

        async fn stat(
            &mut self,
            id: u32,
            _path: String,
        ) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
            Ok(russh_sftp::protocol::Attrs {
                id,
                attrs: russh_sftp::protocol::FileAttributes {
                    size: Some(self.remote_len),
                    permissions: Some(0o100644),
                    ..Default::default()
                },
            })
        }

        async fn setstat(
            &mut self,
            id: u32,
            _path: String,
            _attrs: russh_sftp::protocol::FileAttributes,
        ) -> Result<russh_sftp::protocol::Status, Self::Error> {
            Ok(GoingAwayServer::ok(id))
        }
    }

    /// A provider whose SFTP session runs over the transport of an
    /// [`OpenSshShapedServer`], with the server's WRITE log.
    async fn provider_on_an_openssh_shaped_server(
        remote_len: u64,
    ) -> (SftpProvider, Arc<std::sync::Mutex<Vec<(u64, usize)>>>) {
        let (client, server) = tokio::io::duplex(1 << 20);
        let writes = Arc::new(std::sync::Mutex::new(Vec::new()));
        russh_sftp::server::run(
            server,
            OpenSshShapedServer {
                writes: Arc::clone(&writes),
                remote_len,
            },
        )
        .await;
        let mut provider = provider_without_a_session();
        provider.sftp = Some(SftpChannel::open(client).await.expect("sftp init"));
        (provider, writes)
    }

    /// Every WRITE of an upload but the last is a whole one at OpenSSH's
    /// write length, at the default buffer and on a resume, and the WRITEs
    /// follow each other with no gap. The 256 KiB default is 1 KiB over that
    /// length: handed to russh-sftp a buffer at a time, each buffer went out
    /// as 261120 bytes plus 1024, half of the 8 WRITEs in flight carried
    /// 1 KiB, and a 300 MiB upload to the lab took 18 to 24 s instead of 10
    /// to 11.5 s (2026-10-03).
    #[tokio::test]
    async fn an_upload_sends_whole_writes_at_opensshs_write_length() {
        let len = 3 * 1024 * 1024 + 12_345;
        let dir = tempfile::tempdir().expect("tempdir");
        let local = local_file(&dir, len);
        let mut wrong = Vec::new();
        for (case, partial) in [("upload", 0u64), ("resume", 1_000_000)] {
            let (mut provider, writes) = provider_on_an_openssh_shaped_server(partial).await;
            let outcome = if partial == 0 {
                provider.upload(&local, "/up.bin", None).await
            } else {
                provider
                    .resume_upload(&local, "/up.bin", partial, None)
                    .await
            };
            outcome.unwrap_or_else(|e| panic!("{case}: {e:?}"));
            let writes = writes.lock().expect("write log").clone();
            let mut next = partial;
            for (i, &(offset, n)) in writes.iter().enumerate() {
                if offset != next {
                    wrong.push(format!("{case}: WRITE {i} at {offset}, not {next}"));
                }
                if n != OPENSSH_MAX_WRITE && i + 1 < writes.len() {
                    wrong.push(format!("{case}: WRITE {i} of {n} bytes"));
                }
                next = offset + n as u64;
            }
            if next != len as u64 {
                wrong.push(format!(
                    "{case}: the WRITEs end at {next}, the file at {len}"
                ));
            }
        }
        assert!(
            wrong.is_empty(),
            "only the last WRITE may be short: {wrong:#?}"
        );
    }

    /// An SFTP server for downloads that answers each READ with at most `cap`
    /// bytes of `data` and keeps the offset and length every READ asked for.
    /// With `announce`, it states `cap` as its read length through
    /// `limits@openssh.com`, as OpenSSH does; without it, it states nothing and
    /// simply sends less than it was asked for, as other servers do. With
    /// `first_cap`, its very first reply is shorter still. With `open_limit`,
    /// it refuses an OPEN while that many handles are open, and it keeps the
    /// most handles that were ever open at once.
    struct ReadCappingServer {
        data: Arc<Vec<u8>>,
        cap: usize,
        announce: bool,
        first_cap: Option<usize>,
        open_limit: Option<usize>,
        open_now: usize,
        max_open: Arc<std::sync::atomic::AtomicUsize>,
        reads: Arc<std::sync::Mutex<Vec<(u64, u32)>>>,
    }

    impl ReadCappingServer {
        fn new(data: &[u8], cap: usize, announce: bool) -> Self {
            Self {
                data: Arc::new(data.to_vec()),
                cap,
                announce,
                first_cap: None,
                open_limit: None,
                open_now: 0,
                max_open: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                reads: Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }

        /// A provider whose SFTP session runs over this server's transport.
        async fn serve(self) -> SftpProvider {
            let (client, server) = tokio::io::duplex(1 << 20);
            russh_sftp::server::run(server, self).await;
            let mut provider = provider_without_a_session();
            provider.sftp = Some(SftpChannel::open(client).await.expect("sftp init"));
            provider
        }
    }

    impl russh_sftp::server::Handler for ReadCappingServer {
        type Error = russh_sftp::protocol::StatusCode;

        fn unimplemented(&self) -> Self::Error {
            russh_sftp::protocol::StatusCode::OpUnsupported
        }

        async fn init(
            &mut self,
            _version: u32,
            _extensions: std::collections::HashMap<String, String>,
        ) -> Result<russh_sftp::protocol::Version, Self::Error> {
            let mut version = russh_sftp::protocol::Version::new();
            if self.announce {
                version
                    .extensions
                    .insert(russh_sftp::extensions::LIMITS.to_string(), "1".to_string());
            }
            Ok(version)
        }

        async fn extended(
            &mut self,
            id: u32,
            request: String,
            _data: Vec<u8>,
        ) -> Result<russh_sftp::protocol::Packet, Self::Error> {
            if request != russh_sftp::extensions::LIMITS {
                return Err(self.unimplemented());
            }
            let limits = russh_sftp::extensions::LimitsExtension {
                max_packet_len: 256 * 1024,
                max_read_len: self.cap as u64,
                max_write_len: self.cap as u64,
                max_open_handles: 0,
            };
            let data = russh_sftp::ser::to_bytes(&limits)
                .map_err(|_| russh_sftp::protocol::StatusCode::Failure)?
                .to_vec();
            Ok(russh_sftp::protocol::Packet::ExtendedReply(
                russh_sftp::protocol::ExtendedReply { id, data },
            ))
        }

        async fn open(
            &mut self,
            id: u32,
            filename: String,
            _pflags: russh_sftp::protocol::OpenFlags,
            _attrs: russh_sftp::protocol::FileAttributes,
        ) -> Result<russh_sftp::protocol::Handle, Self::Error> {
            if self.open_limit.is_some_and(|limit| self.open_now >= limit) {
                return Err(russh_sftp::protocol::StatusCode::Failure);
            }
            self.open_now += 1;
            self.max_open
                .fetch_max(self.open_now, std::sync::atomic::Ordering::SeqCst);
            Ok(russh_sftp::protocol::Handle {
                id,
                handle: filename,
            })
        }

        async fn close(
            &mut self,
            id: u32,
            _handle: String,
        ) -> Result<russh_sftp::protocol::Status, Self::Error> {
            self.open_now = self.open_now.saturating_sub(1);
            Ok(GoingAwayServer::ok(id))
        }

        async fn stat(
            &mut self,
            id: u32,
            _path: String,
        ) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
            Ok(russh_sftp::protocol::Attrs {
                id,
                attrs: russh_sftp::protocol::FileAttributes {
                    size: Some(self.data.len() as u64),
                    permissions: Some(0o100644),
                    ..Default::default()
                },
            })
        }

        async fn read(
            &mut self,
            id: u32,
            _handle: String,
            offset: u64,
            len: u32,
        ) -> Result<russh_sftp::protocol::Data, Self::Error> {
            self.reads.lock().expect("read log").push((offset, len));
            let start = usize::try_from(offset).unwrap_or(usize::MAX);
            if start >= self.data.len() {
                return Err(russh_sftp::protocol::StatusCode::Eof);
            }
            let cap = self.first_cap.take().unwrap_or(self.cap);
            let end = self.data.len().min(start + (len as usize).min(cap));
            Ok(russh_sftp::protocol::Data {
                id,
                data: self.data[start..end].to_vec(),
            })
        }
    }

    /// A file's bytes, distinct at every offset that matters.
    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    /// Downloads `/down.bin` through `provider` and returns its bytes.
    async fn download_bytes(provider: &mut SftpProvider) -> Result<Vec<u8>, ProviderError> {
        let dir = tempfile::tempdir().expect("tempdir");
        let local = dir.path().join("down.bin");
        provider
            .download("/down.bin", &local.to_string_lossy(), None)
            .await?;
        Ok(std::fs::read(&local).expect("downloaded file"))
    }

    /// Every read-ahead chunk is one READ, whatever the server sends back for
    /// one: no READ but the first and the last of the file asks for anything
    /// other than a whole reply. The 256 KiB default chunk is 1 KiB over
    /// OpenSSH's read length, and every chunk took a second READ for its last
    /// KiB, sent once the first reply was in and queued behind every reply
    /// already asked for: the handles went round in step and the server sat
    /// with nothing to send for a round trip every round. A 300 MiB download
    /// from the lab took 51 to 57 s on one connection and 38 or 32 s with
    /// chunks of one READ (2026-10-04). A first reply of a few bytes does not
    /// size the chunks below 32 KiB.
    #[tokio::test]
    async fn a_readahead_download_asks_for_each_chunk_in_one_read() {
        let data = pattern(3 * 1024 * 1024 + 12_345);
        let size = data.len() as u64;
        let mut wrong = Vec::new();
        for (case, cap, announce, first_cap, chunk) in [
            (
                "OpenSSH's read length",
                OPENSSH_MAX_WRITE,
                true,
                None,
                OPENSSH_MAX_WRITE,
            ),
            (
                "replies cut at 64 KiB, no limits stated",
                64 * 1024,
                false,
                None,
                64 * 1024,
            ),
            (
                "a first reply of 1000 bytes",
                OPENSSH_MAX_WRITE,
                true,
                Some(1000),
                32 * 1024,
            ),
        ] {
            let mut server = ReadCappingServer::new(&data, cap, announce);
            server.first_cap = first_cap;
            let reads = Arc::clone(&server.reads);
            let mut provider = server.serve().await;
            let got = download_bytes(&mut provider)
                .await
                .unwrap_or_else(|e| panic!("{case}: {e:?}"));
            if got != data {
                wrong.push(format!("{case}: the downloaded bytes differ"));
            }
            for &(offset, len) in reads.lock().expect("read log").iter() {
                let inside = offset > 0 && offset + u64::from(len) < size;
                if inside && len as usize != chunk {
                    wrong.push(format!(
                        "{case}: READ of {len} bytes at {offset}, chunks are {chunk} bytes"
                    ));
                }
            }
        }
        assert!(
            wrong.is_empty(),
            "a read-ahead chunk took more than one READ: {wrong:#?}"
        );
    }

    /// Runs the read-ahead over the whole of `size` bytes of `/down.bin` on the
    /// provider's session, with 256 KiB chunks, and returns what landed in the
    /// output.
    async fn readahead_into_file(
        provider: &SftpProvider,
        size: u64,
        window: usize,
        grow_to: Option<usize>,
    ) -> Result<Vec<u8>, ProviderError> {
        use tokio::io::AsyncWriteExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("out.bin");
        let mut out = tokio::fs::File::create(&path).await.expect("output file");
        out.set_len(size).await.expect("output size");
        sftp_readahead_range_into(
            provider.get_sftp().expect("session"),
            "/down.bin",
            0,
            size,
            &mut out,
            256 * 1024,
            window,
            grow_to,
            &Arc::new(AtomicU64::new(0)),
            &CancellationToken::new(),
            size,
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .await?;
        out.flush().await.expect("flush");
        drop(out);
        Ok(std::fs::read(&path).expect("read back"))
    }

    /// One READ is in flight per handle, so on a server that sends 32 KiB per
    /// READ the 16 handles a shared default gives each of four connections
    /// would hold a quarter of the bytes the release kept in flight. The shared
    /// window grows back to the release's 32 handles a connection, and never
    /// past them; a window that was asked for stays as asked.
    #[tokio::test]
    async fn a_shared_readahead_grows_back_to_the_release_handles_and_no_further() {
        let data = pattern(9 * 1024 * 1024 + 4_321);
        let size = data.len() as u64;
        let mut wrong = Vec::new();
        for (case, window, grow_to, handles) in [
            (
                "the shared default",
                16,
                Some(SFTP_READAHEAD_DEFAULT_WINDOW),
                SFTP_READAHEAD_DEFAULT_WINDOW,
            ),
            ("a window that was asked for", 8, None, 8),
        ] {
            let server = ReadCappingServer::new(&data, 32 * 1024, false);
            let max_open = Arc::clone(&server.max_open);
            let provider = server.serve().await;
            let got = readahead_into_file(&provider, size, window, grow_to)
                .await
                .unwrap_or_else(|e| panic!("{case}: {e:?}"));
            if got != data {
                wrong.push(format!("{case}: the bytes differ"));
            }
            let open = max_open.load(std::sync::atomic::Ordering::SeqCst);
            if open != handles {
                wrong.push(format!(
                    "{case}: {open} handles kept {} KiB in flight, expected {handles} handles",
                    open * 32
                ));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// The handles a smaller read length asks for are opened after the first
    /// READ, and a server that will not open them leaves the read-ahead with
    /// the handles it already has: the range finishes, and the bytes the first
    /// READ brought are kept.
    #[tokio::test]
    async fn a_readahead_goes_on_with_the_handles_it_has_when_the_server_refuses_more() {
        let data = pattern(9 * 1024 * 1024 + 4_321);
        let mut server = ReadCappingServer::new(&data, 32 * 1024, false);
        server.open_limit = Some(2);
        let max_open = Arc::clone(&server.max_open);
        let provider = server.serve().await;
        let got = readahead_into_file(
            &provider,
            data.len() as u64,
            16,
            Some(SFTP_READAHEAD_DEFAULT_WINDOW),
        )
        .await
        .unwrap_or_else(|e| {
            panic!("the download failed when the server refused more handles: {e:?}")
        });
        assert!(got == data, "the downloaded bytes differ");
        assert!(max_open.load(std::sync::atomic::Ordering::SeqCst) <= 2);
    }

    /// Takes at most `cap` bytes per write, as russh-sftp's `File` takes at
    /// most the session's write length, and keeps what it took.
    struct CappedWriter {
        cap: usize,
        lens: Vec<usize>,
        data: Vec<u8>,
    }

    impl tokio::io::AsyncWrite for CappedWriter {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            let n = buf.len().min(self.cap);
            self.lens.push(n);
            self.data.extend_from_slice(&buf[..n]);
            std::task::Poll::Ready(Ok(n))
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// Whatever the buffer and the session's write length, the upload buffer
    /// sends only whole WRITEs until the last one, and the bytes arrive whole
    /// and in order. An empty file sends none.
    #[tokio::test]
    async fn the_upload_buffer_sends_whole_writes_until_the_last() {
        let source: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
        let ended = CancellationToken::new();
        let mut wrong = Vec::new();
        for (buffer, cap, len) in [
            // The default against OpenSSH.
            (256 * 1024, OPENSSH_MAX_WRITE, source.len()),
            // `--buffer-size 2M`, and the 16 MiB cap.
            (2 * 1024 * 1024, OPENSSH_MAX_WRITE, source.len()),
            (16 * 1024 * 1024, OPENSSH_MAX_WRITE, source.len()),
            // A buffer under the write length: every WRITE is the buffer.
            (64 * 1024, OPENSSH_MAX_WRITE, source.len()),
            // Neither a multiple of the other.
            (250_000, 100_000, source.len()),
            (256 * 1024, OPENSSH_MAX_WRITE, 0),
        ] {
            let mut local = &source[..len];
            let mut remote = CappedWriter {
                cap,
                lens: Vec::new(),
                data: Vec::new(),
            };
            let mut pending = SftpWriteBuffer::new(buffer);
            loop {
                pending.refill(&mut local).await.expect("read");
                if pending.is_done() {
                    break;
                }
                pending
                    .write_next(&mut remote, &ended)
                    .await
                    .expect("write");
            }
            let whole = buffer.min(cap);
            let before_last = remote.lens.split_last().map_or(&[][..], |(_, rest)| rest);
            let short: Vec<_> = before_last.iter().filter(|&&n| n != whole).collect();
            if !short.is_empty() {
                wrong.push(format!(
                    "buffer {buffer}, write length {cap}: {} short WRITEs before the last",
                    short.len()
                ));
            }
            if remote.data != source[..len] {
                wrong.push(format!(
                    "buffer {buffer}, write length {cap}: the bytes differ"
                ));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// A request timeout on the way into or out of the remote file fails the
    /// upload (exit 4) instead of reporting a limit with nothing failed (exit
    /// 8): the CREATE, the OPEN of a resume or the CLOSE may have reached the
    /// server and left a 0-byte or partial file. The server here stays and
    /// never answers the request, so russh-sftp's own 10 s is what ends it.
    #[tokio::test(start_paused = true)]
    async fn a_request_timeout_while_writing_a_remote_file_fails_the_transfer() {
        let mut wrong = Vec::new();
        for (case, at) in [
            ("upload create", GoAwayAt::OpenUnanswered),
            ("upload close", GoAwayAt::CloseUnanswered),
            ("resume open", GoAwayAt::OpenUnanswered),
            ("resume close", GoAwayAt::CloseUnanswered),
        ] {
            let mut provider =
                provider_on_a_server_that_goes_away(at, vec![7u8; 1024 * 1024]).await;
            let dir = tempfile::tempdir().expect("tempdir");
            let local = local_file(&dir, 2 * 1024 * 1024);
            let outcome = tokio::time::timeout(GONE_SERVER_BOUND * 4, async {
                if case.starts_with("resume") {
                    provider
                        .resume_upload(&local, "/f.bin", 1024 * 1024, None)
                        .await
                } else {
                    provider.upload(&local, "/f.bin", None).await
                }
            })
            .await
            .unwrap_or_else(|_| panic!("{case}: hung"));
            match outcome {
                Err(ProviderError::TransferFailed(message))
                    if message.contains("timeout") && message.contains("may be incomplete") => {}
                other => wrong.push(format!("{case}: {other:?}")),
            }
        }
        assert!(
            wrong.is_empty(),
            "a timeout that fails an upload is a failed transfer: {wrong:#?}"
        );
    }

    /// The requests around a transfer race the end of the transport too. An
    /// OPEN (a capped download's, an upload's create, a resume's, the one a
    /// small download sends next to its STAT), a SETSTAT (chmod), a REALPATH
    /// (the keepalive) and the symlink check of a stat, each in flight when
    /// the server goes away, fail at once as a lost connection. They waited
    /// out russh-sftp's 10 s and ended as a timeout, and the stat reported
    /// the entry as if the check had answered.
    #[tokio::test(start_paused = true)]
    async fn requests_in_flight_fail_at_once_when_the_server_goes_away() {
        let mut wrong = Vec::new();
        for case in [
            "capped",
            "hinted",
            "upload",
            "resume",
            "chmod",
            "keepalive",
            "stat",
        ] {
            let at = match case {
                "chmod" => GoAwayAt::SetStat,
                "keepalive" => GoAwayAt::RealPath,
                "stat" => GoAwayAt::Stat(2),
                _ => GoAwayAt::Open,
            };
            let mut provider =
                provider_on_a_server_that_goes_away(at, vec![7u8; 1024 * 1024]).await;
            let dir = tempfile::tempdir().expect("tempdir");
            let local = local_file(&dir, 2 * 1024 * 1024);
            let down = dir.path().join("down.bin").to_string_lossy().into_owned();
            let started = tokio::time::Instant::now();
            let outcome = tokio::time::timeout(GONE_SERVER_BOUND * 4, async {
                match case {
                    "capped" => provider
                        .download_to_bytes_capped("/f.bin", 4 * 1024 * 1024)
                        .await
                        .map(|_| ()),
                    "hinted" => {
                        provider
                            .download_with_size_hint("/f.bin", &down, Some(1024), None)
                            .await
                    }
                    "upload" => provider.upload(&local, "/f.bin", None).await,
                    "resume" => {
                        provider
                            .resume_upload(&local, "/f.bin", 1024 * 1024, None)
                            .await
                    }
                    "chmod" => provider.chmod("/f.bin", 0o644).await,
                    "keepalive" => provider.keep_alive().await,
                    _ => provider.stat("/f.bin").await.map(|_| ()),
                }
            })
            .await
            .unwrap_or_else(|_| panic!("{case}: hung"));
            let took = started.elapsed();
            let lost = matches!(&outcome, Err(err) if err.is_connection_lost());
            if !lost || took >= std::time::Duration::from_secs(5) {
                wrong.push(format!("{case}: {outcome:?} after {took:?}"));
            }
        }
        assert!(
            wrong.is_empty(),
            "a lost connection at once, not a timeout 10 s later: {wrong:#?}"
        );
    }

    /// Only a cancellation counts as one: a real error whose text merely
    /// contains the word (a path such as `/data/cancelled/x.bin`) is the
    /// cause, and it is reported over the cancellations it caused.
    #[test]
    fn a_reader_error_that_names_a_cancelled_path_is_the_cause() {
        let cancelled = || {
            Err(ProviderError::TransferFailed(
                "Transfer cancelled by user".to_string(),
            ))
        };
        let real = || {
            Err(ProviderError::TransferFailed(
                "Read error (pipeline): Failure: /data/cancelled/x.bin".to_string(),
            ))
        };
        let picked = first_reader_cause(vec![cancelled(), real()]);
        assert!(
            matches!(&picked, Err(ProviderError::TransferFailed(m)) if m.contains("/data/cancelled/")),
            "{picked:?}"
        );
        let picked = first_reader_cause(vec![Err(ProviderError::Cancelled), real()]);
        assert!(
            matches!(&picked, Err(ProviderError::TransferFailed(m)) if m.contains("/data/cancelled/")),
            "{picked:?}"
        );
    }

    /// A listing whose follow-ups (the STAT of entries sent without
    /// attributes, the symlink checks) are cut by the end of the transport is
    /// a lost connection, not a listing: the follow-ups' errors are dropped
    /// on purpose, and the entries they would have completed came back as
    /// plain files with no mode, as if the listing had worked.
    #[tokio::test(start_paused = true)]
    async fn a_listing_whose_follow_ups_are_cut_is_a_lost_connection() {
        let mut provider =
            provider_on_a_server_that_goes_away(GoAwayAt::ListFollowUp, Vec::new()).await;
        let outcome = tokio::time::timeout(GONE_SERVER_BOUND * 4, provider.list("/"))
            .await
            .expect("the listing hung");
        assert!(
            matches!(&outcome, Err(err) if err.is_connection_lost()),
            "{outcome:?}"
        );
    }

    /// `find` skips a folder it cannot read, but not a transport that ended
    /// in the middle of the walk: that is a lost connection, where it
    /// returned what it had found so far as the whole result.
    #[tokio::test(start_paused = true)]
    async fn a_find_cut_by_the_transport_is_a_lost_connection() {
        let mut provider =
            provider_on_a_server_that_goes_away(GoAwayAt::OpenDirAt(2), Vec::new()).await;
        let outcome = tokio::time::timeout(GONE_SERVER_BOUND * 4, provider.find("/", "*"))
            .await
            .expect("the find hung");
        assert!(
            matches!(&outcome, Err(err) if err.is_connection_lost()),
            "{outcome:?}"
        );
    }
}
