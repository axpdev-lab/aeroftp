//! In-process mock transport for the Strada C native rsync prototype.
//!
//! `MockRemoteShellTransport` replays scripted phases of the wrapper transcript
//! without any real SSH dependency. It lets tests exercise:
//!   1. successful upload-side session
//!   2. successful download-side session
//!   3. at least one failure path (stream open failure, read failure, etc.)
//!
//! The mock is intentionally simple: a probe result, a scripted sequence of
//! inbound frames, and a capture of outbound frames.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use std::sync::atomic::{AtomicBool, Ordering};

use crate::rsync_native_proto::transport::{
    BidirectionalByteStream, RawByteStream, RawRemoteShellTransport, RemoteCommandOutput,
    RemoteExecRequest, RemoteShellTransport, TransportProbe,
};
use crate::rsync_native_proto::types::{NativeRsyncError, ProtocolVersion};

/// Shared outbound frame buffer captured by a `MockStream`.
pub type OutboundBuffer = Arc<Mutex<Vec<Vec<u8>>>>;
/// Shared flag flipped when the `MockStream` is shut down.
pub type ShutdownFlag = Arc<Mutex<bool>>;

/// How the mock should behave when `open_stream` is called.
#[derive(Debug, Clone)]
pub enum OpenStreamBehavior {
    /// Return a stream pre-loaded with the given inbound frames.
    Success { inbound: Vec<Vec<u8>> },
    /// Fail with a transport error.
    Fail(String),
}

/// How the mock should behave when `read_frame` is called and there is no
/// scripted frame left.
#[derive(Debug, Clone, Copy)]
pub enum ReadExhaustedBehavior {
    /// Return a transport error ("remote closed").
    Error,
    /// Return an empty frame (caller must decide how to interpret).
    EmptyFrame,
}

#[derive(Debug, Clone)]
pub struct MockTransportConfig {
    pub probe: TransportProbe,
    pub exec_output: RemoteCommandOutput,
    pub stream_behavior: OpenStreamBehavior,
    pub read_exhausted: ReadExhaustedBehavior,
    /// A2.1 — optional raw-stream behaviour. `None` means `open_raw_stream`
    /// returns `NativeRsyncError::transport("raw stream not configured")`.
    pub raw_stream_behavior: Option<OpenRawStreamBehavior>,
}

/// How the mock should behave when `open_raw_stream` is called. The
/// inbound buffer is a flat `Vec<u8>` (no message framing — raw bytes)
/// and the outbound capture is likewise a flat `Vec<u8>`.
#[derive(Debug, Clone)]
pub enum OpenRawStreamBehavior {
    Success { inbound: Vec<u8> },
    Fail(String),
}

impl MockTransportConfig {
    pub fn healthy_upload() -> Self {
        Self {
            probe: TransportProbe {
                remote_banner: "rsync  version 3.2.7  protocol version 31".to_string(),
                protocol: ProtocolVersion::CURRENT,
                supports_remote_shell: true,
            },
            exec_output: RemoteCommandOutput {
                exit_code: 0,
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
            stream_behavior: OpenStreamBehavior::Success { inbound: Vec::new() },
            read_exhausted: ReadExhaustedBehavior::Error,
            raw_stream_behavior: None,
        }
    }

    pub fn healthy_download() -> Self {
        Self::healthy_upload()
    }

    pub fn stream_open_fails() -> Self {
        let mut base = Self::healthy_upload();
        base.stream_behavior = OpenStreamBehavior::Fail("stream open refused".to_string());
        base
    }

    /// A2.1 helper: configure the raw stream with a pre-loaded inbound
    /// byte buffer. Use for `open_raw_stream` happy-path tests.
    pub fn with_raw_inbound(mut self, inbound: Vec<u8>) -> Self {
        self.raw_stream_behavior = Some(OpenRawStreamBehavior::Success { inbound });
        self
    }
}

#[derive(Debug)]
pub struct MockStream {
    pub inbound: VecDeque<Vec<u8>>,
    pub outbound: OutboundBuffer,
    pub read_exhausted: ReadExhaustedBehavior,
    pub shutdown_called: ShutdownFlag,
}

impl MockStream {
    pub fn new(
        inbound: Vec<Vec<u8>>,
        read_exhausted: ReadExhaustedBehavior,
    ) -> (Self, OutboundBuffer, ShutdownFlag) {
        let outbound = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(Mutex::new(false));
        let stream = Self {
            inbound: VecDeque::from(inbound),
            outbound: outbound.clone(),
            read_exhausted,
            shutdown_called: shutdown.clone(),
        };
        (stream, outbound, shutdown)
    }
}

#[async_trait]
impl BidirectionalByteStream for MockStream {
    async fn write_frame(&mut self, frame: &[u8]) -> Result<(), NativeRsyncError> {
        let mut guard = self
            .outbound
            .lock()
            .map_err(|_| NativeRsyncError::transport("mock outbound mutex poisoned"))?;
        guard.push(frame.to_vec());
        Ok(())
    }

