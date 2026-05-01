//! P3-T01 W3.2(b1) — russh-based SSH session transport for the native rsync
//! batch path.
//!
//! ## Why this exists
//!
//! The legacy [`crate::aerorsync::ssh_transport::SshRemoteShellTransport`] is
//! built on `ssh2` (libssh2 binding). libssh2 is thread-bound: every
//! `open_raw_stream` call ends up in a `spawn_blocking` worker that runs
//! `connect_and_auth` from scratch — one full SSH handshake per file. That is
//! correct for the single-shot path (`AerorsyncDeltaTransport::download` /
//! `upload` use a fresh transport per call), but it makes session reuse
//! impossible for [`crate::aerorsync::AerorsyncBatch`] (W3.2(b2)).
//!
//! `russh` 0.60.1 is already a first-class SSH stack in the project (see
//! `providers/sftp.rs`): pure-Rust, async-native, multi-channel naturally
//! (one [`russh::client::Handle`] opens N [`russh::Channel`]s above a single
//! SSH session). The native rsync batch needs exactly that — open one SSH
//! handshake at `AerorsyncBatch::new` time and exec the remote rsync server
//! over a fresh channel per file.
//!
//! ## Status
//!
//! W3.2(b1)-impl-connect: [`RusshSessionTransport::connect`] is functional.
//! It opens one SSH session via russh, authenticates with a private key
//! (RSA SHA-512/256/1 fallback like `providers::sftp`, ed25519/ecdsa direct),
//! and stores the [`russh::client::Handle`] behind a `tokio::sync::Mutex`
//! shared via `Arc` for cheap `share_session()` clones across the batch.
//!
//! Still TODO (subsequent commits):
//! - W3.2(b1)-impl-channel: `open_raw_channel` opens a channel session,
//!   execs the remote command, and wraps it in [`RusshRawStream`].
//! - W3.2(b1)-impl-raw-stream: [`RawByteStream`] impl driven by
//!   [`russh::ChannelMsg::Data`] / `Eof` / `ExitStatus` events.
//! - W3.2(b1)-impl-trait: [`RemoteShellTransport`] `probe` + `exec` impls.
//! - W3.2(b1)-tests: 4 live tests against the Docker key-auth lane.
//!
//! Spec dettagliata: `docs/dev/roadmap/APPENDIX-C-Y-D/APPENDIX-Y/tasks/2026-05-01_P3-T01_W3_2b1_Spec_Session_Caching.md`.

#![cfg(feature = "aerorsync")]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use russh::client::{self, AuthResult, Config, Handle, Handler};
use russh::keys::{self, Algorithm, HashAlg, PrivateKeyWithHashAlg, PublicKey};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as AsyncMutex;

use crate::aerorsync::ssh_transport::{SshHostKeyPolicy, SshTransportConfig};
use crate::aerorsync::transport::{
    CancelHandle, RawByteStream, RawRemoteShellTransport, RemoteCommandOutput, RemoteExecRequest,
    RemoteShellTransport, TransportProbe,
};
use crate::aerorsync::types::AerorsyncError;

/// russh-based remote shell transport that holds one long-lived SSH session
/// and serves N channel-exec calls over it.
///
/// Cheap to clone: the [`russh::client::Handle`] lives behind an `Arc<Mutex>`,
/// so `share_session` produces a view that points at the same session — no
/// new handshake. Drop the last clone and the russh `Handle` Drop closes
/// the session.
pub struct RusshSessionTransport {
    /// Long-lived russh client handle. `Some` after a successful
    /// [`Self::connect`]. Wrapped in `Arc<AsyncMutex>` so `share_session`
    /// is cheap and concurrent `open_raw_channel` calls serialize on the
    /// mutex (russh `channel_open_session` is `&mut self` on `Handle`).
    handle: Arc<HandleSlot>,
    /// Cooperative cancel flag, shared with all `share_session` clones.
    cancel_flag: Arc<AtomicBool>,
    /// Counter incremented by every successful `connect()`. Production
    /// usage: `BatchStats.session_count = handshake_count` at finalize.
    handshake_count: Arc<AtomicU32>,
    /// Stored for reconnect on transient failure (W3.2(b2) batch logic).
    config: SshTransportConfig,
}

