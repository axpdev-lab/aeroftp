//! Tests for the Strada C native rsync prototype.
//!
//! These tests are local, deterministic, and do not touch real SSH. They cover:
//!   - protocol envelope round-trip and validation
//!   - remote command shape vs golden fixture
//!   - planner determinism
//!   - session state machine legal/illegal transitions
//!   - mock transport replay of upload and download phases
//!   - mock transport failure injection
//!
//! Run with: `cargo test --features proto_native_rsync rsync_native_proto`.

#![cfg(test)]

use crate::rsync_native_proto::driver::{DownloadPlan, SessionDriver, UploadPlan};
use crate::rsync_native_proto::engine_adapter::{
    engine_ops_to_wire, CurrentDeltaSyncBridge, DeltaEngineAdapter,
    DeltaInstructionConversionError, EngineDeltaOp, EngineSignatureBlock,
};
use crate::rsync_native_proto::events::{
    classify_oob_frame, BailingSink, EventSink, NativeRsyncEvent,
};
use crate::rsync_native_proto::fixtures::{
    BaselineCounters, RealRsyncBaselineByteTranscript, RealRsyncTranscriptPaths,
    BASELINE_LITERAL_BYTES, BASELINE_MATCHED_BYTES, DOWNLOAD_REMOTE_COMMAND,
    REAL_RSYNC_FROZEN_TRANSCRIPT_REL, REAL_RSYNC_LANE_PORT, UPLOAD_REMOTE_COMMAND,
};
use crate::rsync_native_proto::mock::{
    MockRemoteShellTransport, MockTransportConfig, OpenStreamBehavior, ReadExhaustedBehavior,
};
use crate::rsync_native_proto::planner::{TransferCandidate, TransferPlanner};
use crate::rsync_native_proto::protocol::{
    DeltaInstruction, ErrorMessage, FileMetadataMessage, FrameCodec, HelloMessage,
    NativeFrameCodec, SignatureBatchMessage, SignatureBlock, SummaryMessage, WireMessage,
    ENVELOPE_VERSION, FRAME_HEADER_SIZE, FRAME_MAGIC,
};
use crate::rsync_native_proto::real_wire::{
    compress_zstd_literal_stream, decode_client_preamble, decode_delta_stream,
    decode_file_list_entry, decode_item_flags, decode_ndx, decode_server_preamble,
    decode_sum_block, decode_sum_head, decode_summary_frame, decompress_zstd_literal_stream,
    encode_client_preamble, encode_delta_stream, encode_file_list_entry, encode_server_preamble,
    encode_sum_block, reassemble_msg_data, reassemble_until_terminal, reassemble_with_events,
    DeltaOp, FileListDecodeOptions, FileListDecodeOutcome, MuxDemuxer, MuxHeader, MuxTag, NdxState,
    NDX_DONE, NDX_FLIST_EOF,
};
use crate::rsync_native_proto::remote_command::RemoteCommandSpec;
use crate::rsync_native_proto::session::{NativeRsyncSession, SessionState};
use crate::rsync_native_proto::transport::CancelHandle;
use crate::rsync_native_proto::transport::RemoteShellTransport;
use crate::rsync_native_proto::types::{
    FeatureFlag, FileEntry, NativeRsyncConfig, NativeRsyncErrorKind, ProtocolVersion, SessionRole,
    TransferStrategy,
};

// ---------------------------------------------------------------------------
// types.rs
// ---------------------------------------------------------------------------

#[test]
fn protocol_version_constant_is_pinned() {
    assert_eq!(ProtocolVersion::CURRENT.as_u32(), 31);
    assert!(ProtocolVersion::CURRENT.is_supported());
    assert!(!ProtocolVersion(30).is_supported());
    assert!(!ProtocolVersion(32).is_supported());
}

// ---------------------------------------------------------------------------
// protocol.rs
// ---------------------------------------------------------------------------

fn sample_hello() -> WireMessage {
    WireMessage::Hello(HelloMessage {
        protocol: ProtocolVersion::CURRENT,
        role: SessionRole::Sender,
        features: vec![FeatureFlag::PreserveTimes, FeatureFlag::DeltaTransfer],
    })
}

#[test]
fn frame_codec_round_trip_hello() {
    let codec = NativeFrameCodec::new(64 * 1024);
    let msg = sample_hello();
    let encoded = codec.encode(&msg).unwrap();
    assert_eq!(&encoded[0..4], &FRAME_MAGIC);
    assert_eq!(encoded[4], ENVELOPE_VERSION);
    assert!(encoded.len() > FRAME_HEADER_SIZE);
    let decoded = codec.decode(&encoded).unwrap();
    assert_eq!(decoded, msg);
}

#[test]
fn frame_codec_round_trip_summary() {
    let codec = NativeFrameCodec::new(64 * 1024);
    let msg = WireMessage::Summary(SummaryMessage {
        bytes_sent: 156_561,
        bytes_received: 17_417,
        literal_bytes: 156_384,
        matched_bytes: 8_232_224,
    });
    let encoded = codec.encode(&msg).unwrap();
    let decoded = codec.decode(&encoded).unwrap();
    assert_eq!(decoded, msg);
}

#[test]
fn frame_codec_rejects_bad_magic() {
    let codec = NativeFrameCodec::new(64 * 1024);
    let mut encoded = codec.encode(&sample_hello()).unwrap();
    encoded[0] = b'X';
    let err = codec.decode(&encoded).unwrap_err();
    assert_eq!(err.kind, NativeRsyncErrorKind::InvalidFrame);
}

#[test]
fn frame_codec_rejects_unknown_envelope_version() {
    let codec = NativeFrameCodec::new(64 * 1024);
    let mut encoded = codec.encode(&sample_hello()).unwrap();
    encoded[4] = 99;
    let err = codec.decode(&encoded).unwrap_err();
    assert_eq!(err.kind, NativeRsyncErrorKind::UnsupportedVersion);
}

#[test]
fn frame_codec_rejects_truncated_payload() {
    let codec = NativeFrameCodec::new(64 * 1024);
    let encoded = codec.encode(&sample_hello()).unwrap();
    let truncated = &encoded[..encoded.len() - 2];
    let err = codec.decode(truncated).unwrap_err();
    assert_eq!(err.kind, NativeRsyncErrorKind::InvalidFrame);
}

#[test]
fn frame_codec_rejects_oversized_frame_on_decode() {
    // Encode with a generous limit, then decode with a small limit.
    let wide = NativeFrameCodec::new(64 * 1024);
    let narrow = NativeFrameCodec::new(FRAME_HEADER_SIZE + 4);
    let encoded = wide.encode(&sample_hello()).unwrap();
    let err = narrow.decode(&encoded).unwrap_err();
    assert_eq!(err.kind, NativeRsyncErrorKind::InvalidFrame);
}

#[test]
fn frame_codec_rejects_oversized_frame_on_encode() {
    let narrow = NativeFrameCodec::new(FRAME_HEADER_SIZE + 4);
    let err = narrow.encode(&sample_hello()).unwrap_err();
    assert_eq!(err.kind, NativeRsyncErrorKind::InvalidFrame);
}

// ---------------------------------------------------------------------------
// remote_command.rs — golden fixture parity
// ---------------------------------------------------------------------------

#[test]
fn upload_remote_command_matches_capture() {
    let spec = RemoteCommandSpec::upload("/workspace/upload/target.bin");
    assert_eq!(spec.remote_role, SessionRole::Receiver);
    assert!(spec.emit_stats);
    assert_eq!(spec.to_command_line(), UPLOAD_REMOTE_COMMAND);
}

#[test]
fn download_remote_command_matches_capture() {
    let spec = RemoteCommandSpec::download("/workspace/download/target.bin");
    assert_eq!(spec.remote_role, SessionRole::Sender);
    assert!(!spec.emit_stats);
    assert_eq!(spec.to_command_line(), DOWNLOAD_REMOTE_COMMAND);
}

#[test]
fn sender_receiver_split_is_explicit_in_args() {
    let up = RemoteCommandSpec::upload("/t").to_args();
    let dn = RemoteCommandSpec::download("/t").to_args();
    assert!(up.iter().all(|a| a != "--sender"));
    assert!(dn.iter().any(|a| a == "--sender"));
}

// ---------------------------------------------------------------------------
// fixtures.rs — baseline invariants
// ---------------------------------------------------------------------------

#[test]
fn baseline_counters_invariants_hold() {
    let up = BaselineCounters::observed_upload();
    let dn = BaselineCounters::observed_download();
    assert!(up.invariants_hold());
    assert!(dn.invariants_hold());
    // Upload and download should disagree on directional counters:
    assert_ne!(up.bytes_sent, dn.bytes_sent);
    assert_ne!(up.bytes_received, dn.bytes_received);
    // But the delta split (literal/matched) should match.
    assert_eq!(up.literal_bytes, dn.literal_bytes);
    assert_eq!(up.matched_bytes, dn.matched_bytes);
    assert_eq!(up.total_file_size, dn.total_file_size);
}

// ---------------------------------------------------------------------------
// planner.rs
// ---------------------------------------------------------------------------

fn regular_entry(path: &str, size: u64) -> FileEntry {
    FileEntry {
        path: path.to_string(),
        size,
        mode: 0o644,
        modified_unix_secs: 0,
        is_dir: false,
    }
}

#[test]
fn planner_prefers_full_copy_below_threshold() {
    let planner = TransferPlanner::new(NativeRsyncConfig::default());
    let candidate = TransferCandidate {
        local: Some(regular_entry("small.bin", 512)),
        remote: Some(regular_entry("small.bin", 512)),
        role: SessionRole::Sender,
    };
    let decision = planner.decide(&candidate);
    assert_eq!(decision.strategy, TransferStrategy::FullCopy);
}

#[test]
fn planner_prefers_delta_for_same_size_large_files() {
    let planner = TransferPlanner::new(NativeRsyncConfig::default());
    let candidate = TransferCandidate {
        local: Some(regular_entry("large.bin", 8 * 1024 * 1024)),
        remote: Some(regular_entry("large.bin", 8 * 1024 * 1024)),
        role: SessionRole::Sender,
    };
    let decision = planner.decide(&candidate);
    assert_eq!(decision.strategy, TransferStrategy::Delta);
    assert!(decision.requires_remote_signatures);
    assert!(decision.block_size.is_some());
}

#[test]
fn planner_is_deterministic_for_identical_input() {
    let planner = TransferPlanner::new(NativeRsyncConfig::default());
    let candidate = TransferCandidate {
        local: Some(regular_entry("deterministic.bin", 4 * 1024 * 1024)),
        remote: Some(regular_entry("deterministic.bin", 4 * 1024 * 1024)),
        role: SessionRole::Sender,
    };
    let a = planner.decide(&candidate);
    let b = planner.decide(&candidate);
    assert_eq!(a.strategy, b.strategy);
    assert_eq!(a.block_size, b.block_size);
    assert_eq!(a.requires_remote_signatures, b.requires_remote_signatures);
}

#[test]
fn planner_skips_directory_candidates() {
    let planner = TransferPlanner::new(NativeRsyncConfig::default());
    let mut dir = regular_entry("tree", 0);
    dir.is_dir = true;
    let candidate = TransferCandidate {
        local: Some(dir.clone()),
        remote: Some(dir),
        role: SessionRole::Sender,
    };
    let decision = planner.decide(&candidate);
    assert_eq!(decision.strategy, TransferStrategy::Skip);
}

// ---------------------------------------------------------------------------
// session.rs — legal/illegal transitions
// ---------------------------------------------------------------------------

fn fresh_session() -> NativeRsyncSession<MockRemoteShellTransport> {
    let transport = MockRemoteShellTransport::new(MockTransportConfig::healthy_upload());
    NativeRsyncSession::new(transport, NativeRsyncConfig::default())
}

#[test]
fn session_legal_forward_path() {
    let mut s = fresh_session();
    assert_eq!(s.state, SessionState::Created);
    s.transition_to(SessionState::Probed).unwrap();
    s.transition_to(SessionState::Negotiated).unwrap();
    s.transition_to(SessionState::FileListPrepared).unwrap();
    s.transition_to(SessionState::Transferring).unwrap();
    s.transition_to(SessionState::Finalized).unwrap();
    assert!(s.state.is_terminal());
}

#[test]
fn session_mark_negotiated_enforces_version_and_stores_role() {
    let mut s = fresh_session();
    s.transition_to(SessionState::Probed).unwrap();
    let hello = HelloMessage {
        protocol: ProtocolVersion::CURRENT,
        role: SessionRole::Receiver,
        features: vec![FeatureFlag::DeltaTransfer],
    };
    s.mark_negotiated(&hello, "rsync  version 3.2.7  protocol version 31".into())
        .unwrap();
    assert_eq!(s.state, SessionState::Negotiated);
    let neg = s.negotiated.expect("negotiated");
    assert_eq!(neg.protocol, ProtocolVersion::CURRENT);
    assert_eq!(neg.role, SessionRole::Receiver);
}

#[test]
fn session_rejects_skipping_phases() {
    let mut s = fresh_session();
    let err = s.transition_to(SessionState::Transferring).unwrap_err();
    assert_eq!(err.kind, NativeRsyncErrorKind::IllegalStateTransition);
}

#[test]
fn session_rejects_moves_after_terminal() {
    let mut s = fresh_session();
    s.cancel();
    assert_eq!(s.state, SessionState::Cancelled);
    let err = s.transition_to(SessionState::Probed).unwrap_err();
    assert_eq!(err.kind, NativeRsyncErrorKind::IllegalStateTransition);
}

#[test]
fn session_rejects_unsupported_remote_version() {
    let mut s = fresh_session();
    s.transition_to(SessionState::Probed).unwrap();
    let hello = HelloMessage {
        protocol: ProtocolVersion(30),
        role: SessionRole::Sender,
        features: vec![],
    };
    let err = s
        .mark_negotiated(&hello, "rsync old".to_string())
        .unwrap_err();
    assert_eq!(err.kind, NativeRsyncErrorKind::UnsupportedVersion);
    // Session stays non-terminal so the caller can decide to fail it.
    assert_eq!(s.state, SessionState::Probed);
}

#[test]
fn session_accumulates_stats_safely() {
    let mut s = fresh_session();
    s.record_sent(10);
    s.record_received(20);
    s.record_literal(5);
    s.record_matched(15);
    assert_eq!(s.stats.bytes_sent, 10);
    assert_eq!(s.stats.bytes_received, 20);
    assert_eq!(s.stats.literal_bytes, 5);
    assert_eq!(s.stats.matched_bytes, 15);
}

// ---------------------------------------------------------------------------
// mock.rs — upload / download / failure replays
// ---------------------------------------------------------------------------

fn encoded_hello(role: SessionRole) -> Vec<u8> {
    let codec = NativeFrameCodec::new(64 * 1024);
    codec
        .encode(&WireMessage::Hello(HelloMessage {
            protocol: ProtocolVersion::CURRENT,
            role,
            features: vec![],
        }))
        .unwrap()
}

fn encoded_summary(up: bool) -> Vec<u8> {
    let codec = NativeFrameCodec::new(64 * 1024);
    let counters = if up {
        BaselineCounters::observed_upload()
    } else {
        BaselineCounters::observed_download()
    };
    codec
        .encode(&WireMessage::Summary(SummaryMessage {
            bytes_sent: counters.bytes_sent,
            bytes_received: counters.bytes_received,
            literal_bytes: counters.literal_bytes,
            matched_bytes: counters.matched_bytes,
        }))
        .unwrap()
}

#[tokio::test]
async fn mock_replays_upload_phase_and_records_outbound() {
    let mut config = MockTransportConfig::healthy_upload();
    config.stream_behavior = OpenStreamBehavior::Success {
        // Remote is Receiver on upload.
        inbound: vec![encoded_hello(SessionRole::Receiver), encoded_summary(true)],
    };
    let transport = MockRemoteShellTransport::new(config);

    let probe = transport.probe().await.unwrap();
    assert!(probe.supports_remote_shell);
    assert_eq!(probe.protocol, ProtocolVersion::CURRENT);

    let exec = RemoteCommandSpec::upload("/workspace/upload/target.bin").to_exec_request();
    let mut stream = transport.open_stream(exec.clone()).await.unwrap();

    // Local writes its Hello first (Sender on upload).
    use crate::rsync_native_proto::transport::BidirectionalByteStream;
    stream
        .write_frame(&encoded_hello(SessionRole::Sender))
        .await
        .unwrap();

    // Read remote Hello then Summary.
    let codec = NativeFrameCodec::new(64 * 1024);
    let remote_hello = codec.decode(&stream.read_frame().await.unwrap()).unwrap();
    match remote_hello {
        WireMessage::Hello(h) => assert_eq!(h.role, SessionRole::Receiver),
        other => panic!("expected Hello, got {other:?}"),
    }
    let remote_summary = codec.decode(&stream.read_frame().await.unwrap()).unwrap();
    if let WireMessage::Summary(s) = remote_summary {
        assert_eq!(s.literal_bytes, 156_384);
        assert_eq!(s.matched_bytes, 8_232_224);
    } else {
        panic!("expected Summary");
    }

    stream.shutdown().await.unwrap();
    assert!(transport.shutdown_was_called());
    assert_eq!(transport.captured_outbound().len(), 1);
    assert_eq!(transport.last_exec_request().unwrap(), exec);
}

#[tokio::test]
async fn mock_replays_download_phase() {
    let mut config = MockTransportConfig::healthy_download();
    config.stream_behavior = OpenStreamBehavior::Success {
        // Remote is Sender on download.
        inbound: vec![encoded_hello(SessionRole::Sender), encoded_summary(false)],
    };
    let transport = MockRemoteShellTransport::new(config);
    let exec = RemoteCommandSpec::download("/workspace/download/target.bin").to_exec_request();
    let mut stream = transport.open_stream(exec.clone()).await.unwrap();

    use crate::rsync_native_proto::transport::BidirectionalByteStream;
    stream
        .write_frame(&encoded_hello(SessionRole::Receiver))
        .await
        .unwrap();

    let codec = NativeFrameCodec::new(64 * 1024);
    let remote_hello = codec.decode(&stream.read_frame().await.unwrap()).unwrap();
    if let WireMessage::Hello(h) = remote_hello {
        assert_eq!(h.role, SessionRole::Sender);
    } else {
        panic!("expected Hello");
    }
    let remote_summary = codec.decode(&stream.read_frame().await.unwrap()).unwrap();
    if let WireMessage::Summary(s) = remote_summary {
        assert_eq!(s.bytes_sent, 17_425);
        assert_eq!(s.bytes_received, 156_554);
    } else {
        panic!("expected Summary");
    }

    // Asserting the remote command args match the captured shape:
    let args = transport.last_exec_request().unwrap().args;
    assert!(args.iter().any(|a| a == "--sender"));
}

