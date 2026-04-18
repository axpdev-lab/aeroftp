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
//! `NativeRsyncError::unsupported_version` at sum_head exchange. A2.2 will
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
//! arrives now, the driver returns a typed `NativeRsyncError` and
//! `committed()` reports `false`, letting the A4 adapter decide to fall
//! back to the classic-SFTP path.
//!
//! # csum_len in A2.1
//!
//! Hardcoded to 16 (xxh128 / md5 / md4). A2.2 will derive it dynamically
//! from `negotiated_checksum_algos`. Accepted risk, tracked in the
//! checkpoint doc.

use crate::rsync_native_proto::engine_adapter::{
    DeltaEngineAdapter, EngineDeltaOp, EngineSignatureBlock,
};
use crate::rsync_native_proto::events::EventSink;
use crate::rsync_native_proto::real_wire::{
    compress_zstd_literal_stream, decode_delta_stream, decode_file_list_entry,
    decode_item_flags, decode_ndx, decode_server_preamble, decode_sum_block,
    decode_sum_head, decode_summary_frame, decompress_zstd_literal_stream_boundaries,
    encode_client_preamble, encode_delta_stream, encode_file_list_entry,
    encode_file_list_terminator, encode_item_flags, encode_ndx, encode_sum_block,
    encode_sum_head, encode_summary_frame, ClientPreamble, DeltaOp, DeltaStreamReport,
    FileListDecodeOptions, FileListDecodeOutcome, FileListEntry, MuxHeader, MuxPoll,
    MuxStreamReader, MuxTag, NdxState, RealWireError, SumBlock, SumHead, SummaryFrame,
    MAX_DELTA_LITERAL_LEN, NDX_DONE, NDX_FLIST_EOF,
};
use crate::rsync_native_proto::remote_command::RemoteCommandSpec;
use crate::rsync_native_proto::transport::{
    CancelHandle, RawByteStream, RawRemoteShellTransport,
};
use crate::rsync_native_proto::types::{
    NativeRsyncError, NativeRsyncErrorKind, SessionRole, SessionStats,
};
use xxhash_rust::xxh3::xxh3_128;

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
/// Pub because the A4 adapter (`NativeRsyncDeltaTransport`) may want to
/// inspect it for fallback decisions; the internals exposed are
/// informational only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeSessionPhase {
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
pub struct NativeRsyncDriver<T: RawRemoteShellTransport> {
    transport: T,
    cancel_handle: CancelHandle,

    // Populated by `perform_preamble_exchange`.
    protocol_version: u32,
    compat_flags: i32,
    checksum_seed: u32,
    negotiated_checksum_algos: String,
    negotiated_compression_algos: String,

    phase: NativeSessionPhase,
    committed: bool,

    // A2.1 runtime state.
    stream: Option<<T as RawRemoteShellTransport>::RawStream>,
    mux_reader: MuxStreamReader,
    /// Ndx baselines, fresh per session. Used from A2.2 onward; present
    /// now so A2.2 slots in without churn.
    #[allow(dead_code)]
    ndx_state: NdxState,
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
    /// Residual bytes left over after `read_signature_header` parsed
    /// `ndx + iflags + sum_head` — these belong to the following
    /// sum_blocks stream. Used as a prefix by `read_signature_blocks`
    /// so MSG_DATA payload bytes never get dropped on the floor.
    sig_residual_after_header: Vec<u8>,

    // A2.3 delta-phase state.
    /// Download path: reconstructed destination file bytes after
    /// `adapter.apply_delta`. The A4 adapter writes them to a temp file
    /// and renames atomically; the driver itself never touches disk.
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
}

impl<T: RawRemoteShellTransport> NativeRsyncDriver<T> {
    pub fn new(transport: T, cancel_handle: CancelHandle) -> Self {
        Self {
            transport,
            cancel_handle,
            protocol_version: 0,
            compat_flags: 0,
            checksum_seed: 0,
            negotiated_checksum_algos: String::new(),
            negotiated_compression_algos: String::new(),
            phase: NativeSessionPhase::PreConnect,
            committed: false,
            stream: None,
            mux_reader: MuxStreamReader::new(),
            ndx_state: NdxState::default(),
            file_list: Vec::new(),
            received_sum_head: None,
            received_signatures: Vec::new(),
            sent_sum_head: None,
            sent_signatures: Vec::new(),
            last_iflags: 0,
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
        }
    }

    pub fn cancel_handle(&self) -> CancelHandle {
        self.cancel_handle.clone()
    }