    async fn read_frame(&mut self) -> Result<Vec<u8>, NativeRsyncError> {
        if let Some(frame) = self.inbound.pop_front() {
            return Ok(frame);
        }
        match self.read_exhausted {
            ReadExhaustedBehavior::Error => Err(NativeRsyncError::transport(
                "mock inbound exhausted: simulated remote close",
            )),
            ReadExhaustedBehavior::EmptyFrame => Ok(Vec::new()),
        }
    }

    async fn shutdown(&mut self) -> Result<(), NativeRsyncError> {
        let mut guard = self
            .shutdown_called
            .lock()
            .map_err(|_| NativeRsyncError::transport("mock shutdown mutex poisoned"))?;
        *guard = true;
        Ok(())
    }
}

pub struct MockRemoteShellTransport {
    pub config: MockTransportConfig,
    pub last_exec: Arc<Mutex<Option<RemoteExecRequest>>>,
    pub cancel_called: ShutdownFlag,
    pub last_outbound: Arc<Mutex<Option<OutboundBuffer>>>,
    pub last_shutdown: Arc<Mutex<Option<ShutdownFlag>>>,
    /// A2.1 — capture of the flat byte buffer written to the most recent
    /// raw stream. None until `open_raw_stream` succeeds.
    pub last_raw_outbound: Arc<Mutex<Option<RawOutboundBuffer>>>,
    pub last_raw_shutdown: Arc<Mutex<Option<ShutdownFlag>>>,
}

impl MockRemoteShellTransport {
    pub fn new(config: MockTransportConfig) -> Self {
        Self {
            config,
            last_exec: Arc::new(Mutex::new(None)),
            cancel_called: Arc::new(Mutex::new(false)),
            last_outbound: Arc::new(Mutex::new(None)),
            last_shutdown: Arc::new(Mutex::new(None)),
            last_raw_outbound: Arc::new(Mutex::new(None)),
            last_raw_shutdown: Arc::new(Mutex::new(None)),
        }
    }

    pub fn captured_outbound(&self) -> Vec<Vec<u8>> {
        let outer = self.last_outbound.lock().unwrap();
        match outer.as_ref() {
            Some(arc) => arc.lock().unwrap().clone(),
            None => Vec::new(),
        }
    }

    pub fn shutdown_was_called(&self) -> bool {
        let outer = self.last_shutdown.lock().unwrap();
        match outer.as_ref() {
            Some(arc) => *arc.lock().unwrap(),
            None => false,
        }
    }

    pub fn cancel_was_called(&self) -> bool {
        *self.cancel_called.lock().unwrap()
    }

    pub fn last_exec_request(&self) -> Option<RemoteExecRequest> {
        self.last_exec.lock().unwrap().clone()
    }
}

#[async_trait]
impl RemoteShellTransport for MockRemoteShellTransport {
    type Stream = MockStream;

    async fn probe(&self) -> Result<TransportProbe, NativeRsyncError> {
        Ok(self.config.probe.clone())
    }

    async fn exec(
        &self,
        request: RemoteExecRequest,
    ) -> Result<RemoteCommandOutput, NativeRsyncError> {
        let mut guard = self.last_exec.lock().unwrap();
        *guard = Some(request);
        Ok(self.config.exec_output.clone())
    }

    async fn open_stream(
        &self,
        request: RemoteExecRequest,
    ) -> Result<Self::Stream, NativeRsyncError> {
        let mut exec_guard = self.last_exec.lock().unwrap();
        *exec_guard = Some(request);
        drop(exec_guard);

        match &self.config.stream_behavior {
            OpenStreamBehavior::Success { inbound } => {
                let (stream, outbound, shutdown) =
                    MockStream::new(inbound.clone(), self.config.read_exhausted);
                *self.last_outbound.lock().unwrap() = Some(outbound);
                *self.last_shutdown.lock().unwrap() = Some(shutdown);
                Ok(stream)
            }
            OpenStreamBehavior::Fail(reason) => {
                Err(NativeRsyncError::transport(reason.clone()))
            }
        }
    }