#[tokio::test]
async fn mock_simulates_stream_open_failure() {
    let transport = MockRemoteShellTransport::new(MockTransportConfig::stream_open_fails());
    let exec = RemoteCommandSpec::upload("/workspace/upload/target.bin").to_exec_request();
    let err = transport.open_stream(exec).await.unwrap_err();
    assert_eq!(err.kind, NativeRsyncErrorKind::TransportFailure);
}

#[tokio::test]
async fn mock_simulates_remote_close_on_read() {
    let mut config = MockTransportConfig::healthy_upload();
    config.stream_behavior = OpenStreamBehavior::Success { inbound: vec![] };
    config.read_exhausted = ReadExhaustedBehavior::Error;
    let transport = MockRemoteShellTransport::new(config);
    let exec = RemoteCommandSpec::upload("/t").to_exec_request();
    let mut stream = transport.open_stream(exec).await.unwrap();
    use crate::rsync_native_proto::transport::BidirectionalByteStream;
    let err = stream.read_frame().await.unwrap_err();
    assert_eq!(err.kind, NativeRsyncErrorKind::TransportFailure);
}

#[tokio::test]
async fn mock_cancel_propagates() {
    let transport = MockRemoteShellTransport::new(MockTransportConfig::healthy_upload());
    assert!(!transport.cancel_was_called());
    transport.cancel().await.unwrap();
    assert!(transport.cancel_was_called());
}

// ---------------------------------------------------------------------------
// engine_adapter.rs — From / TryFrom between protocol and engine shapes
// ---------------------------------------------------------------------------

#[test]
fn signature_block_from_protocol_round_trips_all_fields() {
    let mut strong = [0u8; 32];
    for (i, byte) in strong.iter_mut().enumerate() {
        *byte = i as u8;
    }
    let wire = SignatureBlock {
        index: 42,
        rolling: 0xDEAD_BEEF,
        strong,
        block_len: 8192,
    };
    let engine: EngineSignatureBlock = wire.clone().into();
    assert_eq!(engine.index, wire.index);
    assert_eq!(engine.rolling, wire.rolling);
    assert_eq!(engine.strong, wire.strong);
    assert_eq!(engine.block_len, wire.block_len);
}

#[test]
fn delta_instruction_copy_block_becomes_engine_copy_block() {
    let wire = DeltaInstruction::CopyBlock { index: 7 };
    let engine = EngineDeltaOp::try_from(wire).expect("CopyBlock is always convertible");
    match engine {
        EngineDeltaOp::CopyBlock(index) => assert_eq!(index, 7),
        other => panic!("expected CopyBlock(7), got {other:?}"),
    }
}

#[test]
fn delta_instruction_literal_becomes_engine_literal() {
    let payload = b"hello-delta".to_vec();
    let wire = DeltaInstruction::Literal {
        data: payload.clone(),
    };
    let engine = EngineDeltaOp::try_from(wire).expect("Literal is always convertible");
    match engine {
        EngineDeltaOp::Literal(data) => assert_eq!(data, payload),
        other => panic!("expected Literal payload, got {other:?}"),
    }
}

#[test]
fn delta_instruction_end_of_file_rejects_with_typed_error() {
    let err = EngineDeltaOp::try_from(DeltaInstruction::EndOfFile).unwrap_err();
    assert_eq!(
        err,
        DeltaInstructionConversionError::EndOfFileIsFramingMarker
    );
    // Display is meaningful and stable enough to inline in logs:
    let rendered = format!("{err}");
    assert!(rendered.contains("framing marker"));
}

// ---------------------------------------------------------------------------
// driver.rs — end-to-end session orchestration against the mock transport
// ---------------------------------------------------------------------------

fn driver_codec() -> NativeFrameCodec {
    NativeFrameCodec::new(64 * 1024)
}

fn encode(msg: &WireMessage) -> Vec<u8> {
    driver_codec().encode(msg).unwrap()
}

fn sample_file_metadata() -> FileMetadataMessage {
    FileMetadataMessage {
        path: "target.bin".to_string(),
        size: 8_388_608,
        mode: 0o644,
        modified_unix_secs: 1_713_345_600, // stable fixture timestamp
    }
}

const SAMPLE_BLOCK_SIZE: u32 = 1024;

fn sample_signature_batch() -> Vec<SignatureBlock> {
    (0..4)
        .map(|i| SignatureBlock {
            index: i,
            rolling: 0xCAFE_0000 + i,
            strong: [i as u8; 32],
            block_len: SAMPLE_BLOCK_SIZE,
        })
        .collect()
}

fn sample_signature_batch_message() -> SignatureBatchMessage {
    SignatureBatchMessage {
        block_size: SAMPLE_BLOCK_SIZE,
        blocks: sample_signature_batch(),
    }
}

fn sample_upload_plan() -> UploadPlan {
    UploadPlan {
        file_meta: sample_file_metadata(),
        delta_instructions: vec![
            DeltaInstruction::CopyBlock { index: 0 },
            DeltaInstruction::Literal {
                data: b"hello world".to_vec(),
            },
            DeltaInstruction::CopyBlock { index: 2 },
            DeltaInstruction::EndOfFile,
        ],
    }
}

fn baseline_summary_frame(upload: bool) -> WireMessage {
    let counters = if upload {
        BaselineCounters::observed_upload()
    } else {
        BaselineCounters::observed_download()
    };
    WireMessage::Summary(SummaryMessage {
        bytes_sent: counters.bytes_sent,
        bytes_received: counters.bytes_received,
        literal_bytes: counters.literal_bytes,
        matched_bytes: counters.matched_bytes,
    })
}

fn new_driver(config: MockTransportConfig) -> SessionDriver<MockRemoteShellTransport> {
    let transport = MockRemoteShellTransport::new(config);
    let session = NativeRsyncSession::new(transport, NativeRsyncConfig::default());
    SessionDriver::new(session, driver_codec())
}

// --- happy-path tests --------------------------------------------------------

#[tokio::test]
async fn driver_upload_happy_path_reaches_finalized_and_matches_baseline() {
    // Remote emits: Hello(Receiver), SignatureBatch, Summary (upload direction).
    let inbound = vec![
        encode(&WireMessage::Hello(HelloMessage {
            protocol: ProtocolVersion::CURRENT,
            role: SessionRole::Receiver,
            features: vec![FeatureFlag::DeltaTransfer],
        })),
        encode(&WireMessage::SignatureBatch(
            sample_signature_batch_message(),
        )),
        encode(&baseline_summary_frame(true)),
    ];
    let mut cfg = MockTransportConfig::healthy_upload();
    cfg.stream_behavior = OpenStreamBehavior::Success { inbound };

    let mut driver = new_driver(cfg);
    let outcome = driver
        .drive_upload(
            RemoteCommandSpec::upload("/workspace/upload/target.bin"),
            sample_upload_plan(),
        )
        .await
        .expect("drive_upload succeeds");

    assert_eq!(outcome.final_state, SessionState::Finalized);
    // Summary counters flow into stats and match the real-wrapper baseline:
    assert_eq!(outcome.stats.literal_bytes, BASELINE_LITERAL_BYTES);
    assert_eq!(outcome.stats.matched_bytes, BASELINE_MATCHED_BYTES);
    // Wire-level byte accounting is non-zero (not the rsync numbers — our
    // envelope differs from rsync's — but both directions saw traffic).
    assert!(outcome.stats.bytes_sent > 0);
    assert!(outcome.stats.bytes_received > 0);
    // Sinergia-2 conversions happened: remote signatures arrived as engine sigs,
    // local deltas were converted, EndOfFile was drained (not present in ops).
    assert_eq!(outcome.engine_signatures.len(), 4);
    assert_eq!(outcome.engine_delta_ops.len(), 3);
    // Exec request carried the capture-parity upload command.
    let exec = driver.session.transport.last_exec_request().unwrap();
    assert!(exec.args.iter().any(|a| a == "--stats"));
    assert!(exec.args.iter().all(|a| a != "--sender"));
}

#[tokio::test]
async fn driver_download_happy_path_reaches_finalized_and_matches_baseline() {
    // Remote emits: Hello(Sender), FileMetadata, DeltaBatch, Summary (download).
    let delta_batch = vec![
        DeltaInstruction::CopyBlock { index: 0 },
        DeltaInstruction::Literal {
            data: b"patch".to_vec(),
        },
        DeltaInstruction::EndOfFile,
    ];
    let inbound = vec![
        encode(&WireMessage::Hello(HelloMessage {
            protocol: ProtocolVersion::CURRENT,
            role: SessionRole::Sender,
            features: vec![FeatureFlag::DeltaTransfer],
        })),
        encode(&WireMessage::FileMetadata(sample_file_metadata())),
        encode(&WireMessage::DeltaBatch(delta_batch)),
        encode(&baseline_summary_frame(false)),
    ];
    let mut cfg = MockTransportConfig::healthy_download();
    cfg.stream_behavior = OpenStreamBehavior::Success { inbound };

    let mut driver = new_driver(cfg);
    let plan = DownloadPlan {
        block_size: SAMPLE_BLOCK_SIZE,
        basis_signatures: sample_signature_batch(),
    };
    let outcome = driver
        .drive_download(
            RemoteCommandSpec::download("/workspace/download/target.bin"),
            plan,
        )
        .await
        .expect("drive_download succeeds");

    assert_eq!(outcome.final_state, SessionState::Finalized);
    assert_eq!(outcome.stats.literal_bytes, BASELINE_LITERAL_BYTES);
    assert_eq!(outcome.stats.matched_bytes, BASELINE_MATCHED_BYTES);
    assert_eq!(outcome.engine_signatures.len(), 4);
    // Remote delta had 2 ops + EndOfFile → 2 engine ops.
    assert_eq!(outcome.engine_delta_ops.len(), 2);
    let exec = driver.session.transport.last_exec_request().unwrap();
    assert!(exec.args.iter().any(|a| a == "--sender"));
}

// --- failure tests -----------------------------------------------------------

#[tokio::test]
async fn driver_rejects_unsupported_version_in_remote_hello() {
    let inbound = vec![
        // Remote lies about protocol version (30 is below MIN_SUPPORTED = 31).
        encode(&WireMessage::Hello(HelloMessage {
            protocol: ProtocolVersion(30),
            role: SessionRole::Receiver,
            features: vec![],
        })),
    ];
    let mut cfg = MockTransportConfig::healthy_upload();
    cfg.stream_behavior = OpenStreamBehavior::Success { inbound };

    let mut driver = new_driver(cfg);
    let err = driver
        .drive_upload(RemoteCommandSpec::upload("/t"), sample_upload_plan())
        .await
        .unwrap_err();
    assert_eq!(err.kind, NativeRsyncErrorKind::UnsupportedVersion);
    assert_eq!(driver.session.state, SessionState::Failed);
}

#[tokio::test]
async fn driver_rejects_role_mismatch_in_remote_hello() {
    // Upload expects remote Receiver. Remote lies and says Sender.
    let inbound = vec![encode(&WireMessage::Hello(HelloMessage {
        protocol: ProtocolVersion::CURRENT,
        role: SessionRole::Sender,
        features: vec![],
    }))];
    let mut cfg = MockTransportConfig::healthy_upload();
    cfg.stream_behavior = OpenStreamBehavior::Success { inbound };

    let mut driver = new_driver(cfg);
    let err = driver
        .drive_upload(RemoteCommandSpec::upload("/t"), sample_upload_plan())
        .await
        .unwrap_err();
    assert_eq!(err.kind, NativeRsyncErrorKind::NegotiationFailed);
    assert_eq!(driver.session.state, SessionState::Failed);
}

#[tokio::test]
async fn driver_surfaces_remote_error_frame_as_typed_error() {
    let inbound = vec![
        encode(&WireMessage::Hello(HelloMessage {
            protocol: ProtocolVersion::CURRENT,
            role: SessionRole::Receiver,
            features: vec![],
        })),
        encode(&WireMessage::Error(ErrorMessage {
            code: 23,
            message: "partial transfer due to vanished source files".to_string(),
        })),
    ];
    let mut cfg = MockTransportConfig::healthy_upload();
    cfg.stream_behavior = OpenStreamBehavior::Success { inbound };

    let mut driver = new_driver(cfg);
    let err = driver
        .drive_upload(RemoteCommandSpec::upload("/t"), sample_upload_plan())
        .await
        .unwrap_err();
    assert_eq!(err.kind, NativeRsyncErrorKind::RemoteError);
    assert!(err.detail.contains("23"));
    assert_eq!(driver.session.state, SessionState::Failed);
}

#[tokio::test]
async fn driver_detects_unexpected_message_type() {
    // Remote sends Summary before Hello — violates the phase contract.
    let inbound = vec![encode(&baseline_summary_frame(true))];
    let mut cfg = MockTransportConfig::healthy_upload();
    cfg.stream_behavior = OpenStreamBehavior::Success { inbound };

    let mut driver = new_driver(cfg);
    let err = driver
        .drive_upload(RemoteCommandSpec::upload("/t"), sample_upload_plan())
        .await
        .unwrap_err();
    assert_eq!(err.kind, NativeRsyncErrorKind::UnexpectedMessage);
    assert_eq!(driver.session.state, SessionState::Failed);
}

#[tokio::test]
async fn driver_handles_remote_close_before_summary() {
    // Hello + SignatureBatch, then stream exhausted with `Error` behavior.
    let inbound = vec![
        encode(&WireMessage::Hello(HelloMessage {
            protocol: ProtocolVersion::CURRENT,
            role: SessionRole::Receiver,
            features: vec![],
        })),
        encode(&WireMessage::SignatureBatch(
            sample_signature_batch_message(),
        )),
    ];
    let mut cfg = MockTransportConfig::healthy_upload();
    cfg.stream_behavior = OpenStreamBehavior::Success { inbound };
    cfg.read_exhausted = ReadExhaustedBehavior::Error;

    let mut driver = new_driver(cfg);
    let err = driver
        .drive_upload(RemoteCommandSpec::upload("/t"), sample_upload_plan())
        .await
        .unwrap_err();
    assert_eq!(err.kind, NativeRsyncErrorKind::TransportFailure);
    assert_eq!(driver.session.state, SessionState::Failed);
}

#[tokio::test]
async fn driver_cancel_before_start_produces_cancelled_without_io() {
    let mut driver = new_driver(MockTransportConfig::healthy_upload());
    driver.cancel().await.unwrap();
    assert_eq!(driver.session.state, SessionState::Cancelled);
    assert!(driver.session.transport.cancel_was_called());

    // Subsequent drive call must not perform any I/O and must leave the
    // session in its terminal Cancelled state.
    let outcome = driver
        .drive_upload(RemoteCommandSpec::upload("/t"), sample_upload_plan())
        .await
        .expect("cancelled driver returns Ok with terminal outcome");
    assert_eq!(outcome.final_state, SessionState::Cancelled);
    // No stream was ever opened, so outbound is empty.
    assert!(driver.session.transport.captured_outbound().is_empty());
}

#[tokio::test]
async fn driver_rejects_upload_plan_without_end_of_file_before_any_io() {
    let mut driver = new_driver(MockTransportConfig::healthy_upload());
    let bad_plan = UploadPlan {
        file_meta: sample_file_metadata(),
        delta_instructions: vec![
            DeltaInstruction::CopyBlock { index: 0 },
            // missing EndOfFile
        ],
    };
    let err = driver
        .drive_upload(RemoteCommandSpec::upload("/t"), bad_plan)
        .await
        .unwrap_err();
    assert_eq!(err.kind, NativeRsyncErrorKind::InvalidFrame);
    // No transport I/O was attempted: open_stream was never called.
    assert!(driver.session.transport.last_exec_request().is_none());
    // Session untouched by validation-only failure (still Created).
    assert_eq!(driver.session.state, SessionState::Created);
}

#[tokio::test]
async fn driver_rejects_delta_after_end_of_file_marker() {
    // Ill-formed download-side delta batch: CopyBlock AFTER EndOfFile.
    let bad_delta = vec![
        DeltaInstruction::EndOfFile,
        DeltaInstruction::CopyBlock { index: 0 },
    ];
    let inbound = vec![
        encode(&WireMessage::Hello(HelloMessage {
            protocol: ProtocolVersion::CURRENT,
            role: SessionRole::Sender,
            features: vec![],
        })),
        encode(&WireMessage::FileMetadata(sample_file_metadata())),
        encode(&WireMessage::DeltaBatch(bad_delta)),
        encode(&baseline_summary_frame(false)),
    ];
    let mut cfg = MockTransportConfig::healthy_download();
    cfg.stream_behavior = OpenStreamBehavior::Success { inbound };

    let mut driver = new_driver(cfg);
    let plan = DownloadPlan {
        block_size: SAMPLE_BLOCK_SIZE,
        basis_signatures: sample_signature_batch(),
    };
    let err = driver
        .drive_download(RemoteCommandSpec::download("/t"), plan)
        .await
        .unwrap_err();
    assert_eq!(err.kind, NativeRsyncErrorKind::InvalidFrame);
    assert_eq!(driver.session.state, SessionState::Failed);
}

#[tokio::test]
async fn driver_stream_open_failure_marks_session_failed() {
    let mut driver = new_driver(MockTransportConfig::stream_open_fails());
    let err = driver
        .drive_upload(RemoteCommandSpec::upload("/t"), sample_upload_plan())
        .await
        .unwrap_err();
    assert_eq!(err.kind, NativeRsyncErrorKind::TransportFailure);
    assert_eq!(driver.session.state, SessionState::Failed);
}

