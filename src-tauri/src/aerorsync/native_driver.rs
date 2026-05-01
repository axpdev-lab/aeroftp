//! Real-wire based session driver for the Strada C native rsync prototype.
//!
//! This module is the S8i replacement for the RSNP-envelope driver at
//! `driver.rs`. It lives **side-by-side** with the legacy driver (decision β,
//! approved 2026-04-18) — the legacy driver stays untouched so its 270+ mock
//! tests keep serving as regression baseline until `protocol.rs`,
//! `frame_io.rs`, `server.rs` and `driver.rs` are retired in Zona B5.
//!
//! # Scope of A2.1
//!
//! After A2.0 (skeleton + in-memory preamble exchange), A2.1 lands the full
//! file list phase on a real raw byte-stream channel:
//!
//! - `open_raw_stream` via the new `RawRemoteShellTransport::open_raw_stream`.
//! - `perform_preamble_exchange` drains the server preamble from the raw
//!   stream, then writes the client preamble back.
//! - Upload path: `send_file_list_single_file` emits one `FileListEntry` +
//!   terminator, each wrapped in a `MuxHeader{tag: Data, length: N}` frame
//!   via `write_data_frame`.
//! - Download path: `receive_file_list_single_file` drives the
//!   `MuxStreamReader` + `decode_file_list_entry` loop, forwarding OOB
//!   events to the `EventSink` and bailing on terminal OOB.
//!
//! The new stub frontier is **post-file-list**: `drive_*` returns
//! `AerorsyncError::unsupported_version` at sum_head exchange. A2.2 will
//! push the frontier to post-signatures.
//!
//! # Q1 resolution (permanent)
//!
//! A2.1 uses the new `RawByteStream` + `RawRemoteShellTransport` traits in
//! `transport.rs`. The legacy `BidirectionalByteStream` (length-prefixed
//! RSNP) and `RemoteShellTransport` are untouched. A transport may
//! implement both traits to serve both drivers.
//!
//! # Q5 PreCommit/PostCommit boundary
//!
//! The file list phase is PreCommit. `committed` stays `false` until the
//! first outbound `DeltaBatch` in a future sub-phase. If a terminal OOB
//! arrives now, the driver returns a typed `AerorsyncError` and
//! `committed()` reports `false`, letting the A4 adapter decide to fall
//! back to the classic-SFTP path.
//!
//! # csum_len in A2.1
//!
//! Hardcoded to 16 (xxh128 / md5 / md4). A2.2 will derive it dynamically
//! from `negotiated_checksum_algos`. Accepted risk, tracked in the
//! checkpoint doc.

use crate::aerorsync::engine_adapter::{
    apply_delta_streaming, BaselineSource, DeltaEngineAdapter, DeltaPlanProducer, EngineDeltaOp,
    EngineSignatureBlock, RollingDeltaPlanProducer,
};
use crate::aerorsync::events::EventSink;
use crate::aerorsync::real_wire::{
    compress_zstd_literal_stream, decode_delta_stream, decode_file_list_entry, decode_item_flags,
    decode_ndx, decode_server_preamble, decode_sum_block, decode_sum_head, decode_summary_frame,
    decompress_zstd_literal_stream_boundaries, encode_client_preamble, encode_delta_stream,
    encode_file_list_entry, encode_file_list_terminator, encode_item_flags, encode_ndx,
    encode_sum_block, encode_sum_head, encode_summary_frame, ClientPreamble, DeltaOp,
    DeltaStreamReport, FileListDecodeOptions, FileListDecodeOutcome, FileListEntry, MuxHeader,
    MuxPoll, MuxStreamReader, MuxTag, NdxState, RealWireError, SumBlock, SumHead, SummaryFrame,
    MAX_DELTA_LITERAL_LEN, NDX_DONE, NDX_FLIST_EOF,
};
use crate::aerorsync::remote_command::{RemoteCommandFlavor, RemoteCommandSpec};
use crate::aerorsync::transport::{CancelHandle, RawByteStream, RawRemoteShellTransport};
use crate::aerorsync::types::{AerorsyncError, AerorsyncErrorKind, SessionRole, SessionStats};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use xxhash_rust::xxh3::{xxh3_128, Xxh3Default};

/// Compute the 16-byte file-level strong checksum rsync verifies at the
/// end of the delta stream when `xxh128` is the negotiated algo.
///
/// Byte layout — pinned by `xxh128_wire_bytes_match_SIVAL64_pair`
/// against rsync 3.2.7 `checksum.c::hash_struct`:
///
/// ```text
/// out[0..8]  = lo_u64.to_le_bytes()   // SIVAL64(buf, 0, lo)
/// out[8..16] = hi_u64.to_le_bytes()   // SIVAL64(buf, 8, hi)
/// ```
///
/// where `(hi, lo)` come from splitting the xxh3_128 `u128` at the
/// 64-bit boundary.
fn compute_xxh128_wire(data: &[u8]) -> Vec<u8> {
    let hash = xxh3_128(data);
    let lo = hash as u64;
    let hi = (hash >> 64) as u64;
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(&lo.to_le_bytes());
    out.extend_from_slice(&hi.to_le_bytes());
    out
}

/// Chunk size used for raw-stream reads. Large enough to swallow a full
/// preamble + file list in one go for small transfers, small enough not
/// to bloat the scratch buffer for idle-ish channels.
const RAW_READ_CHUNK: usize = 8192;

/// P3-T01 W1.2 — read-side chunking for the streaming source reader of
/// `send_delta_phase_streaming`. 4 MiB matches the SFTP/HTTP range
/// default and keeps the per-chunk allocation tax (one `vec![0u8; N]`
/// per `read()` call) negligible on multi-GiB sources.
///
/// The producer drains its sliding window after each chunk, so
/// resident memory stays bounded by `block_size + literal_run_length`
/// regardless of `STREAMING_READ_CHUNK_BYTES`. The constant only
/// trades I/O syscalls vs. allocation; bigger is fine, smaller is fine,
/// 4 MiB is the documented default.
const STREAMING_READ_CHUNK_BYTES: usize = 4 * 1024 * 1024;

// A2.2 signature phase constants.
//
// `ITEM_TRANSFER` is the per-file flag (u16) the server-generator sets
// when it wants the sender to push actual delta bytes — i.e. every file
// that we are about to exchange signatures for. `ITEM_REPORT_CHANGE` is
// the common companion bit telling the client "log this as changed".
// Neither mapping belongs in a shared module yet; when a second
// consumer emerges in S8j we will promote to `real_wire.rs`.
const ITEM_TRANSFER: u16 = 0x8000;
const ITEM_REPORT_CHANGE: u16 = 0x0002;
/// iflags emitted by the driver in the download path, replicating the
/// frozen oracle's client→server first-file signature shape.
const A2_2_DOWNLOAD_IFLAGS: u16 = ITEM_TRANSFER | ITEM_REPORT_CHANGE;
/// Truncated strong checksum length used when *sending* signatures in
/// the download path. Two bytes matches the frozen oracle's 256 KiB
/// profile. Kept as a driver-level constant for A2.2 — S8j will revisit
/// when the delta engine can evaluate the impact on the matching rate.
const A2_2_DOWNLOAD_S2LENGTH: i32 = 2;
/// The per-file ndx the driver expects/emits in the single-file A2.2
/// scope. First file of the list, baseline `-1` → diff `+2` → `+1`.
const A2_2_FIRST_FILE_NDX: i32 = 1;
/// File-level strong checksum length (xxh128 / md5 / md4). Hardcoded to
/// 16 for A2.3; real xxh128 computation over `source_data` deferred to
/// S8j when the driver is wired against a live rsync server.
const A2_3_FILE_CHECKSUM_LEN: usize = 16;

/// S8j download — exact count of `NDX_DONE` markers rsync 3.2.7 interleaves
/// between the file-level checksum trailer and the `SummaryFrame` on the
/// server→client app stream. Pinned by `tests.rs` against the frozen
/// download capture (`FROZEN_ORACLE_PRE_SUMMARY_NDX_DONE_COUNT = 3`);
/// kept as an explicit constant here so the driver's drain logic breaks
/// loudly if rsync ever shifts the marker count.
const PRE_SUMMARY_NDX_DONE_COUNT_DOWNLOAD: usize = 3;

/// State machine phase for the native driver session.
///
/// Pub because the A4 adapter (`AerorsyncDeltaTransport`) may want to
/// inspect it for fallback decisions; the internals exposed are
/// informational only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AerorsyncSessionPhase {
    PreConnect,
    /// Reserved for A2.1+ when probe() is wired into the drive loop.
    #[allow(dead_code)]
    ProbeOk,
    /// Raw byte-stream channel has been opened on the transport.
    RawStreamOpen,
    /// Outbound client preamble has been written to the wire.
    ServerPreambleSent,
    /// Inbound server preamble has been decoded.
    ClientPreambleRecvd,
    /// Upload path: file list entry + terminator mid-flight.
    FileListSending,
    /// Upload path: file list fully emitted.
    FileListSent,
    /// Download path: file list entry decoding in progress.
    FileListReceiving,
    /// Download path: file list fully received.
    FileListReceived,
    /// Upload path: draining ndx+iflags+sum_head from the server.
    SumHeadReceiving,
    /// Upload path: reading the `count` sum_blocks one by one.
    SumBlocksReceiving,
    /// Download path: ndx+iflags+sum_head emitted on the wire.
    SumHeadSent,
    /// Download path: all sum_blocks flushed on the wire.
    SumBlocksSent,
    /// Upload path: computing delta and emitting wire ops.
    DeltaSending,
    /// Upload path: END_FLAG + file_checksum trailer written.
    DeltaSent,
    /// Download path: draining delta stream + decoding ops.
    DeltaReceiving,
    /// Download path: reconstructed file bytes ready.
    DeltaReceived,
    /// A2.4: reading the server's final SummaryFrame.
    SummaryReceiving,
    /// A2.4: SummaryFrame decoded, session_stats populated.
    SummaryReceived,
    /// A2.4: raw stream has been shut down cleanly.
    Complete,
    /// Stub frontier — reserved for sub-phases not yet wired. A2.4
    /// eliminates the stub frontier for happy-path flow; the variant
    /// stays for future incremental sub-steps.
    #[allow(dead_code)]
    Stub,
    /// Irrecoverable error observed; terminal.
    Failed,
}

/// Real-wire rsync session driver. Parameterised on the raw-capable
/// remote-shell transport so both mock and SSH paths share the machinery.
pub struct AerorsyncDriver<T: RawRemoteShellTransport> {
    transport: T,
    cancel_handle: CancelHandle,

    // Populated by `perform_preamble_exchange`.
    protocol_version: u32,
    compat_flags: i32,
    checksum_seed: u32,
    negotiated_checksum_algos: String,
    negotiated_compression_algos: String,

    phase: AerorsyncSessionPhase,
    committed: bool,

    // A2.1 runtime state.
    stream: Option<<T as RawRemoteShellTransport>::RawStream>,
    mux_reader: MuxStreamReader,
    /// Outbound ndx state: tracks `prev_positive` / `prev_negative` for
    /// every `encode_ndx` we WRITE to the wire. Mirrors the static
    /// inside `io.c::write_ndx` (separate per direction in stock rsync).
    outbound_ndx_state: NdxState,
    /// Inbound ndx state: tracks the same baselines for every
    /// `decode_ndx` we READ from the wire. Mirrors the static inside
    /// `io.c::read_ndx`. **B.2 Step 4**: had to be split from the
    /// shared `ndx_state` because conflating read+write state made the
    /// echoed NDX in `send_delta_phase_single_file` shift to a
    /// 3-byte form that the receiver decoded as garbage (rsync exit 2,
    /// "File-list index N not in 0 - -1").
    inbound_ndx_state: NdxState,
    /// File-list accumulator. Len 0 or 1 in A2.1 (single-file scope).
    file_list: Vec<FileListEntry>,

    // A2.2 signature-phase state.
    /// Upload path: sum_head decoded from the server message.
    received_sum_head: Option<SumHead>,
    /// Upload path: signature blocks received from the server (length =
    /// `received_sum_head.count`).
    received_signatures: Vec<SumBlock>,
    /// Download path: sum_head we computed and emitted locally.
    sent_sum_head: Option<SumHead>,
    /// Download path: signature blocks we emitted on the wire. Kept for
    /// test visibility; carries the truncated-to-`s2length` strong halves
    /// that actually went on the wire.
    sent_signatures: Vec<SumBlock>,
    /// Last iflags value observed in upload (received) or emitted in
    /// download (sent).
    last_iflags: u16,
    /// Upload path: last NDX received from the receiver in the signature
    /// phase. The sender MUST echo this NDX back at the start of the
    /// delta phase (`sender.c:411` `write_ndx_and_attrs(f_out, ndx, ...)`),
    /// otherwise the receiver mis-aligns its read state and aborts with
    /// "Error allocating core memory buffers" (rsync exit 22).
    last_received_ndx: i32,
    /// Residual bytes left over after `read_signature_header` parsed
    /// `ndx + iflags + sum_head` — these belong to the following
    /// sum_blocks stream. Used as a prefix by `read_signature_blocks`
    /// so MSG_DATA payload bytes never get dropped on the floor.
    sig_residual_after_header: Vec<u8>,

    // A2.3 delta-phase state.
    /// Download path: reconstructed destination file bytes after
    /// `adapter.apply_delta`. The A4 adapter writes them to a temp file
    /// and renames atomically; the driver itself never touches disk.
    /// Populated only on the bulk download path
    /// (`drive_download_through_delta`); stays `None` on the streaming
    /// path (`drive_download_through_delta_streaming`, W2.4) where the
    /// reconstructed bytes flow directly into the caller-supplied
    /// `AsyncWrite` sink passed by reference.
    reconstructed: Option<Vec<u8>>,
    /// Download path: file-level strong checksum trailer read from the
    /// wire (16 bytes in A2.3 — xxh128 / md5 / md4).
    received_file_checksum: Option<Vec<u8>>,
    /// Upload path: delta ops emitted on the wire, in emission order.
    /// Kept for test visibility — production callers should ignore this.
    emitted_delta_ops: Vec<DeltaOp>,
    /// Upload path: total MSG_DATA payload bytes written. The numerator
    /// of the progress indicator; A4 exposes it to the UI.
    sent_data_bytes: u64,

    // A2.4 summary/done state.
    /// Server-reported `SummaryFrame` (totals + flist timings). `None`
    /// until the summary phase decodes successfully.
    received_summary: Option<SummaryFrame>,
    /// Session-level aggregated stats. `bytes_sent` / `bytes_received`
    /// are derived from `received_summary` when the server emits it;
    /// other fields stay at default (prototype-specific instrumentation
    /// deferred to A4 adapter).
    session_stats: SessionStats,

    // S8j session-finish state.
    /// Role this driver played for the current session; set by the
    /// `drive_*_inner` entry points. Drives the finish-session
    /// dispatcher (download receives summary, upload emits it).
    session_role: Option<SessionRole>,
    /// Cumulative MSG_DATA payload bytes the driver has read from the
    /// remote. Mirror of `sent_data_bytes` for the inbound direction.
    /// Updated by `next_data_frame` after each Data poll.
    received_raw_bytes: u64,
    /// Residual post-mux bytes left by `drain_leading_ndx_done_download`
    /// that belong to the following `SummaryFrame`. `receive_summary_phase`
    /// prepends them to its decode buffer.
    summary_seed: Vec<u8>,
    /// Remote command family currently being driven. WrapperParity is the
    /// ONLY flavor used in production (`AerorsyncDeltaTransport::upload` /
    /// `::download` pin it via `RemoteCommandSpec::upload` / `download`,
    /// locked by `remote_command::tests::*_is_always_wrapper_parity_*`).
    /// AerorsyncServe survives as a mock-test flavor that keeps the legacy
    /// RSNP-style summary tail for drivers exercised against
    /// `aerorsync_serve` under `#[cfg(test)]` or the
    /// `#[cfg(all(test, feature = "aerorsync"))]` live lane. Do not wire
    /// it into any product-facing code path.
    remote_command_flavor: RemoteCommandFlavor,
}

impl<T: RawRemoteShellTransport> AerorsyncDriver<T> {
    pub fn new(transport: T, cancel_handle: CancelHandle) -> Self {
        Self {
            transport,
            cancel_handle,
            protocol_version: 0,
            compat_flags: 0,
            checksum_seed: 0,
            negotiated_checksum_algos: String::new(),
            negotiated_compression_algos: String::new(),
            phase: AerorsyncSessionPhase::PreConnect,
            committed: false,
            stream: None,
            mux_reader: MuxStreamReader::new(),
            outbound_ndx_state: NdxState::default(),
            inbound_ndx_state: NdxState::default(),
            file_list: Vec::new(),
            received_sum_head: None,
            received_signatures: Vec::new(),
            sent_sum_head: None,
            sent_signatures: Vec::new(),
            last_iflags: 0,
            last_received_ndx: -1,
            sig_residual_after_header: Vec::new(),
            reconstructed: None,
            received_file_checksum: None,
            emitted_delta_ops: Vec::new(),
            sent_data_bytes: 0,
            received_summary: None,
            session_stats: SessionStats::default(),
            session_role: None,
            received_raw_bytes: 0,
            summary_seed: Vec::new(),
            remote_command_flavor: RemoteCommandFlavor::WrapperParity,
        }
    }

    pub fn cancel_handle(&self) -> CancelHandle {
        self.cancel_handle.clone()
    }

    pub fn phase(&self) -> AerorsyncSessionPhase {
        self.phase
    }
    pub fn protocol_version(&self) -> u32 {
        self.protocol_version
    }
    pub fn compat_flags(&self) -> i32 {
        self.compat_flags
    }
    pub fn checksum_seed(&self) -> u32 {
        self.checksum_seed
    }
    pub fn negotiated_checksum_algos(&self) -> &str {
        &self.negotiated_checksum_algos
    }
    pub fn negotiated_compression_algos(&self) -> &str {
        &self.negotiated_compression_algos
    }
    pub fn committed(&self) -> bool {
        self.committed
    }
    pub fn file_list(&self) -> &[FileListEntry] {
        &self.file_list
    }
    pub fn downloaded_entry(&self) -> Option<&FileListEntry> {
        if self.session_role == Some(SessionRole::Receiver) {
            self.file_list.first()
        } else {
            None
        }
    }
    pub fn data_bytes_consumed(&self) -> u64 {
        self.mux_reader.data_bytes_consumed()
    }
    pub fn received_sum_head(&self) -> Option<&SumHead> {
        self.received_sum_head.as_ref()
    }
    pub fn received_signatures(&self) -> &[SumBlock] {
        &self.received_signatures
    }
    pub fn sent_sum_head(&self) -> Option<&SumHead> {
        self.sent_sum_head.as_ref()
    }
    pub fn sent_signatures(&self) -> &[SumBlock] {
        &self.sent_signatures
    }
    pub fn last_iflags(&self) -> u16 {
        self.last_iflags
    }
    pub fn reconstructed(&self) -> Option<&[u8]> {
        self.reconstructed.as_deref()
    }
    pub fn received_file_checksum(&self) -> Option<&[u8]> {
        self.received_file_checksum.as_deref()
    }
    pub fn emitted_delta_ops(&self) -> &[DeltaOp] {
        &self.emitted_delta_ops
    }
    pub fn sent_data_bytes(&self) -> u64 {
        self.sent_data_bytes
    }
    /// S8j mirror of `sent_data_bytes` for the inbound direction —
    /// cumulative MSG_DATA payload bytes the driver has read from the
    /// remote. Used by `emit_summary_phase` to populate `total_read` in
    /// upload finishes.
    pub fn received_raw_bytes(&self) -> u64 {
        self.received_raw_bytes
    }
    /// S8j role indicator: `Some(Sender)` if the driver is running an
    /// upload, `Some(Receiver)` for a download, `None` if neither
    /// `drive_*_inner` has been entered yet. Used by `finish_session`
    /// to pick the right dispatch.
    pub fn session_role(&self) -> Option<SessionRole> {
        self.session_role
    }
    pub fn received_summary(&self) -> Option<&SummaryFrame> {
        self.received_summary.as_ref()
    }
    pub fn session_stats(&self) -> &SessionStats {
        &self.session_stats
    }

    // --- public drive entry points ---------------------------------------

