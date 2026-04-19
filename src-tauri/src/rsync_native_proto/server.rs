//! Minimal RSNP server for live prototype tests.
//!
//! This server speaks the current prototype framing only. It is intentionally
//! not rsync-wire compatible. Its purpose is to validate transport, state,
//! cancellation, and file mutation over a real SSH exec stream.

use crate::rsync_native_proto::engine_adapter::{
    engine_ops_to_wire, CurrentDeltaSyncBridge, DeltaEngineAdapter,
    DeltaInstructionConversionError, EngineDeltaOp, EngineSignatureBlock,
};
use crate::rsync_native_proto::frame_io::{
    read_length_prefixed_frame, write_length_prefixed_frame,
};
use crate::rsync_native_proto::protocol::{
    DeltaInstruction, FileMetadataMessage, FrameCodec, HelloMessage, NativeFrameCodec,
    SignatureBatchMessage, SignatureBlock, SummaryMessage, WireMessage,
};
use crate::rsync_native_proto::types::{FeatureFlag, ProtocolVersion, SessionRole};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtoServeMode {
    Upload,
    Download,
}

#[derive(Debug, Clone)]
pub struct ProtoServeOptions {
    pub mode: ProtoServeMode,
    pub target: PathBuf,
    pub protocol: ProtocolVersion,
    pub emit_stats: bool,
    pub max_frame_size: usize,
}

impl ProtoServeOptions {
    pub fn probe_banner(protocol: ProtocolVersion) -> String {
        format!("rsnp-proto server protocol {}", protocol.as_u32())
    }
}

pub fn serve_stdio(options: ProtoServeOptions) -> Result<(), String> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    let codec = NativeFrameCodec::new(options.max_frame_size);
    let bridge = CurrentDeltaSyncBridge::new();

    match options.mode {
        ProtoServeMode::Upload => serve_upload(&codec, &bridge, &options, &mut input, &mut output),
        ProtoServeMode::Download => {
            serve_download(&codec, &bridge, &options, &mut input, &mut output)
        }
    }
}

fn serve_upload<R: Read, W: Write>(
    codec: &NativeFrameCodec,
    bridge: &dyn DeltaEngineAdapter,
    options: &ProtoServeOptions,
    input: &mut R,
    output: &mut W,
) -> Result<(), String> {
    let hello = expect_hello(codec, input, SessionRole::Sender)?;
    ensure_protocol(options.protocol, hello.protocol)?;

    send_message(
        codec,
        output,
        &WireMessage::Hello(HelloMessage {
            protocol: options.protocol,
            role: SessionRole::Receiver,
            features: vec![FeatureFlag::DeltaTransfer, FeatureFlag::StructuredErrors],
        }),
    )?;

    let _meta = expect_file_metadata(codec, input)?;
    let basis = read_basis_file(&options.target)?;
    let block_size = bridge.compute_block_size(basis.len() as u64).max(1);
    let sigs: Vec<SignatureBlock> = bridge
        .build_signatures(&basis, block_size)
        .into_iter()
        .map(SignatureBlock::from)
        .collect();

    send_message(
        codec,
        output,
        &WireMessage::SignatureBatch(SignatureBatchMessage {
            block_size: block_size as u32,
            blocks: sigs.clone(),
        }),
    )?;

    let delta_batch = expect_delta_batch(codec, input)?;
    let ops = wire_delta_to_engine(delta_batch)?;
    let rebuilt = bridge
        .apply_delta(&basis, &ops, block_size)
        .map_err(|e| format!("apply_delta failed on upload: {e}"))?;
    atomic_write(&options.target, &rebuilt)?;

    let (literal_bytes, matched_bytes) = summarize_engine_ops(&ops, &sigs)?;
    send_message(
        codec,
        output,
        &WireMessage::Summary(SummaryMessage {
            bytes_sent: literal_bytes,
            bytes_received: matched_bytes,
            literal_bytes,
            matched_bytes,
        }),
    )?;
    expect_done(codec, input)?;
    Ok(())
}