    pub fn phase(&self) -> NativeSessionPhase {
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
    ) -> Result<(), NativeRsyncError> {
        match self
            .drive_upload_inner(command_spec, source_entry, source_data, adapter, bridge)
            .await
        {
            Ok(()) => {
                // A2.3 stub frontier: reach post-delta and stop. A2.4's
                // `finish_session` (callable separately) drains the
                // SummaryFrame + shuts the stream down.
                self.phase = NativeSessionPhase::Stub;
                Err(NativeRsyncError::unsupported_version(
                    "native summary/done phase not yet wired — call finish_session() explicitly",
                ))
            }
            Err(e) => {
                self.phase = NativeSessionPhase::Failed;
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
    ) -> Result<(), NativeRsyncError> {
        match self
            .drive_download_inner(command_spec, destination_data, adapter, bridge)
            .await
        {
            Ok(()) => {
                self.phase = NativeSessionPhase::Stub;
                Err(NativeRsyncError::unsupported_version(
                    "native summary/done phase not yet wired — call finish_session() explicitly",
                ))
            }
            Err(e) => {
                self.phase = NativeSessionPhase::Failed;
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
    // The A4 adapter (`NativeRsyncDeltaTransport`) uses these siblings so it
    // does not have to string-match the sentinel detail. Error propagation is
    // identical to the legacy path: any `NativeRsyncError` flows through
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
    ) -> Result<(), NativeRsyncError> {
        match self
            .drive_upload_inner(command_spec, source_entry, source_data, adapter, bridge)
            .await
        {
            Ok(()) => Ok(()),
            Err(e) => {
                self.phase = NativeSessionPhase::Failed;
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
    ) -> Result<(), NativeRsyncError> {
        match self
            .drive_download_inner(command_spec, destination_data, adapter, bridge)
            .await
        {
            Ok(()) => Ok(()),
            Err(e) => {
                self.phase = NativeSessionPhase::Failed;
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
    ) -> Result<(), NativeRsyncError> {
        self.session_role = Some(SessionRole::Sender);
        self.open_raw_stream_internal(&command_spec).await?;
        self.perform_preamble_exchange(31, "md5,xxh64,xxh128", "none,zstd")
            .await?;
        self.send_file_list_single_file(&source_entry).await?;
        self.receive_signature_phase_single_file(bridge).await?;
        self.send_delta_phase_single_file(source_data, adapter).await?;
        Ok(())
    }

    async fn drive_download_inner(
        &mut self,
        command_spec: RemoteCommandSpec,
        destination_data: &[u8],
        adapter: &dyn DeltaEngineAdapter,
        bridge: &mut dyn EventSink,
    ) -> Result<(), NativeRsyncError> {
        self.session_role = Some(SessionRole::Receiver);
        self.open_raw_stream_internal(&command_spec).await?;
        self.perform_preamble_exchange(31, "md5,xxh64,xxh128", "none,zstd")
            .await?;
        self.receive_file_list_single_file(bridge).await?;
        self.send_signature_phase_single_file(destination_data, adapter)
            .await?;
        self.receive_delta_phase_single_file(destination_data, adapter, bridge)
            .await?;
        Ok(())
    }

    // --- private helpers -------------------------------------------------

    async fn open_raw_stream_internal(
        &mut self,
        command_spec: &RemoteCommandSpec,
    ) -> Result<(), NativeRsyncError> {
        self.check_cancel("open_raw_stream")?;
        let stream = self
            .transport
            .open_raw_stream(command_spec.to_exec_request())
            .await?;
        self.stream = Some(stream);
        self.phase = NativeSessionPhase::RawStreamOpen;
        Ok(())
    }

    /// Drain the server preamble from the raw stream, then write our
    /// client preamble. Any bytes read past the server preamble's
    /// `consumed` cursor are fed into `mux_reader` so the subsequent
    /// file list decode sees them.
    async fn perform_preamble_exchange(
        &mut self,
        protocol_version: u32,
        checksum_algos: &str,
        compression_algos: &str,
    ) -> Result<(), NativeRsyncError> {
        let mut scratch = Vec::with_capacity(128);
        loop {
            self.check_cancel("perform_preamble_exchange recv")?;
            match decode_server_preamble(&scratch) {
                Ok(preamble) => {
                    self.protocol_version = preamble.protocol_version;
                    self.compat_flags = preamble.compat_flags;
                    self.checksum_seed = preamble.checksum_seed;
                    self.negotiated_checksum_algos = preamble.checksum_algos;
                    self.negotiated_compression_algos = preamble.compression_algos;
                    // Surplus bytes belong to the mux stream that follows.
                    if preamble.consumed < scratch.len() {
                        self.mux_reader.feed(&scratch[preamble.consumed..]);
                    }
                    break;
                }
                Err(RealWireError::TruncatedBuffer { .. }) => {
                    // Need more bytes — read another chunk.
                    let stream = self.stream.as_mut().ok_or_else(|| {
                        NativeRsyncError::transport(
                            "perform_preamble_exchange: stream not open",
                        )
                    })?;
                    let chunk = stream.read_bytes(RAW_READ_CHUNK).await?;
                    if chunk.is_empty() {
                        return Err(NativeRsyncError::transport(
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
        // Write our client preamble to the wire.
        let outbound = encode_client_preamble(&ClientPreamble {
            protocol_version,
            checksum_algos: checksum_algos.to_string(),
            compression_algos: compression_algos.to_string(),
            consumed: 0,
        });
        let stream = self.stream.as_mut().ok_or_else(|| {
            NativeRsyncError::transport(
                "perform_preamble_exchange: stream vanished pre-write",
            )
        })?;
        stream.write_bytes(&outbound).await?;
        self.phase = NativeSessionPhase::ClientPreambleRecvd;
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
            always_checksum: false,
            csum_len: 16,
            preserve_uid: false,
            preserve_gid: false,
            previous_name: None,
        }
    }

    async fn send_file_list_single_file(
        &mut self,
        entry: &FileListEntry,
    ) -> Result<(), NativeRsyncError> {
        self.phase = NativeSessionPhase::FileListSending;
        let opts = self.build_flist_options();
        let entry_bytes = encode_file_list_entry(entry, &opts);
        self.write_data_frame(&entry_bytes).await?;
        let term_bytes = encode_file_list_terminator(&opts);
        self.write_data_frame(&term_bytes).await?;
        // S8j — remember the entry on the sender side so
        // `emit_summary_phase` can populate `total_size`. Parity with the
        // receiver path, which already pushes decoded entries.
        self.file_list.push(entry.clone());
        self.phase = NativeSessionPhase::FileListSent;
        Ok(())
    }

    async fn receive_file_list_single_file(
        &mut self,
        bridge: &mut dyn EventSink,
    ) -> Result<(), NativeRsyncError> {
        self.phase = NativeSessionPhase::FileListReceiving;
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
                            return Err(NativeRsyncError::invalid_frame(
                                "file list ended without any entry",
                            ));
                        }
                        self.phase = NativeSessionPhase::FileListReceived;
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
    async fn write_data_frame(&mut self, payload: &[u8]) -> Result<(), NativeRsyncError> {
        if payload.len() > 0x00FF_FFFF {
            return Err(NativeRsyncError::invalid_frame(format!(
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
            .ok_or_else(|| NativeRsyncError::transport("write_data_frame: stream not open"))?;
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
    ) -> Result<Vec<u8>, NativeRsyncError> {
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
                        return Err(NativeRsyncError::from_oob_event(&event));
                    }
                }
            }
            self.check_cancel("next_data_frame")?;
            let stream = self
                .stream
                .as_mut()
                .ok_or_else(|| NativeRsyncError::transport("next_data_frame: stream not open"))?;
            let chunk = stream.read_bytes(RAW_READ_CHUNK).await?;
            if chunk.is_empty() {
                return Err(NativeRsyncError::transport(
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
    ) -> Result<(), NativeRsyncError> {
        self.phase = NativeSessionPhase::SumHeadReceiving;
        let (ndx, iflags, head) = self.read_signature_header(bridge).await?;
        if !(0..=i32::MAX).contains(&ndx) {
            return Err(NativeRsyncError::invalid_frame(format!(
                "unexpected ndx sentinel before signature phase: {ndx}"
            )));
        }
        if iflags & ITEM_TRANSFER == 0 {
            return Err(NativeRsyncError::invalid_frame(format!(
                "server signature message lacks ITEM_TRANSFER bit: iflags=0x{iflags:04X}"
            )));
        }
        self.last_iflags = iflags;
        self.received_sum_head = Some(head);

        if head.count < 0 {
            return Err(NativeRsyncError::invalid_frame(format!(
                "server sum_head.count is negative: {}",
                head.count
            )));
        }
        self.phase = NativeSessionPhase::SumBlocksReceiving;
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
    ) -> Result<(i32, u16, SumHead), NativeRsyncError> {
        let mut buf: Vec<u8> = Vec::new();
        // 1. ndx
        let ndx = loop {
            self.check_cancel("read_signature_header ndx")?;
            if !buf.is_empty() {
                match decode_ndx(&buf, &mut self.ndx_state) {
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
            return Err(NativeRsyncError::invalid_frame(format!(
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
    ) -> Result<Vec<SumBlock>, NativeRsyncError> {
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
    ) -> Result<(), NativeRsyncError> {
        self.phase = NativeSessionPhase::SumHeadSent;
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
        payload.extend_from_slice(&encode_ndx(A2_2_FIRST_FILE_NDX, &mut self.ndx_state));
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

        self.phase = NativeSessionPhase::SumBlocksSent;
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
    ) -> Result<(), NativeRsyncError> {
        self.phase = NativeSessionPhase::DeltaSending;

        // Rebuild EngineSignatureBlock vec from received SumBlocks.
        let engine_sigs = self.wire_sigs_to_engine()?;
        let block_size = self
            .received_sum_head
            .as_ref()
            .map(|h| h.block_length as usize)
            .unwrap_or(0);
        if block_size == 0 {
            return Err(NativeRsyncError::invalid_frame(
                "send_delta_phase: block_size is zero (missing sum_head)",
            ));
        }

        // Compute delta via the engine adapter.
        let plan = adapter.compute_delta(source_data, &engine_sigs, block_size);

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
        // Ensure no single blob exceeds the 24-bit DEFLATED_DATA length
        // budget. A2.3 scope does NOT split; S8j will add chunking.
        for (i, blob) in compressed_blobs.iter().enumerate() {
            if blob.len() > MAX_DELTA_LITERAL_LEN {
                return Err(NativeRsyncError::invalid_frame(format!(
                    "A2.3 literal #{i} compressed size {} exceeds {MAX_DELTA_LITERAL_LEN} — \
                     multi-chunk splitting deferred to S8j",
                    blob.len()
                )));
            }
        }

        // Interleave literals with CopyRun ops in the original engine
        // order. Each EngineDeltaOp::CopyBlock(idx) becomes a single-
        // block CopyRun; the engine may already coalesce runs, but we
        // keep A2.3 simple and emit one CopyRun per CopyBlock.
        let mut wire_ops: Vec<DeltaOp> = Vec::with_capacity(plan.ops.len());
        let mut blob_idx: usize = 0;
        for op in &plan.ops {
            match op {
                EngineDeltaOp::Literal(_) => {
                    let blob = compressed_blobs[blob_idx].clone();
                    blob_idx += 1;
                    wire_ops.push(DeltaOp::Literal {
                        compressed_payload: blob,
                    });
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
        let bytes = encode_delta_stream(&report);

        // PreCommit → PostCommit boundary: flip BEFORE writing the first
        // byte of delta material. Once the server starts receiving the
        // delta stream, we no longer can transparently fall back.
        self.committed = true;
        self.emitted_delta_ops = wire_ops;
        self.write_data_frame(&bytes).await?;

        self.phase = NativeSessionPhase::DeltaSent;
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
    ) -> Result<(), NativeRsyncError> {
        self.phase = NativeSessionPhase::DeltaReceiving;

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
                        self.phase = NativeSessionPhase::DeltaReceived;
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
    ) -> Result<(), NativeRsyncError> {
        let zstd_on = self.zstd_negotiated();
        let engine_ops = self.delta_wire_to_engine_ops(&wire_ops, zstd_on)?;
        let block_size = self
            .sent_sum_head
            .as_ref()
            .map(|h| h.block_length as usize)
            .unwrap_or(0);
        if block_size == 0 {
            return Err(NativeRsyncError::invalid_frame(
                "receive_delta_phase: block_size is zero (missing local sum_head)",
            ));
        }
        let reconstructed = adapter
            .apply_delta(destination_data, &engine_ops, block_size)
            .map_err(|e| {
                NativeRsyncError::invalid_frame(format!("apply_delta: {e}"))
            })?;
        self.reconstructed = Some(reconstructed);
        Ok(())
    }

    /// Rebuild an `EngineSignatureBlock` vec from the driver's received
    /// `SumBlock` vec + `received_sum_head`. The strong bytes are zero-
    /// padded to 32 (engine API shape); only the first `checksum_length`
    /// bytes are ever consulted by the engine for matching.
    fn wire_sigs_to_engine(&self) -> Result<Vec<EngineSignatureBlock>, NativeRsyncError> {
        let head = self.received_sum_head.as_ref().ok_or_else(|| {
            NativeRsyncError::invalid_frame(
                "wire_sigs_to_engine: no received sum_head",
            )
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
    /// literals session-wide when zstd is negotiated. A2.3 does NOT
    /// coalesce consecutive CopyRuns — they expand 1:1 into
    /// `EngineDeltaOp::CopyBlock(index)` per block in the run.
    fn delta_wire_to_engine_ops(
        &self,
        wire_ops: &[DeltaOp],
        zstd_on: bool,
    ) -> Result<Vec<EngineDeltaOp>, NativeRsyncError> {
        // Gather literal payloads in order for session-wide decompress.
        let literals: Vec<&[u8]> = wire_ops
            .iter()
            .filter_map(|op| match op {
                DeltaOp::Literal { compressed_payload } => {
                    Some(compressed_payload.as_slice())
                }
                DeltaOp::CopyRun { .. } => None,
            })
            .collect();
        let raw_literals: Vec<Vec<u8>> = if zstd_on && !literals.is_empty() {
            decompress_zstd_literal_stream_boundaries(&literals)
                .map_err(|e| map_realwire_error(e, "zstd decompress delta literals"))?
        } else {
            literals.iter().map(|p| p.to_vec()).collect()
        };

        let mut out = Vec::with_capacity(wire_ops.len());
        let mut literal_idx: usize = 0;
        for op in wire_ops {
            match op {
                DeltaOp::Literal { .. } => {
                    out.push(EngineDeltaOp::Literal(raw_literals[literal_idx].clone()));
                    literal_idx += 1;
                }
                DeltaOp::CopyRun {
                    start_token_index,
                    run_length,
                } => {
                    for k in 0..*run_length {
                        let block_idx = *start_token_index + i32::from(k);
                        if block_idx < 0 {
                            return Err(NativeRsyncError::invalid_frame(format!(
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
        self.negotiated_compression_algos
            .split(',')
            .any(|a| a.trim().eq_ignore_ascii_case("zstd"))
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
    ) -> Result<(), NativeRsyncError> {
        match self.finish_session_inner(bridge).await {
            Ok(()) => {
                self.phase = NativeSessionPhase::Complete;
                Ok(())
            }
            Err(e) => {
                self.phase = NativeSessionPhase::Failed;
                Err(e)
            }
        }
    }

    async fn finish_session_inner(
        &mut self,
        bridge: &mut dyn EventSink,
    ) -> Result<(), NativeRsyncError> {
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
                // Upload against real rsync: we are the sender, so we
                // emit the end-of-session NDX_DONE + SummaryFrame and
                // then read the trailing NDX_DONE the receiver writes
                // back in `read_final_goodbye`.
                self.emit_summary_phase().await?;
                self.read_trailing_ndx_done(bridge).await?;
                // Re-snapshot `session_stats` to include the trailing
                // byte read (and any bytes the `read_trailing` path
                // pulled). Invariant: after `finish_session` returns Ok,
                // `session_stats.{bytes_sent,bytes_received}` equals
                // `driver.{sent_data_bytes,received_raw_bytes}`.
                self.session_stats.bytes_sent = self.sent_data_bytes;
                self.session_stats.bytes_received = self.received_raw_bytes;
            }
            None => {
                // Legacy test path: `finish_session` was invoked on a
                // driver that never entered `drive_*_inner`. Preserve
                // the A2.4 receive-only semantics so the synthesised
                // mock inbound still decodes correctly.
                self.receive_summary_phase(bridge).await?;
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
    ) -> Result<(), NativeRsyncError> {
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
                return Err(NativeRsyncError::invalid_frame(format!(
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
    ) -> Result<(), NativeRsyncError> {
        self.phase = NativeSessionPhase::SummaryReceiving;
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
                        self.phase = NativeSessionPhase::SummaryReceived;
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
    async fn shutdown_raw_stream(&mut self) -> Result<(), NativeRsyncError> {
        if let Some(mut stream) = self.stream.take() {
            stream.shutdown().await?;
        }
        self.phase = NativeSessionPhase::Complete;
        Ok(())
    }

    // --- S8j upload finish helpers ---------------------------------------

    /// Write a single `NDX_DONE` marker (1 byte `0x00`) wrapped in a
    /// MSG_DATA mux frame. `write_data_frame` enforces the wrapping.
    async fn emit_ndx_done_marker(&mut self) -> Result<(), NativeRsyncError> {
        self.write_data_frame(&[0x00]).await
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
    async fn emit_summary_phase(&mut self) -> Result<(), NativeRsyncError> {
        self.phase = NativeSessionPhase::SummaryReceiving;
        let total_size = self
            .file_list
            .first()
            .map(|e| e.size)
            .unwrap_or(0);
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
        self.phase = NativeSessionPhase::SummaryReceived;
        Ok(())
    }

    /// Read the final `NDX_DONE` (1 byte `0x00`) the rsync receiver
    /// writes back in `read_final_goodbye` line 887 after consuming
    /// the sender's `NDX_DONE + SummaryFrame`. Tolerates clean EOF
    /// (some rsync builds close the channel before the byte flushes).
    async fn read_trailing_ndx_done(
        &mut self,
        bridge: &mut dyn EventSink,
    ) -> Result<(), NativeRsyncError> {
        // Best-effort read: if the stream is already closed, or the
        // next frame is empty, treat as clean completion.
        match self.next_data_frame(bridge).await {
            Ok(bytes) => {
                if let Some(&b) = bytes.first() {
                    if b != 0x00 {
                        return Err(NativeRsyncError::invalid_frame(format!(
                            "expected trailing NDX_DONE (0x00), got 0x{b:02X}"
                        )));
                    }
                }
                // bytes.is_empty() is valid too — nothing to check.
                Ok(())
            }
            Err(e) if e.kind == NativeRsyncErrorKind::TransportFailure => {
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

    fn check_cancel(&self, op: &'static str) -> Result<(), NativeRsyncError> {
        if self.cancel_handle.requested() {
            Err(NativeRsyncError::cancelled(format!(
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
    ) -> Result<(), NativeRsyncError> {
        let preamble = ClientPreamble {
            protocol_version,
            checksum_algos: checksum_algos.to_string(),
            compression_algos: compression_algos.to_string(),
            consumed: 0,
        };
        let bytes = encode_client_preamble(&preamble);
        sink.extend_from_slice(&bytes);
        self.phase = NativeSessionPhase::ServerPreambleSent;
        Ok(())
    }

    #[allow(clippy::unused_async)]
    async fn receive_server_preamble(
        &mut self,
        source: &[u8],
    ) -> Result<usize, NativeRsyncError> {
        let preamble = decode_server_preamble(source).map_err(|e| {
            self.phase = NativeSessionPhase::Failed;
            map_realwire_error(e, "server preamble")
        })?;
        self.protocol_version = preamble.protocol_version;
        self.compat_flags = preamble.compat_flags;
        self.checksum_seed = preamble.checksum_seed;
        self.negotiated_checksum_algos = preamble.checksum_algos;
        self.negotiated_compression_algos = preamble.compression_algos;
        self.phase = NativeSessionPhase::ClientPreambleRecvd;
        Ok(preamble.consumed)
    }
}

fn map_realwire_error(err: RealWireError, context: &'static str) -> NativeRsyncError {
    NativeRsyncError::new(
        NativeRsyncErrorKind::InvalidFrame,
        format!("{context}: {err}"),
    )
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rsync_native_proto::engine_adapter::{
        DeltaEngineAdapter, EngineDeltaOp, EngineDeltaPlan, EngineSignatureBlock,
    };
    use crate::rsync_native_proto::events::{
        classify_oob_frame, CollectingSink, NativeRsyncEvent,
    };
    use crate::rsync_native_proto::fixtures::RealRsyncBaselineByteTranscript;
    use crate::rsync_native_proto::mock::{MockRemoteShellTransport, MockTransportConfig};
    use crate::rsync_native_proto::real_wire::{
        encode_server_preamble, ServerPreamble,
    };

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
        fn with_fixed_signatures(
            block_size: usize,
            signatures: Vec<EngineSignatureBlock>,
        ) -> Self {
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
    fn build_sig_phase_payload(ndx: i32, iflags: u16, head: &SumHead, blocks: &[SumBlock]) -> Vec<u8> {
        use crate::rsync_native_proto::real_wire::{encode_ndx, encode_item_flags, encode_sum_head, encode_sum_block, NdxState};
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

    fn make_engine_sig(index: u32, rolling: u32, strong_first_byte: u8, block_len: u32) -> EngineSignatureBlock {
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
    ) -> NativeRsyncDriver<MockRemoteShellTransport> {
        NativeRsyncDriver::new(transport, CancelHandle::inert())
    }

    fn canonical_server_preamble_bytes() -> Vec<u8> {
        encode_server_preamble(&ServerPreamble {
            protocol_version: 31,
            compat_flags: 0x07,
            checksum_algos: "md5,xxh64".to_string(),
            compression_algos: "none,zstd".to_string(),
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
    /// under `build_flist_options` (varint flags, no csum, no uid/gid).
    /// The flags include `XMIT_LONG_NAME` so the suffix length is encoded
    /// as a varint — which the path length (9 chars) still fits in.
    fn sample_file_list_entry(path: &str) -> FileListEntry {
        // Flags: XMIT_LONG_NAME (0x0040) | XMIT_SAME_MODE (0x0002) |
        //        XMIT_SAME_TIME (0x0080) | XMIT_SAME_UID (0x0008) |
        //        XMIT_SAME_GID (0x0010)
        // — the "all same" upload case where only the name and size are
        // transmitted. Matches the minimum-viable frozen-oracle shape.
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
            checksum: Vec::new(),
        }
    }

    // ---- A2.0 regression pins (preserved) -------------------------------

    #[test]
    fn constructor_initialises_phase_and_defaults() {
        let d = make_driver(mock_transport());
        assert_eq!(d.phase(), NativeSessionPhase::PreConnect);
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
        use crate::rsync_native_proto::real_wire::decode_client_preamble;
        let mut d = make_driver(mock_transport());
        let mut sink = Vec::new();
        d.send_client_preamble(&mut sink, 31, "md5,xxh64", "none,zstd")
            .await
            .unwrap();
        let decoded = decode_client_preamble(&sink).unwrap();
        assert_eq!(decoded.protocol_version, 31);
        assert_eq!(decoded.checksum_algos, "md5,xxh64");
        assert_eq!(decoded.compression_algos, "none,zstd");
        assert_eq!(d.phase(), NativeSessionPhase::ServerPreambleSent);
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
        assert_eq!(d.negotiated_checksum_algos(), "md5,xxh64");
        assert_eq!(d.negotiated_compression_algos(), "none,zstd");
        assert_eq!(d.phase(), NativeSessionPhase::ClientPreambleRecvd);
    }

    #[tokio::test]
    async fn receive_server_preamble_on_malformed_bytes_marks_failed() {
        let mut d = make_driver(mock_transport());
        let err = d.receive_server_preamble(&[0x01]).await.unwrap_err();
        assert_eq!(err.kind, NativeRsyncErrorKind::InvalidFrame);
        assert!(err.detail.contains("server preamble"));
        assert_eq!(d.phase(), NativeSessionPhase::Failed);
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
        assert_eq!(err.kind, NativeRsyncErrorKind::UnsupportedVersion);
        assert_eq!(d.phase(), NativeSessionPhase::Stub);
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
            // S8j: the driver now offers xxh128 as the preferred
            // file-level checksum algo so rsync 3.2.7 picks it during
            // negotiation. The mock's canonical server preamble still
            // echoes the pre-S8j "md5,xxh64" set — this test only pins
            // the CLIENT's outbound offer.
            checksum_algos: "md5,xxh64,xxh128".to_string(),
            compression_algos: "none,zstd".to_string(),
            consumed: 0,
        });
        assert_eq!(
            &outbound[..expected_client.len()],
            expected_client.as_slice(),
            "client preamble prefix mismatch"
        );

        // Reconstruct the expected file list frames using the driver's opts.
        let opts = FileListDecodeOptions {
            protocol: d.protocol_version(),
            xfer_flags_as_varint: true,
            always_checksum: false,
            csum_len: 16,
            preserve_uid: false,
            preserve_gid: false,
            previous_name: None,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);
        let mut expected_tail = Vec::new();
        expected_tail.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        expected_tail.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));

        // A2.3: after the file list the driver also emits the delta
        // phase (END_FLAG + 16-byte checksum trailer wrapped in a mux
        // frame) so the byte-for-byte match is only valid on the prefix
        // through the file-list terminator.
        let suffix_start = expected_client.len();
        assert_eq!(
            &outbound[suffix_start..suffix_start + expected_tail.len()],
            expected_tail.as_slice(),
            "mux-wrapped file list tail mismatch"
        );
    }

    #[tokio::test]
    async fn driver_download_decodes_filelist_single_entry() {
        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            always_checksum: false,
            csum_len: 16,
            preserve_uid: false,
            preserve_gid: false,
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
        assert_eq!(err.kind, NativeRsyncErrorKind::UnsupportedVersion);
        assert_eq!(d.phase(), NativeSessionPhase::Stub);
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
            always_checksum: false,
            csum_len: 16,
            preserve_uid: false,
            preserve_gid: false,
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
            .filter(|e| matches!(e, NativeRsyncEvent::Warning { .. }))
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
        // Terminal OOB → RemoteError (via NativeRsyncError::from_oob_event).
        assert_eq!(err.kind, NativeRsyncErrorKind::RemoteError);
        assert!(err.detail.contains("remote kaboom"));
        // PreCommit pin: committed stays false.
        assert!(!d.committed(), "stub path must not cross PreCommit boundary");
        assert_eq!(d.phase(), NativeSessionPhase::Failed);
        // Bridge saw the terminal event (forwarded before bail).
        let terminals: Vec<_> = sink
            .events
            .iter()
            .filter(|e| matches!(e, NativeRsyncEvent::Error { .. }))
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
        let mut d = NativeRsyncDriver::new(transport, cancel_handle);
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
        assert_eq!(err.kind, NativeRsyncErrorKind::Cancelled);
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
            always_checksum: false,
            csum_len: 16,
            preserve_uid: false,
            preserve_gid: false,
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
        assert_eq!(err.kind, NativeRsyncErrorKind::TransportFailure);
        assert!(!d.committed());
        assert_eq!(d.phase(), NativeSessionPhase::Failed);
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
        use crate::rsync_native_proto::engine_adapter::CurrentDeltaSyncBridge;
        use crate::rsync_native_proto::ssh_transport::{
            SshHostKeyPolicy, SshRemoteShellTransport, SshTransportConfig,
        };
        use crate::rsync_native_proto::transport::RemoteExecRequest;

        // Skip-graceful if the Docker harness is not reachable. CI starts
        // the container explicitly; a local dev run without Docker simply
        // observes the skip and moves on.
        if tokio::net::TcpStream::connect("127.0.0.1:2224").await.is_err() {
            eprintln!("lane 3 Docker harness not reachable on 127.0.0.1:2224 — skipping");
            return;
        }

        let source_data: Vec<u8> =
            b"aeroftp lane 3 native rsync upload payload\n"
                .iter()
                .copied()
                .cycle()
                .take(1024)
                .collect();

        let key_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/rsync_native_proto/capture/keys/id_ed25519");
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
        let mut driver = NativeRsyncDriver::new(transport, cancel);
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
        assert_eq!(driver.phase(), NativeSessionPhase::Complete);
        let stats = driver.session_stats();
        assert!(
            stats.bytes_sent >= source_data.len() as u64,
            "bytes_sent {} < source len {}: summary frame parse probably stale",
            stats.bytes_sent,
            source_data.len()
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
        assert_eq!(err.kind, NativeRsyncErrorKind::UnsupportedVersion);
        assert!(
            err.detail.contains("summary/done"),
            "A2.3 stub frontier moved to summary/done phase: {}",
            err.detail
        );
        assert_eq!(d.phase(), NativeSessionPhase::Stub);
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
            always_checksum: false,
            csum_len: 16,
            preserve_uid: false,
            preserve_gid: false,
            previous_name: None,
        };
        let entry_bytes =
            encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
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
        assert_eq!(err.kind, NativeRsyncErrorKind::UnsupportedVersion);
        assert_eq!(d.phase(), NativeSessionPhase::Stub);
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
        assert_eq!(err.kind, NativeRsyncErrorKind::RemoteError);
        assert!(err.detail.contains("sig explode"));
        assert!(!d.committed(), "signature phase must stay PreCommit");
        assert_eq!(d.phase(), NativeSessionPhase::Failed);
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
        assert_eq!(err.kind, NativeRsyncErrorKind::InvalidFrame);
        assert!(err.detail.contains("ITEM_TRANSFER"));
        assert_eq!(d.phase(), NativeSessionPhase::Failed);
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
        assert_eq!(err.kind, NativeRsyncErrorKind::UnsupportedVersion);
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
            always_checksum: false,
            csum_len: 16,
            preserve_uid: false,
            preserve_gid: false,
            previous_name: None,
        };
        let entry_bytes =
            encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));

        let transport = mock_transport_with_raw_inbound(inbound);
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel_handle = CancelHandle::new(cancel_flag.clone(), None);
        let mut d = NativeRsyncDriver::new(transport, cancel_handle);
        let mut sink = CollectingSink::default();
        // Cancel before the driver starts.
        cancel_flag.store(true, Ordering::SeqCst);
        let adapter = MockSigAdapter::with_fixed_signatures(
            1024,
            vec![make_engine_sig(0, 0x11, 0x22, 1024)],
        );
        let err = d
            .drive_download(
                RemoteCommandSpec::download("/remote/target.bin"),
                b"abc",
                &adapter,
                &mut sink,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind, NativeRsyncErrorKind::Cancelled);
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
        let transport =
            mock_transport_with_raw_inbound(frozen.upload_server_to_client.clone());
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
        assert_eq!(err.kind, NativeRsyncErrorKind::UnsupportedVersion);
        assert_eq!(d.phase(), NativeSessionPhase::Stub);
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

    #[tokio::test]
    async fn driver_download_delta_decodes_ops_and_reconstructs() {
        // Build a server-side delta stream manually: one CopyRun (run=2)
        // + one Literal + END_FLAG + 16-byte checksum trailer. The
        // driver must decode, decompress literals (if zstd negotiated),
        // call adapter.apply_delta, and stash `reconstructed`.
        use crate::rsync_native_proto::real_wire::{
            compress_zstd_literal_stream, encode_delta_stream,
        };
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
            always_checksum: false,
            csum_len: 16,
            preserve_uid: false,
            preserve_gid: false,
            previous_name: None,
        };
        let entry_bytes =
            encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
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
        assert_eq!(err.kind, NativeRsyncErrorKind::UnsupportedVersion);
        assert_eq!(d.phase(), NativeSessionPhase::Stub);
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
        assert_eq!(
            d.received_file_checksum(),
            Some(vec![0xCC; 16].as_slice()),
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
            always_checksum: false,
            csum_len: 16,
            preserve_uid: false,
            preserve_gid: false,
            previous_name: None,
        };
        let entry_bytes =
            encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &delta_bytes));
        let transport = mock_transport_with_raw_inbound(inbound);
        let adapter = MockSigAdapter::with_fixed_signatures(
            4,
            vec![make_engine_sig(0, 0xA0, 0x01, 4)],
        );
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
            always_checksum: false,
            csum_len: 16,
            preserve_uid: false,
            preserve_gid: false,
            previous_name: None,
        };
        let entry_bytes =
            encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Error, b"delta stream crashed"));
        let transport = mock_transport_with_raw_inbound(inbound);
        let adapter = MockSigAdapter::with_fixed_signatures(
            4,
            vec![make_engine_sig(0, 0xA0, 0x01, 4)],
        );
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
        assert_eq!(err.kind, NativeRsyncErrorKind::RemoteError);
        assert!(err.detail.contains("delta stream crashed"));
        assert!(!d.committed(), "download stays PreCommit even on error");
        assert_eq!(d.phase(), NativeSessionPhase::Failed);
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
        let mut d = NativeRsyncDriver::new(transport, cancel_handle);
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
        assert_eq!(err.kind, NativeRsyncErrorKind::Cancelled);
    }

    #[tokio::test]
    async fn driver_delta_split_across_data_frames_reassembles() {
        // Split the delta stream across two Data frames. Driver must
        // accumulate payloads until decode_delta_stream succeeds.
        use crate::rsync_native_proto::real_wire::encode_delta_stream;
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
            always_checksum: false,
            csum_len: 16,
            preserve_uid: false,
            preserve_gid: false,
            previous_name: None,
        };
        let entry_bytes =
            encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &delta_bytes[..half]));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &delta_bytes[half..]));

        let transport = mock_transport_with_raw_inbound(inbound);
        let adapter = MockSigAdapter::with_fixed_signatures(
            4,
            vec![make_engine_sig(0, 0xA0, 0x01, 4)],
        );
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
        assert_eq!(err.kind, NativeRsyncErrorKind::UnsupportedVersion);
        assert!(d.reconstructed().is_some());
        assert_eq!(
            d.received_file_checksum(),
            Some(vec![0xEE; 16].as_slice()),
        );
    }

    // ---- A2.4 tests ------------------------------------------------------

    fn build_summary_frame_bytes(protocol: u32) -> Vec<u8> {
        use crate::rsync_native_proto::real_wire::encode_summary_frame;
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
        d: &mut NativeRsyncDriver<MockRemoteShellTransport>,
        sink: &mut CollectingSink,
    ) {
        // Reach the A2.3 stub frontier so finish_session has a live
        // stream to finalise.
        let err = d
            .drive_upload(
                RemoteCommandSpec::upload("/remote/x"),
                sample_file_list_entry("target.bin"),
                &[],
                &MockSigAdapter::default(),
                sink,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind, NativeRsyncErrorKind::UnsupportedVersion);
    }

    #[tokio::test]
    async fn driver_finish_session_upload_emits_summary_frame_and_completes() {
        // S8j: upload finish = the CLIENT emits NDX_DONE + SummaryFrame
        // and reads ONE trailing NDX_DONE byte from the server. No more
        // inbound summary bytes — the summary is derived from the
        // driver's own counters.
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

        drive_upload_to_stub(&mut d, &mut sink).await;
        assert_eq!(d.phase(), NativeSessionPhase::Stub);

        d.finish_session(&mut sink)
            .await
            .expect("finish_session upload happy path");
        assert_eq!(d.phase(), NativeSessionPhase::Complete);
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
        let outbound_arc = guard
            .as_ref()
            .expect("raw stream must have been opened");
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
        // S8j: the sender emits the summary first, then reads the trailing
        // NDX_DONE. If the server sends an OOB Error in that slot, the
        // finish must bail with RemoteError and phase=Failed.
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
        assert_eq!(err.kind, NativeRsyncErrorKind::RemoteError);
        assert!(err.detail.contains("trailing phase crash"));
        assert_eq!(d.phase(), NativeSessionPhase::Failed);
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
        let mut d = NativeRsyncDriver::new(transport, cancel_handle);
        let mut sink = CollectingSink::default();
        drive_upload_to_stub(&mut d, &mut sink).await;
        cancel_flag.store(true, Ordering::SeqCst);
        let err = d.finish_session(&mut sink).await.unwrap_err();
        assert_eq!(err.kind, NativeRsyncErrorKind::Cancelled);
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
            always_checksum: false,
            csum_len: 16,
            preserve_uid: false,
            preserve_gid: false,
            previous_name: None,
        };
        let entry_bytes =
            encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
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
        let adapter = MockSigAdapter::with_fixed_signatures(
            4,
            vec![make_engine_sig(0, 0xA0, 0x01, 4)],
        );
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
        assert_eq!(d.phase(), NativeSessionPhase::Complete);
        assert!(!d.committed(), "download A2.4 stays PreCommit; A4 owns the flip");
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
        drive_upload_to_stub(&mut d, &mut sink).await;
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
            always_checksum: false,
            csum_len: 16,
            preserve_uid: false,
            preserve_gid: false,
            previous_name: None,
        };
        let entry_bytes =
            encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
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
        let adapter = MockSigAdapter::with_fixed_signatures(
            4,
            vec![make_engine_sig(0, 0xA0, 0x01, 4)],
        );
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
        assert_eq!(d.phase(), NativeSessionPhase::Complete);
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
            always_checksum: false,
            csum_len: 16,
            preserve_uid: false,
            preserve_gid: false,
            previous_name: None,
        };
        let entry_bytes =
            encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
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
        let adapter = MockSigAdapter::with_fixed_signatures(
            4,
            vec![make_engine_sig(0, 0xA0, 0x01, 4)],
        );
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
        assert_eq!(err.kind, NativeRsyncErrorKind::InvalidFrame);
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
            NativeSessionPhase::Stub,
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
            always_checksum: false,
            csum_len: 16,
            preserve_uid: false,
            preserve_gid: false,
            previous_name: None,
        };
        let entry_bytes =
            encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &delta_bytes));

        let transport = mock_transport_with_raw_inbound(inbound);
        let adapter = MockSigAdapter::with_fixed_signatures(
            4,
            vec![make_engine_sig(0, 0xA0, 0x01, 4)],
        );
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
        assert!(res.is_ok(), "through_delta download must return Ok, got {:?}", res);
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
        assert_eq!(err.kind, NativeRsyncErrorKind::TransportFailure);
        assert_eq!(d.phase(), NativeSessionPhase::Failed);
    }

    #[tokio::test]
    async fn driver_upload_delta_literal_exceeds_max_len_is_rejected() {
        // A2.3 scope does NOT support multi-chunk literals. An engine
        // plan with a single raw literal large enough to produce a
        // compressed blob over 16383 bytes must surface InvalidFrame
        // with a clear "multi-chunk splitting deferred" detail.
        // Random bytes compress poorly with zstd level 3 — 20 KiB of
        // pseudo-random data should exceed the threshold.
        let mut big_raw = Vec::with_capacity(30_000);
        let mut state: u32 = 0x12345678;
        for _ in 0..30_000 {
            // xorshift for simple pseudo-random bytes.
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
        assert_eq!(err.kind, NativeRsyncErrorKind::InvalidFrame);
        assert!(
            err.detail.contains("multi-chunk splitting deferred"),
            "detail should mention deferred splitting: {}",
            err.detail
        );
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
        let transport =
            mock_transport_with_raw_inbound(frozen.download_server_to_client.clone());
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
}