// ---------------------------------------------------------------------------
// engine_adapter.rs — Sinergia 4: CurrentDeltaSyncBridge delegates to
// crate::delta_sync (the production delta engine). These tests pin the
// algorithmic invariants that the real engine provides — we are NOT asserting
// against hardcoded rsync numbers, since rsync's block size heuristic and
// block-matching priority differ in detail. We assert the invariants every
// correct delta engine must satisfy.
// ---------------------------------------------------------------------------

/// Deterministic 8 MiB buffer. Seeded polynomial so every test gets the same
/// bytes without adding `rand` or hardcoding a large literal.
fn deterministic_buffer(size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(size);
    let mut state: u32 = 0x9E37_79B9; // golden-ratio seed
    for _ in 0..size {
        state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        out.push((state >> 16) as u8);
    }
    out
}

#[test]
fn bridge_compute_block_size_matches_production_engine() {
    let bridge = CurrentDeltaSyncBridge::new();
    let file_size = 8 * 1024 * 1024;
    let bridge_bs = bridge.compute_block_size(file_size);
    let engine_bs = crate::delta_sync::compute_block_size(file_size);
    assert_eq!(bridge_bs, engine_bs);
    // Must fall within the engine's documented clamp [512, 8192].
    assert!((512..=8192).contains(&bridge_bs));
}

#[test]
fn bridge_signatures_cover_whole_buffer() {
    let bridge = CurrentDeltaSyncBridge::new();
    let data = deterministic_buffer(100_000);
    let bs = bridge.compute_block_size(data.len() as u64);
    let sigs = bridge.build_signatures(&data, bs);
    // Reconstructing file_size from block_len sum must equal actual size —
    // this is the invariant the bridge relies on in `compute_delta`.
    let recovered: u64 = sigs.iter().map(|s| s.block_len as u64).sum();
    assert_eq!(recovered, data.len() as u64);
    let expected_blocks = data.len().div_ceil(bs);
    assert_eq!(sigs.len(), expected_blocks);
}

#[test]
fn bridge_identical_file_produces_zero_literal_bytes() {
    let bridge = CurrentDeltaSyncBridge::new();
    let data = deterministic_buffer(1_048_576); // 1 MiB
    let bs = bridge.compute_block_size(data.len() as u64);
    let sigs = bridge.build_signatures(&data, bs);
    let plan = bridge.compute_delta(&data, &sigs, bs);
    assert_eq!(plan.literal_bytes, 0);
    assert!(plan.copy_blocks > 0);
    // All-copy plan should be deemed worth using.
    assert!(plan.should_use_delta);
    assert!(plan.savings_ratio > 0.9);
}

#[test]
fn bridge_fully_changed_file_produces_no_copy_blocks() {
    let bridge = CurrentDeltaSyncBridge::new();
    let original = vec![0u8; 4096];
    let modified = vec![0xFFu8; 4096];
    let bs = bridge.compute_block_size(original.len() as u64);
    let sigs = bridge.build_signatures(&original, bs);
    let plan = bridge.compute_delta(&modified, &sigs, bs);
    assert_eq!(plan.copy_blocks, 0);
    assert_eq!(plan.literal_bytes, modified.len() as u64);
    // A 100%-literal delta is NOT worth using — engine says so.
    assert!(!plan.should_use_delta);
}

#[test]
fn bridge_localized_change_keeps_most_bytes_matched() {
    // 8 MiB source, destination is source with ~156 KiB overwritten in the
    // middle. The algorithmic invariant: literal ≪ file_size and most of the
    // file is represented via CopyBlock ops.
    let bridge = CurrentDeltaSyncBridge::new();
    let destination = deterministic_buffer(8 * 1024 * 1024);
    let mut source = destination.clone();
    let mutation_start = 4 * 1024 * 1024;
    let mutation_len = 156 * 1024; // 156 KiB ≈ BASELINE_LITERAL_BYTES order-of-magnitude
    for byte in &mut source[mutation_start..mutation_start + mutation_len] {
        *byte = byte.wrapping_add(1);
    }

    let bs = bridge.compute_block_size(destination.len() as u64);
    let sigs = bridge.build_signatures(&destination, bs);
    let plan = bridge.compute_delta(&source, &sigs, bs);

    // Invariant 1: the delta is dominated by copies, not literals.
    assert!(plan.copy_blocks > 0, "expected at least one copy block");
    assert!(
        plan.literal_bytes < (source.len() as u64) / 4,
        "localized 2% mutation produced {} literal bytes on a {}-byte file",
        plan.literal_bytes,
        source.len()
    );
    // Invariant 2: savings are substantial (engine says use it).
    assert!(plan.should_use_delta, "engine should recommend delta here");
    assert!(
        plan.savings_ratio > 0.5,
        "savings_ratio too low: {}",
        plan.savings_ratio
    );
    // Invariant 3: reconstruction via the engine's apply_delta produces
    // exactly the source file, byte-for-byte. This is the strongest
    // algorithmic check possible without any SSH.
    let wire_ops: Vec<crate::delta_sync::DeltaOp> = plan
        .ops
        .iter()
        .cloned()
        .map(|op| match op {
            EngineDeltaOp::CopyBlock(i) => crate::delta_sync::DeltaOp::CopyBlock(i),
            EngineDeltaOp::Literal(b) => crate::delta_sync::DeltaOp::Literal(b),
        })
        .collect();
    let reconstructed = crate::delta_sync::apply_delta(&destination, &wire_ops, bs)
        .expect("apply_delta must succeed on bridge-produced ops");
    assert_eq!(
        reconstructed, source,
        "bridge-produced delta did not round-trip to the source file"
    );
}

// --- engine ↔ protocol inverse conversions (Sinergia 4) ------------------

#[test]
fn engine_signature_inverse_conversion_round_trips() {
    let mut strong = [0u8; 32];
    for (i, byte) in strong.iter_mut().enumerate() {
        *byte = i as u8;
    }
    let original = SignatureBlock {
        index: 17,
        rolling: 0x1234_5678,
        strong,
        block_len: 4096,
    };
    let engine: EngineSignatureBlock = original.clone().into();
    let round_tripped: SignatureBlock = engine.into();
    assert_eq!(round_tripped, original);
}

#[test]
fn engine_delta_op_inverse_conversion_is_total() {
    let payload = b"roundtrip".to_vec();
    let copy: DeltaInstruction = EngineDeltaOp::CopyBlock(42).into();
    let literal: DeltaInstruction = EngineDeltaOp::Literal(payload.clone()).into();
    assert_eq!(copy, DeltaInstruction::CopyBlock { index: 42 });
    assert_eq!(literal, DeltaInstruction::Literal { data: payload });
}

#[test]
fn engine_ops_to_wire_appends_end_of_file_terminator() {
    let ops = vec![
        EngineDeltaOp::CopyBlock(0),
        EngineDeltaOp::Literal(b"payload".to_vec()),
    ];
    let wire = engine_ops_to_wire(ops);
    assert_eq!(wire.len(), 3);
    assert!(matches!(wire[0], DeltaInstruction::CopyBlock { index: 0 }));
    assert!(matches!(wire[1], DeltaInstruction::Literal { .. }));
    assert_eq!(wire[2], DeltaInstruction::EndOfFile);
}

#[test]
fn engine_ops_to_wire_on_empty_still_terminates() {
    let wire = engine_ops_to_wire(Vec::new());
    assert_eq!(wire, vec![DeltaInstruction::EndOfFile]);
}

// --- driver + real engine bridge integration ------------------------------

#[tokio::test]
async fn driver_upload_uses_bridge_produced_plan_and_reaches_finalized() {
    // End-to-end integration: the bridge computes signatures and delta against
    // a destination buffer, the caller converts engine ops to a wire-ready
    // UploadPlan, and the driver streams that plan through the mock transport.
    let bridge = CurrentDeltaSyncBridge::new();

    // Produce a destination + source with a localized mutation. Sizes stay
    // small here (32 KiB) so the test runs fast; the algorithmic test above
    // covers the 8 MiB case.
    let destination = deterministic_buffer(32 * 1024);
    let mut source = destination.clone();
    for byte in &mut source[8192..8192 + 256] {
        *byte = byte.wrapping_add(1);
    }
    let bs = bridge.compute_block_size(destination.len() as u64);
    let dest_sigs_engine = bridge.build_signatures(&destination, bs);
    let plan_engine = bridge.compute_delta(&source, &dest_sigs_engine, bs);

    // Caller converts engine → wire: signatures via into(), delta ops via
    // engine_ops_to_wire (which appends the EndOfFile terminator the
    // driver's pre-flight check requires).
    let wire_signatures: Vec<SignatureBlock> = dest_sigs_engine
        .into_iter()
        .map(SignatureBlock::from)
        .collect();
    let upload_plan = UploadPlan {
        file_meta: sample_file_metadata(),
        delta_instructions: engine_ops_to_wire(plan_engine.ops.clone()),
    };

    // Mock remote emits Hello(Receiver), the bridge-produced SignatureBatch
    // (as if the remote had computed them), and a Summary.
    let sig_batch = SignatureBatchMessage {
        block_size: bs as u32,
        blocks: wire_signatures.clone(),
    };
    let inbound = vec![
        encode(&WireMessage::Hello(HelloMessage {
            protocol: ProtocolVersion::CURRENT,
            role: SessionRole::Receiver,
            features: vec![FeatureFlag::DeltaTransfer],
        })),
        encode(&WireMessage::SignatureBatch(sig_batch)),
        encode(&baseline_summary_frame(true)),
    ];
    let mut cfg = MockTransportConfig::healthy_upload();
    cfg.stream_behavior = OpenStreamBehavior::Success { inbound };

    let mut driver = new_driver(cfg);
    let outcome = driver
        .drive_upload(
            RemoteCommandSpec::upload("/workspace/upload/target.bin"),
            upload_plan,
        )
        .await
        .expect("drive_upload with bridge-produced plan succeeds");

    assert_eq!(outcome.final_state, SessionState::Finalized);
    assert_eq!(outcome.stats.literal_bytes, BASELINE_LITERAL_BYTES);
    assert_eq!(outcome.stats.matched_bytes, BASELINE_MATCHED_BYTES);
    // Engine sigs received == engine sigs sent (since we scripted the mock
    // with them).
    assert_eq!(outcome.engine_signatures.len(), wire_signatures.len());
    // Engine ops delivered to the wire round-tripped back through the
    // driver's TryFrom, and the EndOfFile terminator was drained.
    assert_eq!(outcome.engine_delta_ops.len(), plan_engine.ops.len());
    // Plan claimed savings — assert the bridge's own recommendation.
    assert!(plan_engine.should_use_delta);
    assert!(plan_engine.literal_bytes < source.len() as u64);
    // Block size was propagated through the SignatureBatch wire frame.
    assert_eq!(outcome.block_size, bs as u32);
}

// ---------------------------------------------------------------------------
// driver.rs — Sinergia 5: engine-mode driver orchestration.
//
// The driver computes the delta (upload) or applies the delta (download)
// internally by calling the DeltaEngineAdapter. The caller only provides
// the source or destination bytes — no manual signature/delta juggling.
// ---------------------------------------------------------------------------

fn build_scripted_signature_batch(
    adapter: &dyn DeltaEngineAdapter,
    destination: &[u8],
) -> (u32, SignatureBatchMessage) {
    let bs_usize = adapter.compute_block_size(destination.len() as u64);
    let engine_sigs = adapter.build_signatures(destination, bs_usize);
    let blocks: Vec<SignatureBlock> = engine_sigs.into_iter().map(SignatureBlock::from).collect();
    let bs = bs_usize as u32;
    (
        bs,
        SignatureBatchMessage {
            block_size: bs,
            blocks,
        },
    )
}

#[tokio::test]
async fn driver_upload_with_engine_computes_delta_internally() {
    // Upload end-to-end using the engine. The driver: receives remote
    // SignatureBatch, calls bridge.compute_delta internally, sends
    // DeltaBatch, receives Summary. Caller only supplies source bytes.
    let bridge = CurrentDeltaSyncBridge::new();
    let destination = deterministic_buffer(32 * 1024);
    let mut source = destination.clone();
    for byte in &mut source[4096..4096 + 512] {
        *byte = byte.wrapping_add(7);
    }

    // Scripted remote emits sigs computed over the destination.
    let (_bs, sig_batch) = build_scripted_signature_batch(&bridge, &destination);
    let inbound = vec![
        encode(&WireMessage::Hello(HelloMessage {
            protocol: ProtocolVersion::CURRENT,
            role: SessionRole::Receiver,
            features: vec![FeatureFlag::DeltaTransfer],
        })),
        encode(&WireMessage::SignatureBatch(sig_batch)),
        encode(&baseline_summary_frame(true)),
    ];
    let mut cfg = MockTransportConfig::healthy_upload();
    cfg.stream_behavior = OpenStreamBehavior::Success { inbound };

    let mut driver = new_driver(cfg);
    let outcome = driver
        .drive_upload_with_engine(
            RemoteCommandSpec::upload("/workspace/upload/target.bin"),
            sample_file_metadata(),
            source.clone(),
            &bridge,
        )
        .await
        .expect("engine-mode upload succeeds");

    assert_eq!(outcome.final_state, SessionState::Finalized);
    assert_eq!(outcome.stats.literal_bytes, BASELINE_LITERAL_BYTES);
    assert_eq!(outcome.stats.matched_bytes, BASELINE_MATCHED_BYTES);
    assert!(outcome.block_size > 0);
    // Engine produced ops: CopyBlock dominate, some Literal for the mutation.
    assert!(!outcome.engine_delta_ops.is_empty());
    // No reconstruction on upload path.
    assert!(outcome.reconstructed.is_none());
    // Round-trip sanity: applying the engine's own ops to destination
    // recomputes the source. This ties the driver's output to a
    // functioning end-to-end delta pipeline.
    let reconstructed = bridge
        .apply_delta(
            &destination,
            &outcome.engine_delta_ops,
            outcome.block_size as usize,
        )
        .expect("apply_delta succeeds");
    assert_eq!(reconstructed, source);
}

#[tokio::test]
async fn driver_download_with_engine_reconstructs_source_file() {
    // Download end-to-end using the engine. The driver computes signatures
    // from our destination bytes, sends them, receives the scripted delta,
    // and applies it to reconstruct the source.
    let bridge = CurrentDeltaSyncBridge::new();
    let destination = deterministic_buffer(32 * 1024);
    let mut source = destination.clone();
    for byte in &mut source[10_000..10_000 + 384] {
        *byte = byte.wrapping_add(3);
    }

    // Compute what the engine would do on the remote side, then serialise.
    let bs_usize = bridge.compute_block_size(destination.len() as u64);
    let dest_sigs = bridge.build_signatures(&destination, bs_usize);
    let engine_plan = bridge.compute_delta(&source, &dest_sigs, bs_usize);
    let wire_delta = engine_ops_to_wire(engine_plan.ops.clone());

    let inbound = vec![
        encode(&WireMessage::Hello(HelloMessage {
            protocol: ProtocolVersion::CURRENT,
            role: SessionRole::Sender,
            features: vec![FeatureFlag::DeltaTransfer],
        })),
        encode(&WireMessage::FileMetadata(sample_file_metadata())),
        encode(&WireMessage::DeltaBatch(wire_delta)),
        encode(&baseline_summary_frame(false)),
    ];
    let mut cfg = MockTransportConfig::healthy_download();
    cfg.stream_behavior = OpenStreamBehavior::Success { inbound };

    let mut driver = new_driver(cfg);
    let outcome = driver
        .drive_download_with_engine(
            RemoteCommandSpec::download("/workspace/download/target.bin"),
            destination.clone(),
            &bridge,
        )
        .await
        .expect("engine-mode download succeeds");

    assert_eq!(outcome.final_state, SessionState::Finalized);
    assert_eq!(outcome.block_size, bs_usize as u32);
    let rebuilt = outcome
        .reconstructed
        .expect("download-with-engine rebuilds");
    // THE proof: reconstructed bytes == original source, byte-for-byte.
    assert_eq!(
        rebuilt, source,
        "engine-mode download reconstruction did not match the source"
    );
    // Summary counters flowed through.
    assert_eq!(outcome.stats.literal_bytes, BASELINE_LITERAL_BYTES);
    assert_eq!(outcome.stats.matched_bytes, BASELINE_MATCHED_BYTES);
}

#[tokio::test]
async fn driver_download_with_engine_surfaces_apply_delta_failure() {
    // Remote emits a delta with a CopyBlock index out of range. The engine
    // must reject it; the driver must wrap the rejection as InvalidFrame
    // and mark the session Failed.
    let bridge = CurrentDeltaSyncBridge::new();
    let destination = deterministic_buffer(4096); // only a few blocks
                                                  // Pick a CopyBlock index far beyond any valid one.
    let malicious_delta = vec![
        DeltaInstruction::CopyBlock { index: 999_999 },
        DeltaInstruction::EndOfFile,
    ];
    let inbound = vec![
        encode(&WireMessage::Hello(HelloMessage {
            protocol: ProtocolVersion::CURRENT,
            role: SessionRole::Sender,
            features: vec![FeatureFlag::DeltaTransfer],
        })),
        encode(&WireMessage::FileMetadata(sample_file_metadata())),
        encode(&WireMessage::DeltaBatch(malicious_delta)),
        encode(&baseline_summary_frame(false)),
    ];
    let mut cfg = MockTransportConfig::healthy_download();
    cfg.stream_behavior = OpenStreamBehavior::Success { inbound };

    let mut driver = new_driver(cfg);
    let err = driver
        .drive_download_with_engine(RemoteCommandSpec::download("/t"), destination, &bridge)
        .await
        .unwrap_err();
    assert_eq!(err.kind, NativeRsyncErrorKind::InvalidFrame);
    assert!(err.detail.contains("apply_delta"));
    assert_eq!(driver.session.state, SessionState::Failed);
}