/// Internal cell that holds the optional russh `Handle`. Wrapped in an
/// `AsyncMutex` because `russh::client::Handle::channel_open_session` (and
/// the auth methods) take `&mut self`. Concurrent batches would need
/// serialization at this layer; for P3-T01's sequential batch the lock is
/// always uncontended.
struct HandleSlot {
    inner: AsyncMutex<Option<Handle<RusshHandler>>>,
}

impl HandleSlot {
    fn new(handle: Option<Handle<RusshHandler>>) -> Self {
        Self {
            inner: AsyncMutex::new(handle),
        }
    }
}

/// russh `Handler` that enforces the same [`SshHostKeyPolicy`] semantics as
/// the legacy ssh2 transport (`enforce_host_key_policy` in
/// `ssh_transport.rs`). No known_hosts / TOFU here — that flow lives in
/// `providers::sftp::SshHandler`. The batch path consumes a host key
/// fingerprint already captured by the SFTP session that the user
/// approved.
pub struct RusshHandler {
    policy: SshHostKeyPolicy,
}

impl RusshHandler {
    fn new(policy: SshHostKeyPolicy) -> Self {
        Self { policy }
    }

    /// SHA-256 hex digest (lowercase, colon-free) of the SSH-wire-encoded
    /// public key bytes. Mirrors the `sha256_hex_of(host_key)` shape used
    /// by `enforce_host_key_policy`. Returns `None` if russh fails to
    /// re-encode the key — secure default: refuse to accept.
    fn fingerprint_sha256_hex(key: &PublicKey) -> Option<String> {
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

impl Handler for RusshHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        match &self.policy {
            SshHostKeyPolicy::AcceptAny => Ok(true),
            SshHostKeyPolicy::PinnedFingerprintSha256 { sha256_hex } => {
                let actual = match Self::fingerprint_sha256_hex(server_public_key) {
                    Some(hex) => hex,
                    None => {
                        tracing::error!(
                            "russh: rejecting host key — failed to compute SHA-256 fingerprint \
                             (key encoding error)"
                        );
                        return Ok(false);
                    }
                };
                let expected = sha256_hex.to_ascii_lowercase();
                if actual == expected {
                    Ok(true)
                } else {
                    tracing::error!(
                        "russh: REJECTING host key — fingerprint mismatch (expected {expected}, \
                         got {actual})"
                    );
                    Ok(false)
                }
            }
        }
    }
}

impl RusshSessionTransport {
    /// Open one SSH session via russh: TCP connect + handshake + key auth +
    /// host key policy check. Mirrors the auth shape of
    /// `providers::sftp::authenticate_with_key` (RSA SHA-512/256/1
    /// fallback, ed25519/ecdsa direct).
    ///
    /// Increments `handshake_count` to 1 on success. The batch is expected
    /// to call `connect` once at construction; transient reconnect mid-batch
    /// (W3.2(b2)) calls `connect` again and observes `handshake_count > 1`.
    pub async fn connect(config: SshTransportConfig) -> Result<Self, AerorsyncError> {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let handshake_count = Arc::new(AtomicU32::new(0));

        // 1. Build russh client config. Mirrors `providers::sftp::connect`
        //    with keepalive sufficient for long batches but tighter than
        //    the SFTP defaults so a dead session is detected within ~45s.
        let russh_config = Arc::new(Config {
            inactivity_timeout: Some(Duration::from_millis(config.io_timeout_ms.max(30_000) * 2)),
            keepalive_interval: Some(Duration::from_secs(15)),
            keepalive_max: 3,
            ..Default::default()
        });

        // 2. TCP + SSH handshake. The handler enforces SshHostKeyPolicy.
        let handler = RusshHandler::new(config.host_key_policy.clone());
        let addr = format!("{}:{}", config.host, config.port);
        let mut handle = client::connect(russh_config, &addr, handler)
            .await
            .map_err(|e| {
                AerorsyncError::transport(format!("russh connect {addr}: {e}"))
            })?;

        // 3. Key auth — replicates `authenticate_with_key` from sftp.rs.
        //    No passphrase support yet (RsyncConfig has no key_passphrase
        //    field; if needed it goes through SshTransportConfig later).
        let key_pair = keys::load_secret_key(&config.private_key_path, None).map_err(|e| {
            AerorsyncError::transport(format!(
                "load private key {}: {e}",
                config.private_key_path.display()
            ))
        })?;
        let key_pair = Arc::new(key_pair);

        let is_rsa = matches!(key_pair.algorithm(), Algorithm::Rsa { .. });
        let attempts: Vec<Option<HashAlg>> = if is_rsa {
            // OpenSSH 8.8+ rejects ssh-rsa (SHA-1) by default. Try SHA-512
            // (RFC 8332 preference), SHA-256 fallback, then None for
            // legacy servers that still accept ssh-rsa.
            vec![Some(HashAlg::Sha512), Some(HashAlg::Sha256), None]
        } else {
            vec![None]
        };

        let mut authenticated = false;
        let mut last_error: Option<String> = None;
        for hash in attempts {
            let key_with_hash = PrivateKeyWithHashAlg::new(key_pair.clone(), hash);
            match handle
                .authenticate_publickey(&config.username, key_with_hash)
                .await
            {
                Ok(AuthResult::Success) => {
                    authenticated = true;
                    break;
                }
                Ok(AuthResult::Failure { .. }) => continue,
                Err(e) => {
                    last_error = Some(e.to_string());
                    continue;
                }
            }
        }

        if !authenticated {
            let detail = last_error.unwrap_or_else(|| "auth rejected by server".to_string());
            return Err(AerorsyncError::transport(format!(
                "russh pubkey auth {} failed after RSA SHA-512/256/1 negotiation attempts: {detail}",
                config.username
            )));
        }

        handshake_count.store(1, Ordering::SeqCst);

        Ok(Self {
            handle: Arc::new(HandleSlot::new(Some(handle))),
            cancel_flag,
            handshake_count,
            config,
        })
    }