fn serve_download<R: Read, W: Write>(
    codec: &NativeFrameCodec,
    bridge: &dyn DeltaEngineAdapter,
    options: &ProtoServeOptions,
    input: &mut R,
    output: &mut W,
) -> Result<(), String> {
    let hello = expect_hello(codec, input, SessionRole::Receiver)?;
    ensure_protocol(options.protocol, hello.protocol)?;

    send_message(
        codec,
        output,
        &WireMessage::Hello(HelloMessage {
            protocol: options.protocol,
            role: SessionRole::Sender,
            features: vec![FeatureFlag::DeltaTransfer, FeatureFlag::StructuredErrors],
        }),
    )?;

    let source = read_required_file(&options.target)?;
    send_message(
        codec,
        output,
        &WireMessage::FileMetadata(FileMetadataMessage {
            path: options.target.display().to_string(),
            size: source.len() as u64,
            mode: 0o644,
            modified_unix_secs: 0,
        }),
    )?;

    let sig_batch = expect_signature_batch(codec, input)?;
    let engine_sigs: Vec<EngineSignatureBlock> =
        sig_batch.blocks.into_iter().map(Into::into).collect();
    let plan = bridge.compute_delta(&source, &engine_sigs, sig_batch.block_size as usize);
    let literal_bytes = plan.literal_bytes;
    let matched_bytes = matched_bytes_from_plan(&plan, &engine_sigs)?;

    send_message(
        codec,
        output,
        &WireMessage::DeltaBatch(engine_ops_to_wire(plan.ops)),
    )?;
    send_message(
        codec,
        output,
        &WireMessage::Summary(SummaryMessage {
            bytes_sent: literal_bytes,
            bytes_received: matched_bytes,
            literal_bytes,
            matched_bytes,
        }),
    )?;
    expect_done(codec, input)?;
    Ok(())
}

fn expect_message<R: Read>(codec: &NativeFrameCodec, input: &mut R) -> Result<WireMessage, String> {
    let raw = read_length_prefixed_frame(input, codec.max_frame_size).map_err(|e| e.to_string())?;
    codec.decode(&raw).map_err(|e| e.to_string())
}

fn send_message<W: Write>(
    codec: &NativeFrameCodec,
    output: &mut W,
    msg: &WireMessage,
) -> Result<(), String> {
    let raw = codec.encode(msg).map_err(|e| e.to_string())?;
    write_length_prefixed_frame(output, &raw).map_err(|e| e.to_string())
}

fn expect_hello<R: Read>(
    codec: &NativeFrameCodec,
    input: &mut R,
    expected_role: SessionRole,
) -> Result<HelloMessage, String> {
    match expect_message(codec, input)? {
        WireMessage::Hello(hello) if hello.role == expected_role => Ok(hello),
        WireMessage::Hello(hello) => Err(format!(
            "unexpected hello role: expected {:?}, got {:?}",
            expected_role, hello.role
        )),
        other => Err(format!("expected Hello, got {:?}", other.message_type())),
    }
}

fn expect_file_metadata<R: Read>(
    codec: &NativeFrameCodec,
    input: &mut R,
) -> Result<FileMetadataMessage, String> {
    match expect_message(codec, input)? {
        WireMessage::FileMetadata(meta) => Ok(meta),
        other => Err(format!(
            "expected FileMetadata, got {:?}",
            other.message_type()
        )),
    }
}

fn expect_signature_batch<R: Read>(
    codec: &NativeFrameCodec,
    input: &mut R,
) -> Result<SignatureBatchMessage, String> {
    match expect_message(codec, input)? {
        WireMessage::SignatureBatch(batch) => Ok(batch),
        other => Err(format!(
            "expected SignatureBatch, got {:?}",
            other.message_type()
        )),
    }
}

fn expect_delta_batch<R: Read>(
    codec: &NativeFrameCodec,
    input: &mut R,
) -> Result<Vec<DeltaInstruction>, String> {
    match expect_message(codec, input)? {
        WireMessage::DeltaBatch(delta) => Ok(delta),
        other => Err(format!(
            "expected DeltaBatch, got {:?}",
            other.message_type()
        )),
    }
}

fn expect_done<R: Read>(codec: &NativeFrameCodec, input: &mut R) -> Result<(), String> {
    match expect_message(codec, input)? {
        WireMessage::Done => Ok(()),
        other => Err(format!("expected Done, got {:?}", other.message_type())),
    }
}

fn ensure_protocol(expected: ProtocolVersion, actual: ProtocolVersion) -> Result<(), String> {
    if actual != expected {
        return Err(format!(
            "protocol mismatch: expected {}, got {}",
            expected.as_u32(),
            actual.as_u32()
        ));
    }
    Ok(())
}