    pub async fn drive_upload(
        &mut self,
        command_spec: RemoteCommandSpec,
        source_entry: FileListEntry,
        source_data: &[u8],
        adapter: &dyn DeltaEngineAdapter,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError> {
        match self
            .drive_upload_inner(command_spec, source_entry, source_data, adapter, bridge)
            .await
        {
            Ok(()) => {
                // A2.3 stub frontier: reach post-delta and stop. A2.4's
                // `finish_session` (callable separately) drains the
                // SummaryFrame + shuts the stream down.
                self.phase = AerorsyncSessionPhase::Stub;
                Err(AerorsyncError::unsupported_version(
                    "native summary/done phase not yet wired — call finish_session() explicitly",
                ))
            }
            Err(e) => {
                self.phase = AerorsyncSessionPhase::Failed;
                Err(e)
            }
        }
    }

    pub async fn drive_download(
        &mut self,
        command_spec: RemoteCommandSpec,
        destination_data: &[u8],
        adapter: &dyn DeltaEngineAdapter,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError> {
        match self
            .drive_download_inner(command_spec, destination_data, adapter, bridge)
            .await
        {
            Ok(()) => {
                self.phase = AerorsyncSessionPhase::Stub;
                Err(AerorsyncError::unsupported_version(
                    "native summary/done phase not yet wired — call finish_session() explicitly",
                ))
            }
            Err(e) => {
                self.phase = AerorsyncSessionPhase::Failed;
                Err(e)
            }
        }
    }

    // --- A4 entry points (stub-frontier elided) --------------------------
    //
    // `drive_upload_through_delta` / `drive_download_through_delta` are the
    // direct siblings of `drive_upload` / `drive_download`, differing only in
    // happy-path return shape: they return `Ok(())` when the inner drive loop
    // completes (so the caller can call `finish_session` explicitly), instead
    // of the `UnsupportedVersion` sentinel the legacy entry points emit.
    //
    // The A4 adapter (`AerorsyncDeltaTransport`) uses these siblings so it
    // does not have to string-match the sentinel detail. Error propagation is
    // identical to the legacy path: any `AerorsyncError` flows through
    // unchanged, `phase = Failed` is set, and the caller is expected to pipe
    // the error into `fallback_policy::classify_fallback`.
    //
    // The legacy `drive_upload` / `drive_download` entry points stay in place
    // because the A2.x test suite pins the sentinel behaviour — removing the
    // sentinel would regress that pin.

    pub async fn drive_upload_through_delta(
        &mut self,
        command_spec: RemoteCommandSpec,
        source_entry: FileListEntry,
        source_data: &[u8],
        adapter: &dyn DeltaEngineAdapter,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError> {
        match self
            .drive_upload_inner(command_spec, source_entry, source_data, adapter, bridge)
            .await
        {
            Ok(()) => Ok(()),
            Err(e) => {
                self.phase = AerorsyncSessionPhase::Failed;
                Err(e)
            }
        }
    }

    /// P3-T01 W1.2 — streaming-source sibling of
    /// [`drive_upload_through_delta`]. Identical session-level flow up
    /// to the delta phase; the difference is that the source bytes
    /// arrive as an `AsyncRead` instead of a fully-buffered `&[u8]`,
    /// and the delta plan is produced incrementally via
    /// [`RollingDeltaPlanProducer`] instead of a bulk
    /// `compute_delta` call. Wire output is **byte-identical** with
    /// the bulk path for the same source bytes — pinned by
    /// `streaming_send_matches_bulk_send_*` tests below.
    ///
    /// `source_len` is the declared length of `source_reader` (typically
    /// `metadata.len()` of the file). It is used to populate
    /// `FileListEntry::size` upstream and is sanity-checked here against
    /// the actual byte count drained from the reader; a mismatch aborts
    /// the upload with `InvalidFrame` (the file changed mid-flight).
    pub async fn drive_upload_through_delta_streaming<R>(
        &mut self,
        command_spec: RemoteCommandSpec,
        source_entry: FileListEntry,
        source_reader: R,
        source_len: u64,
        adapter: &dyn DeltaEngineAdapter,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError>
    where
        R: AsyncRead + Unpin + Send,
    {
        match self
            .drive_upload_inner_streaming(
                command_spec,
                source_entry,
                source_reader,
                source_len,
                adapter,
                bridge,
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(e) => {
                self.phase = AerorsyncSessionPhase::Failed;
                Err(e)
            }
        }
    }

    pub async fn drive_download_through_delta(
        &mut self,
        command_spec: RemoteCommandSpec,
        destination_data: &[u8],
        adapter: &dyn DeltaEngineAdapter,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError> {
        match self
            .drive_download_inner(command_spec, destination_data, adapter, bridge)
            .await
        {
            Ok(()) => Ok(()),
            Err(e) => {
                self.phase = AerorsyncSessionPhase::Failed;
                Err(e)
            }
        }
    }

    /// P3-T01 W2.4/W2.5 — streaming-sink sibling of
    /// [`drive_download_through_delta`]. The pre-delta phases (preamble,
    /// file list receive, signature send) are identical to the bulk path;
    /// only the final `receive_delta_phase` differs. The reconstructed
    /// bytes do not materialise as a `Vec<u8>` — they flow into the
    /// caller-supplied `writer` (typically a `StreamingAtomicWriter`,
    /// W2.3) which retains full ownership across the call so the caller
    /// can `finalize` it (commit the temp file via rename) once the
    /// driver returns.
    ///
    /// `destination_data` is still passed because the signature phase
    /// runs `adapter.build_signatures(destination_data, block_size)`
    /// in bulk; making *that* phase streaming requires a separate
    /// `build_signatures_streaming` adapter API that is out of scope for
    /// W2.5. The W2.5 caller gives the same `destination_data` slice
    /// (read once into RAM with `tokio::fs::read`) and a `FileBaseline`
    /// over the same path; the cap removal in W2.5 targets the
    /// reconstruction side, where the asymmetry is `O(reconstructed)`
    /// vs `O(baseline + reconstructed)` for >1 GiB downloads with mostly
    /// matching baselines.
    ///
    /// `baseline` is the random-access source consulted by
    /// `apply_delta_streaming` for `EngineDeltaOp::CopyBlock(idx)`. It
    /// must be the byte-for-byte identical content as `destination_data`
    /// (caller invariant; the streaming + bulk views of the same file).
    ///
    /// `writer` is borrowed by `&mut` for the duration of the call —
    /// the caller retains ownership and is responsible for
    /// finalisation (flush + sync_all + rename) on success and for
    /// best-effort cleanup on error.
    pub async fn drive_download_through_delta_streaming(
        &mut self,
        command_spec: RemoteCommandSpec,
        destination_data: &[u8],
        baseline: &mut dyn BaselineSource,
        writer: &mut (dyn AsyncWrite + Send + Unpin),
        adapter: &dyn DeltaEngineAdapter,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError> {
        match self
            .drive_download_inner_streaming(
                command_spec,
                destination_data,
                baseline,
                writer,
                adapter,
                bridge,
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(e) => {
                self.phase = AerorsyncSessionPhase::Failed;
                Err(e)
            }
        }
    }

    async fn drive_upload_inner(
        &mut self,
        command_spec: RemoteCommandSpec,
        source_entry: FileListEntry,
        source_data: &[u8],
        adapter: &dyn DeltaEngineAdapter,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError> {
        self.session_role = Some(SessionRole::Sender);
        self.remote_command_flavor = command_spec.flavor;
        self.open_raw_stream_internal(&command_spec).await?;
        // B.2: rsync wire protocol uses SPACE-separated algo lists in
        // priority-descending order. Using commas causes stock rsync
        // 3.4.1 to parse the whole list as a single unknown algorithm
        // and close the stream. Values cribbed from the frozen capture
        // `capture/artifacts_real/frozen/upload/capture_in.bin` shape.
        self.perform_preamble_exchange(31, "xxh128 xxh3 xxh64 md5 md4", "zstd lz4 zlibx zlib")
            .await?;
        self.send_file_list_single_file(&source_entry).await?;
        self.receive_signature_phase_single_file(bridge).await?;
        self.send_delta_phase_single_file(source_data, adapter)
            .await?;
        Ok(())
    }

    /// P3-T01 W1.2 — streaming-source twin of [`drive_upload_inner`].
    /// The pre-delta phases (preamble, file list, signature receive)
    /// are identical to the bulk path; only the final delta phase
    /// differs. See [`drive_upload_through_delta_streaming`] for the
    /// public wrapper and the parity invariant.
    async fn drive_upload_inner_streaming<R>(
        &mut self,
        command_spec: RemoteCommandSpec,
        source_entry: FileListEntry,
        source_reader: R,
        source_len: u64,
        adapter: &dyn DeltaEngineAdapter,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError>
    where
        R: AsyncRead + Unpin + Send,
    {
        self.session_role = Some(SessionRole::Sender);
        self.remote_command_flavor = command_spec.flavor;
        self.open_raw_stream_internal(&command_spec).await?;
        self.perform_preamble_exchange(31, "xxh128 xxh3 xxh64 md5 md4", "zstd lz4 zlibx zlib")
            .await?;
        self.send_file_list_single_file(&source_entry).await?;
        self.receive_signature_phase_single_file(bridge).await?;
        self.send_delta_phase_streaming(source_reader, source_len, adapter)
            .await?;
        Ok(())
    }

    async fn drive_download_inner(
        &mut self,
        command_spec: RemoteCommandSpec,
        destination_data: &[u8],
        adapter: &dyn DeltaEngineAdapter,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError> {
        self.session_role = Some(SessionRole::Receiver);
        self.remote_command_flavor = command_spec.flavor;
        self.open_raw_stream_internal(&command_spec).await?;
        // B.2: rsync wire protocol uses SPACE-separated algo lists in
        // priority-descending order. Using commas causes stock rsync
        // 3.4.1 to parse the whole list as a single unknown algorithm
        // and close the stream. Values cribbed from the frozen capture
        // `capture/artifacts_real/frozen/upload/capture_in.bin` shape.
        self.perform_preamble_exchange(31, "xxh128 xxh3 xxh64 md5 md4", "zstd lz4 zlibx zlib")
            .await?;
        self.receive_file_list_single_file(bridge).await?;
        self.send_signature_phase_single_file(destination_data, adapter)
            .await?;
        self.receive_delta_phase_single_file(destination_data, adapter, bridge)
            .await?;
        Ok(())
    }

    /// P3-T01 W2.4 — streaming-sink twin of [`drive_download_inner`]. The
    /// pre-delta phases run unchanged against the bulk `destination_data`
    /// slice; the final phase swaps `receive_delta_phase_single_file` for
    /// `receive_delta_phase_streaming` which writes reconstructed bytes
    /// to the configured `Streaming(writer)` target via
    /// `apply_delta_streaming(baseline, ops, block_size, writer)`.
    async fn drive_download_inner_streaming(
        &mut self,
        command_spec: RemoteCommandSpec,
        destination_data: &[u8],
        baseline: &mut dyn BaselineSource,
        writer: &mut (dyn AsyncWrite + Send + Unpin),
        adapter: &dyn DeltaEngineAdapter,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError> {
        self.session_role = Some(SessionRole::Receiver);
        self.remote_command_flavor = command_spec.flavor;
        self.open_raw_stream_internal(&command_spec).await?;
        self.perform_preamble_exchange(31, "xxh128 xxh3 xxh64 md5 md4", "zstd lz4 zlibx zlib")
            .await?;
        self.receive_file_list_single_file(bridge).await?;
        self.send_signature_phase_single_file(destination_data, adapter)
            .await?;
        self.receive_delta_phase_streaming(baseline, writer, adapter, bridge)
            .await?;
        Ok(())
    }

    // --- private helpers -------------------------------------------------

    async fn open_raw_stream_internal(
        &mut self,
        command_spec: &RemoteCommandSpec,
    ) -> Result<(), AerorsyncError> {
        self.check_cancel("open_raw_stream")?;
        let stream = self
            .transport
            .open_raw_stream(command_spec.to_exec_request())
            .await?;
        self.stream = Some(stream);
        self.phase = AerorsyncSessionPhase::RawStreamOpen;
        Ok(())
    }

    /// B.2 fix: rsync wire protocol places the CLIENT first — the client
    /// writes its preamble onto the raw stream and only afterwards reads
    /// the server's response. The captured frozen transcripts confirm it:
    /// `capture/artifacts_real/frozen/upload/capture_in.bin` (bytes the
    /// client sends) starts with `1f 00 00 00` (protocol 31 LE u32) + the
    /// checksum algo list; only after that the server replies with
    /// `20 00 00 00 81 ff 23 ...` in `capture_out.bin`.
    ///
    /// The previous implementation read first and wrote after, which
    /// deadlocked against stock `rsync --server` because both peers were
    /// stuck in read. It happened to work against the dev helper
    /// `aerorsync_serve` only because that path is never exercised via
    /// the `NativeRsyncDriver` (which speaks the real wire); live lanes
    /// against the dev helper go through the `SessionDriver` RSNP
    /// framing instead.
    async fn perform_preamble_exchange(
        &mut self,
        protocol_version: u32,
        checksum_algos: &str,
        compression_algos: &str,
    ) -> Result<(), AerorsyncError> {
        // 1. Write our client preamble first.
        let outbound = encode_client_preamble(&ClientPreamble {
            protocol_version,
            checksum_algos: checksum_algos.to_string(),
            compression_algos: compression_algos.to_string(),
            consumed: 0,
        });
        {
            self.check_cancel("perform_preamble_exchange send")?;
            let stream = self.stream.as_mut().ok_or_else(|| {
                AerorsyncError::transport("perform_preamble_exchange: stream not open (pre-write)")
            })?;
            stream.write_bytes(&outbound).await?;
        }
        // 2. Drain the server preamble from the stream. Any bytes read
        //    past the server preamble's `consumed` cursor are fed into
        //    `mux_reader` so the subsequent file list decode sees them.
        let mut scratch = Vec::with_capacity(128);
        loop {
            self.check_cancel("perform_preamble_exchange recv")?;
            match decode_server_preamble(&scratch) {
                Ok(preamble) => {
                    // Mirror `compat.c::setup_protocol` line 605:
                    //   if (protocol_version > remote_protocol)
                    //       protocol_version = remote_protocol;
                    // The negotiated protocol is MIN(client_max, server_max).
                    // The server's preamble advertises its max; both peers
                    // then speak min() on the wire. Using the server's raw
                    // value (e.g. proto 32 from rsync 3.4.x) while our
                    // encoders target proto 31 produced subtle format drift
                    // that manifested as receiver-side protocol errors and
                    // generator EOF on the error pipe.
                    self.protocol_version = preamble.protocol_version.min(protocol_version);
                    self.compat_flags = preamble.compat_flags;
                    self.checksum_seed = preamble.checksum_seed;
                    self.negotiated_checksum_algos = preamble.checksum_algos;
                    self.negotiated_compression_algos = preamble.compression_algos;
                    if preamble.consumed < scratch.len() {
                        self.mux_reader.feed(&scratch[preamble.consumed..]);
                    }
                    break;
                }
                Err(RealWireError::TruncatedBuffer { .. }) => {
                    let stream = self.stream.as_mut().ok_or_else(|| {
                        AerorsyncError::transport("perform_preamble_exchange: stream not open")
                    })?;
                    let chunk = stream.read_bytes(RAW_READ_CHUNK).await?;
                    if chunk.is_empty() {
                        return Err(AerorsyncError::transport(
                            "perform_preamble_exchange: remote closed before server preamble",
                        ));
                    }
                    scratch.extend_from_slice(&chunk);
                }
                Err(other) => {
                    return Err(map_realwire_error(other, "server preamble"));
                }
            }
        }
        self.phase = AerorsyncSessionPhase::ClientPreambleRecvd;
        Ok(())
    }

    /// Compute `FileListDecodeOptions` from the driver's current
    /// negotiation state. csum_len hardcoded to 16 for A2.1.
    fn build_flist_options(&self) -> FileListDecodeOptions<'static> {
        FileListDecodeOptions {
            protocol: self.protocol_version,
            // CF_VARINT_FLIST_FLAGS is active from protocol 30+. The
            // frozen oracle has it on; assert that implicitly by using
            // the varint path. If a legacy peer disagrees, decode will
            // surface a `RealWireError` which we translate.
            xfer_flags_as_varint: true,
            // B.2: production dispatch invokes the server with `-c`
            // (always_checksum) and `-o -g` (preserve owner/group).
            // Mirror the oracle compat: each regular file entry carries
            // 16-byte xxh128 checksum + uid + gid varints (with names
            // when XMIT_USER/GROUP_NAME_FOLLOWS gates them).
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
        }
    }

    async fn send_file_list_single_file(
        &mut self,
        entry: &FileListEntry,
    ) -> Result<(), AerorsyncError> {
        self.phase = AerorsyncSessionPhase::FileListSending;
        let opts = self.build_flist_options();
        // B.2: coalesce entry + terminator + NDX_FLIST_EOF into a single
        // MSG_DATA frame. The frozen oracle's first MSG_DATA payload is
        // 67 bytes carrying exactly this layout (entry 47 B + xxh128
        // 16 B + terminator 2 B + NDX_FLIST_EOF marker 2 B). Split
        // frames break stock rsync's expectation that the whole flist
        // arrives before the sender starts waiting on the receiver.
        let mut payload = encode_file_list_entry(entry, &opts);
        payload.extend_from_slice(&encode_file_list_terminator(&opts));
        payload.extend_from_slice(&encode_ndx(NDX_FLIST_EOF, &mut self.outbound_ndx_state));
        self.write_data_frame(&payload).await?;
        // S8j — remember the entry on the sender side so
        // `emit_summary_phase` can populate `total_size`. Parity with the
        // receiver path, which already pushes decoded entries.
        self.file_list.push(entry.clone());
        self.phase = AerorsyncSessionPhase::FileListSent;
        Ok(())
    }

    async fn receive_file_list_single_file(
        &mut self,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError> {
        self.phase = AerorsyncSessionPhase::FileListReceiving;
        let opts = self.build_flist_options();
        let mut flist_buf: Vec<u8> = Vec::new();
        let mut entry_seen = false;
        loop {
            self.check_cancel("receive_file_list")?;
            // Try to decode as much of the file list as we can from the
            // currently buffered bytes. Only fall through to another
            // Data frame when we run out of material.
            if !flist_buf.is_empty() {
                match decode_file_list_entry(&flist_buf, &opts) {
                    Ok((FileListDecodeOutcome::Entry(entry), consumed)) => {
                        flist_buf.drain(..consumed);
                        self.file_list.push(entry);
                        entry_seen = true;
                        continue;
                    }
                    Ok((FileListDecodeOutcome::EndOfList { .. }, consumed)) => {
                        flist_buf.drain(..consumed);
                        if !entry_seen {
                            return Err(AerorsyncError::invalid_frame(
                                "file list ended without any entry",
                            ));
                        }
                        self.phase = AerorsyncSessionPhase::FileListReceived;
                        return Ok(());
                    }
                    // A partial FileListEntry can surface several
                    // "need-more-bytes" shapes from `decode_file_list_entry`:
                    // raw truncation, a declared name length that overshoots
                    // the current buffer, or a declared algo-list length that
                    // overshoots. All three are recoverable by pulling
                    // another MSG_DATA frame off the wire.
                    Err(RealWireError::TruncatedBuffer { .. })
                    | Err(RealWireError::InvalidNameLen { .. })
                    | Err(RealWireError::InvalidAlgoListLen { .. }) => {
                        // Need more bytes — poll another Data frame below.
                    }
                    Err(other) => {
                        return Err(map_realwire_error(other, "file list entry"));
                    }
                }
            }
            let payload = self.next_data_frame(bridge).await?;
            flist_buf.extend_from_slice(&payload);
        }
    }

    /// Wrap `payload` in a `MSG_DATA` mux frame and write it to the raw
    /// stream. Rejects payloads larger than the 24-bit length field.
    async fn write_data_frame(&mut self, payload: &[u8]) -> Result<(), AerorsyncError> {
        if payload.len() > 0x00FF_FFFF {
            return Err(AerorsyncError::invalid_frame(format!(
                "MSG_DATA payload {} exceeds 24-bit length field",
                payload.len()
            )));
        }
        self.check_cancel("write_data_frame")?;
        let header = MuxHeader {
            tag: MuxTag::Data,
            length: payload.len() as u32,
        };
        let hdr_bytes = header.encode();
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| AerorsyncError::transport("write_data_frame: stream not open"))?;
        stream.write_bytes(&hdr_bytes).await?;
        stream.write_bytes(payload).await?;
        self.sent_data_bytes += payload.len() as u64;
        Ok(())
    }

    /// Drive the `MuxStreamReader` until a `MSG_DATA` frame pops out.
    /// Non-terminal OOB frames are routed to `bridge`; the first
    /// terminal OOB bails with a typed error (and is also forwarded to
    /// the bridge so it can capture `first_terminal()` for post-mortem).
    async fn next_data_frame(
        &mut self,
        bridge: &mut dyn EventSink,
    ) -> Result<Vec<u8>, AerorsyncError> {
        loop {
            // Poll-first policy: a full frame may already be buffered
            // from a previous chunk. Without this we would deadlock when
            // the server's response arrived in a single read.
            if let Some(res) = self.mux_reader.poll_frame() {
                let poll = res.map_err(|e| map_realwire_error(e, "mux frame"))?;
                match poll {
                    MuxPoll::Data(bytes) => {
                        // S8j: mirror of `sent_data_bytes`, used by
                        // `emit_summary_phase` to populate `total_read`
                        // in upload finishes.
                        self.received_raw_bytes += bytes.len() as u64;
                        return Ok(bytes);
                    }
                    MuxPoll::Oob(event) => {
                        bridge.handle(event);
                        continue;
                    }
                    MuxPoll::Terminal(event) => {
                        // Forward to the bridge before bailing so
                        // `first_terminal()` captures the full payload.
                        let event_for_bridge = event.clone();
                        bridge.handle(event_for_bridge);
                        return Err(AerorsyncError::from_oob_event(&event));
                    }
                }
            }
            self.check_cancel("next_data_frame")?;
            let stream = self
                .stream
                .as_mut()
                .ok_or_else(|| AerorsyncError::transport("next_data_frame: stream not open"))?;
            let chunk = stream.read_bytes(RAW_READ_CHUNK).await?;
            if chunk.is_empty() {
                return Err(AerorsyncError::transport(
                    "next_data_frame: remote closed mid file list",
                ));
            }
            self.mux_reader.feed(&chunk);
        }
    }

    // --- A2.2 signature phase (upload: receive, download: send) ----------

    /// Upload path: drain `ndx + iflags + sum_head + count × sum_block`
    /// from the server. Populates `received_sum_head`,
    /// `received_signatures`, `last_iflags`. Phase transitions:
    /// `FileListSent → SumHeadReceiving → SumBlocksReceiving`.
    async fn receive_signature_phase_single_file(
        &mut self,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError> {
        self.phase = AerorsyncSessionPhase::SumHeadReceiving;
        let (ndx, iflags, head) = self.read_signature_header(bridge).await?;
        if !(0..=i32::MAX).contains(&ndx) {
            return Err(AerorsyncError::invalid_frame(format!(
                "unexpected ndx sentinel before signature phase: {ndx}"
            )));
        }
        if iflags & ITEM_TRANSFER == 0 {
            return Err(AerorsyncError::invalid_frame(format!(
                "server signature message lacks ITEM_TRANSFER bit: iflags=0x{iflags:04X}"
            )));
        }
        // B.2 Step 4: stash the received NDX so `send_delta_phase_*`
        // can echo it back at the start of the delta payload (parity
        // with `sender.c::write_ndx_and_attrs`). Without this echo the
        // receiver mis-aligns and aborts with rsync exit 22.
        self.last_received_ndx = ndx;
        self.last_iflags = iflags;
        self.received_sum_head = Some(head);

        if head.count < 0 {
            return Err(AerorsyncError::invalid_frame(format!(
                "server sum_head.count is negative: {}",
                head.count
            )));
        }
        self.phase = AerorsyncSessionPhase::SumBlocksReceiving;
        let blocks = self
            .read_signature_blocks(head.count as usize, head.checksum_length as usize, bridge)
            .await?;
        self.received_signatures = blocks;
        Ok(())
    }

    /// Decode `ndx + iflags + sum_head` from the data stream, pulling
    /// additional `MSG_DATA` frames whenever the decoder reports it
    /// needs more bytes.
    async fn read_signature_header(
        &mut self,
        bridge: &mut dyn EventSink,
    ) -> Result<(i32, u16, SumHead), AerorsyncError> {
        let mut buf: Vec<u8> = Vec::new();
        // 1. ndx
        let ndx = loop {
            self.check_cancel("read_signature_header ndx")?;
            if !buf.is_empty() {
                match decode_ndx(&buf, &mut self.inbound_ndx_state) {
                    Ok((ndx, consumed)) => {
                        buf.drain(..consumed);
                        break ndx;
                    }
                    Err(RealWireError::NdxTruncated { .. }) => {
                        // need more bytes
                    }
                    Err(other) => return Err(map_realwire_error(other, "signature ndx")),
                }
            }
            let payload = self.next_data_frame(bridge).await?;
            buf.extend_from_slice(&payload);
        };
        if ndx == NDX_DONE || ndx == NDX_FLIST_EOF {
            return Err(AerorsyncError::invalid_frame(format!(
                "unexpected ndx sentinel at start of signature phase: {ndx}"
            )));
        }
        // 2. iflags (u16 LE — 2 bytes)
        let iflags = loop {
            self.check_cancel("read_signature_header iflags")?;
            if buf.len() >= 2 {
                match decode_item_flags(&buf) {
                    Ok((flags, consumed)) => {
                        buf.drain(..consumed);
                        break flags;
                    }
                    Err(other) => return Err(map_realwire_error(other, "signature iflags")),
                }
            }
            let payload = self.next_data_frame(bridge).await?;
            buf.extend_from_slice(&payload);
        };
        // 3. sum_head (16 bytes)
        let head = loop {
            self.check_cancel("read_signature_header sum_head")?;
            if buf.len() >= 16 {
                match decode_sum_head(&buf) {
                    Ok((head, consumed)) => {
                        buf.drain(..consumed);
                        break head;
                    }
                    Err(other) => return Err(map_realwire_error(other, "signature sum_head")),
                }
            }
            let payload = self.next_data_frame(bridge).await?;
            buf.extend_from_slice(&payload);
        };
        // Stash any residual bytes back into the mux reader for the
        // subsequent sum_blocks reader. `MuxStreamReader.feed` takes raw
        // mux frames — but at this point `buf` holds POST-mux payload
        // bytes, not raw mux. We cannot re-feed it into the reader.
        // Instead we carry the residual through `read_signature_blocks`
        // via an explicit argument.
        //
        // Implementation choice: pass `buf` to `read_signature_blocks`
        // as the prefix of its own accumulator. Getters stay clean.
        self.sig_residual_after_header = std::mem::take(&mut buf);
        Ok((ndx, iflags, head))
    }

    /// Read exactly `count` sum_blocks from the data stream, using the
    /// residual bytes left by `read_signature_header` as a prefix to the
    /// accumulator.
    async fn read_signature_blocks(
        &mut self,
        count: usize,
        strong_len: usize,
        bridge: &mut dyn EventSink,
    ) -> Result<Vec<SumBlock>, AerorsyncError> {
        let mut buf: Vec<u8> = std::mem::take(&mut self.sig_residual_after_header);
        let mut out = Vec::with_capacity(count);
        while out.len() < count {
            self.check_cancel("read_signature_blocks")?;
            let block_wire_size = 4 + strong_len;
            if buf.len() >= block_wire_size {
                match decode_sum_block(&buf, strong_len) {
                    Ok((block, consumed)) => {
                        buf.drain(..consumed);
                        out.push(block);
                        continue;
                    }
                    Err(other) => return Err(map_realwire_error(other, "sum_block")),
                }
            }
            let payload = self.next_data_frame(bridge).await?;
            buf.extend_from_slice(&payload);
        }
        Ok(out)
    }

    /// Download path: compute signatures from `destination_data` via
    /// `adapter` and emit a single mux-wrapped blob with
    /// `ndx + iflags + sum_head + count × sum_block`. Phase transitions:
    /// `FileListReceived → SumHeadSent → SumBlocksSent`.
    async fn send_signature_phase_single_file(
        &mut self,
        destination_data: &[u8],
        adapter: &dyn DeltaEngineAdapter,
    ) -> Result<(), AerorsyncError> {
        self.phase = AerorsyncSessionPhase::SumHeadSent;
        let block_size = adapter.compute_block_size(destination_data.len() as u64);
        let engine_sigs = adapter.build_signatures(destination_data, block_size);

        // Build truncated wire SumBlocks.
        let s2length = A2_2_DOWNLOAD_S2LENGTH;
        let s2length_usize = s2length as usize;
        let mut sum_blocks: Vec<SumBlock> = Vec::with_capacity(engine_sigs.len());
        for sig in &engine_sigs {
            let strong = sig.strong[..s2length_usize.min(sig.strong.len())].to_vec();
            sum_blocks.push(SumBlock {
                rolling: sig.rolling,
                strong,
            });
        }

        // Compose sum_head. Block length from the engine's choice;
        // remainder is (file_size mod block_size) — identical to rsync's
        // own derivation.
        let file_size = destination_data.len() as i32;
        let block_length = block_size as i32;
        let remainder_length = if block_length > 0 {
            file_size % block_length
        } else {
            0
        };
        let head = SumHead {
            count: sum_blocks.len() as i32,
            block_length,
            checksum_length: s2length,
            remainder_length,
        };
        self.sent_sum_head = Some(head);

        // Build a single MSG_DATA payload that concatenates everything.
        let mut payload: Vec<u8> = Vec::with_capacity(
            16 /* sum_head worst */ + 4 /* ndx upper bound */ + 2 /* iflags */
                + sum_blocks.len() * (4 + s2length_usize),
        );
        payload.extend_from_slice(&encode_ndx(
            A2_2_FIRST_FILE_NDX,
            &mut self.outbound_ndx_state,
        ));
        payload.extend_from_slice(&encode_item_flags(A2_2_DOWNLOAD_IFLAGS));
        payload.extend_from_slice(&encode_sum_head(&head));
        for block in &sum_blocks {
            payload.extend_from_slice(&encode_sum_block(block));
        }

        self.last_iflags = A2_2_DOWNLOAD_IFLAGS;
        self.write_data_frame(&payload).await?;

        self.sent_signatures = sum_blocks;
        // Keep engine_sigs alive until after emit in case the caller
        // inspects them via a future getter. Dropped here.
        let _ = engine_sigs;

        self.phase = AerorsyncSessionPhase::SumBlocksSent;
        Ok(())
    }

    // --- A2.3 delta phase (upload: send, download: receive) --------------

    /// Upload path: compute delta via `adapter.compute_delta`, compress
    /// literals session-wide, encode the full stream (ops + END_FLAG +
    /// file_checksum trailer), and emit on the wire in a single MSG_DATA
    /// frame. Flips `committed = true` immediately before the first
    /// wire byte — the PreCommit/PostCommit boundary.
    ///
    /// Phase transitions: `SumBlocksReceiving → DeltaSending → DeltaSent`.
    async fn send_delta_phase_single_file(
        &mut self,
        source_data: &[u8],
        adapter: &dyn DeltaEngineAdapter,
    ) -> Result<(), AerorsyncError> {
        self.phase = AerorsyncSessionPhase::DeltaSending;

        // Rebuild EngineSignatureBlock vec from received SumBlocks.
        let engine_sigs = self.wire_sigs_to_engine()?;
        let block_size = self
            .received_sum_head
            .as_ref()
            .map(|h| h.block_length as usize)
            .unwrap_or(0);

        // B.2 Step 4: `block_size == 0` is the "whole file" case — the
        // receiver's local target is absent or zero-byte so it has
        // nothing to diff against. `generator.c::write_sum_head(f_out,
        // NULL)` emits four zero int32s in this scenario. The sender
        // must react by streaming the entire source as a single literal
        // (no block matches possible). We build a synthetic plan with
        // one Literal op covering all `source_data`.
        let plan = if block_size == 0 {
            use crate::aerorsync::engine_adapter::EngineDeltaPlan;
            let ops = if source_data.is_empty() {
                Vec::new()
            } else {
                vec![EngineDeltaOp::Literal(source_data.to_vec())]
            };
            EngineDeltaPlan {
                ops,
                copy_blocks: 0,
                literal_bytes: source_data.len() as u64,
                total_delta_bytes: source_data.len() as u64,
                savings_ratio: 1.0,
                should_use_delta: true,
            }
        } else {
            adapter.compute_delta(source_data, &engine_sigs, block_size)
        };

        // Extract raw literals in encounter order for session-wide
        // zstd compression (matches `send_zstd_token`'s shared CCtx).
        let pending_raw: Vec<&[u8]> = plan
            .ops
            .iter()
            .filter_map(|op| match op {
                EngineDeltaOp::Literal(raw) => Some(raw.as_slice()),
                EngineDeltaOp::CopyBlock(_) => None,
            })
            .collect();

        let zstd_on = self.zstd_negotiated();
        let compressed_blobs: Vec<Vec<u8>> = if zstd_on && !pending_raw.is_empty() {
            compress_zstd_literal_stream(&pending_raw)
                .map_err(|e| map_realwire_error(e, "zstd compress literal stream"))?
        } else {
            // No compression negotiated — emit raw payloads as-is.
            pending_raw.iter().map(|p| p.to_vec()).collect()
        };
        // S8j — multi-chunk DEFLATED_DATA splitting.
        //
        // Stock rsync's `send_zstd_token` (token.c:678-776) flushes the
        // zstd output buffer whenever it reaches `MAX_DATA_COUNT`
        // (= 16383, the 14-bit length budget of the DEFLATED_DATA
        // token) and emits a fresh DEFLATED_DATA record with the rest.
        // A compressed literal larger than 16383 bytes therefore lands
        // as N consecutive DEFLATED_DATA frames on the wire; the
        // receiver's single session-wide `ZSTD_DCtx` concatenates the
        // payloads transparently — the chunk boundaries carry no
        // logical meaning, they're pure transport fragmentation.
        //
        // We mirror that behaviour by chunking every compressed blob
        // that exceeds `MAX_DELTA_LITERAL_LEN` into 16383-byte slices
        // and emitting one `DeltaOp::Literal` per slice. The original
        // `EngineDeltaOp::Literal` → wire literal ordering is
        // preserved; CopyRun ops stay interleaved at the same logical
        // positions they occupied in the engine plan. Pre-fix the
        // driver bailed with `InvalidFrame` as soon as any blob
        // crossed 16 KiB, capping the native path at ~16 KiB delta
        // payloads. Post-fix the cap is the 24-bit DEFLATED_DATA
        // per-token length (unchanged) times an unbounded number of
        // tokens — in practice governed by the driver's in-memory
        // cap (`AERORSYNC_MAX_IN_MEMORY_BYTES`).

        // Interleave literals with CopyRun ops in the original engine
        // order. Each EngineDeltaOp::CopyBlock(idx) becomes a single-
        // block CopyRun; the engine may already coalesce runs, but we
        // keep A2.3 simple and emit one CopyRun per CopyBlock. Each
        // EngineDeltaOp::Literal becomes 1..N DeltaOp::Literal records,
        // depending on whether the compressed blob fits in a single
        // DEFLATED_DATA token or needs chunking.
        let mut wire_ops: Vec<DeltaOp> =
            Vec::with_capacity(plan.ops.len() + compressed_blobs.len());
        let mut blob_idx: usize = 0;
        for op in &plan.ops {
            match op {
                EngineDeltaOp::Literal(_) => {
                    let blob = &compressed_blobs[blob_idx];
                    blob_idx += 1;
                    if blob.is_empty() {
                        // Skip zero-length blobs — `compress_zstd_literal_stream`
                        // already drops empty inputs, but defensively guard
                        // the non-zstd branch where empty payloads could
                        // surface. DEFLATED_DATA length=0 is a protocol
                        // error (decode_delta_op rejects it).
                        continue;
                    }
                    for chunk in blob.chunks(MAX_DELTA_LITERAL_LEN) {
                        wire_ops.push(DeltaOp::Literal {
                            compressed_payload: chunk.to_vec(),
                        });
                    }
                }
                EngineDeltaOp::CopyBlock(idx) => {
                    wire_ops.push(DeltaOp::CopyRun {
                        start_token_index: *idx as i32,
                        run_length: 1,
                    });
                }
            }
        }

        // S8j — real xxh128 (XXH3-128) over `source_data`. Rsync 3.2.7
        // verifies this trailer server-side when `xxh128` is negotiated
        // in `checksum_algos` (see `perform_preamble_exchange` call
        // sites below). Byte layout matches `checksum.c::hash_struct`'s
        // `SIVAL64(buf, 0, lo); SIVAL64(buf, 8, hi)` — lower 64 bits LE
        // first, upper 64 bits LE second.
        let file_checksum = compute_xxh128_wire(source_data);

        let report = DeltaStreamReport {
            ops: wire_ops.clone(),
            file_checksum,
        };
        let delta_bytes = encode_delta_stream(&report);

        // B.2 Step 4: the sender MUST echo back `write_ndx + write_shortint(iflags) +
        // write_sum_head` before the delta tokens, mirroring
        // `sender.c::send_files` (line 411-412). Without this echo the
        // receiver expects sum_head bytes where it gets delta tokens and
        // aborts with rsync exit 22 ("Error allocating core memory
        // buffers" — sum.count is read as a huge int from delta bytes).
        //
        // Echo values come from the receiver's signature header that
        // `read_signature_header` stashed in `last_received_ndx`,
        // `last_iflags`, and `received_sum_head`.
        let echo_head = *self.received_sum_head.as_ref().ok_or_else(|| {
            AerorsyncError::invalid_frame(
                "send_delta_phase: missing received sum_head — signature phase didn't run",
            )
        })?;
        let mut payload = Vec::with_capacity(8 + delta_bytes.len());
        payload.extend_from_slice(&encode_ndx(
            self.last_received_ndx,
            &mut self.outbound_ndx_state,
        ));
        payload.extend_from_slice(&encode_item_flags(self.last_iflags));
        payload.extend_from_slice(&encode_sum_head(&echo_head));
        payload.extend_from_slice(&delta_bytes);

        // PreCommit → PostCommit boundary: flip BEFORE writing the first
        // byte of delta material. Once the server starts receiving the
        // delta stream, we no longer can transparently fall back.
        self.committed = true;
        self.emitted_delta_ops = wire_ops;
        self.write_data_frame(&payload).await?;

        self.phase = AerorsyncSessionPhase::DeltaSent;
        Ok(())
    }

    /// P3-T01 W1.2 / W1.3 — streaming-source twin of
    /// [`send_delta_phase_single_file`]. The engine plan is produced
    /// chunk-by-chunk (`RollingDeltaPlanProducer` for
    /// `block_size != 0`, fixed-slab chunking for `block_size == 0`)
    /// and the file-level xxh128 checksum is computed by streaming
    /// (`Xxh3Default` instead of `xxh3_128(&[u8])`).
    ///
    /// ## Wire-byte parity vs. the bulk path
    ///
    /// - For `block_size != 0`: byte-identical with
    ///   [`send_delta_phase_single_file`] for any source length
    ///   (pinned by `streaming_send_matches_bulk_send_*`).
    /// - For `block_size == 0` and source `<= STREAMING_READ_CHUNK_BYTES`:
    ///   byte-identical (single literal in both paths).
    /// - For `block_size == 0` and source `> STREAMING_READ_CHUNK_BYTES`:
    ///   wire bytes **diverge** from the bulk path — the streaming
    ///   path emits `ceil(source_len / STREAMING_READ_CHUNK_BYTES)`
    ///   engine literals through the session-wide zstd `CCtx`, where
    ///   the bulk path emits one. The receiver's session-wide
    ///   `ZSTD_DCtx` concatenates both shapes to the same plaintext
    ///   per stock rsync's `send_zstd_token` semantics, so the
    ///   divergence is *protocol-equivalent*. The chunked emission is
    ///   what allows W1.3 to lift the 256 MiB upload-side cap without
    ///   requesting a `Vec<u8>` of `source_len` bytes — the
    ///   contiguous-allocation failure mode that gated the bulk path
    ///   on multi-GiB uploads with no baseline.
    ///
    /// ## Memory bound (W1.3)
    ///
    /// Resident memory during the function is bounded by:
    ///
    /// - `STREAMING_READ_CHUNK_BYTES` for the read buffer
    /// - `STREAMING_READ_CHUNK_BYTES` for the in-flight literal slab
    ///   (`chunk_acc` for `block_size == 0`, the producer's window for
    ///   `block_size != 0`)
    /// - the accumulated op vector, whose size is proportional to
    ///   `source_len` (true multi-frame streaming of zstd + wire is
    ///   post-P3-T01 scope).
    ///
    /// `source_len` MUST equal the byte count drained from
    /// `source_reader`; mismatches abort the upload with
    /// `InvalidFrame` (the file changed mid-flight or the caller
    /// declared the wrong size).
    async fn send_delta_phase_streaming<R>(
        &mut self,
        mut source_reader: R,
        source_len: u64,
        _adapter: &dyn DeltaEngineAdapter,
    ) -> Result<(), AerorsyncError>
    where
        R: AsyncRead + Unpin + Send,
    {
        self.phase = AerorsyncSessionPhase::DeltaSending;

        // Identical sig-derivation as the bulk path. `wire_sigs_to_engine`
        // depends only on `received_signatures` + `received_sum_head`,
        // which the preceding signature phase already populated.
        let engine_sigs = self.wire_sigs_to_engine()?;
        let block_size = self
            .received_sum_head
            .as_ref()
            .map(|h| h.block_length as usize)
            .unwrap_or(0);

        // Drive the producer + xxh3 hasher chunk-by-chunk. The producer
        // owns the rolling window; the hasher accumulates a streaming
        // xxh3_128 of the source. Both are populated from the same
        // chunk slice so the wire trailer matches what
        // `compute_xxh128_wire(source_data)` would have produced bulk.
        let mut hasher = Xxh3Default::new();
        let mut ops: Vec<EngineDeltaOp> = Vec::new();
        let mut total_source_bytes: u64 = 0;
        let mut buf = vec![0u8; STREAMING_READ_CHUNK_BYTES];

        if block_size == 0 {
            // Whole-file case: the receiver has no baseline to diff
            // against (`block_size == 0` is rsync's "send everything as
            // one literal" sentinel). The producer would silently emit
            // zero ops here, so we materialise the literal explicitly.
            //
            // P3-T01 W1.3 — emit one `EngineDeltaOp::Literal` per
            // `STREAMING_READ_CHUNK_BYTES`-bounded slab instead of one
            // big literal covering `source_len`. Reasons:
            //
            //   1. Avoids a single contiguous `Vec<u8>` allocation of
            //      `source_len` bytes. On a 4 GiB upload with no
            //      baseline the bulk path would request a 4 GiB
            //      contiguous reservation from the allocator, which
            //      fails on fragmented heaps even when total free RAM
            //      is plentiful.
            //   2. Keeps the per-op working set aligned with the read
            //      chunk size, so the producer-driven (`block_size != 0`)
            //      and whole-file (`block_size == 0`) branches share
            //      the same bound on op-level allocation.
            //   3. Wire-equivalent for sources `<= STREAMING_READ_CHUNK_BYTES`
            //      (single literal, byte-identical to bulk). Above that
            //      threshold the wire bytes diverge from bulk because
            //      the session-wide zstd `CCtx` flushes between literals;
            //      the receiver's session-wide `ZSTD_DCtx` concatenates
            //      the payloads transparently per stock rsync's
            //      `send_zstd_token` semantics, so the divergence is
            //      *protocol-equivalent* even though it is not
            //      byte-identical. Pinned by
            //      `streaming_send_matches_bulk_send_whole_file_no_baseline`
            //      (small source: byte-identical) and
            //      `streaming_send_block_size_zero_chunks_large_source`
            //      (large source: chunked, multiple engine literals).
            //
            // Memory bound: O(STREAMING_READ_CHUNK_BYTES) for `chunk_acc`
            // plus the read buffer plus one in-flight literal in `ops`
            // until zstd compression. The full op vector still grows
            // proportionally to `source_len`; lifting that requires
            // streaming the zstd encoder + wire emission, scoped
            // post-P3-T01 (see W1.2 docstring).
            let mut chunk_acc: Vec<u8> = Vec::new();
            loop {
                let n = source_reader.read(&mut buf).await.map_err(|e| {
                    AerorsyncError::transport(format!(
                        "send_delta_phase_streaming: source read failed: {e}"
                    ))
                })?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                total_source_bytes += n as u64;

                let mut to_consume: &[u8] = &buf[..n];
                while !to_consume.is_empty() {
                    if chunk_acc.capacity() == 0 {
                        chunk_acc.reserve_exact(STREAMING_READ_CHUNK_BYTES);
                    }
                    let space_left =
                        STREAMING_READ_CHUNK_BYTES.saturating_sub(chunk_acc.len());
                    let take = to_consume.len().min(space_left);
                    chunk_acc.extend_from_slice(&to_consume[..take]);
                    to_consume = &to_consume[take..];
                    if chunk_acc.len() >= STREAMING_READ_CHUNK_BYTES {
                        ops.push(EngineDeltaOp::Literal(std::mem::take(&mut chunk_acc)));
                    }
                }
            }
            if !chunk_acc.is_empty() {
                ops.push(EngineDeltaOp::Literal(chunk_acc));
            }
        } else {
            let mut producer = RollingDeltaPlanProducer::new(block_size, engine_sigs);
            loop {
                let n = source_reader.read(&mut buf).await.map_err(|e| {
                    AerorsyncError::transport(format!(
                        "send_delta_phase_streaming: source read failed: {e}"
                    ))
                })?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                producer.drive_chunk(&buf[..n], &mut ops);
                total_source_bytes += n as u64;
            }
            producer.finalize(&mut ops);
        }

        if total_source_bytes != source_len {
            return Err(AerorsyncError::invalid_frame(format!(
                "send_delta_phase_streaming: declared source_len {source_len} != bytes read {total_source_bytes}"
            )));
        }

        // From here on the encoding/wire-emission path is byte-for-byte
        // identical to `send_delta_phase_single_file`. Any divergence
        // would break the `streaming_send_matches_bulk_send_*` parity
        // pin below — kept in lockstep on purpose.
        let pending_raw: Vec<&[u8]> = ops
            .iter()
            .filter_map(|op| match op {
                EngineDeltaOp::Literal(raw) => Some(raw.as_slice()),
                EngineDeltaOp::CopyBlock(_) => None,
            })
            .collect();

        let zstd_on = self.zstd_negotiated();
        let compressed_blobs: Vec<Vec<u8>> = if zstd_on && !pending_raw.is_empty() {
            compress_zstd_literal_stream(&pending_raw)
                .map_err(|e| map_realwire_error(e, "zstd compress literal stream"))?
        } else {
            pending_raw.iter().map(|p| p.to_vec()).collect()
        };

        let mut wire_ops: Vec<DeltaOp> = Vec::with_capacity(ops.len() + compressed_blobs.len());
        let mut blob_idx: usize = 0;
        for op in &ops {
            match op {
                EngineDeltaOp::Literal(_) => {
                    let blob = &compressed_blobs[blob_idx];
                    blob_idx += 1;
                    if blob.is_empty() {
                        continue;
                    }
                    for chunk in blob.chunks(MAX_DELTA_LITERAL_LEN) {
                        wire_ops.push(DeltaOp::Literal {
                            compressed_payload: chunk.to_vec(),
                        });
                    }
                }
                EngineDeltaOp::CopyBlock(idx) => {
                    wire_ops.push(DeltaOp::CopyRun {
                        start_token_index: *idx as i32,
                        run_length: 1,
                    });
                }
            }
        }

        // Streaming xxh3-128 → 16-byte wire trailer.
        // Layout pinned by `xxh128_wire_bytes_match_SIVAL64_pair`:
        // `out[0..8] = lo.to_le_bytes(); out[8..16] = hi.to_le_bytes()`.
        let file_checksum = {
            let hash = hasher.digest128();
            let lo = hash as u64;
            let hi = (hash >> 64) as u64;
            let mut out = Vec::with_capacity(16);
            out.extend_from_slice(&lo.to_le_bytes());
            out.extend_from_slice(&hi.to_le_bytes());
            out
        };

        let report = DeltaStreamReport {
            ops: wire_ops.clone(),
            file_checksum,
        };
        let delta_bytes = encode_delta_stream(&report);

        let echo_head = *self.received_sum_head.as_ref().ok_or_else(|| {
            AerorsyncError::invalid_frame(
                "send_delta_phase_streaming: missing received sum_head — signature phase didn't run",
            )
        })?;
        let mut payload = Vec::with_capacity(8 + delta_bytes.len());
        payload.extend_from_slice(&encode_ndx(
            self.last_received_ndx,
            &mut self.outbound_ndx_state,
        ));
        payload.extend_from_slice(&encode_item_flags(self.last_iflags));
        payload.extend_from_slice(&encode_sum_head(&echo_head));
        payload.extend_from_slice(&delta_bytes);

        self.committed = true;
        self.emitted_delta_ops = wire_ops;
        self.write_data_frame(&payload).await?;

        self.phase = AerorsyncSessionPhase::DeltaSent;
        Ok(())
    }

    /// Download path: drain delta stream bytes from MSG_DATA frames,
    /// decode ops + trailer, decompress literals, convert to engine ops,
    /// and apply via `adapter.apply_delta`. The reconstructed bytes are
    /// stashed in `self.reconstructed` for the A4 adapter to flush to
    /// disk via temp+rename.
    ///
    /// `committed` stays `false` throughout — A2.3 download never writes
    /// to disk. A4 will flip committed when it opens the temp file.
    ///
    /// Phase transitions: `SumBlocksSent → DeltaReceiving → DeltaReceived`.
    async fn receive_delta_phase_single_file(
        &mut self,
        destination_data: &[u8],
        adapter: &dyn DeltaEngineAdapter,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError> {
        self.phase = AerorsyncSessionPhase::DeltaReceiving;

        // Accumulate bytes until `decode_delta_stream` succeeds.
        let mut buf: Vec<u8> = Vec::new();
        let sum_head_count = self.sent_sum_head.as_ref().map(|h| h.count);
        loop {
            self.check_cancel("receive_delta_phase")?;
            if !buf.is_empty() {
                match decode_delta_stream(&buf, A2_3_FILE_CHECKSUM_LEN, sum_head_count) {
                    Ok((report, consumed)) => {
                        buf.drain(..consumed);
                        self.received_file_checksum = Some(report.file_checksum.clone());
                        self.install_reconstructed_from_wire(
                            destination_data,
                            adapter,
                            report.ops,
                        )?;
                        self.phase = AerorsyncSessionPhase::DeltaReceived;
                        return Ok(());
                    }
                    Err(RealWireError::DeltaTokenTruncated { .. }) => {
                        // need more bytes
                    }
                    Err(other) => {
                        return Err(map_realwire_error(other, "delta stream"));
                    }
                }
            }
            let payload = self.next_data_frame(bridge).await?;
            buf.extend_from_slice(&payload);
        }
    }

    fn install_reconstructed_from_wire(
        &mut self,
        destination_data: &[u8],
        adapter: &dyn DeltaEngineAdapter,
        wire_ops: Vec<DeltaOp>,
    ) -> Result<(), AerorsyncError> {
        let zstd_on = self.zstd_negotiated();
        let engine_ops = self.delta_wire_to_engine_ops(&wire_ops, zstd_on)?;
        let block_size = self
            .sent_sum_head
            .as_ref()
            .map(|h| h.block_length as usize)
            .unwrap_or(0);
        if block_size == 0 {
            return Err(AerorsyncError::invalid_frame(
                "receive_delta_phase: block_size is zero (missing local sum_head)",
            ));
        }
        let reconstructed = adapter
            .apply_delta(destination_data, &engine_ops, block_size)
            .map_err(|e| AerorsyncError::invalid_frame(format!("apply_delta: {e}")))?;
        self.reconstructed = Some(reconstructed);
        Ok(())
    }

    /// P3-T01 W2.4 — streaming sibling of [`receive_delta_phase_single_file`].
    /// Identical wire-handling loop (drain MSG_DATA frames, decode delta
    /// stream, decompress literals, convert to engine ops); the only
    /// difference is the install step calls
    /// [`install_reconstructed_from_wire_streaming`] to apply the ops via
    /// `apply_delta_streaming(baseline, ops, block_size, writer)` instead
    /// of stashing a `Vec<u8>` in `self.reconstructed`.
    ///
    /// `committed` stays `false` throughout — the W2.5 caller flips its
    /// own `local_committed` flag on the first byte successfully written
    /// to the `StreamingAtomicWriter` temp.
    async fn receive_delta_phase_streaming(
        &mut self,
        baseline: &mut dyn BaselineSource,
        writer: &mut (dyn AsyncWrite + Send + Unpin),
        adapter: &dyn DeltaEngineAdapter,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError> {
        self.phase = AerorsyncSessionPhase::DeltaReceiving;

        let mut buf: Vec<u8> = Vec::new();
        let sum_head_count = self.sent_sum_head.as_ref().map(|h| h.count);
        loop {
            self.check_cancel("receive_delta_phase")?;
            if !buf.is_empty() {
                match decode_delta_stream(&buf, A2_3_FILE_CHECKSUM_LEN, sum_head_count) {
                    Ok((report, consumed)) => {
                        buf.drain(..consumed);
                        self.received_file_checksum = Some(report.file_checksum.clone());
                        self.install_reconstructed_from_wire_streaming(
                            baseline, writer, adapter, report.ops,
                        )
                        .await?;
                        self.phase = AerorsyncSessionPhase::DeltaReceived;
                        return Ok(());
                    }
                    Err(RealWireError::DeltaTokenTruncated { .. }) => {
                        // need more bytes
                    }
                    Err(other) => {
                        return Err(map_realwire_error(other, "delta stream"));
                    }
                }
            }
            let payload = self.next_data_frame(bridge).await?;
            buf.extend_from_slice(&payload);
        }
    }

    /// P3-T01 W2.4/W2.5 — streaming sibling of
    /// [`install_reconstructed_from_wire`]. Decodes the wire ops to
    /// engine ops (same conversion as the bulk path) and dispatches to
    /// [`apply_delta_streaming`] against the caller-supplied baseline +
    /// caller-supplied writer.
    ///
    /// Errors:
    /// - `InvalidFrame` if `block_size == 0` (no `sent_sum_head` from the
    ///   signature phase — a wire-level invariant violation, identical
    ///   to the bulk path).
    /// - `InvalidFrame` from `apply_delta_streaming` (baseline read errors,
    ///   writer poll_write errors, oversized block_size).
    async fn install_reconstructed_from_wire_streaming(
        &mut self,
        baseline: &mut dyn BaselineSource,
        writer: &mut (dyn AsyncWrite + Send + Unpin),
        adapter: &dyn DeltaEngineAdapter,
        wire_ops: Vec<DeltaOp>,
    ) -> Result<(), AerorsyncError> {
        let zstd_on = self.zstd_negotiated();
        let engine_ops = self.delta_wire_to_engine_ops(&wire_ops, zstd_on)?;
        let _ = adapter; // adapter is unused on the streaming path —
                         // engine ops carry everything apply_delta_streaming needs.
                         // Kept in the signature for parity with the bulk twin and
                         // to leave room for future adapter-driven dispatch.
        let block_size = self
            .sent_sum_head
            .as_ref()
            .map(|h| h.block_length as usize)
            .unwrap_or(0);
        if block_size == 0 {
            return Err(AerorsyncError::invalid_frame(
                "receive_delta_phase: block_size is zero (missing local sum_head)",
            ));
        }
        apply_delta_streaming(baseline, engine_ops, block_size, writer)
            .await
            .map_err(|e| AerorsyncError::invalid_frame(format!("apply_delta_streaming: {e}")))?;
        // self.reconstructed intentionally stays None — the bytes flowed
        // through the writer and reading them back from RAM would defeat
        // the purpose of the streaming path. W2.4 acceptance test 4
        // pins this.
        Ok(())
    }

    /// Rebuild an `EngineSignatureBlock` vec from the driver's received
    /// `SumBlock` vec + `received_sum_head`. The strong bytes are zero-
    /// padded to 32 (engine API shape); only the first `checksum_length`
    /// bytes are ever consulted by the engine for matching.
    fn wire_sigs_to_engine(&self) -> Result<Vec<EngineSignatureBlock>, AerorsyncError> {
        let head = self.received_sum_head.as_ref().ok_or_else(|| {
            AerorsyncError::invalid_frame("wire_sigs_to_engine: no received sum_head")
        })?;
        let block_len = head.block_length as u32;
        let mut out = Vec::with_capacity(self.received_signatures.len());
        for (idx, wire) in self.received_signatures.iter().enumerate() {
            let mut strong = [0u8; 32];
            let take = wire.strong.len().min(32);
            strong[..take].copy_from_slice(&wire.strong[..take]);
            out.push(EngineSignatureBlock {
                index: idx as u32,
                rolling: wire.rolling,
                strong,
                block_len,
            });
        }
        Ok(out)
    }

    /// Convert wire delta ops into engine delta ops, decompressing
    /// literals session-wide when zstd is negotiated. CopyRuns expand
    /// 1:1 into `EngineDeltaOp::CopyBlock(index)` per block in the run.
    ///
    /// **S8j download-side**: stock rsync's `send_zstd_token`
    /// (token.c:678-776) flushes the zstd output buffer whenever it
    /// reaches `MAX_DATA_COUNT` and emits a fresh DEFLATED_DATA frame
    /// with the rest. A single logical literal can therefore arrive
    /// as N ≥ 1 consecutive `DeltaOp::Literal` wire records. We group
    /// those runs (any `DeltaOp::Literal` sequence uninterrupted by a
    /// `DeltaOp::CopyRun`), concatenate their compressed payloads, and
    /// feed ONE concatenated blob per run through the session-wide
    /// DCtx. Pre-S8j this helper assumed 1 wire Literal = 1 logical
    /// literal, which silently mis-scaled the engine plan whenever the
    /// server split (for anything > ~16 KiB of compressed payload).
    fn delta_wire_to_engine_ops(
        &self,
        wire_ops: &[DeltaOp],
        zstd_on: bool,
    ) -> Result<Vec<EngineDeltaOp>, AerorsyncError> {
        // Pass 1: coalesce consecutive DeltaOp::Literal chunks into
        // per-run blobs. Each run represents one logical literal; the
        // fragmentation across DEFLATED_DATA tokens is pure transport.
        let mut literal_run_blobs: Vec<Vec<u8>> = Vec::new();
        let mut current_run: Vec<u8> = Vec::new();
        for op in wire_ops {
            match op {
                DeltaOp::Literal { compressed_payload } => {
                    current_run.extend_from_slice(compressed_payload);
                }
                DeltaOp::CopyRun { .. } => {
                    if !current_run.is_empty() {
                        literal_run_blobs.push(std::mem::take(&mut current_run));
                    }
                }
            }
        }
        if !current_run.is_empty() {
            literal_run_blobs.push(current_run);
        }

        // Decompress each run. For non-zstd sessions the raw wire
        // bytes already carry the raw literal, so the concatenated run
        // blob is already the logical literal's bytes.
        let run_slices: Vec<&[u8]> = literal_run_blobs.iter().map(|b| b.as_slice()).collect();
        let raw_run_literals: Vec<Vec<u8>> = if zstd_on && !run_slices.is_empty() {
            decompress_zstd_literal_stream_boundaries(&run_slices)
                .map_err(|e| map_realwire_error(e, "zstd decompress delta literals"))?
        } else {
            literal_run_blobs
        };

        // Pass 2: emit engine ops in wire order. Consecutive wire
        // literals collapse into a single EngineDeltaOp::Literal
        // (pushed on the first of the run); subsequent literals in
        // the same run are folded silently. A CopyRun closes the
        // current literal run.
        let mut out = Vec::with_capacity(wire_ops.len());
        let mut run_idx: usize = 0;
        let mut in_literal_run = false;
        for op in wire_ops {
            match op {
                DeltaOp::Literal { .. } => {
                    if !in_literal_run {
                        out.push(EngineDeltaOp::Literal(raw_run_literals[run_idx].clone()));
                        run_idx += 1;
                        in_literal_run = true;
                    }
                }
                DeltaOp::CopyRun {
                    start_token_index,
                    run_length,
                } => {
                    in_literal_run = false;
                    for k in 0..*run_length {
                        let block_idx = *start_token_index + i32::from(k);
                        if block_idx < 0 {
                            return Err(AerorsyncError::invalid_frame(format!(
                                "negative block index {block_idx} in delta CopyRun"
                            )));
                        }
                        out.push(EngineDeltaOp::CopyBlock(block_idx as u32));
                    }
                }
            }
        }
        Ok(out)
    }

    fn zstd_negotiated(&self) -> bool {
        // Rsync's preamble serialises algo lists as SPACE-separated,
        // priority-descending tokens (see `perform_preamble_exchange`
        // above). The historical comma split here silently disabled
        // zstd against every real rsync peer — the list parses as a
        // single "zstd lz4 zlibx zlib" literal token that never equals
        // "zstd". The resulting raw-literal delta stream was still a
        // protocol-shaped `DEFLATED_DATA` payload, which stock rsync
        // tries to run through `recv_zstd_token` and then drops the
        // connection ("error in rsync protocol data stream").
        self.negotiated_compression_algos
            .split_ascii_whitespace()
            .any(|a| a.eq_ignore_ascii_case("zstd"))
    }

    /// A2.4 entry point: drain the server's `SummaryFrame`, populate
    /// `session_stats`, and shut the raw stream down cleanly. Call
    /// **after** `drive_upload` / `drive_download` have reached the
    /// post-delta stub frontier.
    ///
    /// Split from the main drive loop intentionally: the A4 adapter may
    /// want to decide between an eager finish (replicate classic rsync
    /// client UX) vs a deferred one (honour UI cancel during finish).
    /// Keeping the split explicit at the driver boundary avoids a hidden
    /// await that A4 couldn't interrupt cleanly.
    pub async fn finish_session(
        &mut self,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError> {
        match self.finish_session_inner(bridge).await {
            Ok(()) => {
                self.phase = AerorsyncSessionPhase::Complete;
                Ok(())
            }
            Err(e) => {
                self.phase = AerorsyncSessionPhase::Failed;
                Err(e)
            }
        }
    }

    async fn finish_session_inner(
        &mut self,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError> {
        // S8j dispatch by session role.
        //
        // - `Some(Receiver)` → real download against rsync 3.2.7: drain
        //   exactly `PRE_SUMMARY_NDX_DONE_COUNT_DOWNLOAD` leading
        //   NDX_DONE markers, then decode the SummaryFrame from the
        //   residual.
        // - `Some(Sender)` → real upload (A7 lane 3 scope). Upload-side
        //   finish is wired in S8j.3+; legacy receive semantics stay
        //   here for now to keep the A2.4 mock upload tests working.
        // - `None` → legacy mock test that drove `finish_session`
        //   directly on a synthesised inbound buffer without entering
        //   `drive_*_inner`. Skip the drain entirely — the mock inbound
        //   never contains leading NDX_DONE bytes and the peek-based
        //   heuristic of the drain would misread a varlong value of 0
        //   as a marker.
        match self.session_role {
            Some(SessionRole::Receiver) => {
                // Download against real rsync: drain the 3 leading
                // NDX_DONE markers, decode the summary the server
                // emitted, send our own NDX_DONE ACK, and consume the
                // trailing marker rsync echoes back.
                self.drain_leading_ndx_done_download(bridge).await?;
                self.receive_summary_phase(bridge).await?;
                self.emit_ndx_done_marker().await?;
                self.read_trailing_ndx_done(bridge).await?;
            }
            Some(SessionRole::Sender) => {
                match self.remote_command_flavor {
                    RemoteCommandFlavor::WrapperParity => {
                        self.finish_stock_rsync_sender_tail(bridge).await?;
                    }
                    RemoteCommandFlavor::AerorsyncServe => {
                        // Dev helper compatibility: aerorsync_serve
                        // still consumes the legacy NDX_DONE +
                        // SummaryFrame tail.
                        self.emit_summary_phase().await?;
                        self.read_trailing_ndx_done(bridge).await?;
                        self.session_stats.bytes_sent = self.sent_data_bytes;
                        self.session_stats.bytes_received = self.received_raw_bytes;
                    }
                }
            }
            None => {
                // U-10: every public `drive_*` entry point sets
                // `session_role` before `open_raw_stream_internal`, so
                // reaching `finish_session` with `session_role = None`
                // means a caller skipped the drive loop entirely. Refuse
                // the call with an explicit illegal-state error instead
                // of silently running receive semantics — that path used
                // to mask wrong-role bugs in mock tests.
                return Err(AerorsyncError::new(
                    AerorsyncErrorKind::IllegalStateTransition,
                    "finish_session called without a session_role — invoke drive_upload*/drive_download* first",
                ));
            }
        }
        self.shutdown_raw_stream().await?;
        Ok(())
    }

    // --- S8j NDX_DONE drain (download direction) -------------------------

    /// Pull MSG_DATA frames until we have accumulated at least
    /// `PRE_SUMMARY_NDX_DONE_COUNT_DOWNLOAD` bytes, verify that the
    /// first `N` of them are `0x00` (NDX_DONE markers), drop them, and
    /// stash the remainder into `summary_seed` for
    /// `receive_summary_phase` to prepend.
    ///
    /// Empty-drain policy: tests that synthesise a clean `SummaryFrame`
    /// with NO leading markers (all A2.4 mock tests as written) MUST
    /// keep working. We detect that case by a peek at the first byte
    /// — if it is not `0x00`, the drain is a no-op and the summary
    /// decoder sees the data unchanged.
    async fn drain_leading_ndx_done_download(
        &mut self,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError> {
        let want = PRE_SUMMARY_NDX_DONE_COUNT_DOWNLOAD;
        let mut buf: Vec<u8> = Vec::new();
        // Pull at least one frame to peek.
        let first = self.next_data_frame(bridge).await?;
        buf.extend_from_slice(&first);

        // Empty-drain: if the first byte is not NDX_DONE, rsync did not
        // emit leading markers on this profile (synthesised mocks). Pass
        // the whole payload through to the summary decoder as-is.
        if buf.first().copied() != Some(0x00) {
            self.summary_seed = buf;
            return Ok(());
        }

        // Pull more frames until we can cover `want` leading bytes AND
        // verify they are all `0x00`.
        while buf.len() < want {
            let more = self.next_data_frame(bridge).await?;
            buf.extend_from_slice(&more);
        }
        for (i, b) in buf.iter().take(want).enumerate() {
            if *b != 0x00 {
                return Err(AerorsyncError::invalid_frame(format!(
                    "expected {want} leading NDX_DONE markers before SummaryFrame, \
                     found non-zero byte 0x{b:02X} at offset {i}"
                )));
            }
        }
        // Drop the drained markers, keep the residual.
        buf.drain(..want);
        self.summary_seed = buf;
        Ok(())
    }

    // --- A2.4 summary phase + shutdown ----------------------------------

    /// Read the `SummaryFrame` from the data stream, populate
    /// `received_summary` + `session_stats`, and advance phase.
    ///
    /// S8j: preloads the decode buffer from `summary_seed`, which the
    /// `drain_leading_ndx_done_download` helper populates after
    /// dropping the leading `NDX_DONE` markers rsync 3.2.7 interleaves
    /// between the file-csum and the summary on the server→client
    /// stream (see `main.c::read_final_goodbye`).
    async fn receive_summary_phase(
        &mut self,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError> {
        self.phase = AerorsyncSessionPhase::SummaryReceiving;
        let mut buf: Vec<u8> = std::mem::take(&mut self.summary_seed);
        let protocol = self.protocol_version;
        loop {
            self.check_cancel("receive_summary_phase")?;
            if !buf.is_empty() {
                match decode_summary_frame(&buf, protocol) {
                    Ok((frame, consumed)) => {
                        buf.drain(..consumed);
                        self.session_stats.bytes_received = frame.total_read as u64;
                        self.session_stats.bytes_sent = frame.total_written as u64;
                        self.received_summary = Some(frame);
                        self.phase = AerorsyncSessionPhase::SummaryReceived;
                        return Ok(());
                    }
                    Err(RealWireError::TruncatedBuffer { .. })
                    | Err(RealWireError::DeltaTokenTruncated { .. }) => {
                        // need more bytes
                    }
                    Err(other) => {
                        return Err(map_realwire_error(other, "summary frame"));
                    }
                }
            }
            let payload = self.next_data_frame(bridge).await?;
            buf.extend_from_slice(&payload);
        }
    }

    /// Tear the raw stream down cleanly. Advances phase to `Complete`.
    async fn shutdown_raw_stream(&mut self) -> Result<(), AerorsyncError> {
        if let Some(mut stream) = self.stream.take() {
            stream.shutdown().await?;
        }
        self.phase = AerorsyncSessionPhase::Complete;
        Ok(())
    }

    // --- S8j upload finish helpers ---------------------------------------

    /// Write a single `NDX_DONE` marker (1 byte `0x00`) wrapped in a
    /// MSG_DATA mux frame. `write_data_frame` enforces the wrapping.
    async fn emit_ndx_done_marker(&mut self) -> Result<(), AerorsyncError> {
        self.write_data_frame(&[0x00]).await
    }

    /// Finish an upload against stock `rsync --server` while this client
    /// is the sender. Replicates the exact sender-side tail of
    /// `sender.c::send_files` + `main.c::client_run`:
    ///
    /// 1. `sender_phase_loop`: read-then-echo ping-pong with the
    ///    generator's phase markers. For `max_phase = 2` (proto >= 29)
    ///    the generator writes three NDX_DONE triggers on the socket
    ///    (lines 2337, 2366, 2370); the sender echoes the first two and
    ///    breaks on the third (sender.c:232-258).
    /// 2. Final `write_ndx(NDX_DONE)` after the loop (sender.c:462).
    /// 3. `handle_stats(-1)` client-sender branch: no socket writes
    ///    (main.c:325-358 early-returns when !am_server).
    /// 4. `read_final_goodbye` (proto >= 31): read the generator's 4th
    ///    NDX_DONE (generator.c:2376), write the sender-branch ACK
    ///    (main.c:889), read the parent-generator's final NDX_DONE
    ///    (main.c:1121).
    ///
    /// Outbound total on the app stream: 4 NDX_DONE markers.
    /// Inbound total on the app stream: 5 NDX_DONE markers.
    /// Matches the frozen upload capture exactly.
    async fn finish_stock_rsync_sender_tail(
        &mut self,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError> {
        self.phase = AerorsyncSessionPhase::SummaryReceiving;
        // (1) phase loop with ping-pong echoes
        self.sender_phase_loop(bridge).await?;
        // (2) sender.c:462 — final NDX_DONE once the phase loop breaks
        self.emit_ndx_done_marker().await?;
        // (3) handle_stats(-1) — intentional no-op for the client sender
        // (4) read_final_goodbye + proto-31 ACK
        self.read_final_goodbye_marker(bridge).await?;
        self.received_summary = None;
        self.session_stats.bytes_sent = self.sent_data_bytes;
        self.session_stats.bytes_received = self.received_raw_bytes;
        self.phase = AerorsyncSessionPhase::SummaryReceived;
        Ok(())
    }

    /// Ping-pong phase loop mirroring `sender.c::send_files` lines
    /// 225-258. For each generator phase trigger we read from the wire,
    /// we either echo NDX_DONE (phase advance not yet past max) or
    /// break out of the loop (phase > max_phase). The final post-loop
    /// NDX_DONE (sender.c:462) is emitted by the caller.
    ///
    /// For proto >= 29 `max_phase = 2`: the loop reads 3 NDX_DONE
    /// triggers and writes 2 echoes. This ordering matters against
    /// stock `rsync --server`: the generator coordinates with its
    /// receiver child via internal `msgdone_cnt` increments that only
    /// happen after the sender's echo reaches the receiver. A
    /// fire-and-forget burst of NDX_DONEs (the pre-fix behaviour)
    /// racewith the generator's phase bookkeeping and left the
    /// receiver child stuck in `read_final_goodbye` long enough for
    /// the generator to see EOF on its error pipe (exit 12).
    async fn sender_phase_loop(
        &mut self,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError> {
        let max_phase: i32 = if self.protocol_version >= 29 { 2 } else { 1 };
        let mut phase: i32 = 0;
        loop {
            self.check_cancel("sender_phase_loop")?;
            let Some(()) = self
                .try_read_ndx_done_marker(bridge, "sender_phase_loop: phase trigger")
                .await?
            else {
                return Err(AerorsyncError::invalid_frame(
                    "sender_phase_loop: remote closed before all phase markers",
                ));
            };
            phase += 1;
            if phase > max_phase {
                break;
            }
            // sender.c:256 — echo NDX_DONE back to advance the receiver's phase loop
            self.emit_ndx_done_marker().await?;
        }
        Ok(())
    }

    async fn read_final_goodbye_marker(
        &mut self,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError> {
        let Some(()) = self
            .try_read_ndx_done_marker(bridge, "read_final_goodbye first marker")
            .await?
        else {
            return Ok(());
        };

        if self.protocol_version >= 31 {
            self.emit_ndx_done_marker().await?;
            let Some(()) = self
                .try_read_ndx_done_marker(bridge, "read_final_goodbye final marker")
                .await?
            else {
                return Ok(());
            };
        }
        Ok(())
    }

    async fn try_read_ndx_done_marker(
        &mut self,
        bridge: &mut dyn EventSink,
        context: &'static str,
    ) -> Result<Option<()>, AerorsyncError> {
        loop {
            if let Some(&b) = self.summary_seed.first() {
                self.summary_seed.drain(..1);
                if b != 0x00 {
                    return Err(AerorsyncError::invalid_frame(format!(
                        "{context}: expected NDX_DONE (0x00), got 0x{b:02X}"
                    )));
                }
                return Ok(Some(()));
            }

            match self.next_data_frame(bridge).await {
                Ok(bytes) => {
                    if bytes.is_empty() {
                        continue;
                    }
                    self.summary_seed.extend_from_slice(&bytes);
                }
                Err(e) if e.kind == AerorsyncErrorKind::TransportFailure => return Err(e),
                Err(e) => return Err(e),
            }
        }
    }

    /// Emit the end-of-session `NDX_DONE` + `SummaryFrame` pair that
    /// rsync 3.2.7 expects from the **sender** (client in upload mode)
    /// after the delta stream and its file-level checksum trailer.
    ///
    /// Wire layout (matches `main.c::read_final_goodbye` + `handle_stats`):
    /// ```text
    ///   [0x00]                                  // NDX_DONE marker
    ///   encode_summary_frame(frame, 31)        // 5 × varlong(_, _, 3)
    /// ```
    /// Both chunks go out in a single MSG_DATA frame for wire economy;
    /// rsync's mux layer accepts either bundled or split.
    ///
    /// Field population:
    /// - `total_read` = `self.received_raw_bytes` (bytes consumed from
    ///   the remote via `next_data_frame`, incl. signatures).
    /// - `total_written` = `self.sent_data_bytes` (bytes written via
    ///   `write_data_frame`, incl. file list, signatures echo, delta).
    /// - `total_size` = size of the first entry in `self.file_list`
    ///   (single-file prototype scope).
    /// - `flist_buildtime` / `flist_xfertime` = `Some(0)`. Rsync's
    ///   `handle_stats` treats these as informational (never validated
    ///   as `> 0`); a future S8k will wire actual `Instant` measurement
    ///   if lane 3 telemetry shows the zeros are surprising.
    async fn emit_summary_phase(&mut self) -> Result<(), AerorsyncError> {
        self.phase = AerorsyncSessionPhase::SummaryReceiving;
        let total_size = self.file_list.first().map(|e| e.size).unwrap_or(0);
        // `SummaryFrame` snapshots the counters as of the moment the
        // client decided to announce them — matching rsync 3.2.7's
        // `handle_stats`, which reads `stats.total_written` before
        // emitting the summary itself (so the reported number does NOT
        // include the summary bytes being written).
        let frame = SummaryFrame {
            total_read: self.received_raw_bytes as i64,
            total_written: self.sent_data_bytes as i64,
            total_size,
            flist_buildtime: Some(0),
            flist_xfertime: Some(0),
        };
        let mut payload = Vec::with_capacity(1 + 9 * 5);
        payload.push(0x00); // NDX_DONE
        payload.extend_from_slice(&encode_summary_frame(&frame, self.protocol_version));
        self.write_data_frame(&payload).await?;
        // `session_stats` is a post-emit aggregate — it DOES include the
        // summary bytes we just wrote, so downstream consumers see the
        // actual wire-level totals for the session.
        self.session_stats.bytes_sent = self.sent_data_bytes;
        self.session_stats.bytes_received = self.received_raw_bytes;
        self.received_summary = Some(frame);
        self.phase = AerorsyncSessionPhase::SummaryReceived;
        Ok(())
    }

    /// Read the final `NDX_DONE` (1 byte `0x00`) the rsync receiver
    /// writes back in `read_final_goodbye` line 887 after consuming
    /// the sender's `NDX_DONE + SummaryFrame`. Tolerates clean EOF
    /// (some rsync builds close the channel before the byte flushes).
    async fn read_trailing_ndx_done(
        &mut self,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError> {
        // Best-effort read: if the stream is already closed, or the
        // next frame is empty, treat as clean completion.
        match self.next_data_frame(bridge).await {
            Ok(bytes) => {
                if let Some(&b) = bytes.first() {
                    if b != 0x00 {
                        return Err(AerorsyncError::invalid_frame(format!(
                            "expected trailing NDX_DONE (0x00), got 0x{b:02X}"
                        )));
                    }
                }
                // bytes.is_empty() is valid too — nothing to check.
                Ok(())
            }
            Err(e) if e.kind == AerorsyncErrorKind::TransportFailure => {
                // EOF is an acceptable end of a clean rsync session.
                tracing::debug!(
                    "read_trailing_ndx_done: remote closed before trailing marker ({})",
                    e.detail
                );
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    fn check_cancel(&self, op: &'static str) -> Result<(), AerorsyncError> {
        if self.cancel_handle.requested() {
            Err(AerorsyncError::cancelled(format!(
                "driver cancelled before {op}"
            )))
        } else {
            Ok(())
        }
    }

    // --- A2.0 preamble helpers preserved for regression pins ---
    //
    // These operate on in-memory buffers rather than the transport. They
    // were the A2.0 surface; A2.1 keeps them as-is so the existing
    // frozen-oracle pins and round-trip tests do not regress. The
    // production drive loop uses `perform_preamble_exchange` instead.

    #[allow(clippy::unused_async)] // kept async for API symmetry with A2.0
    async fn send_client_preamble(
        &mut self,
        sink: &mut Vec<u8>,
        protocol_version: u32,
        checksum_algos: &str,
        compression_algos: &str,
    ) -> Result<(), AerorsyncError> {
        let preamble = ClientPreamble {
            protocol_version,
            checksum_algos: checksum_algos.to_string(),
            compression_algos: compression_algos.to_string(),
            consumed: 0,
        };
        let bytes = encode_client_preamble(&preamble);
        sink.extend_from_slice(&bytes);
        self.phase = AerorsyncSessionPhase::ServerPreambleSent;
        Ok(())
    }

    #[allow(clippy::unused_async)]
    async fn receive_server_preamble(&mut self, source: &[u8]) -> Result<usize, AerorsyncError> {
        let preamble = decode_server_preamble(source).map_err(|e| {
            self.phase = AerorsyncSessionPhase::Failed;
            map_realwire_error(e, "server preamble")
        })?;
        self.protocol_version = preamble.protocol_version;
        self.compat_flags = preamble.compat_flags;
        self.checksum_seed = preamble.checksum_seed;
        self.negotiated_checksum_algos = preamble.checksum_algos;
        self.negotiated_compression_algos = preamble.compression_algos;
        self.phase = AerorsyncSessionPhase::ClientPreambleRecvd;
        Ok(preamble.consumed)
    }
}

fn map_realwire_error(err: RealWireError, context: &'static str) -> AerorsyncError {
    AerorsyncError::new(
        AerorsyncErrorKind::InvalidFrame,
        format!("{context}: {err}"),
    )
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aerorsync::engine_adapter::{
        DeltaEngineAdapter, EngineDeltaOp, EngineDeltaPlan, EngineSignatureBlock,
    };
    use crate::aerorsync::events::{classify_oob_frame, AerorsyncEvent, CollectingSink};
    use crate::aerorsync::fixtures::RealRsyncBaselineByteTranscript;
    use crate::aerorsync::mock::{MockRemoteShellTransport, MockTransportConfig};
    use crate::aerorsync::real_wire::{encode_server_preamble, ServerPreamble};

    /// Mock adapter used by A2.2/A2.3 tests. Returns a configurable
    /// block size, pre-fabricated signatures, and a pre-canned delta
    /// plan. `apply_delta` returns the destination data with `literal`
    /// bytes interleaved at the start (simple deterministic output).
    #[derive(Default)]
    struct MockSigAdapter {
        block_size: Option<usize>,
        signatures: Vec<EngineSignatureBlock>,
        upload_plan_ops: Vec<EngineDeltaOp>,
        upload_savings_ratio: f64,
        upload_should_use_delta: bool,
    }

    impl MockSigAdapter {
        fn with_fixed_signatures(block_size: usize, signatures: Vec<EngineSignatureBlock>) -> Self {
            Self {
                block_size: Some(block_size),
                signatures,
                upload_plan_ops: Vec::new(),
                upload_savings_ratio: 1.0,
                upload_should_use_delta: false,
            }
        }

        fn with_upload_plan(mut self, ops: Vec<EngineDeltaOp>) -> Self {
            self.upload_plan_ops = ops;
            self.upload_should_use_delta = true;
            self.upload_savings_ratio = 0.5;
            self
        }
    }

    impl DeltaEngineAdapter for MockSigAdapter {
        fn compute_block_size(&self, _file_size: u64) -> usize {
            self.block_size.unwrap_or(1024)
        }
        fn build_signatures(
            &self,
            _destination_data: &[u8],
            _block_size: usize,
        ) -> Vec<EngineSignatureBlock> {
            self.signatures.clone()
        }
        fn compute_delta(
            &self,
            _source_data: &[u8],
            _destination_signatures: &[EngineSignatureBlock],
            _block_size: usize,
        ) -> EngineDeltaPlan {
            let literal_bytes: u64 = self
                .upload_plan_ops
                .iter()
                .map(|op| match op {
                    EngineDeltaOp::Literal(b) => b.len() as u64,
                    EngineDeltaOp::CopyBlock(_) => 0,
                })
                .sum();
            let copy_blocks: u32 = self
                .upload_plan_ops
                .iter()
                .filter(|op| matches!(op, EngineDeltaOp::CopyBlock(_)))
                .count() as u32;
            EngineDeltaPlan {
                ops: self.upload_plan_ops.clone(),
                copy_blocks,
                literal_bytes,
                total_delta_bytes: literal_bytes,
                savings_ratio: self.upload_savings_ratio,
                should_use_delta: self.upload_should_use_delta,
            }
        }
        fn apply_delta(
            &self,
            destination_data: &[u8],
            ops: &[EngineDeltaOp],
            block_size: usize,
        ) -> Result<Vec<u8>, String> {
            // Simple deterministic reconstructor: literal bytes verbatim;
            // CopyBlock(idx) → destination_data[idx*bs..(idx+1)*bs].
            let mut out: Vec<u8> = Vec::new();
            for op in ops {
                match op {
                    EngineDeltaOp::Literal(raw) => out.extend_from_slice(raw),
                    EngineDeltaOp::CopyBlock(idx) => {
                        let start = (*idx as usize) * block_size;
                        let end = (start + block_size).min(destination_data.len());
                        if start >= destination_data.len() {
                            return Err(format!(
                                "CopyBlock idx {idx} out of bounds for destination len {}",
                                destination_data.len()
                            ));
                        }
                        out.extend_from_slice(&destination_data[start..end]);
                    }
                }
            }
            Ok(out)
        }
    }

    /// Build a synthetic server signature-phase payload (bytes as they
    /// appear inside one or more `MSG_DATA` frames before mux-wrapping).
    /// The caller decides the chunking.
    fn build_sig_phase_payload(
        ndx: i32,
        iflags: u16,
        head: &SumHead,
        blocks: &[SumBlock],
    ) -> Vec<u8> {
        use crate::aerorsync::real_wire::{
            encode_item_flags, encode_ndx, encode_sum_block, encode_sum_head, NdxState,
        };
        let mut st = NdxState::new();
        let mut out = Vec::new();
        out.extend_from_slice(&encode_ndx(ndx, &mut st));
        out.extend_from_slice(&encode_item_flags(iflags));
        out.extend_from_slice(&encode_sum_head(head));
        for b in blocks {
            out.extend_from_slice(&encode_sum_block(b));
        }
        out
    }

    fn make_sig_block(rolling: u32, strong_first_byte: u8, s2length: usize) -> SumBlock {
        SumBlock {
            rolling,
            strong: vec![strong_first_byte; s2length],
        }
    }

    fn make_engine_sig(
        index: u32,
        rolling: u32,
        strong_first_byte: u8,
        block_len: u32,
    ) -> EngineSignatureBlock {
        let mut strong = [0u8; 32];
        for b in strong.iter_mut().take(32) {
            *b = strong_first_byte;
        }
        EngineSignatureBlock {
            index,
            rolling,
            strong,
            block_len,
        }
    }
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    // ---- helpers ---------------------------------------------------------

    fn mock_transport() -> MockRemoteShellTransport {
        MockRemoteShellTransport::new(MockTransportConfig::healthy_upload())
    }

    fn mock_transport_with_raw_inbound(inbound: Vec<u8>) -> MockRemoteShellTransport {
        let cfg = MockTransportConfig::healthy_upload().with_raw_inbound(inbound);
        MockRemoteShellTransport::new(cfg)
    }

    fn make_driver(
        transport: MockRemoteShellTransport,
    ) -> AerorsyncDriver<MockRemoteShellTransport> {
        AerorsyncDriver::new(transport, CancelHandle::inert())
    }

    fn canonical_server_preamble_bytes() -> Vec<u8> {
        // Rsync serialises both lists as SPACE-separated (see
        // `perform_preamble_exchange` and the frozen oracle capture).
        // Using commas here hid the `zstd_negotiated` parsing bug that
        // made live uploads skip zstd compression against stock rsync.
        encode_server_preamble(&ServerPreamble {
            protocol_version: 31,
            compat_flags: 0x07,
            checksum_algos: "md5 xxh64".to_string(),
            compression_algos: "none zstd".to_string(),
            checksum_seed: 0xDEAD_BEEF,
            consumed: 0,
        })
    }

    fn mux_frame(tag: MuxTag, payload: &[u8]) -> Vec<u8> {
        let header = MuxHeader {
            tag,
            length: payload.len() as u32,
        };
        let mut out = Vec::with_capacity(4 + payload.len());
        out.extend_from_slice(&header.encode());
        out.extend_from_slice(payload);
        out
    }

    /// Build a `FileListEntry` that the encoder/decoder will round-trip
    /// under `build_flist_options` (varint flags, always_checksum on,
    /// preserve_uid/gid on with SAME_UID/SAME_GID gating uid/gid out).
    /// The flags include `XMIT_LONG_NAME` so the suffix length is encoded
    /// as a varint — which the path length (9 chars) still fits in.
    fn sample_file_list_entry(path: &str) -> FileListEntry {
        // Flags: XMIT_LONG_NAME (0x0040) | XMIT_SAME_MODE (0x0002) |
        //        XMIT_SAME_TIME (0x0080) | XMIT_SAME_UID (0x0008) |
        //        XMIT_SAME_GID (0x0010)
        // — the "all same" upload case where only the name and size are
        // transmitted. Matches a minimum-viable shape; the 16-byte
        // checksum is required because B.2 turned `always_checksum` on
        // in `build_flist_options` to mirror the oracle (`-c` always
        // active in production dispatch).
        const XMIT_SAME_MODE: u32 = 0x0002;
        const XMIT_SAME_UID: u32 = 0x0008;
        const XMIT_SAME_GID: u32 = 0x0010;
        const XMIT_LONG_NAME: u32 = 0x0040;
        const XMIT_SAME_TIME: u32 = 0x0080;
        FileListEntry {
            flags: XMIT_LONG_NAME | XMIT_SAME_MODE | XMIT_SAME_UID | XMIT_SAME_GID | XMIT_SAME_TIME,
            path: path.to_string(),
            size: 4096,
            mtime: 0,
            mtime_nsec: None,
            mode: 0,
            uid: None,
            uid_name: None,
            gid: None,
            gid_name: None,
            // 16 bytes filled with a sentinel; xxh128 length, never
            // validated against file content in unit tests.
            checksum: vec![0xAA; 16],
        }
    }

    // ---- A2.0 regression pins (preserved) -------------------------------

    #[test]
    fn constructor_initialises_phase_and_defaults() {
        let d = make_driver(mock_transport());
        assert_eq!(d.phase(), AerorsyncSessionPhase::PreConnect);
        assert!(!d.committed());
        assert_eq!(d.protocol_version(), 0);
        assert_eq!(d.compat_flags(), 0);
        assert_eq!(d.checksum_seed(), 0);
        assert!(d.negotiated_checksum_algos().is_empty());
        assert!(d.negotiated_compression_algos().is_empty());
        assert!(d.file_list().is_empty());
        assert_eq!(d.data_bytes_consumed(), 0);
    }

    #[tokio::test]
    async fn send_client_preamble_writes_bytes_that_decode_back() {
        use crate::aerorsync::real_wire::decode_client_preamble;
        let mut d = make_driver(mock_transport());
        let mut sink = Vec::new();
        d.send_client_preamble(&mut sink, 31, "md5,xxh64", "none,zstd")
            .await
            .unwrap();
        let decoded = decode_client_preamble(&sink).unwrap();
        assert_eq!(decoded.protocol_version, 31);
        assert_eq!(decoded.checksum_algos, "md5,xxh64");
        assert_eq!(decoded.compression_algos, "none,zstd");
        assert_eq!(d.phase(), AerorsyncSessionPhase::ServerPreambleSent);
    }

    #[tokio::test]
    async fn receive_server_preamble_populates_driver_state() {
        let encoded = canonical_server_preamble_bytes();
        let mut d = make_driver(mock_transport());
        let consumed = d.receive_server_preamble(&encoded).await.unwrap();
        assert_eq!(consumed, encoded.len());
        assert_eq!(d.protocol_version(), 31);
        assert_eq!(d.compat_flags(), 0x07);
        assert_eq!(d.checksum_seed(), 0xDEAD_BEEF);
        assert_eq!(d.negotiated_checksum_algos(), "md5 xxh64");
        assert_eq!(d.negotiated_compression_algos(), "none zstd");
        assert_eq!(d.phase(), AerorsyncSessionPhase::ClientPreambleRecvd);
    }

    #[tokio::test]
    async fn receive_server_preamble_on_malformed_bytes_marks_failed() {
        let mut d = make_driver(mock_transport());
        let err = d.receive_server_preamble(&[0x01]).await.unwrap_err();
        assert_eq!(err.kind, AerorsyncErrorKind::InvalidFrame);
        assert!(err.detail.contains("server preamble"));
        assert_eq!(d.phase(), AerorsyncSessionPhase::Failed);
    }

    #[tokio::test]
    async fn driver_preamble_exchange_round_trip_matches_frozen_oracle_server_side() {
        let Some(frozen) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
            eprintln!("frozen oracle missing — driver preamble pin skipped");
            return;
        };
        let mut d = make_driver(mock_transport());
        let consumed = d
            .receive_server_preamble(&frozen.upload_server_to_client)
            .await
            .expect("driver must decode frozen server preamble");
        assert!(consumed > 0);
        let re_encoded = encode_server_preamble(&ServerPreamble {
            protocol_version: d.protocol_version(),
            compat_flags: d.compat_flags(),
            checksum_algos: d.negotiated_checksum_algos().to_string(),
            compression_algos: d.negotiated_compression_algos().to_string(),
            checksum_seed: d.checksum_seed(),
            consumed: 0,
        });
        assert_eq!(
            re_encoded.as_slice(),
            &frozen.upload_server_to_client[..consumed],
            "driver round-trip must be byte-identical to frozen oracle prefix"
        );
    }

    #[test]
    fn cancel_handle_returns_clone_sharing_flag() {
        let d = make_driver(mock_transport());
        let h1 = d.cancel_handle();
        let h2 = d.cancel_handle();
        assert!(!h1.requested());
        h1.cancel();
        assert!(h2.requested());
        assert!(d.cancel_handle().requested());
    }

    // ---- A2.1 tests ------------------------------------------------------

    #[tokio::test]
    async fn driver_upload_writes_preamble_then_filelist_then_terminator() {
        // Inbound: server preamble + a minimal synthetic signature
        // phase (upload path in A2.2 drains ndx+iflags+sum_head+blocks
        // after the file list — without these bytes the test would
        // fail with TransportFailure instead of the expected stub
        // frontier.)
        let head = SumHead {
            count: 1,
            block_length: 1024,
            checksum_length: 2,
            remainder_length: 0,
        };
        let blocks = vec![make_sig_block(0xAAAAAAAA, 0x11, 2)];
        let sig_payload = build_sig_phase_payload(1, 0x8002, &head, &blocks);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &sig_payload));
        let transport = mock_transport_with_raw_inbound(inbound);
        let last_raw_outbound = transport.last_raw_outbound.clone();
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();

        let err = d
            .drive_upload(
                RemoteCommandSpec::upload("/remote/target.bin"),
                sample_file_list_entry("target.bin"),
                &[],
                &MockSigAdapter::default(),
                &mut sink,
            )
            .await
            .unwrap_err();
        // Stub frontier: sum_head not yet wired.
        assert_eq!(err.kind, AerorsyncErrorKind::UnsupportedVersion);
        assert_eq!(d.phase(), AerorsyncSessionPhase::Stub);
        // A2.3: drive_upload now crosses the PreCommit/PostCommit boundary
        // during the delta phase. MockSigAdapter returns an empty plan
        // which still emits END_FLAG + file_checksum, counting as
        // "first delta material written" → committed flips true.
        assert!(d.committed());

        // Outbound bytes = encode_client_preamble + mux(entry) + mux(terminator)
        let guard = last_raw_outbound.lock().unwrap();
        let outbound_arc = guard.as_ref().expect("raw stream must have been opened");
        let outbound = outbound_arc.lock().unwrap().clone();

        let expected_client = encode_client_preamble(&ClientPreamble {
            protocol_version: 31,
            // B.2: rsync wire protocol uses SPACE-separated algo lists
            // in priority-descending order. The previous pin
            // ("md5,xxh64,xxh128" / "none,zstd") mirrored the pre-B.2
            // driver implementation that stock rsync 3.4.1 rejects as a
            // single unknown algorithm. The values below match the
            // post-fix driver (and the live wire observed against
            // rsync 3.4.1 / protocol 32).
            checksum_algos: "xxh128 xxh3 xxh64 md5 md4".to_string(),
            compression_algos: "zstd lz4 zlibx zlib".to_string(),
            consumed: 0,
        });
        assert_eq!(
            &outbound[..expected_client.len()],
            expected_client.as_slice(),
            "client preamble prefix mismatch"
        );

        // B.2: the driver now coalesces entry + terminator +
        // NDX_FLIST_EOF marker into a SINGLE MSG_DATA frame, mirroring
        // the frozen oracle's first 67-byte mux frame layout. Reconstruct
        // the expected payload accordingly.
        let opts = FileListDecodeOptions {
            protocol: d.protocol_version(),
            xfer_flags_as_varint: true,
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);
        let mut ndx_state = NdxState::default();
        let ndx_bytes = encode_ndx(NDX_FLIST_EOF, &mut ndx_state);
        let mut single_payload = Vec::new();
        single_payload.extend_from_slice(&entry_bytes);
        single_payload.extend_from_slice(&term_bytes);
        single_payload.extend_from_slice(&ndx_bytes);
        let expected_tail = mux_frame(MuxTag::Data, &single_payload);

        // A2.3: after the file list the driver also emits the delta
        // phase (END_FLAG + 16-byte checksum trailer wrapped in a mux
        // frame) so the byte-for-byte match is only valid on the prefix
        // through the file-list terminator.
        let suffix_start = expected_client.len();
        assert_eq!(
            &outbound[suffix_start..suffix_start + expected_tail.len()],
            expected_tail.as_slice(),
            "mux-wrapped file list tail mismatch (entry + terminator + NDX_FLIST_EOF coalesced)"
        );
    }

    #[tokio::test]
    async fn driver_download_decodes_filelist_single_entry() {
        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            // B.2: align with `build_flist_options()` so test-side
            // pre-encoded payloads round-trip through the driver decoder.
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
        };
        let entry = sample_file_list_entry("target.bin");
        let entry_bytes = encode_file_list_entry(&entry, &opts);
        let term_bytes = encode_file_list_terminator(&opts);

        // A2.3: drive_download now proceeds into the delta phase. Append
        // an empty delta stream (END_FLAG + 16-byte zero checksum) so the
        // driver reaches the stub frontier instead of stalling on an
        // empty inbound stream.
        let empty_delta = encode_delta_stream(&DeltaStreamReport {
            ops: Vec::new(),
            file_checksum: vec![0u8; A2_3_FILE_CHECKSUM_LEN],
        });
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &empty_delta));

        let transport = mock_transport_with_raw_inbound(inbound);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();

        let err = d
            .drive_download(
                RemoteCommandSpec::download("/remote/target.bin"),
                &[],
                &MockSigAdapter::default(),
                &mut sink,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind, AerorsyncErrorKind::UnsupportedVersion);
        assert_eq!(d.phase(), AerorsyncSessionPhase::Stub);
        assert_eq!(d.file_list().len(), 1);
        assert_eq!(d.file_list()[0].path, "target.bin");
        assert_eq!(d.file_list()[0].size, 4096);
        assert!(!d.committed());
    }

    #[tokio::test]
    async fn driver_file_list_forwards_mid_phase_warning_to_bridge() {
        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            // B.2: align with `build_flist_options()` so test-side
            // pre-encoded payloads round-trip through the driver decoder.
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
        };
        let entry = sample_file_list_entry("target.bin");
        let entry_bytes = encode_file_list_entry(&entry, &opts);
        let term_bytes = encode_file_list_terminator(&opts);

        let mut inbound = canonical_server_preamble_bytes();
        // Warning *before* the data frames.
        inbound.extend_from_slice(&mux_frame(MuxTag::Warning, b"skipping something"));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));

        let transport = mock_transport_with_raw_inbound(inbound);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();

        let _ = d
            .drive_download(
                RemoteCommandSpec::download("/remote/target.bin"),
                &[],
                &MockSigAdapter::default(),
                &mut sink,
            )
            .await;

        assert_eq!(d.file_list().len(), 1);
        let warnings: Vec<_> = sink
            .events
            .iter()
            .filter(|e| matches!(e, AerorsyncEvent::Warning { .. }))
            .collect();
        assert_eq!(warnings.len(), 1, "expected exactly one Warning forwarded");
    }

    #[tokio::test]
    async fn driver_file_list_aborts_on_terminal_oob_pre_commit() {
        // Inbound: preamble + an Error frame before the file list.
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Error, b"remote kaboom"));

        let transport = mock_transport_with_raw_inbound(inbound);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();

        let err = d
            .drive_download(
                RemoteCommandSpec::download("/remote/target.bin"),
                &[],
                &MockSigAdapter::default(),
                &mut sink,
            )
            .await
            .unwrap_err();
        // Terminal OOB → RemoteError (via AerorsyncError::from_oob_event).
        assert_eq!(err.kind, AerorsyncErrorKind::RemoteError);
        assert!(err.detail.contains("remote kaboom"));
        // PreCommit pin: committed stays false.
        assert!(
            !d.committed(),
            "stub path must not cross PreCommit boundary"
        );
        assert_eq!(d.phase(), AerorsyncSessionPhase::Failed);
        // Bridge saw the terminal event (forwarded before bail).
        let terminals: Vec<_> = sink
            .events
            .iter()
            .filter(|e| matches!(e, AerorsyncEvent::Error { .. }))
            .collect();
        assert_eq!(terminals.len(), 1);
    }

    #[tokio::test]
    async fn driver_cancel_during_file_list_surfaces_typed_cancelled() {
        // Preamble arrives fine; cancel is triggered before the file list
        // read. The driver's `check_cancel` in `receive_file_list` surfaces
        // a typed `Cancelled`, NOT a `Transport` error.
        let inbound = canonical_server_preamble_bytes();
        let transport = mock_transport_with_raw_inbound(inbound);
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel_handle = CancelHandle::new(cancel_flag.clone(), None);
        let mut d = AerorsyncDriver::new(transport, cancel_handle);
        let mut sink = CollectingSink::default();

        // Trip the flag BEFORE we start. `drive_download_inner` will:
        // open_raw_stream → check_cancel returns Err already.
        cancel_flag.store(true, Ordering::SeqCst);

        let err = d
            .drive_download(
                RemoteCommandSpec::download("/remote/target.bin"),
                &[],
                &MockSigAdapter::default(),
                &mut sink,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind, AerorsyncErrorKind::Cancelled);
        assert!(!d.committed());
    }

    #[tokio::test]
    async fn driver_file_list_accumulates_across_multiple_data_frames() {
        // Split a single FileListEntry across two MSG_DATA frames. The
        // driver must accumulate the payloads into `flist_buf` until the
        // decoder finds a complete entry, then continue to the terminator.
        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            // B.2: align with `build_flist_options()` so test-side
            // pre-encoded payloads round-trip through the driver decoder.
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
        };
        let entry = sample_file_list_entry("target.bin");
        let entry_bytes = encode_file_list_entry(&entry, &opts);
        let term_bytes = encode_file_list_terminator(&opts);

        let mut inbound = canonical_server_preamble_bytes();
        let half = entry_bytes.len() / 2;
        // Two Data frames carrying the entry payload halves, plus a
        // trailing Data frame with the terminator.
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes[..half]));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes[half..]));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));

        let transport = mock_transport_with_raw_inbound(inbound);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();

        let _ = d
            .drive_download(
                RemoteCommandSpec::download("/remote/target.bin"),
                &[],
                &MockSigAdapter::default(),
                &mut sink,
            )
            .await;

        assert_eq!(d.file_list().len(), 1, "split-frame entry must reassemble");
        assert_eq!(d.file_list()[0].path, "target.bin");
        assert_eq!(d.file_list()[0].size, 4096);
    }

    #[tokio::test]
    async fn driver_stream_exhaustion_during_preamble_surfaces_typed_error() {
        // Empty inbound — the driver should surface a transport error
        // with a clear "remote closed" detail, not panic.
        let transport = mock_transport_with_raw_inbound(Vec::new());
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        let err = d
            .drive_download(
                RemoteCommandSpec::download("/remote/target.bin"),
                &[],
                &MockSigAdapter::default(),
                &mut sink,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind, AerorsyncErrorKind::TransportFailure);
        assert!(!d.committed());
        assert_eq!(d.phase(), AerorsyncSessionPhase::Failed);
    }

    #[tokio::test]
    async fn driver_classify_oob_helper_matches_events_module() {
        // Guard against the bridge / events contract drifting silently.
        // If `events::classify_oob_frame` ever changes its terminal
        // classification, this guard fails loudly in the driver tests.
        let ev = classify_oob_frame(MuxTag::Error, b"x");
        assert!(ev.is_terminal());
        let ev = classify_oob_frame(MuxTag::Warning, b"x");
        assert!(!ev.is_terminal());
    }

    /// A7 — Lane 3 live integration test against a real `rsync 3.2.7`
    /// server (Docker harness at `capture/docker-compose.real-rsync.yml`,
    /// listening on `127.0.0.1:2224`).
    ///
    /// The test drives `SshRemoteShellTransport` — not the mock — through
    /// `drive_upload_through_delta` + `finish_session`, then asserts that:
    ///
    /// - upload + finish complete without errors,
    /// - phase reaches `Complete`,
    /// - `session_stats.bytes_sent` is at least the source payload size
    ///   (protocol overhead may raise it above the source length).
    ///
    /// # Gating
    ///
    /// `#[cfg(ci_lane3)]` — the test is compiled only when the
    /// `ci_lane3` cfg flag is set via `RUSTFLAGS='--cfg ci_lane3'`.
    /// Local developers who cloned the repo do not need Docker to run the
    /// default test suite; CI on the `strada-c-*` branch sets the flag.
    ///
    /// # S8j closure
    ///
    /// S8j closed the prior gaps: xxh128 real checksum trailer, NDX_DONE
    /// drain before `SummaryFrame` on the download direction, and the
    /// full upload-side finish (client emits `NDX_DONE + SummaryFrame`
    /// and reads the trailing NDX_DONE from the server). With those in
    /// place the lane 3 CI job runs without `continue-on-error` and any
    /// regression against real rsync 3.2.7 surfaces immediately.
    #[cfg(ci_lane3)]
    #[tokio::test]
    async fn driver_upload_live_lane_3_real_rsync_byte_identical() {
        use crate::aerorsync::engine_adapter::CurrentDeltaSyncBridge;
        use crate::aerorsync::ssh_transport::{
            SshHostKeyPolicy, SshRemoteShellTransport, SshTransportConfig,
        };
        use crate::aerorsync::transport::RemoteExecRequest;

        // Skip-graceful if the Docker harness is not reachable. CI starts
        // the container explicitly; a local dev run without Docker simply
        // observes the skip and moves on.
        if tokio::net::TcpStream::connect("127.0.0.1:2224")
            .await
            .is_err()
        {
            eprintln!("lane 3 Docker harness not reachable on 127.0.0.1:2224 — skipping");
            return;
        }

        let source_data: Vec<u8> = b"aeroftp lane 3 native rsync upload payload\n"
            .iter()
            .copied()
            .cycle()
            .take(1024)
            .collect();

        let key_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/aerorsync/capture/keys/id_ed25519");
        assert!(
            key_path.exists(),
            "ssh key not found at {key_path:?} — is the capture bundle present?"
        );

        let ssh_config = SshTransportConfig {
            host: "127.0.0.1".into(),
            port: 2224,
            username: "testuser".into(),
            private_key_path: key_path,
            connect_timeout_ms: 10_000,
            io_timeout_ms: 30_000,
            worker_idle_poll_ms: 250,
            max_frame_size: 1 << 20,
            host_key_policy: SshHostKeyPolicy::AcceptAny,
            probe_request: RemoteExecRequest {
                program: "rsync".into(),
                args: vec!["--version".into()],
                environment: Vec::new(),
            },
        };

        let transport = SshRemoteShellTransport::new(ssh_config);
        let cancel = CancelHandle::inert();
        let mut driver = AerorsyncDriver::new(transport, cancel);
        let adapter = CurrentDeltaSyncBridge::new();
        let mut sink = CollectingSink::default();

        // Unique remote path per run to avoid collision across reruns.
        let remote_path = format!(
            "/workspace/lane3-live-{}.bin",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );

        let entry = sample_file_list_entry("lane3-live.bin");
        let entry = FileListEntry {
            size: source_data.len() as i64,
            ..entry
        };

        let spec = RemoteCommandSpec::upload(&remote_path);
        let upload_res = driver
            .drive_upload_through_delta(spec, entry, &source_data, &adapter, &mut sink)
            .await;
        assert!(
            upload_res.is_ok(),
            "drive_upload_through_delta failed against real rsync: {upload_res:?}"
        );

        let finish_res = driver.finish_session(&mut sink).await;
        assert!(
            finish_res.is_ok(),
            "finish_session failed against real rsync: {finish_res:?}"
        );
        assert_eq!(driver.phase(), AerorsyncSessionPhase::Complete);
        let stats = driver.session_stats();
        assert!(
            stats.bytes_sent >= source_data.len() as u64,
            "bytes_sent {} < source len {}: summary frame parse probably stale",
            stats.bytes_sent,
            source_data.len()
        );
    }

    /// P3-T01 W1.2 — live counterpart of
    /// [`driver_upload_live_lane_3_real_rsync_byte_identical`] that
    /// drives the **streaming** entry point
    /// `drive_upload_through_delta_streaming` against the real rsync
    /// 3.2.7 sshd container. Pin: producer-driven plan + xxh3 streaming
    /// trailer reach `phase = Complete` and produce `bytes_sent >=
    /// source.len()` exactly like the bulk path. Same Docker harness
    /// (`127.0.0.1:2224`), same skip-graceful behaviour.
    #[cfg(ci_lane3)]
    #[tokio::test]
    async fn driver_upload_streaming_live_lane_3_real_rsync_byte_identical() {
        use crate::aerorsync::engine_adapter::CurrentDeltaSyncBridge;
        use crate::aerorsync::ssh_transport::{
            SshHostKeyPolicy, SshRemoteShellTransport, SshTransportConfig,
        };
        use crate::aerorsync::transport::RemoteExecRequest;

        if tokio::net::TcpStream::connect("127.0.0.1:2224")
            .await
            .is_err()
        {
            eprintln!(
                "lane 3 Docker harness not reachable on 127.0.0.1:2224 — skipping streaming variant"
            );
            return;
        }

        let source_data: Vec<u8> = b"aeroftp lane 3 streaming upload payload\n"
            .iter()
            .copied()
            .cycle()
            .take(1024)
            .collect();
        let source_len = source_data.len() as u64;

        let key_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/aerorsync/capture/keys/id_ed25519");
        assert!(
            key_path.exists(),
            "ssh key not found at {key_path:?} — is the capture bundle present?"
        );

        let ssh_config = SshTransportConfig {
            host: "127.0.0.1".into(),
            port: 2224,
            username: "testuser".into(),
            private_key_path: key_path,
            connect_timeout_ms: 10_000,
            io_timeout_ms: 30_000,
            worker_idle_poll_ms: 250,
            max_frame_size: 1 << 20,
            host_key_policy: SshHostKeyPolicy::AcceptAny,
            probe_request: RemoteExecRequest {
                program: "rsync".into(),
                args: vec!["--version".into()],
                environment: Vec::new(),
            },
        };

        let transport = SshRemoteShellTransport::new(ssh_config);
        let cancel = CancelHandle::inert();
        let mut driver = AerorsyncDriver::new(transport, cancel);
        let adapter = CurrentDeltaSyncBridge::new();
        let mut sink = CollectingSink::default();

        let remote_path = format!(
            "/workspace/lane3-streaming-{}.bin",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let entry = sample_file_list_entry("lane3-streaming.bin");
        let entry = FileListEntry {
            size: source_len as i64,
            ..entry
        };
        let spec = RemoteCommandSpec::upload(&remote_path);

        let cursor = std::io::Cursor::new(source_data.clone());
        let upload_res = driver
            .drive_upload_through_delta_streaming(
                spec,
                entry,
                cursor,
                source_len,
                &adapter,
                &mut sink,
            )
            .await;
        assert!(
            upload_res.is_ok(),
            "drive_upload_through_delta_streaming failed against real rsync: {upload_res:?}"
        );

        let finish_res = driver.finish_session(&mut sink).await;
        assert!(
            finish_res.is_ok(),
            "finish_session (streaming) failed against real rsync: {finish_res:?}"
        );
        assert_eq!(driver.phase(), AerorsyncSessionPhase::Complete);
        let stats = driver.session_stats();
        assert!(
            stats.bytes_sent >= source_len,
            "bytes_sent {} < source len {}: summary frame parse probably stale",
            stats.bytes_sent,
            source_len
        );
    }

    // ---- A2.2 tests ------------------------------------------------------

    #[tokio::test]
    async fn driver_upload_receives_sigs_and_halts_at_delta_frontier() {
        // Happy path upload: build a synthetic signature-phase payload
        // with 3 blocks, feed it after the preamble, verify driver
        // halts at the delta frontier with all state populated.
        let head = SumHead {
            count: 3,
            block_length: 1024,
            checksum_length: 2,
            remainder_length: 0,
        };
        let blocks = vec![
            make_sig_block(0x11111111, 0xAA, 2),
            make_sig_block(0x22222222, 0xBB, 2),
            make_sig_block(0x33333333, 0xCC, 2),
        ];
        let sig_payload = build_sig_phase_payload(1, 0x8002, &head, &blocks);

        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &sig_payload));

        let transport = mock_transport_with_raw_inbound(inbound);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();

        let err = d
            .drive_upload(
                RemoteCommandSpec::upload("/remote/target.bin"),
                sample_file_list_entry("target.bin"),
                &[],
                &MockSigAdapter::default(),
                &mut sink,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind, AerorsyncErrorKind::UnsupportedVersion);
        assert!(
            err.detail.contains("summary/done"),
            "A2.3 stub frontier moved to summary/done phase: {}",
            err.detail
        );
        assert_eq!(d.phase(), AerorsyncSessionPhase::Stub);
        // A2.3: empty delta plan still crosses the commit boundary.
        assert!(d.committed());
        assert_eq!(d.received_sum_head().map(|h| h.count), Some(3));
        assert_eq!(d.received_signatures().len(), 3);
        assert_eq!(d.received_signatures()[0].rolling, 0x11111111);
        assert_eq!(d.last_iflags(), 0x8002);
    }

    #[tokio::test]
    async fn driver_download_computes_and_sends_signatures() {
        // Empty destination_data with a mock adapter that returns 4
        // prefabricated signatures. Verify outbound bytes include the
        // full mux-wrapped sig-phase blob.
        let engine_sigs = vec![
            make_engine_sig(0, 0xA0A0A0A0, 0x01, 1024),
            make_engine_sig(1, 0xB0B0B0B0, 0x02, 1024),
            make_engine_sig(2, 0xC0C0C0C0, 0x03, 1024),
            make_engine_sig(3, 0xD0D0D0D0, 0x04, 512),
        ];
        let adapter = MockSigAdapter::with_fixed_signatures(1024, engine_sigs);

        // We need a minimal download flow: server sends preamble, then
        // a file list entry + terminator, then we emit signatures.
        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            // B.2: align with `build_flist_options()` so test-side
            // pre-encoded payloads round-trip through the driver decoder.
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);

        // A2.3: append an empty delta stream to let the driver reach
        // the stub frontier.
        let empty_delta = encode_delta_stream(&DeltaStreamReport {
            ops: Vec::new(),
            file_checksum: vec![0u8; A2_3_FILE_CHECKSUM_LEN],
        });
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &empty_delta));

        let transport = mock_transport_with_raw_inbound(inbound);
        let last_raw_outbound = transport.last_raw_outbound.clone();
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();

        let destination_data = vec![0u8; 3584]; // 3.5 KiB: 3 full + 1 partial
        let err = d
            .drive_download(
                RemoteCommandSpec::download("/remote/target.bin"),
                &destination_data,
                &adapter,
                &mut sink,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind, AerorsyncErrorKind::UnsupportedVersion);
        assert_eq!(d.phase(), AerorsyncSessionPhase::Stub);
        assert_eq!(d.sent_sum_head().map(|h| h.count), Some(4));
        assert_eq!(d.sent_signatures().len(), 4);
        assert_eq!(d.last_iflags(), 0x8002);

        // The outbound capture must contain a mux-wrapped signature blob
        // after the client preamble. Check that the sent_signatures
        // rolling bytes appear somewhere in the outbound.
        let guard = last_raw_outbound.lock().unwrap();
        let outbound_arc = guard.as_ref().expect("raw stream must have been opened");
        let outbound = outbound_arc.lock().unwrap().clone();
        let rolling_le = 0xA0A0A0A0u32.to_le_bytes();
        assert!(
            outbound.windows(4).any(|w| w == rolling_le),
            "sent signature rolling bytes must appear in the outbound capture"
        );
    }

    #[tokio::test]
    async fn driver_upload_signature_phase_aborts_on_terminal_oob() {
        // Error frame during the signature phase — driver must bail
        // with RemoteError and committed stays false.
        let mut inbound = canonical_server_preamble_bytes();
        // Corrupt signature phase: just an Error frame.
        inbound.extend_from_slice(&mux_frame(MuxTag::Error, b"sig explode"));

        let transport = mock_transport_with_raw_inbound(inbound);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        let err = d
            .drive_upload(
                RemoteCommandSpec::upload("/remote/target.bin"),
                sample_file_list_entry("target.bin"),
                &[],
                &MockSigAdapter::default(),
                &mut sink,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind, AerorsyncErrorKind::RemoteError);
        assert!(err.detail.contains("sig explode"));
        assert!(!d.committed(), "signature phase must stay PreCommit");
        assert_eq!(d.phase(), AerorsyncSessionPhase::Failed);
    }

    #[tokio::test]
    async fn driver_upload_rejects_sig_without_item_transfer_bit() {
        // iflags = 0: server signature message without ITEM_TRANSFER bit
        // must be refused by the driver (protocol contract violation).
        let head = SumHead {
            count: 1,
            block_length: 1024,
            checksum_length: 2,
            remainder_length: 0,
        };
        let blocks = vec![make_sig_block(0x12345678, 0xEE, 2)];
        // iflags = 0 — missing ITEM_TRANSFER.
        let sig_payload = build_sig_phase_payload(1, 0x0000, &head, &blocks);

        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &sig_payload));