#[tokio::test]
async fn driver_upload_with_engine_respects_pre_cancel() {
    let bridge = CurrentDeltaSyncBridge::new();
    let mut driver = new_driver(MockTransportConfig::healthy_upload());
    driver.cancel().await.unwrap();

    let outcome = driver
        .drive_upload_with_engine(
            RemoteCommandSpec::upload("/t"),
            sample_file_metadata(),
            deterministic_buffer(4096),
            &bridge,
        )
        .await
        .expect("cancelled driver returns terminal outcome");
    assert_eq!(outcome.final_state, SessionState::Cancelled);
    assert!(outcome.reconstructed.is_none());
    assert!(driver.session.transport.captured_outbound().is_empty());
}

#[tokio::test]
async fn driver_download_with_engine_handles_remote_error_frame() {
    // Mid-session Error frame from remote — engine mode must route it
    // through the same RemoteError surfacing path as caller-plan download.
    let bridge = CurrentDeltaSyncBridge::new();
    let destination = deterministic_buffer(4096);
    let inbound = vec![
        encode(&WireMessage::Hello(HelloMessage {
            protocol: ProtocolVersion::CURRENT,
            role: SessionRole::Sender,
            features: vec![FeatureFlag::DeltaTransfer],
        })),
        encode(&WireMessage::Error(ErrorMessage {
            code: 11,
            message: "write error on remote".into(),
        })),
    ];
    let mut cfg = MockTransportConfig::healthy_download();
    cfg.stream_behavior = OpenStreamBehavior::Success { inbound };

    let mut driver = new_driver(cfg);
    let err = driver
        .drive_download_with_engine(RemoteCommandSpec::download("/t"), destination, &bridge)
        .await
        .unwrap_err();
    assert_eq!(err.kind, NativeRsyncErrorKind::RemoteError);
    assert_eq!(driver.session.state, SessionState::Failed);
}

#[tokio::test]
async fn driver_engine_mode_identical_files_produce_all_copy_delta() {
    // When source == destination, engine-mode upload should produce a
    // delta made entirely of CopyBlock ops (zero literal bytes at the
    // engine level — Summary counters are still scripted for parity).
    let bridge = CurrentDeltaSyncBridge::new();
    let buf = deterministic_buffer(16 * 1024);
    let (_, sig_batch) = build_scripted_signature_batch(&bridge, &buf);
    let inbound = vec![
        encode(&WireMessage::Hello(HelloMessage {
            protocol: ProtocolVersion::CURRENT,
            role: SessionRole::Receiver,
            features: vec![FeatureFlag::DeltaTransfer],
        })),
        encode(&WireMessage::SignatureBatch(sig_batch)),
        encode(&baseline_summary_frame(true)),
    ];
    let mut cfg = MockTransportConfig::healthy_upload();
    cfg.stream_behavior = OpenStreamBehavior::Success { inbound };

    let mut driver = new_driver(cfg);
    let outcome = driver
        .drive_upload_with_engine(
            RemoteCommandSpec::upload("/t"),
            sample_file_metadata(),
            buf.clone(),
            &bridge,
        )
        .await
        .expect("engine upload on identical files");

    assert_eq!(outcome.final_state, SessionState::Finalized);
    assert!(outcome
        .engine_delta_ops
        .iter()
        .all(|op| matches!(op, EngineDeltaOp::CopyBlock(_))));
    // Round-trip: apply to destination == buf → recover buf exactly.
    let rebuilt = bridge
        .apply_delta(&buf, &outcome.engine_delta_ops, outcome.block_size as usize)
        .expect("apply_delta identical-files reconstructs source");
    assert_eq!(rebuilt, buf);
}

// ---------------------------------------------------------------------------
// Sinergia 7 — CancelHandle semantics & transport default
// ---------------------------------------------------------------------------

#[test]
fn cancel_handle_inert_cancel_sets_flag_and_is_safe() {
    let handle = CancelHandle::inert();
    assert!(!handle.requested());
    handle.cancel();
    assert!(handle.requested());
    // Idempotent: calling cancel() again must not panic or flip the flag back.
    handle.cancel();
    assert!(handle.requested());
}

#[test]
fn cancel_handle_waker_is_invoked_exactly_once_per_cancel_call() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let counter = Arc::new(AtomicUsize::new(0));
    let counter_for_waker = counter.clone();
    let waker: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        counter_for_waker.fetch_add(1, Ordering::SeqCst);
    });
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let handle = CancelHandle::new(flag, Some(waker));

    handle.cancel();
    assert!(handle.requested());
    assert_eq!(counter.load(Ordering::SeqCst), 1);

    // Second call still invokes the waker. `CancelHandle` is intentionally
    // not idempotent on the waker side — the caller decides what "double
    // cancel" means in their domain.
    handle.cancel();
    assert_eq!(counter.load(Ordering::SeqCst), 2);
}

#[test]
fn cancel_handle_clone_shares_state_with_original() {
    let original = CancelHandle::inert();
    let clone = original.clone();
    assert!(!original.requested());
    assert!(!clone.requested());
    clone.cancel();
    // Both handles reflect the cancel — the flag is `Arc<AtomicBool>`.
    assert!(original.requested());
    assert!(clone.requested());
}

#[tokio::test]
async fn mock_transport_cancel_handle_defaults_to_inert_and_is_independent() {
    // The mock does not override `cancel_handle()`, so it must return the
    // default inert handle. Cancelling through the handle must NOT flip
    // the mock's own `cancel_called` flag (which is only driven by the
    // async `cancel()` method). This guarantees the default-impl escape
    // hatch never silently mutates an implementation's own state.
    let transport = MockRemoteShellTransport::new(MockTransportConfig::healthy_upload());
    let handle = transport.cancel_handle();
    assert!(!handle.requested());
    handle.cancel();
    assert!(handle.requested());
    assert!(
        !transport.cancel_was_called(),
        "default cancel_handle() must not invoke RemoteShellTransport::cancel()"
    );
}

// -------------------------------------------------------------------------
// Sinergia 8a: real rsync byte-oracle fixture consistency
// -------------------------------------------------------------------------

#[test]
fn real_rsync_frozen_transcript_path_layout_is_stable() {
    // The path constants are what `run_real_rsync_capture.sh` writes and
    // what future sinergie (S8b+) will parse. A rename on either side
    // would break the oracle silently — catch it here at compile/test
    // time.
    assert!(REAL_RSYNC_FROZEN_TRANSCRIPT_REL.starts_with("src/rsync_native_proto/capture/"));
    assert!(REAL_RSYNC_FROZEN_TRANSCRIPT_REL.ends_with("/frozen"));
    assert_eq!(REAL_RSYNC_LANE_PORT, 2224);

    let paths = RealRsyncTranscriptPaths::rooted_at(env!("CARGO_MANIFEST_DIR"));
    assert!(
        paths.summary_env.ends_with("summary.env"),
        "summary.env missing from layout"
    );
    assert!(paths.upload_capture_out.ends_with("upload/capture_out.bin"));
    assert!(paths
        .download_capture_out
        .ends_with("download/capture_out.bin"));
}

#[test]
fn real_rsync_frozen_transcript_loads_when_present() {
    // This test is a *conditional* oracle: if the harness has been run
    // at least once, the frozen transcript must be coherent (non-empty
    // byte streams, a recognisable rsync protocol version in the first
    // 4 bytes of upload/capture_out.bin). If no harness run has
    // happened yet on this checkout, the test skips.
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!(
            "skipping: no frozen real-rsync transcript at \
             src/rsync_native_proto/capture/artifacts_real/frozen/ \
             (run capture/run_real_rsync_capture.sh to produce one)"
        );
        return;
    };

    assert!(
        !transcript.upload_server_to_client.is_empty(),
        "upload capture_out.bin is empty — did the harness fail?"
    );
    assert!(
        !transcript.download_server_to_client.is_empty(),
        "download capture_out.bin is empty — did the harness fail?"
    );

    let version = transcript
        .upload_greeting_protocol_version_le()
        .expect("upload capture_out.bin must have >= 4 bytes for the version prefix");
    assert!(
        (30..=40).contains(&version),
        "unexpected rsync protocol version in greeting: {version} \
         (expected 31 or 32, tolerance is 30..=40 for forward compat)"
    );
}

// -------------------------------------------------------------------------
// Sinergia 8b: real rsync wire format (decode-only) against frozen oracle
// -------------------------------------------------------------------------

#[test]
fn real_wire_parses_frozen_client_preamble_upload() {
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    let preamble = decode_client_preamble(&transcript.upload_client_to_server)
        .expect("client preamble must decode");

    assert_eq!(preamble.protocol_version, 31);
    assert_eq!(preamble.checksum_algos, "xxh128 xxh3 xxh64 md5 md4 sha1");
    assert_eq!(preamble.compression_algos, "zstd lz4 zlibx zlib");
    assert_eq!(
        preamble.consumed, 55,
        "client preamble must consume exactly 55 bytes for this algo profile"
    );
}

#[test]
fn real_wire_parses_frozen_server_preamble_upload() {
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    let preamble = decode_server_preamble(&transcript.upload_server_to_client)
        .expect("server preamble must decode");

    assert_eq!(preamble.protocol_version, 32);
    // compat_flags on the wire is a rsync varint; S8d replaced the
    // stale S8b fixed-2-byte read. The frozen handshake negotiates all
    // nine CF_* bits (`0x01FF`), so the varint encodes as the 2-byte
    // sequence 0x81 0xFF — consumed width 2, same as the S8b fiction,
    // but now the field is typed correctly.
    const CF_VARINT_FLIST_FLAGS: i32 = 1 << 7;
    assert_eq!(preamble.compat_flags, 0x01FF);
    assert!(
        preamble.compat_flags & CF_VARINT_FLIST_FLAGS != 0,
        "frozen oracle negotiates CF_VARINT_FLIST_FLAGS; S8d file-list parser depends on it"
    );
    assert_eq!(
        preamble.checksum_algos,
        "xxh128 xxh3 xxh64 md5 md4 sha1 none"
    );
    assert_eq!(preamble.compression_algos, "zstd lz4 zlibx zlib none");
    assert_eq!(
        preamble.consumed, 71,
        "server preamble must consume exactly 71 bytes for this algo profile"
    );
    // checksum_seed is non-zero and stable within one run — we assert
    // only that it's not obviously garbage.
    assert_ne!(preamble.checksum_seed, 0);
    assert_ne!(preamble.checksum_seed, u32::MAX);
}

#[test]
fn real_wire_demuxes_frozen_server_post_preamble_to_msg_data_frames() {
    // The full server-to-client stream of a successful upload run breaks
    // down into: 71-byte preamble, one big MSG_DATA frame carrying the
    // bulk of the rsync application protocol payload, then a handful of
    // small MSG_DATA tail frames. If the decomposition is wrong, every
    // byte fed into S8d onwards will look like garbage.
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    let preamble = decode_server_preamble(&transcript.upload_server_to_client).unwrap();
    let mux_tail = &transcript.upload_server_to_client[preamble.consumed..];

    let mut frames = Vec::new();
    let mut demuxer = MuxDemuxer::new(mux_tail);
    for frame in &mut demuxer {
        let (header, payload) = frame.expect("every frame must decode cleanly");
        frames.push((header, payload.len()));
    }

    assert!(
        !frames.is_empty(),
        "expected at least one mux frame after the server preamble"
    );

    for (header, payload_len) in &frames {
        assert!(
            matches!(header.tag, MuxTag::Data),
            "unexpected mux tag in upload server stream: {:?} (len={})",
            header.tag,
            payload_len
        );
        assert_eq!(
            header.length as usize, *payload_len,
            "header length must match payload length after slicing"
        );
    }

    let total_payload: usize = frames.iter().map(|(_, len)| *len).sum();
    let total_headers: usize = frames.len() * crate::rsync_native_proto::real_wire::MUX_HEADER_LEN;
    assert_eq!(
        preamble.consumed + total_headers + total_payload,
        transcript.upload_server_to_client.len(),
        "every byte of the upload capture_out.bin must be accounted for \
         (preamble + mux headers + payloads)"
    );

    // The largest frame carries the bulk of the protocol payload. Pin
    // its presence so S8c/S8d know what to feed into the file-list /
    // signature / delta parsers.
    let largest = frames
        .iter()
        .map(|(_, len)| *len)
        .max()
        .expect("non-empty frame list");
    assert!(
        largest > 1000,
        "expected a big MSG_DATA frame (>1000 bytes) in the upload \
         transcript; got max {largest}"
    );
}

#[test]
fn real_wire_demuxes_frozen_server_post_preamble_for_download_direction() {
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    let preamble = decode_server_preamble(&transcript.download_server_to_client).unwrap();
    let mux_tail = &transcript.download_server_to_client[preamble.consumed..];

    let mut total_payload = 0usize;
    let mut frame_count = 0usize;
    for frame in MuxDemuxer::new(mux_tail) {
        let (header, payload) = frame.expect("every download frame must decode cleanly");
        assert!(
            matches!(header.tag, MuxTag::Data),
            "unexpected download mux tag: {:?}",
            header.tag
        );
        total_payload += payload.len();
        frame_count += 1;
    }

    assert!(frame_count >= 1);
    assert_eq!(
        preamble.consumed
            + frame_count * crate::rsync_native_proto::real_wire::MUX_HEADER_LEN
            + total_payload,
        transcript.download_server_to_client.len(),
        "byte accounting must close for the download direction too"
    );
}

// -------------------------------------------------------------------------
// Sinergia 8c: reassembly + client direction raw/mux discrimination
// -------------------------------------------------------------------------

#[test]
fn real_wire_reassembles_upload_server_stream_to_app_bytes() {
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    let preamble = decode_server_preamble(&transcript.upload_server_to_client).unwrap();
    let mux_tail = &transcript.upload_server_to_client[preamble.consumed..];
    let report = reassemble_msg_data(mux_tail).unwrap();

    // Upload transcript has 4 MSG_DATA frames totalling 2269+1+3+1 = 2274 bytes.
    assert_eq!(report.frames_consumed, 4);
    assert_eq!(report.app_stream.len(), 2274);
    assert!(
        report.out_of_band.is_empty(),
        "upload server stream should contain only MSG_DATA in this profile"
    );
}

#[test]
fn real_wire_reassembles_download_server_stream_to_app_bytes() {
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    let preamble = decode_server_preamble(&transcript.download_server_to_client).unwrap();
    let mux_tail = &transcript.download_server_to_client[preamble.consumed..];
    let report = reassemble_msg_data(mux_tail).unwrap();

    // The download direction is smaller but should still produce >= 1 frame
    // and close byte accounting against the raw mux tail.
    assert!(report.frames_consumed >= 1);
    let headers_consumed =
        report.frames_consumed * crate::rsync_native_proto::real_wire::MUX_HEADER_LEN;
    let oob_payload: usize = report.out_of_band.iter().map(|(_, n)| *n as usize).sum();
    assert_eq!(
        headers_consumed + report.app_stream.len() + oob_payload,
        mux_tail.len(),
        "every byte of the download mux tail must be accounted for"
    );
}

#[test]
fn real_wire_client_to_server_upload_is_multiplexed_like_server_side() {
    // Empirical finding from the frozen oracle: contrary to a first
    // reading of the rsync source, the client -> server channel IS
    // multiplexed from immediately after the client preamble in
    // remote-shell mode with protocol 31/32. Decoding the full 445-byte
    // tail of `upload/capture_in.bin` yields 5 chained MSG_DATA frames
    // whose concatenated payload contains, in order: the file-list
    // entry with `upload.bin` at path length 10, the uid/gid strings
    // `axpnet`, and the payload marker `real-live-upload` that
    // `run_real_rsync_capture.sh` injects into the mutated source file.
    // That is all valid rsync application traffic — the stream really
    // is multiplexed.
    //
    // Pin this property so the mux decoder is applied uniformly to
    // both directions; treating the client stream as raw would leave
    // the embedded mux headers inline with the app payload and break
    // every subsequent parser.
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    let preamble = decode_client_preamble(&transcript.upload_client_to_server).unwrap();
    let tail = &transcript.upload_client_to_server[preamble.consumed..];

    let report = reassemble_msg_data(tail).expect("client tail must decode as mux chain");
    assert!(
        report.frames_consumed >= 1,
        "expected at least one mux frame in client post-preamble stream"
    );
    // The known marker proves reassembly output is real file content,
    // not a lucky coincidence of header-shaped bytes.
    let window = b"real-live-upload";
    assert!(
        report.app_stream.windows(window.len()).any(|w| w == window),
        "reassembled client stream does not contain the injected marker"
    );

    // Byte accounting: preamble + (headers + payloads) + oob payloads
    // must fill the capture exactly.
    let headers_consumed =
        report.frames_consumed * crate::rsync_native_proto::real_wire::MUX_HEADER_LEN;
    let oob_payload: usize = report.out_of_band.iter().map(|(_, n)| *n as usize).sum();
    assert_eq!(
        preamble.consumed + headers_consumed + report.app_stream.len() + oob_payload,
        transcript.upload_client_to_server.len(),
        "client->server upload byte accounting must close"
    );
}

#[test]
fn real_wire_app_stream_first_bytes_are_nonzero_nonmarker() {
    // Pre-S8d anchor. The reassembled app stream must start with the
    // rsync application payload, not mux header leakage.
    // Weak-but-useful assertions:
    //   1. At least one of the first 4 bytes is non-zero (a pure
    //      `0 0 0 0` would terminate the file list immediately in
    //      protocol 31+).
    //   2. The reassembly report shape round-trips cleanly. Structural
    //      smoke test, not a wire assertion.
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };
    let preamble = decode_server_preamble(&transcript.upload_server_to_client).unwrap();
    let mux_tail = &transcript.upload_server_to_client[preamble.consumed..];
    let report = reassemble_msg_data(mux_tail).unwrap();
    assert!(report.app_stream.len() >= 4, "app stream too short");
    let head = &report.app_stream[..4];
    assert!(
        head.iter().any(|&b| b != 0),
        "first 4 bytes of app stream are all zero: {head:?}"
    );

    let _ = MuxHeader {
        tag: MuxTag::Data,
        length: 1,
    }
    .encode();
}

// ---------------------------------------------------------------------------
// real_wire — Sinergia 8d file-list entry decoder vs frozen oracle
// ---------------------------------------------------------------------------