    /// Number of SSH handshakes performed over the lifetime of this
    /// transport (counting `connect` + any reconnect on transient
    /// failure). `BatchStats.session_count` source.
    pub fn handshake_count(&self) -> u32 {
        self.handshake_count.load(Ordering::SeqCst)
    }

    /// View of the same session — clones the inner Arc so `do_upload` /
    /// `do_download` can consume a transport by value without a new
    /// handshake.
    pub fn share_session(&self) -> Self {
        Self {
            handle: self.handle.clone(),
            cancel_flag: self.cancel_flag.clone(),
            handshake_count: self.handshake_count.clone(),
            config: self.config.clone(),
        }
    }

    /// Open a new channel over the existing SSH session and exec the
    /// rsync remote command. Returns a [`RusshRawStream`] that the driver
    /// can drive as a [`RawByteStream`].
    ///
    /// W3.2(b1)-impl-channel TODO: lock the handle, call
    /// `channel_open_session` + `channel.exec(cmd)`, wrap in RusshRawStream.
    pub async fn open_raw_channel(
        &self,
        _request: RemoteExecRequest,
    ) -> Result<RusshRawStream, AerorsyncError> {
        Err(AerorsyncError::transport(
            "RusshSessionTransport::open_raw_channel not implemented yet \
             (W3.2(b1)-impl-channel)",
        ))
    }

    /// Cooperative cancel. Flips the flag and tears down the russh handle
    /// (which closes any in-flight channels). Idempotent.
    pub async fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::SeqCst);
        // Best-effort handle teardown. If a channel_open_session is in
        // flight on another task, dropping the handle here unblocks it
        // with a transport error.
        let mut guard = self.handle.inner.lock().await;
        if let Some(handle) = guard.take() {
            // russh::client::Handle has no public disconnect API; dropping
            // it sends Disconnect and closes the underlying TCP. The
            // explicit drop here makes the lifecycle visible.
            drop(handle);
        }
    }

    /// Tear down the SSH session ordinately. Idempotent. After `close()`
    /// returns, subsequent `open_raw_channel` calls fail with a typed
    /// transport error.
    pub async fn close(&self) -> Result<(), AerorsyncError> {
        let mut guard = self.handle.inner.lock().await;
        if let Some(handle) = guard.take() {
            // Same as cancel — drop closes the session.
            drop(handle);
        }
        Ok(())
    }
}

/// Channel-backed [`RawByteStream`] returned by
/// [`RusshSessionTransport::open_raw_channel`].
///
/// W3.2(b1) scaffold body. The impl step replaces `_unused` with the
/// real `russh::Channel<Msg>` and drains `ChannelMsg::Data` events.
pub struct RusshRawStream {
    cancel_flag: Arc<AtomicBool>,
    /// Surplus bytes from prior `ChannelMsg::Data` events that didn't fit
    /// in the previous `read_bytes` allocation.
    pending: Vec<u8>,
    /// True after `ChannelMsg::Eof` or `ExitStatus`. Once true,
    /// subsequent `read_bytes` returns `Ok(vec![])` (per RawByteStream
    /// contract).
    eof: bool,
}

