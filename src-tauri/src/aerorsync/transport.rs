//! Transport abstraction for the Strada C native rsync prototype.
//!
//! The first native subset targets remote-shell mode: a single SSH exec call
//! that opens a bidirectional byte stream. Real SSH is deliberately not wired
//! here yet. The traits live here; one mock implementation lives in `mock.rs`.

use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::aerorsync::shell_escape::shell_escape_posix;
use crate::aerorsync::types::{AerorsyncError, ProtocolVersion};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportProbe {
    pub remote_banner: String,
    pub protocol: ProtocolVersion,
    pub supports_remote_shell: bool,
}

/// Describes the remote invocation to spawn over SSH exec.
///
/// In real rsync remote-shell mode this becomes, for upload:
/// `rsync --server -logDtprze.iLsfxCIvu --stats . /workspace/upload/target.bin`
/// and for download:
/// `rsync --server --sender -logDtprze.iLsfxCIvu . /workspace/download/target.bin`
///
/// See `fixtures::UPLOAD_REMOTE_COMMAND` / `DOWNLOAD_REMOTE_COMMAND`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteExecRequest {
    pub program: String,
    pub args: Vec<String>,
    pub environment: Vec<(String, String)>,
}

impl RemoteExecRequest {
    pub fn full_command_line(&self) -> String {
        let mut parts = Vec::with_capacity(1 + self.args.len());
        parts.push(shell_escape_posix(&self.program));
        parts.extend(self.args.iter().map(|arg| shell_escape_posix(arg)));
        parts.join(" ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCommandOutput {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Bidirectional framed byte stream returned by the transport layer.
///
/// Implementations are responsible for whatever low-level SSH channel handling
/// is needed. The prototype mock is single-threaded and in-process.
#[async_trait]
pub trait BidirectionalByteStream: Send {
    async fn write_frame(&mut self, frame: &[u8]) -> Result<(), AerorsyncError>;
    async fn read_frame(&mut self) -> Result<Vec<u8>, AerorsyncError>;
    async fn shutdown(&mut self) -> Result<(), AerorsyncError>;
}

#[async_trait]
pub trait RemoteShellTransport: Send + Sync {
    type Stream: BidirectionalByteStream + Send;

    async fn probe(&self) -> Result<TransportProbe, AerorsyncError>;

    async fn exec(&self, request: RemoteExecRequest)
        -> Result<RemoteCommandOutput, AerorsyncError>;

    async fn open_stream(&self, request: RemoteExecRequest)
        -> Result<Self::Stream, AerorsyncError>;

    async fn cancel(&self) -> Result<(), AerorsyncError>;

    /// Returns a lightweight cancel handle that callers can drive from an
    /// async `select!` without having to hold the whole transport.
    ///
    /// Default implementation returns an inert handle: a mock or a transport
    /// that has no meaningful cancellation semantics (yet) still satisfies
    /// the contract without opening a race window. Real transports override
    /// this to share their internal `AtomicBool` + wake-up mechanism.
    fn cancel_handle(&self) -> CancelHandle {
        CancelHandle::inert()
    }
}

/// A clonable cancel handle that can be awaited or polled independently of
/// the transport's owned state. It is deliberately small: one flag plus an
/// optional type-erased "wake" closure.
///
/// `requested()` is the single source of truth; the wake closure only exists
/// to unblock a sync worker stuck in a blocking read. If the closure is
/// absent, the handle behaves as a pure best-effort flag: good for mocks.
#[derive(Clone)]
pub struct CancelHandle {
    flag: Arc<AtomicBool>,
    waker: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl CancelHandle {
    pub fn inert() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
            waker: None,
        }
    }

    pub fn new(flag: Arc<AtomicBool>, waker: Option<Arc<dyn Fn() + Send + Sync>>) -> Self {
        Self { flag, waker }
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        if let Some(waker) = self.waker.as_ref() {
            waker();
        }
    }

    pub fn requested(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

impl std::fmt::Debug for CancelHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelHandle")
            .field("requested", &self.requested())
            .field("has_waker", &self.waker.is_some())
            .finish()
    }
}

// =============================================================================
// S8i / A2.1: Raw byte-stream transport layer.
//
// `BidirectionalByteStream` above is length-prefixed (RSNP envelope). The
// native real-wire rsync driver needs raw bytes: framing is done by the
// `MuxHeader` layer inside the stream, not by the transport. We add a new
// trait `RawByteStream` for byte-level I/O, and a sub-trait
// `RawRemoteShellTransport: RemoteShellTransport` with an associated raw
// stream type + `open_raw_stream()`. This keeps the legacy RSNP path
// untouched; a transport may implement both traits to serve both drivers.
// =============================================================================

/// Raw, unframed bidirectional byte stream. `read_bytes(max)` returns up
/// to `max` bytes (short reads are valid and expected: SSH does not
/// guarantee a single read matches a message boundary). `write_bytes`
/// writes the whole slice. `shutdown` tears the remote end down.
#[async_trait]
pub trait RawByteStream: Send {
    async fn read_bytes(&mut self, max: usize) -> Result<Vec<u8>, AerorsyncError>;
    async fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), AerorsyncError>;
    async fn shutdown(&mut self) -> Result<(), AerorsyncError>;
}

/// Transport that can open a raw byte-stream session in addition to the
/// RSNP-framed one. Implemented by `MockRemoteShellTransport` (A2.1) and
/// `SshRemoteShellTransport` (A2.1 step 7). The legacy `driver.rs` path
/// only needs `RemoteShellTransport`: it never calls `open_raw_stream`.
#[async_trait]
pub trait RawRemoteShellTransport: RemoteShellTransport {
    type RawStream: RawByteStream + Send;

    async fn open_raw_stream(
        &self,
        request: RemoteExecRequest,
    ) -> Result<Self::RawStream, AerorsyncError>;
}