#[test]
fn real_wire_decodes_first_file_list_entry_from_frozen_upload_client_stream() {
    // Upload direction: the client sends the file list to the server.
    // The app stream of `upload/capture_in.bin` (after consuming the
    // client preamble and reassembling MSG_DATA frames) must start with
    // a regular-file entry for "upload.bin", size 262_144, uid/gid
    // "axpnet", followed by a 16-byte xxh128 checksum (the lane runs
    // with `--checksum`).
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    let client_preamble = decode_client_preamble(&transcript.upload_client_to_server).unwrap();
    let mux_tail = &transcript.upload_client_to_server[client_preamble.consumed..];
    let report = reassemble_msg_data(mux_tail).unwrap();

    let opts = FileListDecodeOptions::frozen_oracle_default();
    let (outcome, consumed) = decode_file_list_entry(&report.app_stream, &opts)
        .expect("first file-list entry must decode");

    let entry = match outcome {
        FileListDecodeOutcome::Entry(e) => e,
        other => panic!("expected Entry, got {other:?}"),
    };

    assert_eq!(
        entry.path, "upload.bin",
        "frozen upload transcript uploads upload.bin -> target.bin"
    );
    assert_eq!(entry.size, 262_144, "frozen upload uses a 256 KiB payload");
    assert_eq!(
        entry.uid_name.as_deref(),
        Some("axpnet"),
        "container ForceCommand runs rsync --server as the axpnet user"
    );
    assert_eq!(
        entry.gid_name.as_deref(),
        Some("axpnet"),
        "container's axpnet user has gid=axpnet"
    );
    assert!(
        entry.mode & 0o170_000 == 0o100_000,
        "mode high bits must denote a regular file; got {:#o}",
        entry.mode
    );
    assert_eq!(
        entry.checksum.len(),
        opts.csum_len,
        "xxh128 checksum is always 16 bytes"
    );
    assert!(
        consumed <= report.app_stream.len(),
        "consumed exceeds app stream — decoder ran off the end"
    );
    assert!(
        consumed >= 47,
        "per-field byte accounting: ≥ 47 bytes for this profile"
    );
}

#[test]
fn real_wire_file_list_terminator_follows_first_entry_in_frozen_upload() {
    // Anchor for S8e: after the first (and only) file-list entry, the
    // very next byte must be the terminator varint(0) = 0x00. Any
    // deviation means our field-accounting is off by some number of
    // bytes and subsequent parsers will drift.
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    let client_preamble = decode_client_preamble(&transcript.upload_client_to_server).unwrap();
    let report =
        reassemble_msg_data(&transcript.upload_client_to_server[client_preamble.consumed..])
            .unwrap();

    let opts = FileListDecodeOptions::frozen_oracle_default();
    let (_, consumed_first) =
        decode_file_list_entry(&report.app_stream, &opts).expect("first entry must decode");

    assert!(
        consumed_first < report.app_stream.len(),
        "no room left after first entry for the terminator"
    );
    let terminator_byte = report.app_stream[consumed_first];
    assert_eq!(
        terminator_byte, 0x00,
        "byte at offset {consumed_first} must be the file-list terminator (varint 0), \
         got {terminator_byte:#04x}"
    );

    // Double-check: feeding the remainder to decode_file_list_entry
    // again must round-trip to EndOfList, consuming exactly one byte.
    let (outcome, consumed_end) = decode_file_list_entry(
        &report.app_stream[consumed_first..],
        &FileListDecodeOptions {
            previous_name: Some("upload.bin"),
            ..opts
        },
    )
    .unwrap();
    assert_eq!(consumed_end, 1);
    assert!(matches!(
        outcome,
        FileListDecodeOutcome::EndOfList { io_error: 0 }
    ));
}

#[test]
fn real_wire_decodes_first_file_list_entry_from_frozen_download_server_stream() {
    // Download direction: the SERVER sends the file list to the client.
    // The file being downloaded from the capture harness is
    // `/workspace/real/upload/target.bin`, and rsync transmits it as
    // the basename "target.bin" since the transfer is a single-file
    // pull. We validate the same structural invariants as the upload
    // direction to prove symmetry.
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    let server_preamble = decode_server_preamble(&transcript.download_server_to_client).unwrap();
    let report =
        reassemble_msg_data(&transcript.download_server_to_client[server_preamble.consumed..])
            .unwrap();

    let opts = FileListDecodeOptions::frozen_oracle_default();
    let (outcome, consumed) = decode_file_list_entry(&report.app_stream, &opts)
        .expect("first file-list entry must decode on download direction too");

    let entry = match outcome {
        FileListDecodeOutcome::Entry(e) => e,
        other => panic!("expected Entry, got {other:?}"),
    };

    assert_eq!(
        entry.path, "target.bin",
        "download transcript pulls target.bin from the server"
    );
    assert_eq!(entry.size, 262_144);
    assert_eq!(entry.checksum.len(), opts.csum_len);
    assert!(entry.mode & 0o170_000 == 0o100_000, "regular file mode");

    // Terminator anchor on this direction too.
    let terminator = report.app_stream[consumed];
    assert_eq!(
        terminator, 0x00,
        "download direction also terminates with varint(0)"
    );
}

// ---------------------------------------------------------------------------
// real_wire — Sinergia 8e: ndx / item_flags / sum_head / sum_block vs
// frozen oracle. The receiver side of the upload lane (server->client) is
// the cleanest lane: it contains exactly one per-file header, one
// sum_head, 375 signature blocks and a trailing run of NDX_DONE markers.
// ---------------------------------------------------------------------------

/// Expected wire parameters for the frozen oracle's 256 KiB transfer with
/// `--checksum`. Locked in by S8e so any drift in the capture harness (or
/// in rsync's own `sum_sizes_sqroot`) surfaces as a test failure rather
/// than a silent decode drift.
const FROZEN_SUM_HEAD_COUNT: i32 = 375;
const FROZEN_SUM_HEAD_BLENGTH: i32 = 700;
const FROZEN_SUM_HEAD_S2LENGTH: i32 = 2;
const FROZEN_SUM_HEAD_REMAINDER: i32 = 344;

/// Total bytes rsync charges per signature block: rolling u32 LE + the
/// truncated strong checksum.
const FROZEN_SUM_BLOCK_BYTES: usize = 4 + FROZEN_SUM_HEAD_S2LENGTH as usize;

/// Exact length of the NDX_DONE tail observed on the frozen upload
/// server->client stream — matches `send_files`' two-phase loop plus the
/// final done marker.
const FROZEN_UPLOAD_RECEIVER_NDX_DONE_TAIL: usize = 5;

#[test]
fn real_wire_decodes_sum_head_from_frozen_upload_server_stream() {
    // Structural anchor for S8e. The receiver's stream in the upload
    // direction starts with `write_ndx(first_file) + write_shortint(iflags)
    // + write_sum_head(…)`. All four sum_head fields must match the
    // expected profile exactly.
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    let preamble = decode_server_preamble(&transcript.upload_server_to_client).unwrap();
    let app = reassemble_msg_data(&transcript.upload_server_to_client[preamble.consumed..])
        .unwrap()
        .app_stream;

    let mut state = NdxState::new();
    let (first_ndx, ndx_bytes) = decode_ndx(&app, &mut state).expect("first ndx must decode");
    assert!(
        first_ndx >= 0,
        "first ndx on the receiver stream must be a positive file index, got {first_ndx}"
    );

    let (iflags, iflags_bytes) = decode_item_flags(&app[ndx_bytes..]).expect("iflags");
    // ITEM_TRANSFER (1<<15) must be set — this is the whole point of the
    // message; without it the receiver would not generate sums at all.
    assert!(
        iflags & 0x8000 != 0,
        "iflags must have ITEM_TRANSFER set; got {iflags:#06x}"
    );

    let (head, head_bytes) = decode_sum_head(&app[ndx_bytes + iflags_bytes..]).expect("sum_head");
    assert_eq!(head.count, FROZEN_SUM_HEAD_COUNT);
    assert_eq!(head.block_length, FROZEN_SUM_HEAD_BLENGTH);
    assert_eq!(head.checksum_length, FROZEN_SUM_HEAD_S2LENGTH);
    assert_eq!(head.remainder_length, FROZEN_SUM_HEAD_REMAINDER);

    // Byte accounting up to this point: 1 (ndx) + 2 (iflags) + 16 (sum_head).
    assert_eq!(ndx_bytes + iflags_bytes + head_bytes, 1 + 2 + 16);
}

#[test]
fn real_wire_decodes_all_375_sum_blocks_from_frozen_upload_server_stream() {
    // Walk all 375 signature blocks and assert:
    //   - every block decodes with strong.len() == s2length (2 in this profile)
    //   - the first block's rolling checksum is non-zero (pseudo-random
    //     content guarantees this; a zero would mean we lost alignment)
    //   - exact byte accounting: 19 bytes of header + 375 * 6 = 2269 bytes
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    let preamble = decode_server_preamble(&transcript.upload_server_to_client).unwrap();
    let app = reassemble_msg_data(&transcript.upload_server_to_client[preamble.consumed..])
        .unwrap()
        .app_stream;

    let mut cursor = 0usize;
    let mut state = NdxState::new();
    cursor += decode_ndx(&app[cursor..], &mut state).unwrap().1;
    cursor += decode_item_flags(&app[cursor..]).unwrap().1;
    let (head, head_bytes) = decode_sum_head(&app[cursor..]).unwrap();
    cursor += head_bytes;
    let strong_len = head.checksum_length as usize;

    let mut first_rolling = 0u32;
    let mut seen = 0i32;
    for i in 0..head.count {
        let (block, consumed) = decode_sum_block(&app[cursor..], strong_len)
            .unwrap_or_else(|e| panic!("block #{i} failed to decode: {e}"));
        assert_eq!(block.strong.len(), strong_len);
        if i == 0 {
            first_rolling = block.rolling;
        }
        cursor += consumed;
        seen += 1;
    }
    assert_eq!(seen, FROZEN_SUM_HEAD_COUNT);
    assert_eq!(
        cursor,
        1 + 2 + 16 + FROZEN_SUM_HEAD_COUNT as usize * FROZEN_SUM_BLOCK_BYTES
    );
    assert_ne!(
        first_rolling, 0,
        "first block rolling checksum is zero — likely misalignment"
    );
}

#[test]
fn real_wire_trailing_ndx_done_markers_close_byte_accounting_on_upload_receiver_stream() {
    // After the per-file header + sum_head + 375 blocks, the remaining
    // bytes of the receiver stream must be exactly 5 copies of
    // write_ndx(NDX_DONE) — one per `send_files` phase transition plus the
    // final done marker. Decoding them as ndx values proves the tail is
    // well-formed protocol traffic, not trailing garbage.
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    let preamble = decode_server_preamble(&transcript.upload_server_to_client).unwrap();
    let app = reassemble_msg_data(&transcript.upload_server_to_client[preamble.consumed..])
        .unwrap()
        .app_stream;

    let header_and_blocks = 1 + 2 + 16 + FROZEN_SUM_HEAD_COUNT as usize * FROZEN_SUM_BLOCK_BYTES;
    let tail = &app[header_and_blocks..];
    assert_eq!(
        tail.len(),
        FROZEN_UPLOAD_RECEIVER_NDX_DONE_TAIL,
        "tail length drifted from 5 NDX_DONE markers — check phase transitions"
    );

    let mut state = NdxState::new();
    let mut off = 0usize;
    for i in 0..FROZEN_UPLOAD_RECEIVER_NDX_DONE_TAIL {
        let (value, consumed) = decode_ndx(&tail[off..], &mut state)
            .unwrap_or_else(|e| panic!("trailing ndx #{i} failed: {e}"));
        assert_eq!(value, NDX_DONE, "trailing ndx #{i} must be NDX_DONE");
        assert_eq!(consumed, 1);
        off += consumed;
    }
    assert_eq!(off, FROZEN_UPLOAD_RECEIVER_NDX_DONE_TAIL);
}

#[test]
fn real_wire_decodes_ndx_flist_eof_after_flist_terminator_on_client_upload_stream() {
    // The sender's post-flist sequence (client -> server, upload) opens
    // with two varint(0) bytes from `write_end_of_flist` then a
    // write_ndx(NDX_FLIST_EOF) that the S8d scout observed as `ff 01`.
    // Pin the mapping so the S8e decoder keeps behaving per `io.c::read_ndx`.
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    let preamble = decode_client_preamble(&transcript.upload_client_to_server).unwrap();
    let app = reassemble_msg_data(&transcript.upload_client_to_server[preamble.consumed..])
        .unwrap()
        .app_stream;

    // Advance past the file-list entry + its two-byte terminator.
    let opts = FileListDecodeOptions::frozen_oracle_default();
    let (_, entry_bytes) = decode_file_list_entry(&app, &opts).unwrap();
    let post_flist = &app[entry_bytes..];
    // First two bytes are the terminator pair: varint(0) + varint(io_error=0).
    assert_eq!(post_flist[0], 0x00);
    assert_eq!(post_flist[1], 0x00);

    let mut state = NdxState::new();
    let (value, consumed) =
        decode_ndx(&post_flist[2..], &mut state).expect("NDX_FLIST_EOF must decode");
    assert_eq!(value, NDX_FLIST_EOF);
    assert_eq!(consumed, 2, "NDX_FLIST_EOF is encoded as two bytes (FF 01)");
}

// ---------------------------------------------------------------------------
// S8e scouting — hex-dump post-flist bytes on both directions to identify
// the sum_head start and resolve the `00 FF 01` mystery left open by S8d.
// Intentionally panics so stderr becomes visible under `cargo test -- --nocapture`.
// Remove or downgrade to a non-panicking assertion once S8e decodes cleanly.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "scouting: hex-dumps bytes to guide S8e, not a regression gate"]
fn s8e_scout_hex_dump_post_flist_regions() {
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    fn hexdump(label: &str, bytes: &[u8]) {
        eprintln!("--- {} ({} bytes) ---", label, bytes.len());
        for (i, chunk) in bytes.chunks(16).enumerate() {
            let hex: Vec<String> = chunk.iter().map(|b| format!("{:02x}", b)).collect();
            let ascii: String = chunk
                .iter()
                .map(|&b| {
                    if (0x20..0x7f).contains(&b) {
                        b as char
                    } else {
                        '.'
                    }
                })
                .collect();
            eprintln!("{:04x}  {:<48}  {}", i * 16, hex.join(" "), ascii);
        }
    }

    // ---- UPLOAD, client -> server (sender's flist + tail) ----
    let cpre = decode_client_preamble(&transcript.upload_client_to_server).unwrap();
    let up_client_app = reassemble_msg_data(&transcript.upload_client_to_server[cpre.consumed..])
        .unwrap()
        .app_stream;
    let opts = FileListDecodeOptions::frozen_oracle_default();
    let (_, flist_end) = decode_file_list_entry(&up_client_app, &opts).unwrap();
    eprintln!(
        "UPLOAD client->server app_stream={} bytes, flist entry ends at {}",
        up_client_app.len(),
        flist_end
    );
    hexdump(
        "UPLOAD c->s: bytes from flist_end onwards",
        &up_client_app[flist_end..],
    );

    // ---- UPLOAD, server -> client (receiver's sum stream) ----
    let spre = decode_server_preamble(&transcript.upload_server_to_client).unwrap();
    let up_server_app = reassemble_msg_data(&transcript.upload_server_to_client[spre.consumed..])
        .unwrap()
        .app_stream;
    eprintln!(
        "UPLOAD server->client app_stream={} bytes (expected to carry sum stream + summary)",
        up_server_app.len()
    );
    hexdump(
        "UPLOAD s->c: first 128 bytes of app_stream",
        &up_server_app[..up_server_app.len().min(128)],
    );
    hexdump(
        "UPLOAD s->c: last 32 bytes of app_stream",
        &up_server_app[up_server_app.len().saturating_sub(32)..],
    );

    // ---- DOWNLOAD, server -> client (sender's flist + delta stream) ----
    let spre_d = decode_server_preamble(&transcript.download_server_to_client).unwrap();
    let dn_server_app =
        reassemble_msg_data(&transcript.download_server_to_client[spre_d.consumed..])
            .unwrap()
            .app_stream;
    let (_, flist_end_d) = decode_file_list_entry(&dn_server_app, &opts).unwrap();
    eprintln!(
        "DOWNLOAD server->client app_stream={} bytes, flist entry ends at {}",
        dn_server_app.len(),
        flist_end_d
    );
    hexdump(
        "DOWNLOAD s->c: 96 bytes from flist_end onwards (delta/summary territory)",
        &dn_server_app[flist_end_d..(flist_end_d + 96).min(dn_server_app.len())],
    );

    // ---- DOWNLOAD, client -> server (receiver's sum stream) ----
    let cpre_d = decode_client_preamble(&transcript.download_client_to_server).unwrap();
    let dn_client_app =
        reassemble_msg_data(&transcript.download_client_to_server[cpre_d.consumed..])
            .unwrap()
            .app_stream;
    eprintln!(
        "DOWNLOAD client->server app_stream={} bytes (expected sum_head + N blocks + summary)",
        dn_client_app.len()
    );
    hexdump(
        "DOWNLOAD c->s: first 96 bytes of app_stream",
        &dn_client_app[..dn_client_app.len().min(96)],
    );
    hexdump(
        "DOWNLOAD c->s: last 32 bytes of app_stream",
        &dn_client_app[dn_client_app.len().saturating_sub(32)..],
    );

    // Scouting aid — not a regression gate. Run with:
    //   cargo test --features proto_native_rsync --lib \
    //     s8e_scout_hex_dump_post_flist_regions -- --ignored --nocapture
}

// ---------------------------------------------------------------------------
// real_wire — Sinergia 8f delta instruction decoder vs frozen oracle.
// Target: the sender direction of the upload lane (client->server), where
// rsync emits the full sequence `end_of_flist + io_error +
// NDX_FLIST_EOF + ndx + iflags + sum_head + <delta stream> + END_FLAG +
// file_checksum + NDX_DONE…`. The frozen oracle's 256 KiB fixture runs
// with `-z` + `--checksum` and negotiates zstd via `CF_VARINT_FLIST_FLAGS`.
// ---------------------------------------------------------------------------