    async fn cancel(&self) -> Result<(), NativeRsyncError> {
        *self.cancel_called.lock().unwrap() = true;
        Ok(())
    }
}

// =============================================================================
// S8i / A2.1 — Raw byte-stream mock.
//
// `MockRawStream` is the byte-raw counterpart to `MockStream`. It holds a
// flat `Vec<u8>` inbound buffer with a cursor and a shared outbound
// `Vec<u8>` capture. `read_bytes(max)` returns up to `max` bytes,
// honouring SSH's "short-read is valid" semantics. An optional cancel
// flag is observed on each read and reported as `NativeRsyncError::
// Cancelled` so driver-level cancel tests can pin the typed error.
// =============================================================================

/// Shared outbound byte capture for a `MockRawStream`.
pub type RawOutboundBuffer = Arc<Mutex<Vec<u8>>>;

pub struct MockRawStream {
    inbound: Vec<u8>,
    inbound_cursor: usize,
    outbound: RawOutboundBuffer,
    shutdown_called: ShutdownFlag,
    /// Optional flag consulted on each read/write. When it flips to
    /// `true` the operation returns `NativeRsyncError::Cancelled`.
    cancel_flag: Option<Arc<AtomicBool>>,
}

impl MockRawStream {
    fn new(inbound: Vec<u8>) -> (Self, RawOutboundBuffer, ShutdownFlag) {
        let outbound: RawOutboundBuffer = Arc::new(Mutex::new(Vec::new()));
        let shutdown: ShutdownFlag = Arc::new(Mutex::new(false));
        let stream = Self {
            inbound,
            inbound_cursor: 0,
            outbound: outbound.clone(),
            shutdown_called: shutdown.clone(),
            cancel_flag: None,
        };
        (stream, outbound, shutdown)
    }

    /// Attach a cancel flag that will be observed by every subsequent
    /// `read_bytes` / `write_bytes`. Used by driver tests that need to
    /// inject a cancellation between transport round-trips.
    pub fn attach_cancel_flag(&mut self, flag: Arc<AtomicBool>) {
        self.cancel_flag = Some(flag);
    }

    fn check_cancel(&self, op: &'static str) -> Result<(), NativeRsyncError> {
        if let Some(flag) = &self.cancel_flag {
            if flag.load(Ordering::SeqCst) {
                return Err(NativeRsyncError::cancelled(format!(
                    "mock raw stream cancelled before {op}"
                )));
            }
        }
        Ok(())
    }
}

#[async_trait]
impl RawByteStream for MockRawStream {
    async fn read_bytes(&mut self, max: usize) -> Result<Vec<u8>, NativeRsyncError> {
        self.check_cancel("read_bytes")?;
        if self.inbound_cursor >= self.inbound.len() {
            return Err(NativeRsyncError::transport(
                "mock raw inbound exhausted: simulated remote close",
            ));
        }
        let available = self.inbound.len() - self.inbound_cursor;
        let take = max.min(available);
        let slice = self.inbound[self.inbound_cursor..self.inbound_cursor + take].to_vec();
        self.inbound_cursor += take;
        Ok(slice)
    }

    async fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), NativeRsyncError> {
        self.check_cancel("write_bytes")?;
        let mut guard = self
            .outbound
            .lock()
            .map_err(|_| NativeRsyncError::transport("mock raw outbound mutex poisoned"))?;
        guard.extend_from_slice(bytes);
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), NativeRsyncError> {
        let mut guard = self
            .shutdown_called
            .lock()
            .map_err(|_| NativeRsyncError::transport("mock raw shutdown mutex poisoned"))?;
        *guard = true;
        Ok(())
    }
}

impl MockRemoteShellTransport {
    /// A2.1 helper: collect the flat byte capture produced by the most
    /// recent `open_raw_stream`. Returns an empty `Vec` if no raw stream
    /// has been opened yet.
    pub fn captured_raw_outbound(&self) -> Vec<u8> {
        let guard = self.last_raw_outbound.lock().unwrap();
        match guard.as_ref() {
            Some(arc) => arc.lock().unwrap().clone(),
            None => Vec::new(),
        }
    }
}

#[async_trait]
impl RawRemoteShellTransport for MockRemoteShellTransport {
    type RawStream = MockRawStream;

    async fn open_raw_stream(
        &self,
        request: RemoteExecRequest,
    ) -> Result<Self::RawStream, NativeRsyncError> {
        {
            let mut exec_guard = self.last_exec.lock().unwrap();
            *exec_guard = Some(request);
        }
        match &self.config.raw_stream_behavior {
            Some(OpenRawStreamBehavior::Success { inbound }) => {
                let (stream, outbound, shutdown) = MockRawStream::new(inbound.clone());
                *self.last_raw_outbound.lock().unwrap() = Some(outbound);
                *self.last_raw_shutdown.lock().unwrap() = Some(shutdown);
                Ok(stream)
            }
            Some(OpenRawStreamBehavior::Fail(reason)) => {
                Err(NativeRsyncError::transport(reason.clone()))
            }
            None => Err(NativeRsyncError::transport(
                "raw stream not configured on MockTransportConfig",
            )),
        }
    }
}