fn wire_delta_to_engine(delta: Vec<DeltaInstruction>) -> Result<Vec<EngineDeltaOp>, String> {
    let mut out = Vec::new();
    let mut seen_eof = false;
    for instruction in delta {
        match instruction {
            DeltaInstruction::EndOfFile => {
                seen_eof = true;
                break;
            }
            other => out.push(
                EngineDeltaOp::try_from(other)
                    .map_err(|e: DeltaInstructionConversionError| e.to_string())?,
            ),
        }
    }
    if !seen_eof {
        return Err("delta stream missing EndOfFile terminator".to_string());
    }
    Ok(out)
}

fn summarize_engine_ops(
    ops: &[EngineDeltaOp],
    sigs: &[SignatureBlock],
) -> Result<(u64, u64), String> {
    let mut literal = 0u64;
    let mut matched = 0u64;
    for op in ops {
        match op {
            EngineDeltaOp::Literal(bytes) => literal += bytes.len() as u64,
            EngineDeltaOp::CopyBlock(index) => {
                let block = sigs
                    .iter()
                    .find(|sig| sig.index == *index)
                    .ok_or_else(|| format!("missing signature block {}", index))?;
                matched += block.block_len as u64;
            }
        }
    }
    Ok((literal, matched))
}

fn matched_bytes_from_plan(
    plan: &crate::rsync_native_proto::engine_adapter::EngineDeltaPlan,
    sigs: &[EngineSignatureBlock],
) -> Result<u64, String> {
    let mut matched = 0u64;
    for op in &plan.ops {
        if let EngineDeltaOp::CopyBlock(index) = op {
            let block = sigs
                .iter()
                .find(|sig| sig.index == *index)
                .ok_or_else(|| format!("missing engine signature block {}", index))?;
            matched += block.block_len as u64;
        }
    }
    Ok(matched)
}

fn read_basis_file(path: &Path) -> Result<Vec<u8>, String> {
    if path.exists() {
        fs::read(path).map_err(|e| format!("read basis file {}: {e}", path.display()))
    } else {
        Ok(Vec::new())
    }
}

fn read_required_file(path: &Path) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|e| format!("read source file {}: {e}", path.display()))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("target {} has no parent directory", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|e| format!("create parent directory {}: {e}", parent.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("target {} has invalid file name", path.display()))?;
    let tmp = parent.join(format!(".{}.rsnp.tmp", file_name));
    fs::write(&tmp, bytes).map_err(|e| format!("write temp file {}: {e}", tmp.display()))?;
    fs::rename(&tmp, path)
        .map_err(|e| format!("rename {} -> {}: {e}", tmp.display(), path.display()))
}

#[cfg(test)]
mod tests {
    use super::{ProtoServeMode, ProtoServeOptions};
    use crate::rsync_native_proto::engine_adapter::{CurrentDeltaSyncBridge, DeltaEngineAdapter};
    use crate::rsync_native_proto::frame_io::{
        read_length_prefixed_frame, write_length_prefixed_frame,
    };
    use crate::rsync_native_proto::protocol::{
        FileMetadataMessage, FrameCodec, HelloMessage, NativeFrameCodec, SignatureBatchMessage,
        SignatureBlock, SummaryMessage, WireMessage,
    };
    use crate::rsync_native_proto::types::{FeatureFlag, ProtocolVersion, SessionRole};
    use std::fs;
    use std::io::Cursor;
    use tempfile::tempdir;

    fn codec() -> NativeFrameCodec {
        NativeFrameCodec::new(256 * 1024)
    }

    fn encode(msg: &WireMessage) -> Vec<u8> {
        codec().encode(msg).unwrap()
    }

    fn pack(msgs: &[WireMessage]) -> Vec<u8> {
        let mut out = Vec::new();
        for msg in msgs {
            write_length_prefixed_frame(&mut out, &encode(msg)).unwrap();
        }
        out
    }

    fn unpack(mut raw: &[u8]) -> Vec<WireMessage> {
        let codec = codec();
        let mut out = Vec::new();
        while !raw.is_empty() {
            let mut cursor = Cursor::new(raw);
            let frame = read_length_prefixed_frame(&mut cursor, codec.max_frame_size).unwrap();
            let consumed = cursor.position() as usize;
            out.push(codec.decode(&frame).unwrap());
            raw = &raw[consumed..];
        }
        out
    }