/// File-level strong checksum length for the frozen oracle profile. Both
/// peers are xxh3-linked, so the negotiated algo is xxh128 (16 bytes).
/// Locked in as a constant so a build linked against a different hash
/// library would surface as a test failure instead of silent drift.
const FROZEN_FILE_CHECKSUM_LEN: usize = 16;

/// ZSTD frame magic, RFC 8478 §3.1.1. Checked against the first LITERAL
/// payload emitted on the upload sender stream.
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// Helper — advance past `end_of_flist + io_error_varint + NDX_FLIST_EOF +
/// ndx + iflags + sum_head` on an app_stream that starts at the flist
/// terminator. Returns (bytes_consumed, sum_head). Fails loudly if any
/// field disagrees with the frozen profile so callers can trust the
/// offset arithmetic.
fn advance_past_sum_head(
    app: &[u8],
    entry_bytes: usize,
) -> (usize, crate::rsync_native_proto::real_wire::SumHead) {
    // Terminator varint(0).
    assert_eq!(app[entry_bytes], 0x00, "file-list terminator must be 0x00");
    // io_error varint(0).
    assert_eq!(app[entry_bytes + 1], 0x00, "io_error must be 0");
    let mut cursor = entry_bytes + 2;

    let mut state = NdxState::new();
    let (eof, c) = decode_ndx(&app[cursor..], &mut state).expect("NDX_FLIST_EOF must decode");
    assert_eq!(eof, NDX_FLIST_EOF);
    cursor += c;

    let (ndx1, c) = decode_ndx(&app[cursor..], &mut state).expect("first ndx must decode");
    assert_eq!(ndx1, 1, "first ndx must be 1 (single-file fixture)");
    cursor += c;

    let (iflags, c) = decode_item_flags(&app[cursor..]).expect("iflags");
    assert!(
        iflags & 0x8000 != 0,
        "ITEM_TRANSFER must be set; got {iflags:#06x}"
    );
    cursor += c;

    let (head, c) = decode_sum_head(&app[cursor..]).expect("sum_head");
    cursor += c;

    (cursor, head)
}

#[test]
fn real_wire_decodes_full_delta_stream_from_frozen_upload_client_stream() {
    // End-to-end parse of the sender's post-flist tail on the upload
    // direction. Must produce a non-empty op list whose CopyRun total
    // covers most of the 375 blocks, plus at least one Literal carrying
    // the zstd-compressed literal payload.
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    let cpre = decode_client_preamble(&transcript.upload_client_to_server).unwrap();
    let app = reassemble_msg_data(&transcript.upload_client_to_server[cpre.consumed..])
        .unwrap()
        .app_stream;

    let opts = FileListDecodeOptions::frozen_oracle_default();
    let (_, entry_bytes) =
        decode_file_list_entry(&app, &opts).expect("first file-list entry must decode");

    let (header_end, head) = advance_past_sum_head(&app, entry_bytes);
    assert_eq!(head.count, 375);
    assert_eq!(head.checksum_length, 2);

    let (report, consumed) = decode_delta_stream(
        &app[header_end..],
        FROZEN_FILE_CHECKSUM_LEN,
        Some(head.count),
    )
    .expect("delta stream must decode cleanly");

    assert!(
        !report.ops.is_empty(),
        "delta stream must contain at least one op"
    );
    assert_eq!(
        report.file_checksum.len(),
        FROZEN_FILE_CHECKSUM_LEN,
        "file-level strong checksum must be xxh128 (16 bytes)"
    );

    // Structural invariants: COPY totals must cover near-all of the 375
    // blocks (one block's worth is literal per --stats: 700 bytes) and
    // Literal ops must exist for the injected marker to have a home.
    let matched: i64 = report
        .ops
        .iter()
        .filter_map(|op| match op {
            DeltaOp::CopyRun { run_length, .. } => Some(i64::from(*run_length)),
            _ => None,
        })
        .sum();
    let literal_count = report
        .ops
        .iter()
        .filter(|op| matches!(op, DeltaOp::Literal { .. }))
        .count();

    assert!(
        literal_count >= 1,
        "must have at least one Literal for the injected real-live-upload marker"
    );
    // Matched count lives in a narrow window: 1 literal block means 374
    // must be matched, but the matcher may round slightly depending on
    // where the injected marker falls inside a block. Allow a small slop.
    assert!(
        (370..=374).contains(&matched),
        "matched block count {matched} outside expected window [370..=374]"
    );

    // Byte accounting: consumed = delta tokens + END_FLAG + file_checksum.
    // Any remaining bytes on the app stream are the trailing NDX_DONE run.
    let tail = &app[header_end + consumed..];
    assert!(
        !tail.is_empty(),
        "app stream must have a trailing NDX_DONE run after the file checksum"
    );
    assert!(
        tail.iter().all(|&b| b == 0),
        "trailing bytes after file_checksum must all be NDX_DONE (0x00); got {tail:?}"
    );
    // Decode each trailing byte as a fresh NDX_DONE — proves they really
    // are well-formed ndx values, not random zeros.
    let mut state = NdxState::new();
    for (i, _) in tail.iter().enumerate() {
        let (value, n) = decode_ndx(&tail[i..], &mut state).unwrap();
        assert_eq!(value, NDX_DONE, "trailing ndx #{i} must be NDX_DONE");
        assert_eq!(n, 1);
    }
}

#[test]
fn real_wire_first_delta_literal_starts_with_zstd_frame_magic() {
    // Proves the negotiated compressor is zstd (CPRES_ZSTD, activated via
    // CF_VARINT_FLIST_FLAGS + negotiate_the_strings with `zstd` first in
    // the valid_compressions list).
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    let cpre = decode_client_preamble(&transcript.upload_client_to_server).unwrap();
    let app = reassemble_msg_data(&transcript.upload_client_to_server[cpre.consumed..])
        .unwrap()
        .app_stream;

    let opts = FileListDecodeOptions::frozen_oracle_default();
    let (_, entry_bytes) = decode_file_list_entry(&app, &opts).unwrap();
    let (header_end, head) = advance_past_sum_head(&app, entry_bytes);

    let (report, _) = decode_delta_stream(
        &app[header_end..],
        FROZEN_FILE_CHECKSUM_LEN,
        Some(head.count),
    )
    .expect("delta stream must decode");

    let first_literal = report
        .ops
        .iter()
        .find_map(|op| match op {
            DeltaOp::Literal { compressed_payload } => Some(compressed_payload),
            _ => None,
        })
        .expect("at least one Literal op expected");
    assert!(
        first_literal.len() >= 4,
        "first literal payload too short to hold a zstd magic: {}",
        first_literal.len()
    );
    assert_eq!(
        &first_literal[..4],
        &ZSTD_MAGIC,
        "first literal must start with ZSTD frame magic (CPRES_ZSTD negotiated, not zlib)"
    );
}

#[test]
fn real_wire_delta_literal_carries_real_live_upload_marker_bytes() {
    // The capture harness injects the ASCII marker `real-live-upload` into
    // the sender's file AFTER the generator computes signatures. The
    // marker lands in a LITERAL block on the wire, and zstd's low
    // compression ratio on the surrounding pattern leaves the marker
    // visible as-is inside the compressed payload. Finding it proves the
    // decoder has extracted the right payload bytes.
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    let cpre = decode_client_preamble(&transcript.upload_client_to_server).unwrap();
    let app = reassemble_msg_data(&transcript.upload_client_to_server[cpre.consumed..])
        .unwrap()
        .app_stream;

    let opts = FileListDecodeOptions::frozen_oracle_default();
    let (_, entry_bytes) = decode_file_list_entry(&app, &opts).unwrap();
    let (header_end, head) = advance_past_sum_head(&app, entry_bytes);

    let (report, _) = decode_delta_stream(
        &app[header_end..],
        FROZEN_FILE_CHECKSUM_LEN,
        Some(head.count),
    )
    .unwrap();

    const MARKER: &[u8] = b"real-live-upload";
    let has_marker = report.ops.iter().any(|op| {
        matches!(op, DeltaOp::Literal { compressed_payload }
                 if compressed_payload.windows(MARKER.len()).any(|w| w == MARKER))
    });
    assert!(
        has_marker,
        "expected to find `real-live-upload` marker in one of the Literal payloads"
    );
}

#[test]
fn real_wire_delta_stream_byte_accounting_closes_on_upload_client_stream() {
    // Full byte accounting of the sender stream:
    //   entry(63) + terminator(1) + io_error(1) + NDX_FLIST_EOF(2) +
    //   ndx(1)(1) + iflags(2) + sum_head(16) + <delta+END_FLAG>(N) +
    //   file_checksum(16) + NDX_DONE_tail(M) = app_stream.len().
    // Any drift in this sum means we lost alignment somewhere.
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    let cpre = decode_client_preamble(&transcript.upload_client_to_server).unwrap();
    let app = reassemble_msg_data(&transcript.upload_client_to_server[cpre.consumed..])
        .unwrap()
        .app_stream;

    let opts = FileListDecodeOptions::frozen_oracle_default();
    let (_, entry_bytes) = decode_file_list_entry(&app, &opts).unwrap();
    let (header_end, head) = advance_past_sum_head(&app, entry_bytes);

    let (_, delta_consumed) = decode_delta_stream(
        &app[header_end..],
        FROZEN_FILE_CHECKSUM_LEN,
        Some(head.count),
    )
    .unwrap();

    let tail_len = app.len() - (header_end + delta_consumed);

    // The full accounting identity: every byte is placed in exactly one
    // bucket, and the sum equals the reassembled app stream length.
    let total = entry_bytes + 2 + 2 + 1 + 2 + 16 + delta_consumed + tail_len;
    assert_eq!(
        total,
        app.len(),
        "byte accounting mismatch: entry={entry_bytes}, header=22, delta={delta_consumed}, tail={tail_len}, total={total}, app_len={}",
        app.len()
    );

    // Sanity: the tail must be at least 1 NDX_DONE (the final
    // end-of-transfer marker). Multiple zeros are fine — they correspond
    // to phase transitions in send_files.
    assert!(
        tail_len >= 1,
        "expected at least 1 trailing NDX_DONE byte, got {tail_len}"
    );
    // And the tail length stays small — rsync never emits hundreds of
    // trailing NDX_DONE bytes. Pin the upper bound loosely so an unrelated
    // regression (e.g. forgotten consume_some in reassembly) can't hide
    // by padding the tail.
    assert!(
        tail_len <= 8,
        "trailing NDX_DONE run suspiciously long ({tail_len}) — check reassembly"
    );
}

// ---------------------------------------------------------------------------
// S8g scouting — locate the end-of-session summary frame.
//
// After the last NDX_DONE of a real-rsync transfer the sender emits a
// final stats block (`io.c::write_stats` called by `log.c::report`).
// Wire shape and carrier channel (app MSG_DATA tail vs MSG_STATS OOB
// frame) are unknown and must be identified against the frozen oracle
// before writing the decoder. This scout dumps, for each of the four
// streams:
//   - app_stream length and out-of-band frame log (so `MSG_STATS`,
//     `MSG_INFO`, `MSG_ERROR` frames become visible);
//   - the first 32 bytes of app_stream (sanity anchor against past
//     sinergie);
//   - the LAST ~200 bytes of app_stream (where summary + NDX_DONE tail
//     live);
//   - a greedy walk that parses trailing NDX frames with `decode_ndx`
//     until it hits bytes that don't decode — the leftover tail is the
//     summary candidate.
//
// Intentionally `#[ignore]`: scouting aid, not a regression gate. Run
// with: cargo test --features proto_native_rsync --lib \
//   s8g_scout_hex_dump_stream_tails -- --ignored --nocapture
// ---------------------------------------------------------------------------