impl RusshRawStream {
    #[allow(dead_code)]
    fn new(cancel_flag: Arc<AtomicBool>) -> Self {
        Self {
            cancel_flag,
            pending: Vec::new(),
            eof: false,
        }
    }
}

#[async_trait]
impl RawByteStream for RusshRawStream {
    async fn read_bytes(&mut self, _max: usize) -> Result<Vec<u8>, AerorsyncError> {
        if self.cancel_flag.load(Ordering::SeqCst) {
            return Err(AerorsyncError::cancelled(
                "RusshRawStream cancelled before read_bytes",
            ));
        }
        if self.eof {
            return Ok(Vec::new());
        }
        // Surplus from a previous Data event takes priority.
        if !self.pending.is_empty() {
            // Real impl: drain up to `max` bytes from `pending` here.
        }
        Err(AerorsyncError::transport(
            "RusshRawStream::read_bytes not implemented yet (W3.2(b1)-impl-raw-stream)",
        ))
    }

    async fn write_bytes(&mut self, _bytes: &[u8]) -> Result<(), AerorsyncError> {
        if self.cancel_flag.load(Ordering::SeqCst) {
            return Err(AerorsyncError::cancelled(
                "RusshRawStream cancelled before write_bytes",
            ));
        }
        Err(AerorsyncError::transport(
            "RusshRawStream::write_bytes not implemented yet (W3.2(b1)-impl-raw-stream)",
        ))
    }

    async fn shutdown(&mut self) -> Result<(), AerorsyncError> {
        // Mark eof so subsequent read_bytes return Ok(empty). Real impl:
        // `channel.eof().await` + drain trailing ChannelMsg events.
        self.eof = true;
        Ok(())
    }
}

#[async_trait]
impl RemoteShellTransport for RusshSessionTransport {
    type Stream = RusshUnusedStream;

    async fn probe(&self) -> Result<TransportProbe, AerorsyncError> {
        Err(AerorsyncError::transport(
            "RusshSessionTransport::probe not implemented yet (W3.2(b1)-impl-trait)",
        ))
    }

    async fn exec(
        &self,
        _request: RemoteExecRequest,
    ) -> Result<RemoteCommandOutput, AerorsyncError> {
        Err(AerorsyncError::transport(
            "RusshSessionTransport::exec not implemented yet (W3.2(b1)-impl-trait)",
        ))
    }

    async fn open_stream(
        &self,
        _request: RemoteExecRequest,
    ) -> Result<Self::Stream, AerorsyncError> {
        // The russh transport intentionally does not support the legacy
        // RSNP framed stream — only the raw byte path. The legacy
        // `BidirectionalByteStream` driver path was wired on the ssh2
        // transport and is not on the migration list.
        Err(AerorsyncError::transport(
            "RusshSessionTransport does not support the legacy RSNP framed stream",
        ))
    }

    async fn cancel(&self) -> Result<(), AerorsyncError> {
        Self::cancel(self).await;
        Ok(())
    }

    fn cancel_handle(&self) -> CancelHandle {
        CancelHandle::new(self.cancel_flag.clone(), None)
    }
}

#[async_trait]
impl RawRemoteShellTransport for RusshSessionTransport {
    type RawStream = RusshRawStream;

    async fn open_raw_stream(
        &self,
        request: RemoteExecRequest,
    ) -> Result<Self::RawStream, AerorsyncError> {
        self.open_raw_channel(request).await
    }
}

/// Placeholder type for the unused `RemoteShellTransport::Stream` slot.
/// Lives here to keep the impl block self-contained without dragging the
/// legacy `SshProtoStream` into the russh module.
pub struct RusshUnusedStream;

#[async_trait]
impl crate::aerorsync::transport::BidirectionalByteStream for RusshUnusedStream {
    async fn write_frame(&mut self, _frame: &[u8]) -> Result<(), AerorsyncError> {
        Err(AerorsyncError::transport(
            "RusshUnusedStream cannot be driven — RusshSessionTransport \
             only supports the raw byte path",
        ))
    }

    async fn read_frame(&mut self) -> Result<Vec<u8>, AerorsyncError> {
        Err(AerorsyncError::transport(
            "RusshUnusedStream cannot be driven — RusshSessionTransport \
             only supports the raw byte path",
        ))
    }

