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
//! W3.2(b1) **scaffold**: types, signatures, and stubs that return
//! `Err(AerorsyncError::transport("…not implemented yet…"))`. cargo check
//! passes. The aerorsync test suite is invariant — this module is not yet
//! wired into any production path.
//!
//! Subsequent commits flesh out:
//! - W3.2(b1)-impl-connect: real russh handshake + key auth + host key
//!   check (mirrors `providers::sftp::authenticate_with_key`).
//! - W3.2(b1)-impl-channel: `open_raw_channel` opens a channel session,
//!   execs the remote command, and wraps it in [`RusshRawStream`].
//! - W3.2(b1)-impl-raw-stream: [`RawByteStream`] impl driven by
//!   [`russh::ChannelMsg::Data`] / `Eof` / `ExitStatus` events.
//! - W3.2(b1)-impl-trait: full [`RemoteShellTransport`] +
//!   [`RawRemoteShellTransport`] impls so the driver can consume this as
//!   a drop-in alternative to `SshRemoteShellTransport`.
//! - W3.2(b1)-tests: smoke + 4 live tests against the Docker key-auth lane.
//!
//! Spec dettagliata: `docs/dev/roadmap/APPENDIX-C-Y-D/APPENDIX-Y/tasks/2026-05-01_P3-T01_W3_2b1_Spec_Session_Caching.md`.

#![cfg(feature = "aerorsync")]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use crate::aerorsync::ssh_transport::SshTransportConfig;
use crate::aerorsync::transport::{
    CancelHandle, RawByteStream, RawRemoteShellTransport, RemoteCommandOutput, RemoteExecRequest,
    RemoteShellTransport, TransportProbe,
};
use crate::aerorsync::types::AerorsyncError;

/// russh-based remote shell transport that holds one long-lived SSH session
/// and serves N channel-exec calls over it.
///
/// Cheap to clone: the [`russh::client::Handle`] lives behind an `Arc`, so
/// `share_session` produces a view that points at the same session — no new
/// handshake. Drop the last clone and the session closes.
///
/// W3.2(b1) scaffold: the struct fields are populated, but the `connect` /
/// `open_raw_channel` paths return `Err(transport)` until the impl steps
/// land. The single-shot path on `SshRemoteShellTransport` is unaffected.
pub struct RusshSessionTransport {
    /// Long-lived russh client handle. `Some` after a successful
    /// [`Self::connect`]. Wrapped in an `Arc` for cheap `share_session`
    /// clones across `do_upload` / `do_download` invocations within one
    /// batch.
    ///
    /// Concrete type elided in the scaffold to keep the module compiling
    /// without russh handler boilerplate. Implementation step replaces
    /// this with `Option<Arc<russh::client::Handle<RusshHandler>>>`.
    handle: Arc<HandleSlot>,
    /// Cooperative cancel flag, shared with all `share_session` clones.
    cancel_flag: Arc<AtomicBool>,
    /// Counter incremented by every successful `connect()`. Production
    /// usage: `BatchStats.session_count = handshake_count` at finalize.
    handshake_count: Arc<AtomicU32>,
    /// Stored for reconnect on transient failure (W3.2(b2) batch logic).
    config: SshTransportConfig,
}

/// Internal placeholder for the russh handle. Replaced in the impl step
/// with `Option<russh::client::Handle<RusshHandler>>` guarded by an async
/// mutex (russh `Handle` is `!Sync` for some operations — confirmed by
/// `providers::sftp` patterns).
///
/// Kept as a separate type so the impl step is a localized refactor.
#[derive(Default)]
struct HandleSlot {
    // TODO(W3.2(b1)-impl-connect): replace `_placeholder` with
    // `inner: tokio::sync::Mutex<Option<russh::client::Handle<RusshHandler>>>`.
    _placeholder: (),
}

impl RusshSessionTransport {
    /// Open one SSH session via russh: TCP connect + handshake + key auth +
    /// host key policy check. Mirrors the auth shape of
    /// `providers::sftp::authenticate_with_key`.
    ///
    /// Idempotent in design: a future caller-side `ensure_session()` will
    /// short-circuit when the handle is already populated. For the
    /// scaffold the call always returns an error.
    pub async fn connect(config: SshTransportConfig) -> Result<Self, AerorsyncError> {
        // Construct the empty shell so callers can reason about the type
        // surface even before the impl lands. Real connect path:
        // 1. russh::client::Config { inactivity_timeout, keepalive_interval }
        // 2. russh::client::connect(Arc::new(config), addr, RusshHandler::with_policy(...))
        // 3. handle.authenticate_publickey(user, pkey).await
        // 4. enforce_host_key_policy(... via RusshHandler::check_server_key)
        let _stub = Self {
            handle: Arc::new(HandleSlot::default()),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            handshake_count: Arc::new(AtomicU32::new(0)),
            config,
        };
        Err(AerorsyncError::transport(
            "RusshSessionTransport::connect not implemented yet (W3.2(b1)-impl-connect)",
        ))
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
    /// W3.2(b1) scaffold: returns `Err(transport)`. The impl step calls
    /// `handle.channel_open_session().await` + `channel.exec(cmd).await`
    /// + wraps the channel into `RusshRawStream`.
    pub async fn open_raw_channel(
        &self,
        _request: RemoteExecRequest,
    ) -> Result<RusshRawStream, AerorsyncError> {
        Err(AerorsyncError::transport(
            "RusshSessionTransport::open_raw_channel not implemented yet \
             (W3.2(b1)-impl-channel)",
        ))
    }

    /// Cooperative cancel. Flips the flag and (when wired) calls
    /// `handle.disconnect(...)` to teardown channel + session.
    pub async fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::SeqCst);
        // TODO(W3.2(b1)-impl-channel): handle.disconnect(Disconnect::ByApplication, "", "en").await
    }

    /// Tear down the SSH session. Idempotent.
    pub async fn close(&self) -> Result<(), AerorsyncError> {
        // TODO(W3.2(b1)-impl-channel): drop the inner Mutex<Handle> if Some.
        // For now: succeed unconditionally so the batch finalize path can
        // be developed independently of close behavior.
        Ok(())
    }
}

/// Channel-backed [`RawByteStream`] returned by
/// [`RusshSessionTransport::open_raw_channel`].
///
/// W3.2(b1) scaffold: the channel field is elided. The impl step holds
/// `russh::Channel<russh::client::Msg>` plus a `pending: Vec<u8>` buffer
/// (russh delivers `ChannelMsg::Data` chunks that may exceed the caller's
/// `max` parameter; surplus bytes are stashed for the next read).
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
            host_key_policy: crate::aerorsync::ssh_transport::SshHostKeyPolicy::AcceptAny,
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

    #[tokio::test]
    async fn scaffold_connect_returns_typed_unimplemented_error() {
        let cfg = dummy_config();
        match RusshSessionTransport::connect(cfg).await {
            Err(AerorsyncError { .. }) => {
                // Expected — scaffold path. Once the impl step lands,
                // this test will be replaced with a Docker live test.
            }
            Ok(_) => panic!(
                "scaffold connect should not succeed; if this fails, the \
                 impl step has landed and the test must be updated"
            ),
        }
    }

    #[tokio::test]
    async fn scaffold_share_session_clones_arc_state() {
        // Build a stub transport via the same field shape connect uses.
        // We do not call `connect` because the scaffold rejects it; this
        // exercises only the share_session bookkeeping.
        let original = RusshSessionTransport {
            handle: Arc::new(HandleSlot::default()),
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
}