    #[test]
    fn upload_server_applies_delta_and_writes_target() {
        let temp = tempdir().unwrap();
        let target = temp.path().join("upload.bin");
        let bridge = CurrentDeltaSyncBridge::new();
        let basis = vec![1u8; 8192];
        let mut source = basis.clone();
        for byte in &mut source[1024..1024 + 256] {
            *byte = byte.wrapping_add(1);
        }
        fs::write(&target, &basis).unwrap();

        let bs = bridge.compute_block_size(basis.len() as u64);
        let sigs: Vec<SignatureBlock> = bridge
            .build_signatures(&basis, bs)
            .into_iter()
            .map(SignatureBlock::from)
            .collect();
        let engine_sigs = sigs.iter().cloned().map(Into::into).collect::<Vec<_>>();
        let plan = bridge.compute_delta(&source, &engine_sigs, bs);
        let delta = crate::rsync_native_proto::engine_adapter::engine_ops_to_wire(plan.ops);

        let input = pack(&[
            WireMessage::Hello(HelloMessage {
                protocol: ProtocolVersion::CURRENT,
                role: SessionRole::Sender,
                features: vec![FeatureFlag::DeltaTransfer],
            }),
            WireMessage::FileMetadata(FileMetadataMessage {
                path: target.display().to_string(),
                size: source.len() as u64,
                mode: 0o644,
                modified_unix_secs: 0,
            }),
            WireMessage::DeltaBatch(delta),
            WireMessage::Done,
        ]);
        let mut reader = Cursor::new(input);
        let mut writer = Vec::new();
        let options = ProtoServeOptions {
            mode: ProtoServeMode::Upload,
            target: target.clone(),
            protocol: ProtocolVersion::CURRENT,
            emit_stats: true,
            max_frame_size: 256 * 1024,
        };

        super::serve_upload(&codec(), &bridge, &options, &mut reader, &mut writer).unwrap();
        let output = unpack(&writer);
        assert!(matches!(output[0], WireMessage::Hello(_)));
        assert!(matches!(output[1], WireMessage::SignatureBatch(_)));
        assert!(matches!(
            output[2],
            WireMessage::Summary(SummaryMessage { .. })
        ));
        assert_eq!(fs::read(target).unwrap(), source);
    }

    #[test]
    fn download_server_emits_delta_and_summary() {
        let temp = tempdir().unwrap();
        let target = temp.path().join("download.bin");
        let bridge = CurrentDeltaSyncBridge::new();
        let mut source = vec![7u8; 8192];
        for byte in &mut source[2048..2048 + 128] {
            *byte = byte.wrapping_add(2);
        }
        let basis = vec![7u8; 8192];
        fs::write(&target, &source).unwrap();

        let bs = bridge.compute_block_size(basis.len() as u64);
        let sigs: Vec<SignatureBlock> = bridge
            .build_signatures(&basis, bs)
            .into_iter()
            .map(SignatureBlock::from)
            .collect();

        let input = pack(&[
            WireMessage::Hello(HelloMessage {
                protocol: ProtocolVersion::CURRENT,
                role: SessionRole::Receiver,
                features: vec![FeatureFlag::DeltaTransfer],
            }),
            WireMessage::SignatureBatch(SignatureBatchMessage {
                block_size: bs as u32,
                blocks: sigs,
            }),
            WireMessage::Done,
        ]);
        let mut reader = Cursor::new(input);
        let mut writer = Vec::new();
        let options = ProtoServeOptions {
            mode: ProtoServeMode::Download,
            target,
            protocol: ProtocolVersion::CURRENT,
            emit_stats: false,
            max_frame_size: 256 * 1024,
        };

        super::serve_download(&codec(), &bridge, &options, &mut reader, &mut writer).unwrap();
        let output = unpack(&writer);
        assert!(matches!(output[0], WireMessage::Hello(_)));
        assert!(matches!(output[1], WireMessage::FileMetadata(_)));
        assert!(matches!(output[2], WireMessage::DeltaBatch(_)));
        assert!(matches!(output[3], WireMessage::Summary(_)));
    }

    #[test]
    fn probe_banner_mentions_protocol() {
        let banner = ProtoServeOptions::probe_banner(ProtocolVersion::CURRENT);
        assert!(banner.contains("31"));
    }
}