    async fn shutdown(&mut self) -> Result<(), AerorsyncError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn dummy_config() -> SshTransportConfig {
        SshTransportConfig {
            host: "127.0.0.1".into(),
            port: 22,
            username: "test".into(),
            private_key_path: PathBuf::from("/dev/null"),
            connect_timeout_ms: 5_000,
            io_timeout_ms: 30_000,
            worker_idle_poll_ms: 250,
            max_frame_size: 1 << 20,
            host_key_policy: SshHostKeyPolicy::AcceptAny,
            probe_request: RemoteExecRequest {
                program: "rsync".into(),
                args: vec!["--version".into()],
                environment: Vec::new(),
            },
        }
    }

    /// Compile-time guard — RusshSessionTransport implements both
    /// `RemoteShellTransport` and `RawRemoteShellTransport`. If a future
    /// trait change makes the impl invalid, this fails to compile.
    #[test]
    fn russh_transport_satisfies_remote_shell_traits() {
        fn assert_traits<T: RemoteShellTransport + RawRemoteShellTransport>() {}
        assert_traits::<RusshSessionTransport>();
    }

    /// Compile-time guard — `Send + Sync`. Critical for the batch since
    /// the same transport view is consumed by `do_upload` / `do_download`
    /// across tokio task boundaries.
    #[test]
    fn russh_transport_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RusshSessionTransport>();
        assert_send_sync::<RusshRawStream>();
    }

    /// Now that `connect` is real, an unreachable host (port 22 on
    /// 127.0.0.1 with no SSH server in CI) returns `Err` from the
    /// `client::connect` step. Pin: the typed error path holds.
    #[tokio::test]
    async fn connect_with_unreachable_host_returns_typed_transport_error() {
        let cfg = SshTransportConfig {
            // Use a port that no SSH server listens on in CI.
            port: 1, // privileged port, almost certainly nothing listening
            ..dummy_config()
        };
        match RusshSessionTransport::connect(cfg).await {
            Err(_) => {
                // Expected — connect fails at TCP layer (1 is privileged
                // and not bound by anything sane in CI).
            }
            Ok(_) => panic!(
                "connect to port 1 should not succeed in CI — fixture must have an SSH \
                 server bound there which would be a security red flag"
            ),
        }
    }

    #[tokio::test]
    async fn share_session_clones_arc_state() {
        // Build a stub transport via the same field shape connect uses.
        // We do not call `connect` because it would require a real SSH
        // server; this test exercises only the share_session bookkeeping.
        let original = RusshSessionTransport {
            handle: Arc::new(HandleSlot::new(None)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            handshake_count: Arc::new(AtomicU32::new(0)),
            config: dummy_config(),
        };
        let clone = original.share_session();

        // Pin: the underlying counter is shared. Bump on the original,
        // observe on the clone.
        original.handshake_count.fetch_add(1, Ordering::SeqCst);
        assert_eq!(clone.handshake_count(), 1);
        assert_eq!(original.handshake_count(), 1);

        // Same for cancel_flag.
        clone.cancel_flag.store(true, Ordering::SeqCst);
        assert!(original.cancel_flag.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn close_is_idempotent_and_succeeds_on_empty_handle() {
        let transport = RusshSessionTransport {
            handle: Arc::new(HandleSlot::new(None)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            handshake_count: Arc::new(AtomicU32::new(0)),
            config: dummy_config(),
        };
        // Closing twice without panic — idempotent contract.
        transport.close().await.expect("first close ok");
        transport.close().await.expect("second close ok");
    }

    #[tokio::test]
    async fn cancel_flips_flag_and_drops_handle() {
        let transport = RusshSessionTransport {
            handle: Arc::new(HandleSlot::new(None)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            handshake_count: Arc::new(AtomicU32::new(0)),
            config: dummy_config(),
        };
        assert!(!transport.cancel_flag.load(Ordering::SeqCst));
        transport.cancel().await;
        assert!(transport.cancel_flag.load(Ordering::SeqCst));
        // Idempotent — second call does not panic.
        transport.cancel().await;
    }

    #[test]
    fn host_key_policy_accept_any_compiles() {
        let _h = RusshHandler::new(SshHostKeyPolicy::AcceptAny);
    }

    #[test]
    fn host_key_policy_pinned_compiles() {
        let _h = RusshHandler::new(SshHostKeyPolicy::PinnedFingerprintSha256 {
            sha256_hex: "deadbeef".into(),
        });
    }
}