#[test]
#[ignore = "scouting: hex-dumps stream tails to locate the S8g summary frame"]
fn s8g_scout_hex_dump_stream_tails() {
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    fn hexdump(label: &str, bytes: &[u8], offset_base: usize) {
        eprintln!("--- {} ({} bytes) ---", label, bytes.len());
        for (i, chunk) in bytes.chunks(16).enumerate() {
            let hex: Vec<String> = chunk.iter().map(|b| format!("{:02x}", b)).collect();
            let ascii: String = chunk
                .iter()
                .map(|&b| {
                    if (0x20..0x7f).contains(&b) {
                        b as char
                    } else {
                        '.'
                    }
                })
                .collect();
            eprintln!(
                "{:04x}  {:<48}  {}",
                offset_base + i * 16,
                hex.join(" "),
                ascii
            );
        }
    }

    /// Walk trailing NDX frames greedily. Returns the offset at which
    /// NDX parsing stopped (i.e. the start of the leftover tail that is
    /// NOT a valid NDX sequence — the summary candidate region).
    fn trailing_ndx_walk(app: &[u8], walk_start: usize) -> (usize, Vec<i32>) {
        let mut offset = walk_start;
        let mut ndxs: Vec<i32> = Vec::new();
        let mut state = NdxState::new();
        while offset < app.len() {
            match decode_ndx(&app[offset..], &mut state) {
                Ok((ndx, consumed)) => {
                    ndxs.push(ndx);
                    offset += consumed;
                }
                Err(_) => break,
            }
        }
        (offset, ndxs)
    }

    for (label, bytes, is_client_side) in [
        (
            "UPLOAD c->s",
            transcript.upload_client_to_server.as_slice(),
            true,
        ),
        (
            "UPLOAD s->c",
            transcript.upload_server_to_client.as_slice(),
            false,
        ),
        (
            "DOWNLOAD c->s",
            transcript.download_client_to_server.as_slice(),
            true,
        ),
        (
            "DOWNLOAD s->c",
            transcript.download_server_to_client.as_slice(),
            false,
        ),
    ] {
        eprintln!("\n============== {} ==============", label);
        eprintln!("raw stream = {} bytes", bytes.len());

        let preamble_consumed = if is_client_side {
            decode_client_preamble(bytes).unwrap().consumed
        } else {
            decode_server_preamble(bytes).unwrap().consumed
        };
        eprintln!("preamble = {} bytes", preamble_consumed);

        let report = reassemble_msg_data(&bytes[preamble_consumed..]).unwrap();
        eprintln!(
            "mux: {} frames consumed, app_stream = {} bytes, {} OOB frames",
            report.frames_consumed,
            report.app_stream.len(),
            report.out_of_band.len()
        );
        for (i, (tag, len)) in report.out_of_band.iter().enumerate() {
            eprintln!("  OOB[{}]: tag={:?} len={}", i, tag, len);
        }

        let app = &report.app_stream;
        let head_len = app.len().min(32);
        hexdump(&format!("{}: app_stream HEAD", label), &app[..head_len], 0);

        // Tail: last 256 bytes (or less).
        let tail_start = app.len().saturating_sub(256);
        hexdump(
            &format!("{}: app_stream TAIL", label),
            &app[tail_start..],
            tail_start,
        );

        // Greedy NDX walk from the last 128 bytes — surfaces trailing
        // NDX_DONE / NDX_DEL_STATS runs and identifies the summary start.
        let walk_start = app.len().saturating_sub(128);
        let (stop_at, ndxs) = trailing_ndx_walk(app, walk_start);
        eprintln!(
            "greedy NDX walk from offset {}: parsed {} ndx values {:?}, stopped at offset {} (tail = {} bytes)",
            walk_start,
            ndxs.len(),
            ndxs,
            stop_at,
            app.len() - stop_at,
        );
        if stop_at < app.len() {
            hexdump(
                &format!(
                    "{}: bytes AFTER last parseable NDX (summary candidate)",
                    label
                ),
                &app[stop_at..],
                stop_at,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// real_wire — Sinergia 8g summary frame decoder vs frozen oracle.
//
// Target: the download server->client direction, where the server is the
// sender and therefore emits `handle_stats(f_out)` per main.c:960. The
// full wire layout after the flist on this stream is:
//   flist_entry + 0x00 (terminator) + 0x00 (io_error) + NDX_FLIST_EOF
//   + ndx + iflags + sum_head + delta_stream + file_csum
//   + N × NDX_DONE (send_files phase transitions — sender.c:246/254/460 —
//     count depends on inc_recurse and max_phase, typically 2-4)
//   + summary (5 × varlong(3) for proto 31)
//   + [trailing NDX_DONE from read_final_goodbye, may or may not be
//      captured depending on proxy tee flush timing]
// ---------------------------------------------------------------------------

/// Count of trailing NDX_DONE markers (`0x00` single bytes) emitted by
/// `send_files` between the last file_csum and `handle_stats` in the
/// download server-to-client direction on the frozen oracle. Derived
/// from sender.c:246/254/460 combined with max_phase + inc_recurse
/// defaults at proto 31. A greedy "consume all 0x00" drain would
/// over-consume because the summary's first varlong can legitimately
/// start with 0x00 (small total_read encoded as `00 xx xx`).
///
/// Pinned here so a regression changing the phase logic fails loudly
/// instead of silently shifting the summary decode point.
const FROZEN_ORACLE_PRE_SUMMARY_NDX_DONE_COUNT: usize = 3;

/// Consume exactly `count` trailing NDX_DONE markers. Used for
/// integration decode on the frozen oracle where the count is known.
fn consume_exact_ndx_done(app: &[u8], start: usize, count: usize) -> usize {
    for i in 0..count {
        assert_eq!(
            app[start + i],
            0x00,
            "expected NDX_DONE at offset {} (count {} of {})",
            start + i,
            i,
            count
        );
    }
    start + count
}

/// Helper: decode the full app stream of the download server->client
/// direction up to but NOT including the trailing NDX_DONE + summary
/// + NDX_DONE tail.
///
/// Returns the offset at which the file_csum ends — the caller can
/// then step over the session-level trailer.
fn decode_download_s2c_up_to_file_csum(
    app: &[u8],
    opts: &FileListDecodeOptions<'_>,
) -> (usize, Vec<u8>) {
    let (_entry, flist_end) = decode_file_list_entry(app, opts).unwrap();
    let mut cursor = flist_end;

    // end_of_flist terminator + io_error.
    assert_eq!(app[cursor], 0x00, "end_of_flist terminator must be 0x00");
    cursor += 1;
    assert_eq!(app[cursor], 0x00, "io_error byte must be 0x00");
    cursor += 1;

    let mut ndx_state = NdxState::new();
    let (ndx_eof, n) = decode_ndx(&app[cursor..], &mut ndx_state).unwrap();
    assert_eq!(ndx_eof, NDX_FLIST_EOF);
    cursor += n;

    // First (and only) file ndx + iflags + sum_head.
    let (file_ndx, n) = decode_ndx(&app[cursor..], &mut ndx_state).unwrap();
    assert!(
        file_ndx >= 0,
        "file ndx must be non-negative, got {}",
        file_ndx
    );
    cursor += n;
    let (_iflags, n) = decode_item_flags(&app[cursor..]).unwrap();
    cursor += n;
    let (sum_head, n) = decode_sum_head(&app[cursor..]).unwrap();
    cursor += n;

    // Delta stream + file_csum. `decode_delta_stream` terminates on
    // END_FLAG and then reads exactly `file_checksum_len` bytes — this
    // is the FILE-level strong checksum negotiated upfront (xxh128 =
    // 16 bytes for the frozen oracle), NOT the per-block strong
    // checksum length carried in sum_head (which is 2 bytes here).
    const FROZEN_ORACLE_FILE_CHECKSUM_LEN: usize = 16;
    let (report, n) = decode_delta_stream(
        &app[cursor..],
        FROZEN_ORACLE_FILE_CHECKSUM_LEN,
        Some(sum_head.count),
    )
    .unwrap_or_else(|e| panic!("decode_delta_stream failed: {:?}", e));
    cursor += n;

    (cursor, report.file_checksum)
}

#[test]
fn real_wire_decodes_summary_frame_from_frozen_download_s2c_stream() {
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    // Reassemble the app stream of download server->client (server is
    // sender; summary frame is emitted here per main.c:347-352).
    let spre = decode_server_preamble(&transcript.download_server_to_client).unwrap();
    let report =
        reassemble_msg_data(&transcript.download_server_to_client[spre.consumed..]).unwrap();
    assert!(
        report.out_of_band.is_empty(),
        "download s->c must be pure MSG_DATA"
    );
    let app = &report.app_stream;

    let opts = FileListDecodeOptions::frozen_oracle_default();
    let (after_csum, file_csum) = decode_download_s2c_up_to_file_csum(app, &opts);
    assert_eq!(file_csum.len(), 16, "xxh128 file checksum must be 16 bytes");
    assert!(after_csum < app.len(), "must have bytes left for trailer");

    // send_files emits a fixed number of trailing NDX_DONE markers
    // before handle_stats writes the summary. The count comes from
    // sender.c:246/254/460 (phase transitions + final write_ndx) and
    // is pinned for the frozen oracle — a greedy drain would
    // over-consume because a small total_read varlong can legitimately
    // start with 0x00.
    let summary_start =
        consume_exact_ndx_done(app, after_csum, FROZEN_ORACLE_PRE_SUMMARY_NDX_DONE_COUNT);

    // Protocol 31 → 5 × varlong(3). Decode the summary frame.
    let (summary, summary_consumed) = decode_summary_frame(&app[summary_start..], 31)
        .unwrap_or_else(|e| panic!("decode_summary_frame failed: {:?}", e));

    // Sanity checks against the known frozen profile. The transfer
    // fixture is a 262_144-byte file seeded with a real-live marker.
    assert_eq!(
        summary.total_size, 262_144,
        "total_size must match fixture file size"
    );
    assert!(summary.flist_buildtime.is_some());
    assert!(summary.flist_xfertime.is_some());
    assert!(
        summary.total_read >= 0 && summary.total_read < 10_000_000,
        "total_read must be a sane sender byte count, got {}",
        summary.total_read
    );
    assert!(
        summary.total_written >= 0 && summary.total_written < 10_000_000,
        "total_written must be a sane sender byte count, got {}",
        summary.total_written
    );
    assert!(
        summary.flist_buildtime.unwrap() >= 0,
        "flist_buildtime must be non-negative"
    );
    assert!(
        summary.flist_xfertime.unwrap() >= 0,
        "flist_xfertime must be non-negative"
    );

    // After summary, read_final_goodbye's write_ndx (main.c:887) may
    // emit one trailing NDX_DONE. The capture proxy can cut the tee
    // before this last write is flushed, so accept 0 or 1.
    let tail_cursor = summary_start + summary_consumed;
    let trailing_zeros = app[tail_cursor..]
        .iter()
        .take_while(|&&b| b == 0x00)
        .count();
    assert_eq!(
        tail_cursor + trailing_zeros,
        app.len(),
        "non-zero bytes remain after summary tail"
    );
    assert!(
        trailing_zeros <= 4,
        "trailing NDX_DONE run after summary suspiciously long ({})",
        trailing_zeros
    );
}

#[test]
fn real_wire_summary_frame_byte_accounting_closes_on_download_s2c_stream() {
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    let spre = decode_server_preamble(&transcript.download_server_to_client).unwrap();
    let report =
        reassemble_msg_data(&transcript.download_server_to_client[spre.consumed..]).unwrap();
    let app = &report.app_stream;

    let opts = FileListDecodeOptions::frozen_oracle_default();
    let (after_csum, _) = decode_download_s2c_up_to_file_csum(app, &opts);

    // Inter-phase NDX_DONE markers + summary + optional trailing
    // NDX_DONE must exactly consume the rest of the app stream.
    let summary_start =
        consume_exact_ndx_done(app, after_csum, FROZEN_ORACLE_PRE_SUMMARY_NDX_DONE_COUNT);
    let (_, summary_len) = decode_summary_frame(&app[summary_start..], 31).unwrap();
    let tail_start = summary_start + summary_len;
    let trailing_zeros = app[tail_start..].iter().take_while(|&&b| b == 0x00).count();
    assert_eq!(
        tail_start + trailing_zeros,
        app.len(),
        "byte accounting did not close on download s->c stream"
    );
}

#[test]
fn real_wire_summary_frame_flist_times_in_realistic_range() {
    // Separate assertion so a later regression in time encoding lands
    // on a dedicated test name (easier triage than a wall-of-asserts
    // in the primary test).
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    let spre = decode_server_preamble(&transcript.download_server_to_client).unwrap();
    let report =
        reassemble_msg_data(&transcript.download_server_to_client[spre.consumed..]).unwrap();
    let app = &report.app_stream;

    let opts = FileListDecodeOptions::frozen_oracle_default();
    let (after_csum, _) = decode_download_s2c_up_to_file_csum(app, &opts);

    let summary_start =
        consume_exact_ndx_done(app, after_csum, FROZEN_ORACLE_PRE_SUMMARY_NDX_DONE_COUNT);
    let (summary, _) = decode_summary_frame(&app[summary_start..], 31).unwrap();

    // The frozen oracle is a local Docker transfer of a 262 KiB file.
    // flist_buildtime + flist_xfertime are in seconds (wall-clock deltas
    // cached in `stats`). Their sum should be < 10 (very loose bound
    // against an unexpected unit change or wide-int regression).
    let sum = summary.flist_buildtime.unwrap() + summary.flist_xfertime.unwrap();
    assert!(
        (0..10).contains(&sum),
        "flist_buildtime + flist_xfertime = {} looks out of range — unit regression?",
        sum
    );
}

#[test]
fn real_wire_summary_frame_pre_ndx_done_count_matches_frozen_oracle() {
    // Hardening: pin the exact number of NDX_DONE markers emitted by
    // send_files between file_csum and summary. If a future rsync
    // version changes max_phase or inc_recurse behaviour this test
    // catches the shift BEFORE decode_summary_frame is called at the
    // wrong offset (where it would produce bogus totals).
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    let spre = decode_server_preamble(&transcript.download_server_to_client).unwrap();
    let report =
        reassemble_msg_data(&transcript.download_server_to_client[spre.consumed..]).unwrap();
    let app = &report.app_stream;
    let opts = FileListDecodeOptions::frozen_oracle_default();
    let (after_csum, _) = decode_download_s2c_up_to_file_csum(app, &opts);

    let count = app[after_csum..].iter().take_while(|&&b| b == 0x00).count();
    // The summary starts with total_read (small value → first byte 0x00),
    // so a greedy zero-drain would report 4. The pinned value is 3.
    // Verify the byte AT `after_csum + 3` is part of the summary varlong
    // and NOT a standalone NDX_DONE by checking that decoding a summary
    // from that offset yields total_size = 262_144.
    assert!(
        count >= FROZEN_ORACLE_PRE_SUMMARY_NDX_DONE_COUNT,
        "not enough leading zeros to cover {} NDX_DONE markers: {}",
        FROZEN_ORACLE_PRE_SUMMARY_NDX_DONE_COUNT,
        count
    );
    let summary_start = after_csum + FROZEN_ORACLE_PRE_SUMMARY_NDX_DONE_COUNT;
    let (summary, _) = decode_summary_frame(&app[summary_start..], 31).unwrap();
    assert_eq!(summary.total_size, 262_144);

    // Also pin the observed upper bound so a silent infrastructure
    // change (e.g. a capture that adds phases) fails here.
    assert!(
        count <= FROZEN_ORACLE_PRE_SUMMARY_NDX_DONE_COUNT + 1,
        "observed more leading zeros ({}) than expected — summary offset is drifting",
        count
    );
}

#[test]
fn real_wire_summary_frame_absent_on_upload_server_to_client_stream() {
    // Per main.c:346 the summary is emitted ONLY by server-sender. In
    // upload the server is receiver, so its s->c stream must NOT carry
    // a summary frame. This test pins that contract: the upload s->c
    // tail is sum_block payloads + trailing NDX_DONE markers, not a
    // summary block.
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    let spre = decode_server_preamble(&transcript.upload_server_to_client).unwrap();
    let report = reassemble_msg_data(&transcript.upload_server_to_client[spre.consumed..]).unwrap();
    let app = &report.app_stream;

    // Hard pin #1: the stream must terminate with a bare 0x00
    // (NDX_DONE), not with the last byte of a summary varlong. Every
    // proto-31 summary frame ends with the last byte of
    // `flist_xfertime`'s varlong — which for any realistic non-zero
    // time value is NOT zero.
    assert_eq!(
        app[app.len() - 1],
        0x00,
        "upload s->c must terminate with NDX_DONE, not summary data"
    );

    // Hard pin #2: a sane lower bound on the number of trailing 0x00
    // bytes. The upload s->c direction ends with a multi-phase
    // NDX_DONE sequence (receiver→sender path signals end-of-session
    // on both phases). If we were to wrongly assume a summary sits
    // here, the first 0x00 would encode summary_total_read=0 and the
    // field-order pin would explode before any trailing NDX_DONE is
    // even reached. Having MORE than 2 trailing zero bytes on this
    // stream rules out a summary ever being present.
    let mut trailing_zeros = 0usize;
    for &b in app.iter().rev() {
        if b == 0x00 {
            trailing_zeros += 1;
        } else {
            break;
        }
    }
    assert!(
        trailing_zeros >= 3,
        "upload s->c tail has only {} trailing zero bytes — unexpected layout",
        trailing_zeros
    );
}

// ---------------------------------------------------------------------------
// real_wire — Sinergia 8f-bis literal decompression vs frozen oracle.
//
// S8f left `DeltaOp::Literal { compressed_payload }` as opaque zstd
// bytes. S8f-bis wires the `zstd` crate (feature-gated) so callers can
// recover the raw uncompressed file bytes. The frozen oracle's upload
// client->server delta stream has TWO Literal records whose combined
// uncompressed length is 700 bytes — the same number that shows up in
// sum_head.remainder (block_length 700 + 375 full blocks = file_size
// 262_144, with the 700-byte trailing segment covered by literals).
// ---------------------------------------------------------------------------

#[test]
fn real_wire_decompresses_upload_c2s_literals_to_700_bytes_total() {
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    let cpre = decode_client_preamble(&transcript.upload_client_to_server).unwrap();
    let app = reassemble_msg_data(&transcript.upload_client_to_server[cpre.consumed..])
        .unwrap()
        .app_stream;

    let opts = FileListDecodeOptions::frozen_oracle_default();
    let (_, entry_bytes) = decode_file_list_entry(&app, &opts).unwrap();
    let (header_end, head) = advance_past_sum_head(&app, entry_bytes);

    let (report, _) = decode_delta_stream(
        &app[header_end..],
        FROZEN_FILE_CHECKSUM_LEN,
        Some(head.count),
    )
    .unwrap();

    let literal_slices: Vec<&[u8]> = report
        .ops
        .iter()
        .filter_map(|op| match op {
            DeltaOp::Literal { compressed_payload } => Some(compressed_payload.as_slice()),
            _ => None,
        })
        .collect();
    assert_eq!(
        literal_slices.len(),
        2,
        "frozen oracle carries exactly two Literal records on upload c->s"
    );

    // Feed all literals through a single streaming decoder (one zstd
    // context per session — see token.c:681 `ZSTD_e_continue`).
    let decompressed = decompress_zstd_literal_stream(&literal_slices)
        .unwrap_or_else(|e| panic!("literal stream decompression failed: {:?}", e));
    assert_eq!(
        decompressed.len(),
        700,
        "combined uncompressed literal size must equal sum_head.remainder=700"
    );
}

#[test]
fn real_wire_decompressed_upload_literal_contains_real_live_upload_marker() {
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    let cpre = decode_client_preamble(&transcript.upload_client_to_server).unwrap();
    let app = reassemble_msg_data(&transcript.upload_client_to_server[cpre.consumed..])
        .unwrap()
        .app_stream;

    let opts = FileListDecodeOptions::frozen_oracle_default();
    let (_, entry_bytes) = decode_file_list_entry(&app, &opts).unwrap();
    let (header_end, head) = advance_past_sum_head(&app, entry_bytes);

    let (report, _) = decode_delta_stream(
        &app[header_end..],
        FROZEN_FILE_CHECKSUM_LEN,
        Some(head.count),
    )
    .unwrap();

    const MARKER: &[u8] = b"real-live-upload";
    let literal_slices: Vec<&[u8]> = report
        .ops
        .iter()
        .filter_map(|op| match op {
            DeltaOp::Literal { compressed_payload } => Some(compressed_payload.as_slice()),
            _ => None,
        })
        .collect();
    let raw = decompress_zstd_literal_stream(&literal_slices).unwrap();
    assert!(
        raw.windows(MARKER.len()).any(|w| w == MARKER),
        "expected `real-live-upload` marker in the DECOMPRESSED literal bytes"
    );
}

#[test]
fn real_wire_decompressed_literals_cover_full_700_bytes_and_carry_zstd_magic() {
    // Hardening: the two Literal records combined must produce exactly
    // 700 uncompressed bytes (sum_head.remainder on the fixture) and
    // the first compressed payload must start with the ZSTD frame
    // magic — so a silent swap to a bare-deflate container would
    // fail here before the session even begins.
    //
    // NOTE: the decompressed bytes are NOT a contiguous slice of the
    // original file — rsync emits literals for the gaps between
    // matched block runs. For a multi-run delta with overlapping
    // matches (as on this fixture), literals cover sparse file
    // regions. A pattern check like `(i % 256)` would be wrong.
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping: frozen real-rsync transcript not available");
        return;
    };

    let cpre = decode_client_preamble(&transcript.upload_client_to_server).unwrap();
    let app = reassemble_msg_data(&transcript.upload_client_to_server[cpre.consumed..])
        .unwrap()
        .app_stream;

    let opts = FileListDecodeOptions::frozen_oracle_default();
    let (_, entry_bytes) = decode_file_list_entry(&app, &opts).unwrap();
    let (header_end, head) = advance_past_sum_head(&app, entry_bytes);

    let (report, _) = decode_delta_stream(
        &app[header_end..],
        FROZEN_FILE_CHECKSUM_LEN,
        Some(head.count),
    )
    .unwrap();

    let literal_slices: Vec<&[u8]> = report
        .ops
        .iter()
        .filter_map(|op| match op {
            DeltaOp::Literal { compressed_payload } => Some(compressed_payload.as_slice()),
            _ => None,
        })
        .collect();
    assert_eq!(literal_slices.len(), 2);
    // Only the first literal in a session starts with the ZSTD frame
    // magic — subsequent literals are flush-block continuations of
    // the same frame and carry only block headers.
    assert_eq!(
        &literal_slices[0][..4],
        &[0x28, 0xB5, 0x2F, 0xFD],
        "first literal must begin with the ZSTD frame magic"
    );

    let raw = decompress_zstd_literal_stream(&literal_slices).unwrap();
    assert_eq!(raw.len(), 700);
}

// =============================================================================
// Sinergia 8h — OOB event integration vs frozen oracle.
// =============================================================================

/// Pin the empirical observation captured by the S8e/S8g scouts: the
/// frozen oracle has ZERO out-of-band frames on every direction. Any
/// regression here means either the capture changed or a previous-step
/// decoder is mis-reading the demux high byte. Use as canary.
#[test]
fn real_wire_frozen_oracle_has_zero_oob_events_on_all_four_streams() {
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping S8h frozen oracle OOB pin: frozen capture not present");
        return;
    };
    let server_pre = decode_server_preamble(&transcript.upload_server_to_client).unwrap();
    let client_pre = decode_client_preamble(&transcript.upload_client_to_server).unwrap();
    let server_pre_dl = decode_server_preamble(&transcript.download_server_to_client).unwrap();
    let client_pre_dl = decode_client_preamble(&transcript.download_client_to_server).unwrap();

    let streams: [(&str, &[u8]); 4] = [
        (
            "upload_server_to_client",
            &transcript.upload_server_to_client[server_pre.consumed..],
        ),
        (
            "upload_client_to_server",
            &transcript.upload_client_to_server[client_pre.consumed..],
        ),
        (
            "download_server_to_client",
            &transcript.download_server_to_client[server_pre_dl.consumed..],
        ),
        (
            "download_client_to_server",
            &transcript.download_client_to_server[client_pre_dl.consumed..],
        ),
    ];

    for (name, mux_tail) in streams {
        let report = reassemble_with_events(mux_tail).unwrap();
        assert!(
            report.events.is_empty(),
            "stream {name} unexpectedly carries OOB events: {:?}",
            report.events
        );
        assert!(
            report.terminal.is_none(),
            "stream {name} unexpectedly hit a terminal event"
        );
        // Cross-check the legacy reassembly contract — `app_stream`
        // must be byte-identical between the two entry points.
        let legacy = reassemble_msg_data(mux_tail).unwrap();
        assert_eq!(
            legacy.app_stream, report.app_stream,
            "reassemble_with_events drifted from reassemble_msg_data on {name}"
        );
        assert!(
            legacy.oob_frames.is_empty(),
            "stream {name} legacy oob_frames not empty"
        );
    }
}

/// End-to-end demonstration of how the future S8i driver will use the
/// new event layer: take a mux byte buffer, run it through
/// `reassemble_until_terminal`, drain the events into a `BailingSink`,
/// and verify the bail semantics + payload preservation.
///
/// This test exercises the COMBINED path (real_wire + events) on a
/// hand-crafted buffer that mimics the shape of a real download stream
/// truncated by a mid-session error: a few app-stream frames, a
/// non-terminal warning, then a fatal `MSG_ERROR`, then app-stream
/// bytes that the receiver MUST NOT consume.
#[test]
fn real_wire_events_mock_driver_bails_cleanly_on_mid_session_error() {
    let mut buf = Vec::new();
    // Pretend file-list entry bytes.
    buf.extend_from_slice(
        &MuxHeader {
            tag: MuxTag::Data,
            length: 5,
        }
        .encode(),
    );
    buf.extend_from_slice(b"FLIST");
    // Receiver sends a benign info frame.
    buf.extend_from_slice(
        &MuxHeader {
            tag: MuxTag::Info,
            length: 12,
        }
        .encode(),
    );
    buf.extend_from_slice(b"recv ready\r\n");
    // Pretend ndx + sum_head bytes.
    buf.extend_from_slice(
        &MuxHeader {
            tag: MuxTag::Data,
            length: 4,
        }
        .encode(),
    );
    buf.extend_from_slice(b"NDXS");
    // Sender warning before the failure (still recorded).
    buf.extend_from_slice(
        &MuxHeader {
            tag: MuxTag::Warning,
            length: 6,
        }
        .encode(),
    );
    buf.extend_from_slice(b"slow\r\n");
    // Fatal error mid-transfer.
    buf.extend_from_slice(
        &MuxHeader {
            tag: MuxTag::Error,
            length: 26,
        }
        .encode(),
    );
    buf.extend_from_slice(b"file system full on remote");
    // The remote may push trailing bytes after the failure (RST race,
    // pending sender writes). They MUST NOT touch the app stream.
    buf.extend_from_slice(
        &MuxHeader {
            tag: MuxTag::Data,
            length: 8,
        }
        .encode(),
    );
    buf.extend_from_slice(b"GHOSTBYT");
    buf.extend_from_slice(
        &MuxHeader {
            tag: MuxTag::ErrorExit,
            length: 4,
        }
        .encode(),
    );
    buf.extend_from_slice(&[0x0B, 0x00, 0x00, 0x00]); // RERR_FILEIO=11

    let report = reassemble_until_terminal(&buf).unwrap();

    // App stream stops at "FLIST" + "NDXS" — the trailing GHOSTBYT
    // is never appended.
    assert_eq!(report.app_stream, b"FLISTNDXS");

    // The non-terminal Info + Warning are surfaced as classified events
    // with newlines stripped (mirrors log.c:353 display semantics).
    assert_eq!(report.events.len(), 2);
    match &report.events[0] {
        NativeRsyncEvent::Info { message } => assert_eq!(message, "recv ready"),
        other => panic!("expected Info, got {other:?}"),
    }
    match &report.events[1] {
        NativeRsyncEvent::Warning { message } => assert_eq!(message, "slow"),
        other => panic!("expected Warning, got {other:?}"),
    }

    // The terminal event is the Error frame, classified with the full
    // message text.
    let terminal = report.terminal.as_ref().expect("expected terminal event");
    match terminal {
        NativeRsyncEvent::Error { message } => {
            assert_eq!(message, "file system full on remote");
        }
        other => panic!("expected Error, got {other:?}"),
    }

    // Now feed everything (events + terminal) into a BailingSink the
    // way the future S8i driver will. The sink classification mirrors
    // `is_terminal` enforcement in the driver loop.
    let mut sink = BailingSink::default();
    for ev in report.events.into_iter() {
        sink.handle(ev);
    }
    if let Some(t) = report.terminal {
        sink.handle(t);
    }
    assert!(sink.bailed());
    assert_eq!(sink.before_terminal.len(), 2);
    match sink.first_terminal().unwrap() {
        NativeRsyncEvent::Error { message } => assert_eq!(message, "file system full on remote"),
        other => panic!("expected first terminal Error, got {other:?}"),
    }
    assert!(sink.trailing.is_empty());
}

/// Direct cross-check between the new `reassemble_with_events` path
/// and `events::classify_oob_frame` over the legacy `oob_frames` field
/// on the frozen oracle. Both routes MUST produce the same event list
/// for the same input bytes (single source of truth: the classifier).
#[test]
fn real_wire_events_classifier_agrees_with_reassemble_with_events_on_oob_frames() {
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping S8h classifier-vs-reassemble cross-check");
        return;
    };
    let server_pre = decode_server_preamble(&transcript.upload_server_to_client).unwrap();
    let mux_tail = &transcript.upload_server_to_client[server_pre.consumed..];

    let legacy = reassemble_msg_data(mux_tail).unwrap();
    let from_oob: Vec<NativeRsyncEvent> = legacy
        .oob_frames
        .into_iter()
        .map(|(tag, payload)| classify_oob_frame(tag, &payload))
        .collect();

    let new = reassemble_with_events(mux_tail).unwrap();
    assert_eq!(from_oob, new.events);
}

/// Hand-crafted hardening: inject a mid-summary `MSG_ERROR` and prove
/// the consumer never sees a partially-decoded summary frame. The
/// scenario mirrors a realistic failure mode where the server's
/// `handle_stats()` write (`main.c:960`) is interrupted by a remote
/// `rwrite(FERROR, …)` half-way through the 5 varlong fields.
#[test]
fn real_wire_events_terminal_inside_summary_region_yields_no_partial_summary() {
    use crate::rsync_native_proto::real_wire::encode_summary_frame;
    use crate::rsync_native_proto::real_wire::SummaryFrame;

    let summary = encode_summary_frame(
        &SummaryFrame {
            total_read: 12_345,
            total_written: 678,
            total_size: 262_144,
            flist_buildtime: Some(1),
            flist_xfertime: Some(0),
        },
        31,
    );

    let err_msg = b"summary aborted";
    let mut buf = Vec::new();
    // First 7 bytes of summary — half-shipped before the error.
    buf.extend_from_slice(
        &MuxHeader {
            tag: MuxTag::Data,
            length: 7,
        }
        .encode(),
    );
    buf.extend_from_slice(&summary[..7]);
    // Error mid-summary.
    buf.extend_from_slice(
        &MuxHeader {
            tag: MuxTag::Error,
            length: err_msg.len() as u32,
        }
        .encode(),
    );
    buf.extend_from_slice(err_msg);
    // Trailer that MUST NOT land in app_stream.
    buf.extend_from_slice(
        &MuxHeader {
            tag: MuxTag::Data,
            length: (summary.len() - 7) as u32,
        }
        .encode(),
    );
    buf.extend_from_slice(&summary[7..]);

    let report = reassemble_until_terminal(&buf).unwrap();
    // Only the first 7 bytes of summary land in app_stream — the
    // trailer is dropped, so attempting to decode a summary off this
    // partial buffer would correctly fail with TruncatedBuffer.
    assert_eq!(report.app_stream.len(), 7);
    assert_eq!(report.app_stream, &summary[..7]);
    assert!(matches!(
        report.terminal.as_ref().unwrap(),
        NativeRsyncEvent::Error { message } if message == "summary aborted"
    ));
}

// =============================================================================
// Sinergia 8i-encode — byte-identical writers vs frozen oracle.
// =============================================================================

/// Re-encode the frozen oracle's server preamble through
/// `encode_server_preamble` and verify the bytes match the captured
/// prefix exactly. The TRUE byte-identical pin: a regression in any of
/// the 5 sub-encoders (varint, ASCII algo lists, checksum_seed) would
/// surface here against real captured bytes, not synthetic round-trip.
#[test]
fn encode_server_preamble_matches_frozen_oracle_byte_for_byte() {
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping S8i-encode preamble byte-identical pin: frozen capture not present");
        return;
    };
    let original = decode_server_preamble(&transcript.upload_server_to_client).unwrap();
    let re_encoded = encode_server_preamble(&original);
    let frozen_prefix = &transcript.upload_server_to_client[..original.consumed];
    assert_eq!(
        re_encoded,
        frozen_prefix,
        "encode_server_preamble drifted from frozen bytes (len {} vs {})",
        re_encoded.len(),
        frozen_prefix.len(),
    );
}