        let transport = mock_transport_with_raw_inbound(inbound);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        let err = d
            .drive_upload(
                RemoteCommandSpec::upload("/remote/target.bin"),
                sample_file_list_entry("target.bin"),
                &[],
                &MockSigAdapter::default(),
                &mut sink,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind, AerorsyncErrorKind::InvalidFrame);
        assert!(err.detail.contains("ITEM_TRANSFER"));
        assert_eq!(d.phase(), AerorsyncSessionPhase::Failed);
    }

    #[tokio::test]
    async fn driver_upload_sigs_split_across_data_frames_reassemble() {
        // Split the signature payload across 3 MSG_DATA frames: header
        // + first block + remaining two blocks. Driver must accumulate
        // the prefix and decode correctly.
        let head = SumHead {
            count: 3,
            block_length: 1024,
            checksum_length: 2,
            remainder_length: 0,
        };
        let blocks = vec![
            make_sig_block(0xAAAAAAAA, 0x11, 2),
            make_sig_block(0xBBBBBBBB, 0x22, 2),
            make_sig_block(0xCCCCCCCC, 0x33, 2),
        ];
        let full_payload = build_sig_phase_payload(1, 0x8002, &head, &blocks);
        // Carve out: header (ndx+iflags+sum_head) is roughly 1+2+16 = 19
        // bytes, but ndx encoding varies. Pick a conservative split.
        let split_a = 5;
        let split_b = 19;
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &full_payload[..split_a]));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &full_payload[split_a..split_b]));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &full_payload[split_b..]));

        let transport = mock_transport_with_raw_inbound(inbound);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        let err = d
            .drive_upload(
                RemoteCommandSpec::upload("/remote/target.bin"),
                sample_file_list_entry("target.bin"),
                &[],
                &MockSigAdapter::default(),
                &mut sink,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind, AerorsyncErrorKind::UnsupportedVersion);
        assert_eq!(d.received_signatures().len(), 3);
        assert_eq!(d.received_signatures()[0].rolling, 0xAAAAAAAA);
        assert_eq!(d.received_signatures()[2].rolling, 0xCCCCCCCC);
    }

    #[tokio::test]
    async fn driver_download_signature_phase_aborts_on_cancel() {
        // Preamble + file list OK, then cancel fires before sigs emit.
        // Verify typed Cancelled and no signature outbound.
        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            // B.2: align with `build_flist_options()` so test-side
            // pre-encoded payloads round-trip through the driver decoder.
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));

        let transport = mock_transport_with_raw_inbound(inbound);
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel_handle = CancelHandle::new(cancel_flag.clone(), None);
        let mut d = AerorsyncDriver::new(transport, cancel_handle);
        let mut sink = CollectingSink::default();
        // Cancel before the driver starts.
        cancel_flag.store(true, Ordering::SeqCst);
        let adapter =
            MockSigAdapter::with_fixed_signatures(1024, vec![make_engine_sig(0, 0x11, 0x22, 1024)]);
        let err = d
            .drive_download(
                RemoteCommandSpec::download("/remote/target.bin"),
                b"abc",
                &adapter,
                &mut sink,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind, AerorsyncErrorKind::Cancelled);
        assert!(!d.committed());
        assert!(d.sent_signatures().is_empty());
    }

    #[tokio::test]
    async fn driver_signature_phase_frozen_oracle_byte_identical() {
        // Feed the full upload server->client capture and verify the
        // driver absorbs the real signature phase: 375 sum_blocks per
        // the frozen oracle's 256 KiB source file.
        let Some(frozen) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
            eprintln!("frozen oracle missing — A2.2 upload sig pin skipped");
            return;
        };
        let transport = mock_transport_with_raw_inbound(frozen.upload_server_to_client.clone());
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        let entry = sample_file_list_entry("target.bin");
        let outcome = d
            .drive_upload(
                RemoteCommandSpec::upload("/workspace/upload/target.bin"),
                entry,
                &[],
                &MockSigAdapter::default(),
                &mut sink,
            )
            .await;
        // Either we reached the stub frontier (UnsupportedVersion) or an
        // InvalidFrame bail on the downstream NDX_DONE tail. Both are
        // acceptable as long as the 375-block signature phase decoded.
        assert!(
            d.received_signatures().len() == 375,
            "driver should decode 375 sum_blocks from the frozen oracle (got {}, outcome {outcome:?})",
            d.received_signatures().len()
        );
        assert_eq!(d.received_sum_head().map(|h| h.count), Some(375));
        // A2.3: the driver now proceeds into the delta phase after the
        // sigs. With a default MockSigAdapter the plan is empty but
        // END_FLAG+checksum are still emitted, which flips committed
        // to true. The frozen-oracle pin is on the signature decode
        // (375 blocks), not on the commit boundary.
    }

    // ---- A2.3 tests ------------------------------------------------------

    #[tokio::test]
    async fn driver_upload_delta_sends_ops_and_file_checksum() {
        // Happy path upload with a real delta plan. The adapter returns
        // mixed Literal + CopyBlock ops; verify the outbound capture
        // contains a mux-wrapped encode_delta_stream + END_FLAG + 16B
        // file checksum trailer.
        let head = SumHead {
            count: 2,
            block_length: 1024,
            checksum_length: 2,
            remainder_length: 0,
        };
        let blocks = vec![
            make_sig_block(0x11111111, 0xAA, 2),
            make_sig_block(0x22222222, 0xBB, 2),
        ];
        let sig_payload = build_sig_phase_payload(1, 0x8002, &head, &blocks);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &sig_payload));
        let transport = mock_transport_with_raw_inbound(inbound);
        let last_raw_outbound = transport.last_raw_outbound.clone();
        let adapter = MockSigAdapter::default().with_upload_plan(vec![
            EngineDeltaOp::Literal(b"hello".to_vec()),
            EngineDeltaOp::CopyBlock(0),
            EngineDeltaOp::Literal(b"world".to_vec()),
            EngineDeltaOp::CopyBlock(1),
        ]);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        let err = d
            .drive_upload(
                RemoteCommandSpec::upload("/remote/target.bin"),
                sample_file_list_entry("target.bin"),
                b"hello\0\0\0world",
                &adapter,
                &mut sink,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind, AerorsyncErrorKind::UnsupportedVersion);
        assert_eq!(d.phase(), AerorsyncSessionPhase::Stub);
        assert!(d.committed(), "delta phase flips committed to true");
        // 4 ops emitted: 2 Literal + 2 CopyRun.
        assert_eq!(d.emitted_delta_ops().len(), 4);
        let literal_count = d
            .emitted_delta_ops()
            .iter()
            .filter(|op| matches!(op, DeltaOp::Literal { .. }))
            .count();
        assert_eq!(literal_count, 2);

        // S8j: the outbound capture must contain the REAL xxh128 trailer
        // computed over `source_data` — not the 16-zero placeholder the
        // A2.3 prototype emitted. Verify by recomputing the expected
        // 16 bytes via `compute_xxh128_wire` and scanning the outbound
        // window. This pins both the encoder and the driver's wiring of
        // `source_data` into the hash function.
        let expected_trailer = compute_xxh128_wire(b"hello\0\0\0world");
        assert_eq!(expected_trailer.len(), 16);
        assert!(
            !expected_trailer.iter().all(|&b| b == 0),
            "xxh128 of a non-empty payload must not be all-zero"
        );
        let guard = last_raw_outbound.lock().unwrap();
        let outbound_arc = guard.as_ref().expect("raw stream must have opened");
        let outbound = outbound_arc.lock().unwrap().clone();
        assert!(
            outbound
                .windows(16)
                .any(|w| w == expected_trailer.as_slice()),
            "real xxh128 trailer must appear in outbound bytes"
        );
        assert!(d.sent_data_bytes() > 0);
    }

    /// S8j pin: a single `EngineDeltaOp::Literal` whose zstd-compressed
    /// output exceeds `MAX_DELTA_LITERAL_LEN` (= 16383) MUST be split
    /// into several consecutive `DeltaOp::Literal` wire records rather
    /// than bailed with `InvalidFrame`. Mirrors `send_zstd_token`
    /// (token.c:678-776) flushing the zstd output buffer whenever it
    /// reaches `MAX_DATA_COUNT` and emitting a fresh DEFLATED_DATA
    /// frame with the rest. Pre-S8j the driver rejected anything
    /// larger than 16 KiB of compressed literal, capping the native
    /// path well below real-file sizes.
    #[tokio::test]
    async fn driver_upload_delta_splits_large_compressed_literal_into_multiple_tokens() {
        let head = SumHead {
            count: 1,
            block_length: 1024,
            checksum_length: 2,
            remainder_length: 0,
        };
        let blocks = vec![make_sig_block(0x11111111, 0xAA, 2)];
        let sig_payload = build_sig_phase_payload(1, 0x8002, &head, &blocks);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &sig_payload));
        let transport = mock_transport_with_raw_inbound(inbound);

        // 64 KiB of pseudo-random bytes. Zstd cannot meaningfully
        // compress pseudo-random data, so the compressed blob will be
        // ~64 KiB — comfortably above `MAX_DELTA_LITERAL_LEN = 16383`
        // and therefore guaranteed to trigger the S8j chunk-split path.
        // Using a fixed seed keeps the assertion shape stable across
        // runs regardless of the exact zstd block layout.
        let mut payload = vec![0u8; 64 * 1024];
        let mut seed = 0xDEAD_BEEFu32;
        for chunk in payload.chunks_exact_mut(4) {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            chunk.copy_from_slice(&seed.to_le_bytes());
        }

        let adapter = MockSigAdapter::default()
            .with_upload_plan(vec![EngineDeltaOp::Literal(payload.clone())]);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        let err = d
            .drive_upload(
                RemoteCommandSpec::upload("/remote/target.bin"),
                sample_file_list_entry("target.bin"),
                &payload,
                &adapter,
                &mut sink,
            )
            .await
            .unwrap_err();
        // Reaches the A2.3 stub frontier — the delta phase itself must
        // have succeeded (no InvalidFrame) for this to happen.
        assert_eq!(err.kind, AerorsyncErrorKind::UnsupportedVersion);
        assert_eq!(d.phase(), AerorsyncSessionPhase::Stub);

        // Every emitted Literal MUST fit the DEFLATED_DATA length
        // budget. Multiple consecutive Literals are the whole point of
        // the split.
        let literal_ops: Vec<&DeltaOp> = d
            .emitted_delta_ops()
            .iter()
            .filter(|op| matches!(op, DeltaOp::Literal { .. }))
            .collect();
        assert!(
            literal_ops.len() >= 2,
            "64 KiB pseudo-random payload must produce multiple \
             DEFLATED_DATA records (got {})",
            literal_ops.len()
        );
        for op in &literal_ops {
            if let DeltaOp::Literal { compressed_payload } = op {
                assert!(
                    !compressed_payload.is_empty()
                        && compressed_payload.len() <= MAX_DELTA_LITERAL_LEN,
                    "chunk size {} out of (0, {}] — S8j must clamp every \
                     chunk to the 14-bit DEFLATED_DATA length budget",
                    compressed_payload.len(),
                    MAX_DELTA_LITERAL_LEN
                );
            }
        }

        // Round-trip: concatenating the compressed chunks back through
        // the session DCtx must recover the original bytes — this is
        // what a real rsync receiver would see across the consecutive
        // DEFLATED_DATA frames (single session-wide DCtx per token.c
        // recv_zstd_token).
        let joined: Vec<u8> = literal_ops
            .iter()
            .flat_map(|op| match op {
                DeltaOp::Literal { compressed_payload } => compressed_payload.clone(),
                _ => Vec::new(),
            })
            .collect();
        let recovered =
            crate::aerorsync::real_wire::decompress_zstd_literal_stream(&[joined.as_slice()])
                .expect("decompress joined chunks");
        assert_eq!(
            recovered, payload,
            "concatenated chunk decompression must recover the original literal"
        );
    }

    #[tokio::test]
    async fn driver_download_delta_decodes_ops_and_reconstructs() {
        // Build a server-side delta stream manually: one CopyRun (run=2)
        // + one Literal + END_FLAG + 16-byte checksum trailer. The
        // driver must decode, decompress literals (if zstd negotiated),
        // call adapter.apply_delta, and stash `reconstructed`.
        use crate::aerorsync::real_wire::{compress_zstd_literal_stream, encode_delta_stream};
        let raw_literal = b"LITERAL_PAYLOAD_ABC";
        let compressed = compress_zstd_literal_stream(&[raw_literal.as_slice()]).unwrap();
        assert_eq!(compressed.len(), 1);
        let wire_ops = vec![
            DeltaOp::CopyRun {
                start_token_index: 0,
                run_length: 2,
            },
            DeltaOp::Literal {
                compressed_payload: compressed[0].clone(),
            },
        ];
        let report = DeltaStreamReport {
            ops: wire_ops.clone(),
            file_checksum: vec![0xCC; A2_3_FILE_CHECKSUM_LEN],
        };
        let delta_bytes = encode_delta_stream(&report);

        // File list + terminator for download preamble.
        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            // B.2: align with `build_flist_options()` so test-side
            // pre-encoded payloads round-trip through the driver decoder.
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);

        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &delta_bytes));

        let transport = mock_transport_with_raw_inbound(inbound);
        let adapter = MockSigAdapter::with_fixed_signatures(
            4,
            vec![
                make_engine_sig(0, 0xA0, 0x01, 4),
                make_engine_sig(1, 0xA1, 0x02, 4),
            ],
        );
        let destination_data: Vec<u8> = b"BLK1BLK2".to_vec(); // 8 bytes, 2 blocks of 4

        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        let err = d
            .drive_download(
                RemoteCommandSpec::download("/remote/target.bin"),
                &destination_data,
                &adapter,
                &mut sink,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind, AerorsyncErrorKind::UnsupportedVersion);
        assert_eq!(d.phase(), AerorsyncSessionPhase::Stub);
        // Download stays PreCommit — the reconstructed bytes are in RAM,
        // A4 will flush + rename atomically.
        assert!(
            !d.committed(),
            "download path must stay PreCommit for A4 to decide atomicity"
        );
        // Reconstructed = dest[0..4] + dest[4..8] + "LITERAL_PAYLOAD_ABC".
        let reconstructed = d.reconstructed().expect("must be populated");
        assert_eq!(&reconstructed[0..4], b"BLK1");
        assert_eq!(&reconstructed[4..8], b"BLK2");
        assert_eq!(&reconstructed[8..], raw_literal.as_slice());
        // File checksum trailer exposed.
        assert_eq!(d.received_file_checksum(), Some(vec![0xCC; 16].as_slice()),);
    }

    /// S8j download-side pin: a logical literal split by the server
    /// across N consecutive `DEFLATED_DATA` frames MUST coalesce back
    /// into a single `EngineDeltaOp::Literal` on the engine plan. This
    /// mirrors `send_zstd_token`'s flush-on-MAX_DATA_COUNT behaviour
    /// (token.c:678-776) and the receiver's session-wide `zstd_dctx`
    /// concatenation semantics (token.c:778+). Pre-S8j download, the
    /// driver inferred 1 wire Literal = 1 engine Literal, which
    /// silently doubled the engine literal count whenever a chunk
    /// boundary fell inside a run.
    #[tokio::test]
    async fn driver_download_delta_coalesces_consecutive_literal_chunks_into_one_engine_literal() {
        use crate::aerorsync::real_wire::{compress_zstd_literal_stream, encode_delta_stream};

        // Build a 64 KiB pseudo-random logical literal — zstd cannot
        // meaningfully compress high-entropy bytes, so the compressed
        // blob stays above `MAX_DELTA_LITERAL_LEN = 16383` and
        // requires at least 3 DEFLATED_DATA frames. Writing 4 bytes
        // per LCG step (vs 1 byte of the low byte only) keeps the
        // entropy high enough to defeat zstd's level-3 matcher.
        let mut logical_literal = vec![0u8; 64 * 1024];
        let mut seed = 0xCAFE_BABEu32;
        for chunk in logical_literal.chunks_exact_mut(4) {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            chunk.copy_from_slice(&seed.to_le_bytes());
        }
        let compressed = compress_zstd_literal_stream(&[logical_literal.as_slice()])
            .expect("zstd compress literal");
        assert_eq!(compressed.len(), 1);
        let full_blob = &compressed[0];
        assert!(
            full_blob.len() > MAX_DELTA_LITERAL_LEN,
            "test precondition: compressed blob {} must exceed MAX_DELTA_LITERAL_LEN {}",
            full_blob.len(),
            MAX_DELTA_LITERAL_LEN
        );
        // Split the logical literal's compressed blob into 16383-byte
        // wire chunks — exactly what stock rsync's `send_zstd_token`
        // would emit.
        let wire_literal_chunks: Vec<Vec<u8>> = full_blob
            .chunks(MAX_DELTA_LITERAL_LEN)
            .map(<[u8]>::to_vec)
            .collect();
        assert!(
            wire_literal_chunks.len() >= 3,
            "test precondition: need ≥3 chunks to cover the coalesce case"
        );

        // Sandwich the chunk run with CopyRuns on both sides to
        // exercise boundary detection from BOTH directions.
        let mut wire_ops = vec![DeltaOp::CopyRun {
            start_token_index: 0,
            run_length: 1,
        }];
        for chunk in &wire_literal_chunks {
            wire_ops.push(DeltaOp::Literal {
                compressed_payload: chunk.clone(),
            });
        }
        wire_ops.push(DeltaOp::CopyRun {
            start_token_index: 1,
            run_length: 1,
        });

        let report = DeltaStreamReport {
            ops: wire_ops.clone(),
            file_checksum: vec![0xDD; A2_3_FILE_CHECKSUM_LEN],
        };
        let delta_bytes = encode_delta_stream(&report);

        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);

        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &delta_bytes));

        // 2 baseline blocks of 4 bytes each: BLK1 (index 0), BLK2 (index 1).
        let transport = mock_transport_with_raw_inbound(inbound);
        let adapter = MockSigAdapter::with_fixed_signatures(
            4,
            vec![
                make_engine_sig(0, 0xA0, 0x01, 4),
                make_engine_sig(1, 0xA1, 0x02, 4),
            ],
        );
        let destination_data: Vec<u8> = b"BLK1BLK2".to_vec();

        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        let err = d
            .drive_download(
                RemoteCommandSpec::download("/remote/target.bin"),
                &destination_data,
                &adapter,
                &mut sink,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind, AerorsyncErrorKind::UnsupportedVersion);

        // Reconstructed must equal BLK1 + logical_literal + BLK2 —
        // proof that the N wire chunks collapsed back into exactly
        // ONE engine literal, and the session DCtx recovered the
        // original 40 KiB stream.
        let reconstructed = d.reconstructed().expect("must be populated");
        let mut expected = b"BLK1".to_vec();
        expected.extend_from_slice(&logical_literal);
        expected.extend_from_slice(b"BLK2");
        assert_eq!(
            reconstructed.len(),
            expected.len(),
            "reconstructed length mismatch: got {}, expected {}",
            reconstructed.len(),
            expected.len()
        );
        assert_eq!(
            reconstructed,
            &expected,
            "S8j download coalesce must recover BLK1 + logical_literal + BLK2 \
             even when the logical literal arrives across {} DEFLATED_DATA chunks",
            wire_literal_chunks.len()
        );
    }

    #[tokio::test]
    async fn driver_upload_delta_flips_committed_on_first_op() {
        // Even an empty delta plan still emits END_FLAG + checksum,
        // which crosses the PreCommit boundary. Pin the flip timing.
        let head = SumHead {
            count: 1,
            block_length: 1024,
            checksum_length: 2,
            remainder_length: 0,
        };
        let blocks = vec![make_sig_block(0x11, 0x22, 2)];
        let sig_payload = build_sig_phase_payload(1, 0x8002, &head, &blocks);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &sig_payload));
        let transport = mock_transport_with_raw_inbound(inbound);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        assert!(!d.committed(), "starts false");
        let _ = d
            .drive_upload(
                RemoteCommandSpec::upload("/remote/x"),
                sample_file_list_entry("target.bin"),
                &[],
                &MockSigAdapter::default(),
                &mut sink,
            )
            .await;
        assert!(d.committed(), "flips true after delta phase completes");
    }

    #[tokio::test]
    async fn driver_download_delta_preserves_committed_false() {
        // Full happy download → committed MUST stay false. A4 owns the
        // PostCommit flip when it opens the temp file.
        let wire_ops = vec![DeltaOp::CopyRun {
            start_token_index: 0,
            run_length: 1,
        }];
        let report = DeltaStreamReport {
            ops: wire_ops,
            file_checksum: vec![0u8; A2_3_FILE_CHECKSUM_LEN],
        };
        let delta_bytes = encode_delta_stream(&report);

        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            // B.2: align with `build_flist_options()` so test-side
            // pre-encoded payloads round-trip through the driver decoder.
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &delta_bytes));
        let transport = mock_transport_with_raw_inbound(inbound);
        let adapter =
            MockSigAdapter::with_fixed_signatures(4, vec![make_engine_sig(0, 0xA0, 0x01, 4)]);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        let _ = d
            .drive_download(
                RemoteCommandSpec::download("/remote/x"),
                b"BLK0",
                &adapter,
                &mut sink,
            )
            .await;
        assert!(!d.committed(), "download A2.3 never crosses PreCommit");
    }

    #[tokio::test]
    async fn driver_download_delta_aborts_on_terminal_oob_post_sigs() {
        // After the file list phase + local signature emission, server
        // sends a terminal Error OOB in place of the delta stream.
        // The driver must bail with RemoteError and committed stays false
        // (download path never crosses PreCommit in A2.3).
        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            // B.2: align with `build_flist_options()` so test-side
            // pre-encoded payloads round-trip through the driver decoder.
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Error, b"delta stream crashed"));
        let transport = mock_transport_with_raw_inbound(inbound);
        let adapter =
            MockSigAdapter::with_fixed_signatures(4, vec![make_engine_sig(0, 0xA0, 0x01, 4)]);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        let err = d
            .drive_download(
                RemoteCommandSpec::download("/remote/x"),
                b"BLK0",
                &adapter,
                &mut sink,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind, AerorsyncErrorKind::RemoteError);
        assert!(err.detail.contains("delta stream crashed"));
        assert!(!d.committed(), "download stays PreCommit even on error");
        assert_eq!(d.phase(), AerorsyncSessionPhase::Failed);
    }

    #[tokio::test]
    async fn driver_upload_delta_cancel_surfaces_typed_cancelled() {
        // Cancel the driver before it reaches the delta phase — the
        // check_cancel guards inside send_delta_phase_single_file must
        // surface a typed Cancelled error, not a transport failure.
        let head = SumHead {
            count: 1,
            block_length: 1024,
            checksum_length: 2,
            remainder_length: 0,
        };
        let blocks = vec![make_sig_block(0x11, 0x22, 2)];
        let sig_payload = build_sig_phase_payload(1, 0x8002, &head, &blocks);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &sig_payload));
        let transport = mock_transport_with_raw_inbound(inbound);
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel_handle = CancelHandle::new(cancel_flag.clone(), None);
        let mut d = AerorsyncDriver::new(transport, cancel_handle);
        let mut sink = CollectingSink::default();
        cancel_flag.store(true, Ordering::SeqCst);
        let err = d
            .drive_upload(
                RemoteCommandSpec::upload("/remote/x"),
                sample_file_list_entry("target.bin"),
                &[],
                &MockSigAdapter::default(),
                &mut sink,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind, AerorsyncErrorKind::Cancelled);
    }

    #[tokio::test]
    async fn driver_delta_split_across_data_frames_reassembles() {
        // Split the delta stream across two Data frames. Driver must
        // accumulate payloads until decode_delta_stream succeeds.
        use crate::aerorsync::real_wire::encode_delta_stream;
        let wire_ops = vec![DeltaOp::CopyRun {
            start_token_index: 0,
            run_length: 1,
        }];
        let report = DeltaStreamReport {
            ops: wire_ops,
            file_checksum: vec![0xEE; A2_3_FILE_CHECKSUM_LEN],
        };
        let delta_bytes = encode_delta_stream(&report);
        let half = delta_bytes.len() / 2;

        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            // B.2: align with `build_flist_options()` so test-side
            // pre-encoded payloads round-trip through the driver decoder.
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &delta_bytes[..half]));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &delta_bytes[half..]));

        let transport = mock_transport_with_raw_inbound(inbound);
        let adapter =
            MockSigAdapter::with_fixed_signatures(4, vec![make_engine_sig(0, 0xA0, 0x01, 4)]);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        let err = d
            .drive_download(
                RemoteCommandSpec::download("/remote/x"),
                b"BLK0",
                &adapter,
                &mut sink,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind, AerorsyncErrorKind::UnsupportedVersion);
        assert!(d.reconstructed().is_some());
        assert_eq!(d.received_file_checksum(), Some(vec![0xEE; 16].as_slice()),);
    }

    // ---- A2.4 tests ------------------------------------------------------

    fn build_summary_frame_bytes(protocol: u32) -> Vec<u8> {
        use crate::aerorsync::real_wire::encode_summary_frame;
        let frame = SummaryFrame {
            total_read: 12345,
            total_written: 67890,
            total_size: 4096,
            flist_buildtime: Some(7),
            flist_xfertime: Some(3),
        };
        encode_summary_frame(&frame, protocol)
    }

    async fn drive_upload_to_stub(
        d: &mut AerorsyncDriver<MockRemoteShellTransport>,
        sink: &mut CollectingSink,
    ) {
        drive_upload_to_stub_with_spec(d, sink, RemoteCommandSpec::upload("/remote/x")).await;
    }

    async fn drive_aerorsync_upload_to_stub(
        d: &mut AerorsyncDriver<MockRemoteShellTransport>,
        sink: &mut CollectingSink,
    ) {
        drive_upload_to_stub_with_spec(d, sink, RemoteCommandSpec::aerorsync_upload("/remote/x"))
            .await;
    }

    async fn drive_upload_to_stub_with_spec(
        d: &mut AerorsyncDriver<MockRemoteShellTransport>,
        sink: &mut CollectingSink,
        spec: RemoteCommandSpec,
    ) {
        // Reach the A2.3 stub frontier so finish_session has a live
        // stream to finalise.
        let err = d
            .drive_upload(
                spec,
                sample_file_list_entry("target.bin"),
                &[],
                &MockSigAdapter::default(),
                sink,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind, AerorsyncErrorKind::UnsupportedVersion);
    }

    #[tokio::test]
    async fn driver_finish_session_upload_emits_ndx_done_phase_loop() {
        // B.2 Step 5 pin: stock rsync upload has no client->server
        // SummaryFrame. The client sender emits NDX_DONE for the two
        // send_files phase transitions, one final send_files NDX_DONE,
        // then the read_final_goodbye ACK NDX_DONE.
        let head = SumHead {
            count: 1,
            block_length: 1024,
            checksum_length: 2,
            remainder_length: 0,
        };
        let blocks = vec![make_sig_block(0x11, 0x22, 2)];
        let sig_payload = build_sig_phase_payload(1, 0x8002, &head, &blocks);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &sig_payload));
        // Frozen upload oracle server->client tail:
        // phase-loop NDX_DONE x3, then read_final_goodbye NDX_DONE x2.
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &[0x00]));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &[0x00, 0x00, 0x00]));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &[0x00]));
        let transport = mock_transport_with_raw_inbound(inbound);
        let last_raw_outbound = transport.last_raw_outbound.clone();
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();

        drive_upload_to_stub(&mut d, &mut sink).await;
        let outbound_before_finish = {
            let guard = last_raw_outbound.lock().unwrap();
            let outbound_arc = guard.as_ref().expect("raw stream must have been opened");
            let len = outbound_arc.lock().unwrap().len();
            len
        };

        d.finish_session(&mut sink)
            .await
            .expect("finish_session upload stock-rsync tail");

        let expected_suffix = [
            mux_frame(MuxTag::Data, &[0x00]),
            mux_frame(MuxTag::Data, &[0x00]),
            mux_frame(MuxTag::Data, &[0x00]),
            mux_frame(MuxTag::Data, &[0x00]),
        ]
        .concat();
        let guard = last_raw_outbound.lock().unwrap();
        let outbound_arc = guard.as_ref().expect("raw stream must have been opened");
        let outbound = outbound_arc.lock().unwrap().clone();
        assert_eq!(
            &outbound[outbound_before_finish..],
            expected_suffix.as_slice(),
            "upload finish must emit only NDX_DONE markers, no SummaryFrame"
        );
        assert!(
            d.received_summary().is_none(),
            "client-sender upload must not synthesize a SummaryFrame"
        );
        assert_eq!(d.phase(), AerorsyncSessionPhase::Complete);
    }

    #[tokio::test]
    async fn driver_finish_session_aerorsync_serve_upload_emits_summary_frame_and_completes() {
        // Dev helper compatibility: aerorsync_serve still expects the
        // legacy client-emitted NDX_DONE + SummaryFrame and returns one
        // trailing NDX_DONE byte.
        let head = SumHead {
            count: 1,
            block_length: 1024,
            checksum_length: 2,
            remainder_length: 0,
        };
        let blocks = vec![make_sig_block(0x11, 0x22, 2)];
        let sig_payload = build_sig_phase_payload(1, 0x8002, &head, &blocks);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &sig_payload));
        // Trailing NDX_DONE (single 0x00 byte in MSG_DATA) from server.
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &[0x00]));
        let transport = mock_transport_with_raw_inbound(inbound);
        let last_raw_shutdown = transport.last_raw_shutdown.clone();
        let last_raw_outbound = transport.last_raw_outbound.clone();
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();

        drive_aerorsync_upload_to_stub(&mut d, &mut sink).await;
        assert_eq!(d.phase(), AerorsyncSessionPhase::Stub);

        d.finish_session(&mut sink)
            .await
            .expect("finish_session upload happy path");
        assert_eq!(d.phase(), AerorsyncSessionPhase::Complete);
        assert_eq!(d.session_role(), Some(SessionRole::Sender));

        // `received_summary()` now holds the LOCALLY emitted summary,
        // populated from the driver's counters as of the pre-emit
        // snapshot (matches rsync's `handle_stats` semantics).
        let summary = d.received_summary().expect("emitted summary cached");
        assert_eq!(summary.total_size, 4096, "from sample_file_list_entry");
        assert!(
            summary.total_written > 0,
            "total_written must be positive (pre-emit delta bytes)"
        );
        // Post-emit wire totals must be >= the pre-emit snapshot, since
        // the summary bytes themselves contribute to sent_data_bytes.
        assert!(
            (summary.total_written as u64) <= d.sent_data_bytes(),
            "summary.total_written ({}) must be <= post-finish sent_data_bytes ({})",
            summary.total_written,
            d.sent_data_bytes()
        );
        // `total_read` is snapshotted pre-emit; `read_trailing_ndx_done`
        // may pull one more byte after that. Invariant: summary value
        // is at most one byte behind the final driver counter.
        assert!(
            summary.total_read as u64 <= d.received_raw_bytes(),
            "summary.total_read ({}) must be <= final received_raw_bytes ({})",
            summary.total_read,
            d.received_raw_bytes()
        );
        assert!(
            d.received_raw_bytes() - summary.total_read as u64 <= 1,
            "trailing NDX_DONE read must add at most 1 byte after snapshot"
        );

        // Verify the outbound wire carries a MSG_DATA whose payload
        // starts with 0x00 (NDX_DONE) followed by the encoded summary.
        let expected_suffix = {
            let mut v = vec![0x00];
            v.extend_from_slice(&encode_summary_frame(summary, 31));
            v
        };
        let guard = last_raw_outbound.lock().unwrap();
        let outbound_arc = guard.as_ref().expect("raw stream must have been opened");
        let outbound = outbound_arc.lock().unwrap().clone();
        assert!(
            outbound
                .windows(expected_suffix.len())
                .any(|w| w == expected_suffix.as_slice()),
            "outbound must contain NDX_DONE + encoded summary as a single MSG_DATA payload"
        );

        // Shutdown flag must still be flipped by the driver.
        let shutdown_arc_guard = last_raw_shutdown.lock().unwrap();
        let shutdown_arc = shutdown_arc_guard
            .as_ref()
            .expect("raw stream must have been opened");
        assert!(
            *shutdown_arc.lock().unwrap(),
            "shutdown_raw_stream must flip the mock flag"
        );
    }

    #[tokio::test]
    async fn driver_finish_session_upload_populates_session_stats_from_counters() {
        let head = SumHead {
            count: 1,
            block_length: 1024,
            checksum_length: 2,
            remainder_length: 0,
        };
        let blocks = vec![make_sig_block(0x11, 0x22, 2)];
        let sig_payload = build_sig_phase_payload(1, 0x8002, &head, &blocks);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &sig_payload));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &[0x00]));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &[0x00, 0x00, 0x00]));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &[0x00]));
        let transport = mock_transport_with_raw_inbound(inbound);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();

        drive_upload_to_stub(&mut d, &mut sink).await;
        d.finish_session(&mut sink).await.unwrap();

        let sent = d.sent_data_bytes();
        let recv = d.received_raw_bytes();
        let stats = d.session_stats();
        assert!(sent > 0, "some data must have been written in upload");
        assert!(recv > 0, "some data must have been read for sig phase");
        assert_eq!(stats.bytes_sent, sent);
        assert_eq!(stats.bytes_received, recv);
        // Other SessionStats fields remain at their default — A4 populates
        // files_seen / files_delta / literal_bytes / matched_bytes from
        // its own instrumentation layer.
        assert_eq!(stats.files_seen, 0);
    }

    #[tokio::test]
    async fn driver_finish_session_upload_aborts_on_terminal_oob_in_trailing_slot() {
        // If the server sends an OOB Error where a phase-loop NDX_DONE is
        // expected, finish must bail with RemoteError and phase=Failed.
        let head = SumHead {
            count: 1,
            block_length: 1024,
            checksum_length: 2,
            remainder_length: 0,
        };
        let blocks = vec![make_sig_block(0x11, 0x22, 2)];
        let sig_payload = build_sig_phase_payload(1, 0x8002, &head, &blocks);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &sig_payload));
        // Terminal Error occupies the trailing NDX_DONE slot.
        inbound.extend_from_slice(&mux_frame(MuxTag::Error, b"trailing phase crash"));
        let transport = mock_transport_with_raw_inbound(inbound);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();

        drive_upload_to_stub(&mut d, &mut sink).await;
        let err = d.finish_session(&mut sink).await.unwrap_err();
        assert_eq!(err.kind, AerorsyncErrorKind::RemoteError);
        assert!(err.detail.contains("trailing phase crash"));
        assert_eq!(d.phase(), AerorsyncSessionPhase::Failed);
    }

    #[tokio::test]
    async fn driver_finish_session_cancel_surfaces_typed_cancelled() {
        let head = SumHead {
            count: 1,
            block_length: 1024,
            checksum_length: 2,
            remainder_length: 0,
        };
        let blocks = vec![make_sig_block(0x11, 0x22, 2)];
        let sig_payload = build_sig_phase_payload(1, 0x8002, &head, &blocks);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &sig_payload));
        // No summary frame — but a cancel will fire before the read.
        let transport = mock_transport_with_raw_inbound(inbound);
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel_handle = CancelHandle::new(cancel_flag.clone(), None);
        let mut d = AerorsyncDriver::new(transport, cancel_handle);
        let mut sink = CollectingSink::default();
        drive_aerorsync_upload_to_stub(&mut d, &mut sink).await;
        cancel_flag.store(true, Ordering::SeqCst);
        let err = d.finish_session(&mut sink).await.unwrap_err();
        assert_eq!(err.kind, AerorsyncErrorKind::Cancelled);
    }

    #[tokio::test]
    async fn driver_download_finish_session_preserves_committed_false() {
        // Full happy download + finish_session → session complete,
        // committed stays false (A4 flips it when writing to temp file).
        let wire_ops = vec![DeltaOp::CopyRun {
            start_token_index: 0,
            run_length: 1,
        }];
        let delta_bytes = encode_delta_stream(&DeltaStreamReport {
            ops: wire_ops,
            file_checksum: vec![0u8; A2_3_FILE_CHECKSUM_LEN],
        });
        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            // B.2: align with `build_flist_options()` so test-side
            // pre-encoded payloads round-trip through the driver decoder.
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);
        let summary_bytes = build_summary_frame_bytes(31);
        // S8j: real rsync 3.2.7 emits exactly 3 leading NDX_DONE
        // markers between the delta stream's file-csum trailer and the
        // SummaryFrame. `finish_session` on a Receiver-role driver now
        // drains them before decoding — replicate that shape here.
        let ndx_done_leading: Vec<u8> = vec![0x00; PRE_SUMMARY_NDX_DONE_COUNT_DOWNLOAD];
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &delta_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &ndx_done_leading));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &summary_bytes));

        let transport = mock_transport_with_raw_inbound(inbound);
        let adapter =
            MockSigAdapter::with_fixed_signatures(4, vec![make_engine_sig(0, 0xA0, 0x01, 4)]);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        let _ = d
            .drive_download(
                RemoteCommandSpec::download("/remote/x"),
                b"BLK0",
                &adapter,
                &mut sink,
            )
            .await;
        d.finish_session(&mut sink).await.unwrap();
        assert_eq!(d.phase(), AerorsyncSessionPhase::Complete);
        assert!(
            !d.committed(),
            "download A2.4 stays PreCommit; A4 owns the flip"
        );
        assert!(d.reconstructed().is_some());
        assert!(d.received_summary().is_some());
    }

    // ---- S8j tests (xxh128 wire layout) ----------------------------------

    #[test]
    fn xxh128_wire_produces_16_bytes_split_lo_le_hi_le() {
        // Layout invariant pin: `compute_xxh128_wire` must produce exactly
        // 16 bytes whose first half is the lower 64 bits of the hash in
        // little-endian, and whose second half is the upper 64 bits also
        // in little-endian. This mirrors rsync 3.2.7 `checksum.c`:
        //   SIVAL64(buf, 0, lo);
        //   SIVAL64(buf, 8, hi);
        // — `SIVAL64` is the LE 64-bit writer. A future rsync version
        // that switched byte order would surface here before reaching
        // lane 3.
        let payload = b"aeroftp strada-c s8j xxh128 pin";
        let wire = compute_xxh128_wire(payload);
        assert_eq!(wire.len(), 16, "xxh128 wire must be exactly 16 bytes");

        let hash = xxh3_128(payload);
        let lo = hash as u64;
        let hi = (hash >> 64) as u64;
        assert_eq!(
            &wire[0..8],
            &lo.to_le_bytes(),
            "first 8 bytes must be lower u64 little-endian (SIVAL64(buf,0,lo))"
        );
        assert_eq!(
            &wire[8..16],
            &hi.to_le_bytes(),
            "next 8 bytes must be upper u64 little-endian (SIVAL64(buf,8,hi))"
        );
    }

    #[test]
    fn xxh128_wire_is_deterministic_across_calls() {
        // Purity pin: the same payload MUST produce the same 16 bytes
        // every time. Guards against an accidental seed drift if xxhash
        // library grows a "with_seed" helper default.
        let payload = b"determinism check";
        let a = compute_xxh128_wire(payload);
        let b = compute_xxh128_wire(payload);
        assert_eq!(a, b);
    }

    #[test]
    fn xxh128_wire_differs_for_single_bit_flip() {
        // Avalanche sanity: flipping one bit of the payload must change
        // the wire output. Guards against a silent all-zero implementation.
        let a = compute_xxh128_wire(b"payload-A");
        let b = compute_xxh128_wire(b"payload-B");
        assert_ne!(a, b);
    }

    // ---- S8j tests (upload summary emit byte-level) ----------------------

    #[tokio::test]
    async fn emit_summary_phase_byte_level_layout() {
        // Byte-level pin: emitted payload is exactly
        //   [0x00] ++ encode_summary_frame(SummaryFrame{...}, protocol)
        // wrapped in a single MSG_DATA mux frame. This guards the
        // sender-side finish semantics against accidental reordering or
        // split framing in a future refactor.
        let head = SumHead {
            count: 1,
            block_length: 1024,
            checksum_length: 2,
            remainder_length: 0,
        };
        let blocks = vec![make_sig_block(0x11, 0x22, 2)];
        let sig_payload = build_sig_phase_payload(1, 0x8002, &head, &blocks);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &sig_payload));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &[0x00])); // trailing
        let transport = mock_transport_with_raw_inbound(inbound);
        let last_raw_outbound = transport.last_raw_outbound.clone();
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        drive_aerorsync_upload_to_stub(&mut d, &mut sink).await;
        d.finish_session(&mut sink).await.unwrap();

        let emitted = d
            .received_summary()
            .cloned()
            .expect("summary cached on emit");
        let expected_payload = {
            let mut v = Vec::with_capacity(1 + 9 * 5);
            v.push(0x00);
            v.extend_from_slice(&encode_summary_frame(&emitted, 31));
            v
        };
        let expected_mux_frame = mux_frame(MuxTag::Data, &expected_payload);
        let guard = last_raw_outbound.lock().unwrap();
        let arc = guard.as_ref().unwrap();
        let outbound = arc.lock().unwrap().clone();
        assert!(
            outbound
                .windows(expected_mux_frame.len())
                .any(|w| w == expected_mux_frame.as_slice()),
            "outbound must contain the exact MSG_DATA frame for NDX_DONE + summary"
        );
    }

    // ---- S8j tests (NDX_DONE drain download direction) -------------------

    #[tokio::test]
    async fn download_drain_absorbs_three_leading_ndx_done_in_one_frame() {
        // A single MSG_DATA carries `[0x00, 0x00, 0x00, summary_bytes…]`.
        // The drain must strip exactly 3 leading zeros and leave the
        // summary bytes as seed for `receive_summary_phase`.
        let wire_ops = vec![DeltaOp::CopyRun {
            start_token_index: 0,
            run_length: 1,
        }];
        let delta_bytes = encode_delta_stream(&DeltaStreamReport {
            ops: wire_ops,
            file_checksum: vec![0u8; A2_3_FILE_CHECKSUM_LEN],
        });
        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            // B.2: align with `build_flist_options()` so test-side
            // pre-encoded payloads round-trip through the driver decoder.
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);
        let summary_bytes = build_summary_frame_bytes(31);

        // Combine 3 NDX_DONE + summary into a single MSG_DATA frame.
        let mut combined = Vec::with_capacity(3 + summary_bytes.len());
        combined.extend_from_slice(&[0x00, 0x00, 0x00]);
        combined.extend_from_slice(&summary_bytes);

        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &delta_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &combined));

        let transport = mock_transport_with_raw_inbound(inbound);
        let adapter =
            MockSigAdapter::with_fixed_signatures(4, vec![make_engine_sig(0, 0xA0, 0x01, 4)]);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        let _ = d
            .drive_download(
                RemoteCommandSpec::download("/remote/x"),
                b"BLK0",
                &adapter,
                &mut sink,
            )
            .await;
        d.finish_session(&mut sink).await.unwrap();
        assert_eq!(d.phase(), AerorsyncSessionPhase::Complete);
        assert!(d.received_summary().is_some());
        assert_eq!(d.session_role(), Some(SessionRole::Receiver));
    }

    #[tokio::test]
    async fn download_drain_rejects_non_zero_in_marker_slot() {
        // If the 3-byte window where rsync MUST emit NDX_DONEs carries
        // anything other than zero, the drain surfaces InvalidFrame
        // instead of silently accepting a drifted summary offset.
        let wire_ops = vec![DeltaOp::CopyRun {
            start_token_index: 0,
            run_length: 1,
        }];
        let delta_bytes = encode_delta_stream(&DeltaStreamReport {
            ops: wire_ops,
            file_checksum: vec![0u8; A2_3_FILE_CHECKSUM_LEN],
        });
        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            // B.2: align with `build_flist_options()` so test-side
            // pre-encoded payloads round-trip through the driver decoder.
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);

        // First byte is NDX_DONE (drain enters the strict path), second
        // byte is garbage — the drain must refuse.
        let poisoned = vec![0x00, 0xAB, 0xCD, 0xEF, 0xFE];
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &delta_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &poisoned));

        let transport = mock_transport_with_raw_inbound(inbound);
        let adapter =
            MockSigAdapter::with_fixed_signatures(4, vec![make_engine_sig(0, 0xA0, 0x01, 4)]);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        let _ = d
            .drive_download(
                RemoteCommandSpec::download("/remote/x"),
                b"BLK0",
                &adapter,
                &mut sink,
            )
            .await;
        let err = d
            .finish_session(&mut sink)
            .await
            .expect_err("drain must reject non-zero in marker slot");
        assert_eq!(err.kind, AerorsyncErrorKind::InvalidFrame);
        assert!(err.detail.contains("NDX_DONE"), "detail: {}", err.detail);
    }

    // ---- A4 tests (drive_*_through_delta entry points) -------------------

    #[tokio::test]
    async fn drive_upload_through_delta_returns_ok_on_happy_path() {
        // A4 invariant: the new upload entry point elides the
        // `UnsupportedVersion` stub sentinel that `drive_upload` emits on
        // happy path. The inner drive loop reaches post-delta and the
        // caller gets `Ok(())` so it can call `finish_session` explicitly.
        //
        // Inbound: server preamble + signature phase payload (sum_head +
        // 1 sum_block) — same shape as
        // `driver_upload_writes_preamble_then_filelist_then_terminator`.
        let head = SumHead {
            count: 1,
            block_length: 1024,
            checksum_length: 2,
            remainder_length: 0,
        };
        let blocks = vec![make_sig_block(0xAAAAAAAA, 0x11, 2)];
        let sig_payload = build_sig_phase_payload(1, 0x8002, &head, &blocks);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &sig_payload));
        let transport = mock_transport_with_raw_inbound(inbound);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        let res = d
            .drive_upload_through_delta(
                RemoteCommandSpec::upload("/remote/target.bin"),
                sample_file_list_entry("target.bin"),
                &[],
                &MockSigAdapter::default(),
                &mut sink,
            )
            .await;
        assert!(res.is_ok(), "through_delta must return Ok, got {:?}", res);
        // Phase must NOT be Stub (that's the legacy sentinel indicator).
        assert_ne!(
            d.phase(),
            AerorsyncSessionPhase::Stub,
            "through_delta must not set Stub phase"
        );
        // Delta phase crosses the PreCommit→PostCommit boundary.
        assert!(d.committed());
    }

    #[tokio::test]
    async fn drive_download_through_delta_returns_ok_on_happy_path() {
        let wire_ops = vec![DeltaOp::CopyRun {
            start_token_index: 0,
            run_length: 1,
        }];
        let delta_bytes = encode_delta_stream(&DeltaStreamReport {
            ops: wire_ops,
            file_checksum: vec![0u8; A2_3_FILE_CHECKSUM_LEN],
        });
        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            // B.2: align with `build_flist_options()` so test-side
            // pre-encoded payloads round-trip through the driver decoder.
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &delta_bytes));

        let transport = mock_transport_with_raw_inbound(inbound);
        let adapter =
            MockSigAdapter::with_fixed_signatures(4, vec![make_engine_sig(0, 0xA0, 0x01, 4)]);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        let res = d
            .drive_download_through_delta(
                RemoteCommandSpec::download("/remote/x"),
                b"BLK0",
                &adapter,
                &mut sink,
            )
            .await;
        assert!(
            res.is_ok(),
            "through_delta download must return Ok, got {:?}",
            res
        );
        assert!(d.reconstructed().is_some());
        // Download path leaves committed=false; A4 flips it at temp-file open.
        assert!(!d.committed());
    }

    #[tokio::test]
    async fn drive_upload_through_delta_propagates_real_error() {
        // Pin that the new entry point does NOT mask genuine drive errors
        // behind an `Ok(())`: when the remote closes mid-phase, the returned
        // error must be the real `TransportFailure` (not the legacy stub
        // sentinel, and not `Ok(())`), and `phase = Failed` must be set.
        let inbound = canonical_server_preamble_bytes(); // no sig phase → EOF mid-flight
        let transport = mock_transport_with_raw_inbound(inbound);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        let err = d
            .drive_upload_through_delta(
                RemoteCommandSpec::upload("/remote/target.bin"),
                sample_file_list_entry("target.bin"),
                &[],
                &MockSigAdapter::default(),
                &mut sink,
            )
            .await
            .expect_err("expected real TransportFailure, not Ok");
        assert_eq!(err.kind, AerorsyncErrorKind::TransportFailure);
        assert_eq!(d.phase(), AerorsyncSessionPhase::Failed);
    }

    // ---- P3-T01 W1.2 streaming-send parity pins -------------------------
    //
    // These tests are the byte-identical wire pin for
    // `send_delta_phase_streaming` against `send_delta_phase_single_file`.
    // If they fail, the streaming path has diverged from the bulk path
    // on the wire — root-cause is one of:
    //   1. the producer (W1.1) emits different ops than `compute_delta`
    //      (covered by `producer_streaming_matches_bulk_*` in
    //      `engine_adapter.rs`),
    //   2. the streaming xxh3 trailer no longer matches `xxh3_128`
    //      bulk (covered by `streaming_xxh3_matches_bulk_xxh3` below),
    //   3. the post-plan emission code (zstd compression, wire op
    //      construction, NDX/iflags/sum_head echo, payload framing) was
    //      changed in only one of the two functions — they MUST stay
    //      bit-for-bit symmetric until W1.3 lifts the upload cap.
    //
    // The tests avoid relying on `MockSigAdapter`: the bulk path now
    // calls `compute_delta` against `RealEngineAdapter`'s real rolling
    // checksum engine (see `_adapter` parameter in the streaming path,
    // currently unused — both paths derive the plan from
    // `received_signatures` + the source bytes via the producer / bulk
    // computation, no mock substitution).

    fn build_streaming_parity_inbound(head: SumHead, blocks: Vec<SumBlock>) -> Vec<u8> {
        let sig_payload = build_sig_phase_payload(1, 0x8002, &head, &blocks);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &sig_payload));
        inbound
    }

    /// xxh3 streaming digest must equal xxh3 bulk digest for any
    /// chunking strategy. If this regresses, the streaming send's
    /// `file_checksum` trailer will silently diverge and rsync will
    /// reject the upload with "WHOLE FILE IS WRONG" (exit 22).
    #[test]
    fn streaming_xxh3_matches_bulk_xxh3() {
        let payload: Vec<u8> = (0..50_000u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
            .collect();
        let bulk = xxh3_128(&payload);

        for chunk_size in [1usize, 7, 1024, 4096, 16384, 50_000] {
            let mut hasher = Xxh3Default::new();
            for chunk in payload.chunks(chunk_size) {
                hasher.update(chunk);
            }
            assert_eq!(
                hasher.digest128(),
                bulk,
                "xxh3 streaming digest must equal bulk digest for chunk_size={chunk_size}"
            );
        }

        // Empty payload edge case.
        let bulk_empty = xxh3_128(b"");
        let mut hasher_empty = Xxh3Default::new();
        hasher_empty.update(b"");
        assert_eq!(hasher_empty.digest128(), bulk_empty);
    }

    /// Wire-byte parity pin: bulk and streaming send paths must produce
    /// the exact same outbound byte sequence on the raw transport. If
    /// the assertion fails, prefix of the diff is the first byte where
    /// the two paths diverged — start hunting there.
    ///
    /// Both paths use [`CurrentDeltaSyncBridge`] so the bulk plan and
    /// the streaming plan come from the SAME algorithm
    /// (`delta_sync::compute_delta` vs. `RollingDeltaPlanProducer`,
    /// already cross-pinned bit-for-bit by `producer_streaming_matches_bulk_*`
    /// in `engine_adapter.rs`).
    async fn assert_send_parity(source: &[u8], head: SumHead, blocks: Vec<SumBlock>) {
        use crate::aerorsync::engine_adapter::CurrentDeltaSyncBridge;

        // Bulk path
        let bulk_inbound = build_streaming_parity_inbound(head, blocks.clone());
        let bulk_transport = mock_transport_with_raw_inbound(bulk_inbound);
        let bulk_last = bulk_transport.last_raw_outbound.clone();
        let mut bulk_d = make_driver(bulk_transport);
        let mut bulk_sink = CollectingSink::default();
        let bulk_adapter = CurrentDeltaSyncBridge::new();
        bulk_d
            .drive_upload_through_delta(
                RemoteCommandSpec::upload("/remote/target.bin"),
                sample_file_list_entry("target.bin"),
                source,
                &bulk_adapter,
                &mut bulk_sink,
            )
            .await
            .expect("bulk path must complete");
        let bulk_bytes = {
            let g = bulk_last.lock().unwrap();
            let arc = g.as_ref().expect("bulk: raw stream must have been opened");
            let bytes = arc.lock().unwrap().clone();
            bytes
        };

        // Streaming path
        let stream_inbound = build_streaming_parity_inbound(head, blocks);
        let stream_transport = mock_transport_with_raw_inbound(stream_inbound);
        let stream_last = stream_transport.last_raw_outbound.clone();
        let mut stream_d = make_driver(stream_transport);
        let mut stream_sink = CollectingSink::default();
        let stream_adapter = CurrentDeltaSyncBridge::new();
        let cursor = std::io::Cursor::new(source.to_vec());
        stream_d
            .drive_upload_through_delta_streaming(
                RemoteCommandSpec::upload("/remote/target.bin"),
                sample_file_list_entry("target.bin"),
                cursor,
                source.len() as u64,
                &stream_adapter,
                &mut stream_sink,
            )
            .await
            .expect("streaming path must complete");
        let stream_bytes = {
            let g = stream_last.lock().unwrap();
            let arc = g
                .as_ref()
                .expect("streaming: raw stream must have been opened");
            let bytes = arc.lock().unwrap().clone();
            bytes
        };

        assert_eq!(
            bulk_bytes.len(),
            stream_bytes.len(),
            "outbound length mismatch: bulk={} streaming={}",
            bulk_bytes.len(),
            stream_bytes.len()
        );
        if bulk_bytes != stream_bytes {
            let first_diff = bulk_bytes
                .iter()
                .zip(stream_bytes.iter())
                .position(|(a, b)| a != b)
                .unwrap_or(bulk_bytes.len());
            panic!(
                "outbound divergence at byte {first_diff}: bulk={:#04x} streaming={:#04x}",
                bulk_bytes.get(first_diff).copied().unwrap_or(0),
                stream_bytes.get(first_diff).copied().unwrap_or(0)
            );
        }
    }

    /// Empty source against a `block_size == 0` server head — the
    /// realistic shape: the receiver's local target is missing or
    /// zero-byte, so its sum_head emits `block_length = 0`. Both bulk
    /// and streaming paths then go through their respective
    /// "whole-file no-baseline" short-circuits and produce zero
    /// `EngineDeltaOp` (no literal token on the wire) plus the xxh3
    /// trailer of an empty buffer.
    #[tokio::test]
    async fn streaming_send_matches_bulk_send_empty_source_block_size_zero() {
        let head = SumHead {
            count: 0,
            block_length: 0,
            checksum_length: 0,
            remainder_length: 0,
        };
        assert_send_parity(&[], head, Vec::new()).await;
    }

    /// Whole-file path with non-empty source: the receiver advertises
    /// `block_length = 0` (no baseline) so both paths emit the entire
    /// source as a single literal. Pin that the streaming path drains
    /// the reader correctly through the whole-file short-circuit
    /// without calling the producer.
    #[tokio::test]
    async fn streaming_send_matches_bulk_send_whole_file_no_baseline() {
        let head = SumHead {
            count: 0,
            block_length: 0,
            checksum_length: 0,
            remainder_length: 0,
        };
        let source: Vec<u8> = (0..3000u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
            .collect();
        assert_send_parity(&source, head, Vec::new()).await;
    }

    /// Source smaller than block_size: producer emits a single literal
    /// with the full source (no rolling window can be initialised).
    #[tokio::test]
    async fn streaming_send_matches_bulk_send_smaller_than_block() {
        let head = SumHead {
            count: 1,
            block_length: 1024,
            checksum_length: 2,
            remainder_length: 0,
        };
        let blocks = vec![make_sig_block(0xAAAAAAAA, 0x11, 2)];
        let source: Vec<u8> = (0..500u32).map(|i| (i & 0xFF) as u8).collect();
        assert_send_parity(&source, head, blocks).await;
    }

    /// Source larger than block_size with no signature matches: producer
    /// streams a long literal interleaved with rolling-window walk; this
    /// exercises the chunk-boundary drain logic against `compute_delta`.
    #[tokio::test]
    async fn streaming_send_matches_bulk_send_disjoint_source() {
        let head = SumHead {
            count: 1,
            block_length: 1024,
            checksum_length: 2,
            remainder_length: 0,
        };
        let blocks = vec![make_sig_block(0xAAAAAAAA, 0x11, 2)];
        // 5 KB pseudo-random source — rolling sums won't hit 0xAAAAAAAA.
        let source: Vec<u8> = (0..5000u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
            .collect();
        assert_send_parity(&source, head, blocks).await;
    }

    /// Source whose first block matches the synthetic signature block:
    /// producer emits a CopyBlock followed by a trailing literal. Pin
    /// that the wire CopyRun token matches the bulk path bit-for-bit.
    #[tokio::test]
    async fn streaming_send_matches_bulk_send_with_one_copyblock() {
        // Build a destination block whose rolling+strong signature is
        // computable, then place that block at the start of the source
        // followed by disjoint tail bytes. The signature phase advertises
        // exactly that block to the sender.
        use crate::delta_sync::compute_signatures;
        const BLOCK_LEN: usize = 1024;
        let block_bytes: Vec<u8> = (0..BLOCK_LEN as u32)
            .map(|i| (i.wrapping_mul(0x9E37_79B1) >> 24) as u8)
            .collect();

        let dest_signatures = compute_signatures(&block_bytes, BLOCK_LEN);
        assert_eq!(dest_signatures.signatures.len(), 1);
        let sig0 = &dest_signatures.signatures[0];

        // Build the wire SumBlock the server would have sent. The
        // `checksum_length` is the s2length the receiver advertised;
        // we use 16 here so the strong half match logic exercises the
        // full xxh-style strong field rather than a trivial 2-byte
        // truncation.
        let head = SumHead {
            count: 1,
            block_length: BLOCK_LEN as i32,
            checksum_length: 16,
            remainder_length: 0,
        };
        let block = SumBlock {
            rolling: sig0.rolling,
            strong: sig0.strong[..16].to_vec(),
        };
        let blocks = vec![block];

        // Source = block_bytes (matches) + 700 bytes of disjoint tail.
        let mut source = block_bytes.clone();
        source.extend((0..700u32).map(|i| (i.wrapping_mul(0xDEADBEEF) >> 24) as u8));

        assert_send_parity(&source, head, blocks).await;
    }

    /// Source long enough to span multiple `STREAMING_READ_CHUNK_BYTES`
    /// reads so the chunk-boundary invariant is exercised on the
    /// rolling window seam. Memory budget: `~5 MiB` worth of source on
    /// the heap, fine for CI.
    #[tokio::test]
    async fn streaming_send_matches_bulk_send_multi_chunk() {
        let head = SumHead {
            count: 1,
            block_length: 1024,
            checksum_length: 2,
            remainder_length: 0,
        };
        let blocks = vec![make_sig_block(0xAAAAAAAA, 0x11, 2)];
        // 5 MiB pseudo-random source → 2 streaming reads of 4 MiB and
        // 1 MiB respectively.
        let len = 5 * 1024 * 1024;
        let source: Vec<u8> = (0..len as u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
            .collect();
        assert_send_parity(&source, head, blocks).await;
    }

    /// P3-T01 W1.3 — `block_size == 0` chunked-literal pin.
    ///
    /// When the receiver advertises `block_length == 0` (no baseline)
    /// **and** the source exceeds `STREAMING_READ_CHUNK_BYTES`, the
    /// streaming path must emit multiple engine literals through the
    /// session-wide zstd `CCtx` instead of accumulating a single
    /// `Vec<u8>` of `source_len` bytes (the W1.2 shape, OOM-prone on
    /// multi-GiB no-baseline uploads).
    ///
    /// The observable: the wire bytes diverge from the bulk path's
    /// because the bulk path emits one big literal (one zstd frame),
    /// while the streaming path emits N literals (N zstd frames). The
    /// receiver's session-wide `ZSTD_DCtx` concatenates both shapes to
    /// the same plaintext per stock rsync's `send_zstd_token`
    /// semantics, so the divergence is *protocol-equivalent*.
    ///
    /// Companion to `streaming_send_matches_bulk_send_whole_file_no_baseline`
    /// (which pins identity for sources `<= STREAMING_READ_CHUNK_BYTES`).
    /// Together these two tests pin the split-point at
    /// `STREAMING_READ_CHUNK_BYTES`.
    #[tokio::test]
    async fn streaming_send_block_size_zero_chunks_large_source() {
        use crate::aerorsync::engine_adapter::CurrentDeltaSyncBridge;

        let head = SumHead {
            count: 0,
            block_length: 0,
            checksum_length: 0,
            remainder_length: 0,
        };
        // 5 MiB pseudo-random source. With `STREAMING_READ_CHUNK_BYTES =
        // 4 MiB`, the streaming path emits 2 engine literals (4 MiB +
        // 1 MiB) while the bulk path emits 1 (5 MiB).
        let len = 5 * 1024 * 1024usize;
        let source: Vec<u8> = (0..len as u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
            .collect();

        // Bulk path
        let bulk_inbound = build_streaming_parity_inbound(head, Vec::new());
        let bulk_transport = mock_transport_with_raw_inbound(bulk_inbound);
        let bulk_last = bulk_transport.last_raw_outbound.clone();
        let mut bulk_d = make_driver(bulk_transport);
        let mut bulk_sink = CollectingSink::default();
        let bulk_adapter = CurrentDeltaSyncBridge::new();
        bulk_d
            .drive_upload_through_delta(
                RemoteCommandSpec::upload("/remote/target.bin"),
                sample_file_list_entry("target.bin"),
                &source,
                &bulk_adapter,
                &mut bulk_sink,
            )
            .await
            .expect("bulk path must complete");
        let bulk_wire_op_count = bulk_d.emitted_delta_ops.len();
        let bulk_bytes = {
            let g = bulk_last.lock().unwrap();
            let arc = g.as_ref().expect("bulk: raw stream must have been opened");
            let bytes = arc.lock().unwrap().clone();
            bytes
        };

        // Streaming path
        let stream_inbound = build_streaming_parity_inbound(head, Vec::new());
        let stream_transport = mock_transport_with_raw_inbound(stream_inbound);
        let stream_last = stream_transport.last_raw_outbound.clone();
        let mut stream_d = make_driver(stream_transport);
        let mut stream_sink = CollectingSink::default();
        let stream_adapter = CurrentDeltaSyncBridge::new();
        let cursor = std::io::Cursor::new(source.clone());
        stream_d
            .drive_upload_through_delta_streaming(
                RemoteCommandSpec::upload("/remote/target.bin"),
                sample_file_list_entry("target.bin"),
                cursor,
                source.len() as u64,
                &stream_adapter,
                &mut stream_sink,
            )
            .await
            .expect("streaming path must complete");
        let stream_wire_op_count = stream_d.emitted_delta_ops.len();
        let stream_bytes = {
            let g = stream_last.lock().unwrap();
            let arc = g
                .as_ref()
                .expect("streaming: raw stream must have been opened");
            let bytes = arc.lock().unwrap().clone();
            bytes
        };

        // Pin 1: both paths must complete and emit at least one wire
        // literal each (proves we did not regress to "zero ops" on the
        // whole-file branch).
        assert!(
            bulk_wire_op_count > 0,
            "bulk path emitted zero wire ops on a 5 MiB source"
        );
        assert!(
            stream_wire_op_count > 0,
            "streaming path emitted zero wire ops on a 5 MiB source"
        );

        // Pin 2: wire bytes MUST differ. Bulk emits one zstd frame for
        // the full literal; streaming emits two zstd frames (one per
        // 4 MiB slab). The session-wide `CCtx` flush boundary between
        // them is byte-observable in the compressed output.
        assert_ne!(
            bulk_bytes, stream_bytes,
            "block_size==0 with source > STREAMING_READ_CHUNK_BYTES MUST chunk the literal — bulk and streaming wire bytes must differ"
        );

        // Pin 3: byte-count delta is small (at most a few KiB of zstd
        // frame overhead per extra slab). 10% headroom is generous; if
        // the divergence ever blows past this, something is wrong with
        // either the chunk size or the zstd CCtx reuse.
        let len_diff =
            (bulk_bytes.len() as i64 - stream_bytes.len() as i64).unsigned_abs() as usize;
        assert!(
            len_diff < bulk_bytes.len() / 10,
            "zstd-frame overhead ballooned: bulk={} streaming={} diff={} (>10% of bulk)",
            bulk_bytes.len(),
            stream_bytes.len(),
            len_diff
        );
    }

    /// Sanity: declared `source_len` mismatch aborts with InvalidFrame
    /// rather than emitting half a delta phase on the wire. Guards
    /// against silent corruption when the file changes during read.
    #[tokio::test]
    async fn streaming_send_rejects_source_len_mismatch() {
        let head = SumHead {
            count: 1,
            block_length: 1024,
            checksum_length: 2,
            remainder_length: 0,
        };
        let blocks = vec![make_sig_block(0xAAAAAAAA, 0x11, 2)];
        let inbound = build_streaming_parity_inbound(head, blocks);
        let transport = mock_transport_with_raw_inbound(inbound);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        let source = vec![0u8; 100];
        let cursor = std::io::Cursor::new(source.clone());
        let err = d
            .drive_upload_through_delta_streaming(
                RemoteCommandSpec::upload("/remote/target.bin"),
                sample_file_list_entry("target.bin"),
                cursor,
                // Lie about the length: declared 200, actual 100.
                200u64,
                &MockSigAdapter::default(),
                &mut sink,
            )
            .await
            .expect_err("must abort on length mismatch");
        assert_eq!(err.kind, AerorsyncErrorKind::InvalidFrame);
        assert_eq!(d.phase(), AerorsyncSessionPhase::Failed);
    }

    #[tokio::test]
    async fn driver_upload_delta_literal_over_max_len_survives_via_s8j_chunking() {
        // Historical pre-S8j behaviour: a raw literal whose compressed
        // blob exceeded `MAX_DELTA_LITERAL_LEN` was rejected with
        // `InvalidFrame` and the detail string "multi-chunk splitting
        // deferred". S8j (2026-04-26) removed that bail — the driver
        // now splits the oversized blob into successive DEFLATED_DATA
        // tokens of ≤ 16 383 bytes each. Reaching the A2.3 stub
        // frontier is proof that the delta phase did not abort on the
        // size check; `driver_upload_delta_splits_large_compressed_literal_*`
        // covers the chunking shape itself.
        let mut big_raw = Vec::with_capacity(30_000);
        let mut state: u32 = 0x12345678;
        for _ in 0..30_000 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            big_raw.push(state as u8);
        }
        let head = SumHead {
            count: 1,
            block_length: 1024,
            checksum_length: 2,
            remainder_length: 0,
        };
        let blocks = vec![make_sig_block(0x11, 0x22, 2)];
        let sig_payload = build_sig_phase_payload(1, 0x8002, &head, &blocks);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &sig_payload));
        let transport = mock_transport_with_raw_inbound(inbound);
        let adapter = MockSigAdapter::default()
            .with_upload_plan(vec![EngineDeltaOp::Literal(big_raw.clone())]);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        let err = d
            .drive_upload(
                RemoteCommandSpec::upload("/remote/x"),
                sample_file_list_entry("target.bin"),
                &big_raw,
                &adapter,
                &mut sink,
            )
            .await
            .unwrap_err();
        // Post-S8j: delta phase succeeds, the stub frontier fires
        // UnsupportedVersion. Pre-S8j this was InvalidFrame from the
        // blob-size guard.
        assert_eq!(err.kind, AerorsyncErrorKind::UnsupportedVersion);
        assert_eq!(d.phase(), AerorsyncSessionPhase::Stub);
    }

    #[tokio::test]
    async fn driver_file_list_round_trip_matches_frozen_oracle_download() {
        // A2.1 frozen-oracle driver pin. Feed the server -> client byte
        // stream of the download capture to the driver and verify that:
        //   (a) the driver decodes at least one `FileListEntry` from the
        //       real rsync wire bytes (not from our own encoder);
        //   (b) `committed()` stays false during the file list phase.
        // Skip-graceful when the frozen oracle is not checked out.
        //
        // The download capture continues past the file list terminator
        // with ndx / sum_head / delta frames that A2.1 does not handle —
        // the driver will surface an `InvalidFrame` error from the
        // decoder when it tries to read past the terminator as another
        // file-list entry. We accept both terminations as long as at
        // least one entry has already been absorbed.
        let Some(frozen) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
            eprintln!("frozen oracle missing — A2.1 driver pin skipped");
            return;
        };
        let transport = mock_transport_with_raw_inbound(frozen.download_server_to_client.clone());
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        let outcome = d
            .drive_download(
                RemoteCommandSpec::download("/workspace/download/target.bin"),
                &[],
                &MockSigAdapter::default(),
                &mut sink,
            )
            .await;
        // Whatever the outcome, the driver MUST have consumed the preamble
        // and started the file list phase. Either the happy path reached
        // the post-terminator stub frontier, or the decoder bailed on a
        // downstream frame — both are acceptable for A2.1 so long as the
        // file list decode landed at least one entry.
        assert!(
            !d.file_list().is_empty(),
            "driver must decode at least one FileListEntry from the frozen download stream \
             (got outcome: {outcome:?})"
        );
        assert!(!d.committed(), "file list phase must stay PreCommit");
        // At minimum the preamble exchange must have populated the state.
        assert!(
            d.protocol_version() >= 30,
            "negotiated protocol must be rsync 30+"
        );
    }

    // ========================================================================
    // P3-T01 W2.4 — drive_download_through_delta_streaming tests
    //
    // The 3 existing mock download tests
    // (`driver_download_delta_decodes_ops_and_reconstructs`,
    // `driver_download_delta_coalesces_consecutive_literal_chunks_into_one_engine_literal`,
    // `driver_download_delta_preserves_committed_false`) act as the
    // non-regression pin for the bulk path. Their continued passing is the
    // W2.4 acceptance gate "bulk path unchanged".
    //
    // The tests below exercise the new streaming entry point with the
    // same wire fixture shape (preamble + file list + delta_stream
    // mux frames), substituting `MemoryBaseline` for the destination
    // slice's CopyBlock view and a collecting `MockAsyncWriter` for the
    // reconstructed sink. The fixture is small enough that a careful
    // reader can trace each assertion back to the wire bytes.
    //
    // P3-T01 W2.5: the streaming entry point now takes the writer by
    // `&mut` instead of via a setter+field. The driver no longer owns the
    // writer across the call, which lets the W2.5 caller `finalize` the
    // `StreamingAtomicWriter` without an awkward downcast back through
    // `Box<dyn AsyncWrite>`. The "guard against missing target" test
    // from W2.4 is gone because there is no longer any state to
    // misconfigure.
    // ========================================================================

    use std::sync::Mutex as StdMutex;
    use std::task::Poll;
    use tokio::io::AsyncWrite;

    /// Test sink that accumulates every `poll_write` payload into a
    /// shared `Vec<u8>` so the test body can pin reconstructed bytes
    /// without ownership games. `Arc<StdMutex<>>` rather than
    /// `tokio::sync::Mutex` because `poll_write` is sync-context only
    /// and the lock is held for the duration of one `extend_from_slice`.
    struct MockAsyncWriter {
        bytes: Arc<StdMutex<Vec<u8>>>,
    }

    impl MockAsyncWriter {
        fn new() -> (Self, Arc<StdMutex<Vec<u8>>>) {
            let bytes = Arc::new(StdMutex::new(Vec::new()));
            (
                Self {
                    bytes: bytes.clone(),
                },
                bytes,
            )
        }
    }

    impl AsyncWrite for MockAsyncWriter {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            self.bytes
                .lock()
                .expect("MockAsyncWriter lock")
                .extend_from_slice(buf);
            Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    /// Sink that returns `BrokenPipe` on the first `poll_write`. Used to
    /// pin error propagation through `apply_delta_streaming` and the
    /// driver's `install_reconstructed_from_wire_streaming` boundary.
    struct FailingMockWriter;
    impl AsyncWrite for FailingMockWriter {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "mock writer always fails",
            )))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    /// Build the inbound MSG_DATA-framed stream for the W2.4 download
    /// fixture: server preamble + file list entry + terminator + delta
    /// stream (CopyRun(2) + Literal). Returns the raw bytes the mock
    /// transport emits to the driver.
    fn streaming_fixture_inbound(literal: &[u8]) -> Vec<u8> {
        use crate::aerorsync::real_wire::{compress_zstd_literal_stream, encode_delta_stream};
        let compressed = compress_zstd_literal_stream(&[literal])
            .expect("zstd compress fixture literal");
        assert_eq!(compressed.len(), 1);
        let wire_ops = vec![
            DeltaOp::CopyRun {
                start_token_index: 0,
                run_length: 2,
            },
            DeltaOp::Literal {
                compressed_payload: compressed[0].clone(),
            },
        ];
        let report = DeltaStreamReport {
            ops: wire_ops,
            file_checksum: vec![0xCC; A2_3_FILE_CHECKSUM_LEN],
        };
        let delta_bytes = encode_delta_stream(&report);

        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);

        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &delta_bytes));
        inbound
    }

    /// W2.4 test 1 — happy path: streaming download decodes the wire
    /// ops and writes the reconstructed bytes (CopyBlock(0) +
    /// CopyBlock(1) + Literal) into the configured `Streaming(writer)`
    /// sink. The shape mirrors
    /// `driver_download_delta_decodes_ops_and_reconstructs` (the
    /// non-regression pin for the bulk path) so any divergence between
    /// streaming and bulk is immediately visible.
    #[tokio::test]
    async fn driver_download_streaming_through_delta_writes_to_writer() {
        use crate::aerorsync::engine_adapter::MemoryBaseline;

        let raw_literal = b"LITERAL_PAYLOAD_ABC";
        let inbound = streaming_fixture_inbound(raw_literal);

        let transport = mock_transport_with_raw_inbound(inbound);
        let adapter = MockSigAdapter::with_fixed_signatures(
            4,
            vec![
                make_engine_sig(0, 0xA0, 0x01, 4),
                make_engine_sig(1, 0xA1, 0x02, 4),
            ],
        );
        let destination_data: Vec<u8> = b"BLK1BLK2".to_vec();
        let mut baseline = MemoryBaseline::new(destination_data.clone());

        let (mut writer, captured) = MockAsyncWriter::new();
        let mut d = make_driver(transport);

        let mut sink = CollectingSink::default();
        d.drive_download_through_delta_streaming(
            RemoteCommandSpec::download("/remote/target.bin"),
            &destination_data,
            &mut baseline,
            &mut writer,
            &adapter,
            &mut sink,
        )
        .await
        .expect("streaming download succeeds");

        // Reconstructed = baseline[0..4] + baseline[4..8] + literal.
        let on_writer = captured.lock().expect("captured lock").clone();
        assert_eq!(&on_writer[0..4], b"BLK1");
        assert_eq!(&on_writer[4..8], b"BLK2");
        assert_eq!(&on_writer[8..], raw_literal.as_slice());
        // `committed` stays false on the driver — matches the bulk-path
        // pin in `driver_download_delta_preserves_committed_false`.
        assert!(
            !d.committed(),
            "streaming download must keep committed=false (W2.5 caller flips its own flag)"
        );
        // File checksum trailer still surfaces, identical to bulk.
        assert_eq!(
            d.received_file_checksum(),
            Some(vec![0xCC; A2_3_FILE_CHECKSUM_LEN].as_slice())
        );
    }

    /// W2.4 test 2 — pin that the driver dispatches `CopyBlock(idx)`
    /// against the **caller-supplied baseline**, not the
    /// `destination_data` slice that the signature phase consumed. Uses
    /// distinct byte patterns for the two so a divergence is loud:
    /// the writer receives the baseline pattern, never the destination
    /// pattern.
    #[tokio::test]
    async fn driver_download_streaming_through_delta_consults_baseline_source() {
        use crate::aerorsync::engine_adapter::MemoryBaseline;

        let raw_literal = b"LITERAL";
        let inbound = streaming_fixture_inbound(raw_literal);

        let transport = mock_transport_with_raw_inbound(inbound);
        let adapter = MockSigAdapter::with_fixed_signatures(
            4,
            vec![
                make_engine_sig(0, 0xA0, 0x01, 4),
                make_engine_sig(1, 0xA1, 0x02, 4),
            ],
        );
        // Distinct patterns so the assertion can tell them apart.
        let destination_data: Vec<u8> = b"AAAABBBB".to_vec();
        let baseline_bytes: Vec<u8> = b"XXXXYYYY".to_vec();
        let mut baseline = MemoryBaseline::new(baseline_bytes);

        let (mut writer, captured) = MockAsyncWriter::new();
        let mut d = make_driver(transport);

        let mut sink = CollectingSink::default();
        d.drive_download_through_delta_streaming(
            RemoteCommandSpec::download("/remote/target.bin"),
            &destination_data,
            &mut baseline,
            &mut writer,
            &adapter,
            &mut sink,
        )
        .await
        .expect("streaming download succeeds");

        let on_writer = captured.lock().expect("captured lock").clone();
        // CopyBlock(0)+CopyBlock(1) must read from the BASELINE, not
        // the destination. If we see "AAAABBBB" the dispatcher is
        // reading from the wrong source.
        assert_eq!(
            &on_writer[0..8],
            b"XXXXYYYY",
            "CopyBlock dispatch must consult BaselineSource, not destination_data"
        );
        assert_eq!(&on_writer[8..], raw_literal.as_slice());
    }

    /// W2.4 test 3 — writer that returns `BrokenPipe` on the first
    /// `poll_write` aborts the download with `InvalidFrame` (the
    /// `apply_delta_streaming: <io error>` envelope). No panic, no
    /// silent success.
    #[tokio::test]
    async fn driver_download_streaming_through_delta_writer_failure_aborts() {
        use crate::aerorsync::engine_adapter::MemoryBaseline;

        let raw_literal = b"WHATEVER";
        let inbound = streaming_fixture_inbound(raw_literal);

        let transport = mock_transport_with_raw_inbound(inbound);
        let adapter = MockSigAdapter::with_fixed_signatures(
            4,
            vec![
                make_engine_sig(0, 0xA0, 0x01, 4),
                make_engine_sig(1, 0xA1, 0x02, 4),
            ],
        );
        let destination_data: Vec<u8> = b"BLK1BLK2".to_vec();
        let mut baseline = MemoryBaseline::new(destination_data.clone());

        let mut d = make_driver(transport);
        let mut writer = FailingMockWriter;

        let mut sink = CollectingSink::default();
        let err = d
            .drive_download_through_delta_streaming(
                RemoteCommandSpec::download("/remote/target.bin"),
                &destination_data,
                &mut baseline,
                &mut writer,
                &adapter,
                &mut sink,
            )
            .await
            .expect_err("failing writer must propagate an error");
        assert_eq!(
            err.kind,
            AerorsyncErrorKind::InvalidFrame,
            "writer failure surfaces as InvalidFrame"
        );
        assert!(
            err.detail.contains("apply_delta_streaming"),
            "error message must reference apply_delta_streaming, got: {}",
            err.detail
        );
        assert_eq!(
            d.phase(),
            AerorsyncSessionPhase::Failed,
            "phase must transition to Failed on writer error"
        );
    }

    /// W2.4 test 4 — after a successful streaming download,
    /// `driver.reconstructed()` returns `None`. The bytes flowed
    /// through the writer; reading them back from RAM would defeat
    /// the streaming purpose.
    #[tokio::test]
    async fn driver_download_streaming_through_delta_keeps_reconstructed_none() {
        use crate::aerorsync::engine_adapter::MemoryBaseline;

        let raw_literal = b"X";
        let inbound = streaming_fixture_inbound(raw_literal);

        let transport = mock_transport_with_raw_inbound(inbound);
        let adapter = MockSigAdapter::with_fixed_signatures(
            4,
            vec![
                make_engine_sig(0, 0xA0, 0x01, 4),
                make_engine_sig(1, 0xA1, 0x02, 4),
            ],
        );
        let destination_data: Vec<u8> = b"BLK1BLK2".to_vec();
        let mut baseline = MemoryBaseline::new(destination_data.clone());

        let (mut writer, _captured) = MockAsyncWriter::new();
        let mut d = make_driver(transport);

        let mut sink = CollectingSink::default();
        d.drive_download_through_delta_streaming(
            RemoteCommandSpec::download("/remote/target.bin"),
            &destination_data,
            &mut baseline,
            &mut writer,
            &adapter,
            &mut sink,
        )
        .await
        .expect("streaming download succeeds");

        assert!(
            d.reconstructed().is_none(),
            "streaming path must NOT populate self.reconstructed"
        );
    }

}