#[test]
fn encode_client_preamble_matches_frozen_oracle_byte_for_byte() {
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping S8i-encode client preamble byte-identical pin");
        return;
    };
    let original = decode_client_preamble(&transcript.upload_client_to_server).unwrap();
    let re_encoded = encode_client_preamble(&original);
    let frozen_prefix = &transcript.upload_client_to_server[..original.consumed];
    assert_eq!(
        re_encoded, frozen_prefix,
        "encode_client_preamble drifted from frozen bytes",
    );
}

/// Re-encode the first file-list entry from the frozen upload
/// transcript and verify byte-identical match. Pin the most demanding
/// encoder (12 sub-fields gated by 7 distinct XMIT_* flags + varint /
/// varlong / utf-8 paths / xxh128 checksum) at the wire level.
#[test]
fn encode_file_list_entry_matches_frozen_oracle_byte_for_byte() {
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping S8i-encode flist entry byte-identical pin");
        return;
    };
    let preamble = decode_client_preamble(&transcript.upload_client_to_server).unwrap();
    let app = reassemble_msg_data(&transcript.upload_client_to_server[preamble.consumed..])
        .unwrap()
        .app_stream;

    let opts = FileListDecodeOptions::frozen_oracle_default();
    let (outcome, consumed) = decode_file_list_entry(&app, &opts).unwrap();
    let entry = match outcome {
        FileListDecodeOutcome::Entry(e) => e,
        other => panic!("expected Entry, got {other:?}"),
    };

    let re_encoded = encode_file_list_entry(&entry, &opts);
    let frozen_slice = &app[..consumed];
    assert_eq!(
        re_encoded.len(),
        frozen_slice.len(),
        "re-encoded length {} != frozen length {}",
        re_encoded.len(),
        frozen_slice.len(),
    );
    assert_eq!(
        re_encoded,
        frozen_slice,
        "encode_file_list_entry drifted from frozen bytes; first divergence at offset {:?}",
        re_encoded
            .iter()
            .zip(frozen_slice.iter())
            .position(|(a, b)| a != b)
    );
}

/// Re-encode the full delta stream from the frozen upload client→server
/// transcript and verify byte-identical match. Pin: `encode_delta_op`
/// must select the same wire form (TOKEN_REL / TOKEN_LONG / TOKENRUN_*
/// / DEFLATED_DATA) that rsync 3.2.7 picked for every op, AND the
/// `Literal` payloads must round-trip raw byte-for-byte (the zstd
/// payload is opaque to the outer encoder — what we verify here is the
/// outer framing including the 2-byte DEFLATED_DATA header). Trailing
/// `END_FLAG` + 16-byte file checksum must also match.
#[test]
fn encode_delta_stream_matches_frozen_oracle_byte_for_byte() {
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping S8i-encode delta stream byte-identical pin");
        return;
    };
    let preamble = decode_client_preamble(&transcript.upload_client_to_server).unwrap();
    let app = reassemble_msg_data(&transcript.upload_client_to_server[preamble.consumed..])
        .unwrap()
        .app_stream;

    // Mirrors real_wire_decodes_full_delta_stream_from_frozen_upload_client_stream:
    // the client→server upload stream is `flist + sum_head` (NO sum_blocks
    // here — those are emitted by the server on the OTHER direction)
    // followed directly by the delta stream. `advance_past_sum_head`
    // consumes flist terminator + io_error + NDX_FLIST_EOF + ndx + iflags
    // + sum_head and lands at delta_start.
    let opts = FileListDecodeOptions::frozen_oracle_default();
    let (_, entry_bytes) = decode_file_list_entry(&app, &opts).unwrap();
    let (delta_start, head) = advance_past_sum_head(&app, entry_bytes);

    let (report, consumed) =
        decode_delta_stream(&app[delta_start..], 16, Some(head.count)).unwrap();

    let re_encoded = encode_delta_stream(&report);
    let frozen_slice = &app[delta_start..delta_start + consumed];
    assert_eq!(
        re_encoded.len(),
        frozen_slice.len(),
        "re-encoded delta stream length {} != frozen {}",
        re_encoded.len(),
        frozen_slice.len(),
    );
    assert_eq!(
        re_encoded,
        frozen_slice,
        "encode_delta_stream drifted from frozen bytes; first divergence at offset {:?}",
        re_encoded
            .iter()
            .zip(frozen_slice.iter())
            .position(|(a, b)| a != b)
    );
}

/// Re-encode every captured signature block from the frozen upload
/// stream and verify byte-identical match. Pins encode_sum_block at
/// the wire format used in the oracle's negotiated profile
/// (checksum_length = 2).
#[test]
fn encode_sum_block_matches_frozen_oracle_byte_for_byte() {
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping S8i-encode sum_block byte-identical pin");
        return;
    };
    let preamble = decode_server_preamble(&transcript.upload_server_to_client).unwrap();
    let app = reassemble_msg_data(&transcript.upload_server_to_client[preamble.consumed..])
        .unwrap()
        .app_stream;
    // Mirror real_wire_decodes_all_375_sum_blocks_from_frozen_upload_server_stream:
    // upload_server_to_client opens with `ndx + iflags + sum_head +
    // N×sum_block`. No flist on this direction (the client owns the flist
    // in upload mode).
    let mut cursor = 0usize;
    let mut state = NdxState::new();
    cursor += decode_ndx(&app[cursor..], &mut state).unwrap().1;
    cursor += decode_item_flags(&app[cursor..]).unwrap().1;
    let (head, head_bytes) = decode_sum_head(&app[cursor..]).unwrap();
    cursor += head_bytes;
    let strong_len = head.checksum_length as usize;
    let block_size = 4 + strong_len;
    for i in 0..head.count as usize {
        let frozen = &app[cursor..cursor + block_size];
        let (block, _) = decode_sum_block(frozen, strong_len).unwrap();
        let re_encoded = encode_sum_block(&block);
        assert_eq!(
            re_encoded, frozen,
            "sum_block #{i} drifted from frozen bytes (offset {cursor})",
        );
        cursor += block_size;
    }
}

/// End-to-end round trip vs frozen oracle's zstd literal payloads:
/// decompress the captured Literals to raw bytes (S8f-bis path), then
/// re-compress the raw bytes through `compress_zstd_literal_stream`,
/// then decompress the new blobs and assert byte-identical match.
///
/// Note: the re-compressed BYTES will not match the frozen oracle
/// blob-for-blob — zstd compression is non-deterministic across
/// versions / levels / context state — but the SEMANTIC round trip
/// (raw → compressed → raw') MUST be the identity. This pins the
/// sender↔receiver session-wide context discipline against real
/// captured payloads, not synthetic ones.
#[test]
fn compress_zstd_literal_stream_round_trips_through_frozen_oracle_payloads() {
    let Some(transcript) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
        eprintln!("skipping S8i-encode zstd round-trip vs frozen oracle");
        return;
    };
    let cpre = decode_client_preamble(&transcript.upload_client_to_server).unwrap();
    let app = reassemble_msg_data(&transcript.upload_client_to_server[cpre.consumed..])
        .unwrap()
        .app_stream;
    let opts = FileListDecodeOptions::frozen_oracle_default();
    let (_, entry_bytes) = decode_file_list_entry(&app, &opts).unwrap();
    let (delta_start, head) =
        crate::rsync_native_proto::tests::advance_past_sum_head(&app, entry_bytes);
    let (report, _) = decode_delta_stream(&app[delta_start..], 16, Some(head.count)).unwrap();

    // Step 1: decompress the captured Literals (S8f-bis verified path).
    let literal_slices: Vec<&[u8]> = report
        .ops
        .iter()
        .filter_map(|op| match op {
            DeltaOp::Literal { compressed_payload } => Some(compressed_payload.as_slice()),
            _ => None,
        })
        .collect();
    assert!(
        !literal_slices.is_empty(),
        "frozen oracle must have at least one zstd literal"
    );
    let raw = decompress_zstd_literal_stream(&literal_slices).unwrap();
    assert_eq!(
        raw.len(),
        700,
        "frozen oracle decompressed literal must total 700 bytes (sum_head.remainder)"
    );

    // Step 2: feed the raw bytes back through OUR encoder. We split
    // them at the same boundaries the original ones had — proving the
    // encoder's per-payload Flush discipline survives the round.
    let mut split_payloads: Vec<&[u8]> = Vec::new();
    let mut cursor = 0;
    for original in &literal_slices {
        // We don't know the original UNCOMPRESSED chunk sizes — they
        // are an artefact of rsync's matcher. So split raw evenly over
        // the same number of chunks as the frozen oracle (the
        // round-trip works regardless of chunk boundaries; this just
        // exercises the multi-payload session path).
        let chunk_size = raw.len() / literal_slices.len();
        let _ = original;
        let end = cursor + chunk_size.min(raw.len() - cursor);
        if end > cursor {
            split_payloads.push(&raw[cursor..end]);
        }
        cursor = end;
    }
    if cursor < raw.len() {
        split_payloads.push(&raw[cursor..]);
    }
    let blobs = compress_zstd_literal_stream(&split_payloads).unwrap();
    assert!(!blobs.is_empty(), "compressor must produce blobs");

    // Step 3: decompress the freshly-compressed blobs through the
    // session decoder and assert byte-identical match with `raw`.
    let blob_refs: Vec<&[u8]> = blobs.iter().map(|v| v.as_slice()).collect();
    let raw_again = decompress_zstd_literal_stream(&blob_refs).unwrap();
    assert_eq!(
        raw_again, raw,
        "round-trip raw -> compress -> decompress drifted from original"
    );
}
