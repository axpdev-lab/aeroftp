//! Real-wire based session driver for the Strada C native rsync prototype.
//!
//! This module is the S8i replacement for the RSNP-envelope driver at
//! `driver.rs`. It lives **side-by-side** with the legacy driver (decision β,
//! approved 2026-04-18): the legacy driver stays untouched so its 270+ mock
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
//! # Negotiated file-checksum length (CLAUDE-AV-B3-18)
//!
//! File-list checksums and delta-stream trailers have no length prefix.
//! Their width must therefore follow the algorithm that won checksum
//! negotiation. [`AerorsyncDriver::negotiated_file_checksum_len`] mirrors
//! rsync 3.2.7 `checksum.c::csum_len_for_type`; assuming 16 made downloads
//! from `xxh64` and `xxh3` peers wait forever for eight bytes that were
//! never coming.

use crate::aerorsync::engine_adapter::{
    apply_delta_streaming, BaselineSource, BlockStrongAlgo, DeltaEngineAdapter, DeltaPlanProducer,
    EngineDeltaOp, EngineSignatureBlock, RollingDeltaPlanProducer,
};
use crate::aerorsync::events::EventSink;
use crate::aerorsync::real_wire::{
    compress_zstd_literal_stream, decode_delta_stream, decode_file_list_entry, decode_item_flags,
    decode_ndx, decode_server_preamble, decode_sum_block, decode_sum_head, decode_summary_frame,
    decode_varint, decompress_zstd_literal_stream_boundaries, encode_client_preamble,
    encode_delta_stream, encode_file_list_entry, encode_file_list_terminator, encode_item_flags,
    encode_ndx, encode_sum_block, encode_sum_head, encode_summary_frame,
    encode_xattr_datum_section, is_symlink_mode, ClientPreamble, DeltaOp, DeltaStreamReport,
    FileListDecodeOptions, FileListDecodeOutcome, FileListEntry, MuxHeader, MuxPoll,
    MuxStreamReader, MuxTag, NdxState, RealWireError, SumBlock, SumHead, SummaryFrame,
    MAX_DELTA_LITERAL_LEN, NDX_DONE, NDX_FLIST_EOF,
};
use crate::aerorsync::remote_command::{RemoteCommandFlavor, RemoteCommandSpec};
use crate::aerorsync::transport::{CancelHandle, RawByteStream, RawRemoteShellTransport};
use crate::aerorsync::types::{AerorsyncError, AerorsyncErrorKind, SessionRole, SessionStats};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt, AsyncWrite, SeekFrom};
use xxhash_rust::xxh3::{xxh3_128, xxh3_128_with_seed, xxh3_64, Xxh3Default};
use xxhash_rust::xxh64::{xxh64, Xxh64};

/// Compute the 16-byte file-level strong checksum rsync verifies at the
/// end of the delta stream when `xxh128` is the negotiated algo.
///
/// Byte layout: pinned by `xxh128_wire_bytes_match_SIVAL64_pair`
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
    compute_xxh128_wire_with_seed(data, 0)
}

fn compute_xxh128_wire_with_seed(data: &[u8], seed: u64) -> Vec<u8> {
    let hash = if seed == 0 {
        xxh3_128(data)
    } else {
        xxh3_128_with_seed(data, seed)
    };
    xxh128_wire_bytes(hash)
}

/// CLAUDE-AV-B3-12: split an xxh3-128 digest into rsync's 16-byte wire
/// layout. Extracted so the streaming sender, the bulk sender, and the
/// download-side whole-file verify all serialise the digest through one
/// place instead of three hand-rolled copies of the same byte order.
pub(crate) fn xxh128_wire_bytes(hash: u128) -> Vec<u8> {
    let lo = hash as u64;
    let hi = (hash >> 64) as u64;
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(&lo.to_le_bytes());
    out.extend_from_slice(&hi.to_le_bytes());
    out
}

/// `SumHead.remainder_length` for a `file_size`-byte baseline with the
/// given wire `block_length`: `file_size mod block_length`, computed in
/// u64 so baselines >= 2 GiB do not wrap through a signed 32-bit cast.
/// Mirrors `generator.c`'s derivation (`(int32)(size % blength)`); the
/// result always fits i32 because it is strictly smaller than
/// `block_length`, itself an i32 on the wire.
fn sum_head_remainder(file_size: u64, block_length: i32) -> i32 {
    if block_length <= 0 {
        return 0;
    }
    (file_size % block_length as u64) as i32
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileChecksumKind {
    Xxh128,
    Xxh3,
    Xxh64,
    Md5,
    Md4,
    Sha1,
}

impl FileChecksumKind {
    fn from_negotiated_name(name: Option<&str>) -> Self {
        match name {
            Some(XXH3_ALGO_NAME) => Self::Xxh3,
            Some(XXH64_ALGO_NAME) => Self::Xxh64,
            Some(MD5_ALGO_NAME) => Self::Md5,
            Some(MD4_ALGO_NAME) => Self::Md4,
            Some(SHA1_ALGO_NAME) => Self::Sha1,
            // Preserve the historical xxh128 behavior for an absent or
            // unsupported winner. The default production profile always
            // negotiates one of the explicitly supported algorithms.
            _ => Self::Xxh128,
        }
    }

    /// Whole-file digest for the delta trailer and the `--checksum`
    /// file-list field. UNSEEDED for every kind: rsync 3.2.7 `sum_init`
    /// ignores its seed argument on all the negotiable paths (only the
    /// pre-negotiation `CSUM_MD4_OLD/BUSTED/ARCHAIC` variants mix it in,
    /// and name negotiation can never select those), and `file_checksum`
    /// never seeds. Per-block digests seed via [`BlockStrongAlgo`]
    /// instead: that asymmetry is the B3-12 pin.
    fn digest(self, data: &[u8]) -> Vec<u8> {
        match self {
            Self::Xxh128 => compute_xxh128_wire(data),
            Self::Xxh3 => xxh3_64(data).to_le_bytes().to_vec(),
            Self::Xxh64 => xxh64(data, 0).to_le_bytes().to_vec(),
            Self::Md5 => {
                use md5::{Digest, Md5};
                Md5::digest(data).to_vec()
            }
            Self::Md4 => {
                use md4::{Digest, Md4};
                Md4::digest(data).to_vec()
            }
            Self::Sha1 => {
                use sha1::{Digest, Sha1};
                Sha1::digest(data).to_vec()
            }
        }
    }

    fn streaming_hasher(self) -> FileChecksumHasher {
        use md5::Digest;
        match self {
            Self::Xxh128 => FileChecksumHasher::Xxh128(Xxh3Default::new()),
            Self::Xxh3 => FileChecksumHasher::Xxh3(Xxh3Default::new()),
            Self::Xxh64 => FileChecksumHasher::Xxh64(Xxh64::new(0)),
            Self::Md5 => FileChecksumHasher::Md5(md5::Md5::new()),
            Self::Md4 => FileChecksumHasher::Md4(md4::Md4::new()),
            Self::Sha1 => FileChecksumHasher::Sha1(sha1::Sha1::new()),
        }
    }
}

enum FileChecksumHasher {
    Xxh128(Xxh3Default),
    Xxh3(Xxh3Default),
    Xxh64(Xxh64),
    Md5(md5::Md5),
    Md4(md4::Md4),
    Sha1(sha1::Sha1),
}

impl FileChecksumHasher {
    fn update(&mut self, data: &[u8]) {
        use md5::Digest;
        match self {
            Self::Xxh128(hasher) | Self::Xxh3(hasher) => hasher.update(data),
            Self::Xxh64(hasher) => hasher.update(data),
            Self::Md5(hasher) => Digest::update(hasher, data),
            Self::Md4(hasher) => Digest::update(hasher, data),
            Self::Sha1(hasher) => Digest::update(hasher, data),
        }
    }

    fn finish(self) -> Vec<u8> {
        use md5::Digest;
        match self {
            Self::Xxh128(hasher) => xxh128_wire_bytes(hasher.digest128()),
            Self::Xxh3(hasher) => hasher.digest().to_le_bytes().to_vec(),
            Self::Xxh64(hasher) => hasher.digest().to_le_bytes().to_vec(),
            Self::Md5(hasher) => hasher.finalize().to_vec(),
            Self::Md4(hasher) => hasher.finalize().to_vec(),
            Self::Sha1(hasher) => hasher.finalize().to_vec(),
        }
    }
}

/// Checksum algorithm names the download-side whole-file verify can
/// recompute in-tree. Peers that negotiate anything else (sha256,
/// sha512, none, ...) keep the pre-verify delta path untouched: the
/// check is a deliberate no-op for unimplemented algorithms so a verify
/// that assumed the wrong digest cannot silently disable delta forever.
pub(crate) const XXH128_ALGO_NAME: &str = "xxh128";
/// CLAUDE-AV-B3-14: md5 whole-file trailer, the practical fallback
/// real rsync uses when xxh* is unavailable. Same 16-byte length as
/// xxh128 (`A2_3_FILE_CHECKSUM_LEN`), unseeded (see `sum_init` for
/// `CSUM_MD5` in rsync 3.2.7). CLAUDE-AV-B3-17: also the per-block
/// strong algo via `get_checksum2` (seeded; see `CF_CHKSUM_SEED_FIX`).
pub(crate) const MD5_ALGO_NAME: &str = "md5";
/// CLAUDE-AV-B3-18: rsync's two 64-bit xxhash names. Despite its name,
/// `xxh3` is the 8-byte variant; the 16-byte variant is `xxh128`.
pub(crate) const XXH3_ALGO_NAME: &str = "xxh3";
pub(crate) const XXH64_ALGO_NAME: &str = "xxh64";
/// Y-RSC.3: legacy md4, the last-resort compatibility entry of the
/// advertisement. Whole-file trailer and file-list digest are UNSEEDED
/// (rsync 3.2.7 `sum_init` mixes the seed only for the pre-negotiation
/// `CSUM_MD4_OLD/BUSTED/ARCHAIC` variants, never for the negotiated
/// `CSUM_MD4`); the per-block strong via the builtin `get_checksum2`
/// branch APPENDS the seed to the data (see [`BlockStrongAlgo::Md4`]).
pub(crate) const MD4_ALGO_NAME: &str = "md4";
/// Y-RSC.3: sha1, negotiable when a peer advertises it (stock rsync
/// builds it via OpenSSL EVP). NOT part of our default advertisement;
/// reachable through the `AEROFTP_RSYNC_CSUM_ALGOS` override. 20-byte
/// digest; whole-file trailer unseeded, per-block seeded seed-first
/// (see [`BlockStrongAlgo::Sha1`]).
pub(crate) const SHA1_ALGO_NAME: &str = "sha1";

/// rsync.h `CF_CHKSUM_SEED_FIX` (`1<<5`). When set in the server's
/// `compat_flags`, `proper_seed_order=1` and `get_checksum2` for
/// `CSUM_MD5` feeds seed LE bytes *before* the block data. Without it
/// the legacy order is data then seed. CLAUDE-AV-B3-17.
const CF_CHKSUM_SEED_FIX: i32 = 1 << 5;

/// Chunk size used for raw-stream reads. Large enough to swallow a full
/// preamble + file list in one go for small transfers, small enough not
/// to bloat the scratch buffer for idle-ish channels.
const RAW_READ_CHUNK: usize = 8192;

/// P3-T01 W1.2: read-side chunking for the streaming source reader of
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
// when it wants the sender to push actual delta bytes: i.e. every file
// that we are about to exchange signatures for. `ITEM_REPORT_CHANGE` is
// the common companion bit telling the client "log this as changed".
// Neither mapping belongs in a shared module yet; when a second
// consumer emerges in S8j we will promote to `real_wire.rs`.
const ITEM_TRANSFER: u16 = 0x8000;
const ITEM_REPORT_CHANGE: u16 = 0x0002;
/// X.2b: `ITEM_REPORT_XATTR` (`1<<8` in `rsync.h`). Set on a per-file
/// iflags word when that entry carries extended attributes, and it is
/// this bit, not the presence of any over-threshold value, that says an
/// out-of-band xattr datum section follows the header on the wire.
///
/// Measured as the difference between the `02 a0` and `02 a1` shortints
/// in `06-xattr-oob-wire-evidence.md` §3: with `-X` on, a file with no
/// attribute emits `a0` and no section, a file with even a single small
/// attribute emits `a1` and a section consisting of the lone terminator.
const ITEM_REPORT_XATTR: u16 = 0x0100;
/// iflags emitted by the driver in the download path, replicating the
/// frozen oracle's client→server first-file signature shape.
const A2_2_DOWNLOAD_IFLAGS: u16 = ITEM_TRANSFER | ITEM_REPORT_CHANGE;
/// Truncated strong checksum length used when *sending* signatures in
/// the download path. Two bytes matches the frozen oracle's 256 KiB
/// profile. Kept as a driver-level constant for A2.2: S8j will revisit
/// when the delta engine can evaluate the impact on the matching rate.
const A2_2_DOWNLOAD_S2LENGTH: i32 = 2;
/// The per-file ndx the driver expects/emits in the single-file A2.2
/// scope. First file of the list, baseline `-1` → diff `+2` → `+1`.
const A2_2_FIRST_FILE_NDX: i32 = 1;
/// Frozen download client->server stream starts with four zero bytes
/// before the server sends the file list and before the receiver's
/// first per-file signature header. Stock rsync emits this
/// receiver-side housekeeping prefix before `write_sum_head`; without
/// it the remote sender waits and never produces the file list.
const A2_2_DOWNLOAD_SIGNATURE_PREFIX_ZEROS: usize = 4;
/// Frozen download client->server stream appends five `NDX_DONE`
/// markers after the receiver's per-file signatures. These finish the
/// receiver-side phase bookkeeping before the remote sender starts
/// producing delta bytes.
const A2_2_DOWNLOAD_SIGNATURE_TAIL_NDX_DONE_COUNT: usize = 5;
/// Historical default for file-level strong checksums (xxh128 / md5 /
/// md4). CLAUDE-AV-B3-18: receive paths use the negotiated width instead;
/// this remains the conservative fallback for absent or unknown winners.
const A2_3_FILE_CHECKSUM_LEN: usize = 16;

/// S8j download: exact count of `NDX_DONE` markers rsync 3.2.7 interleaves
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
    /// Stub frontier: reserved for sub-phases not yet wired. A2.4
    /// eliminates the stub frontier for happy-path flow; the variant
    /// stays for future incremental sub-steps.
    #[allow(dead_code)]
    Stub,
    /// Irrecoverable error observed; terminal.
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignatureHeader {
    Transfer {
        ndx: i32,
        iflags: u16,
        head: SumHead,
    },
    NoopDone,
    /// Stock rsync skips an already-up-to-date file with `ndx + iflags`
    /// lacking `ITEM_TRANSFER` and sends **no sum_head** (the signature
    /// only follows a real transfer request, `sender.c` /
    /// `generator.c::recv_generator`). First observed live 2026-07-21
    /// against bookworm rsync 3.2.7 (proto 32 advertised): identical
    /// upload answered `ndx=1, iflags=0x0008` then the phase markers —
    /// the pre-fix driver blocked forever reading a sum_head that was
    /// never coming.
    Skipped {
        ndx: i32,
        iflags: u16,
    },
}

/// Per-endpoint capability advertisement used in the preamble exchange.
///
/// rsync's preamble carries a space-separated, priority-descending list
/// of checksum and compression algorithms. Stock rsync 3.2.x/3.4.x
/// accepts the full default advertisement (byte-pinned in CI lane 3).
/// This type exists so the driver can advertise a reduced list to a
/// non-stock `rsync --server` wrapper if a future endpoint needs it,
/// without touching the byte-pinned default path.
///
/// The default keeps the historical values verbatim so every existing
/// caller, frozen-byte fixture and the byte-identical CI lane are
/// unaffected. [`Self::for_host`] returns the default for every host
/// today; the per-host hook and the [`Self::with_env_overrides`]
/// live-tuning knobs are the supported way to deviate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreambleProfile {
    pub checksum_algos: String,
    pub compression_algos: String,
}

impl Default for PreambleProfile {
    fn default() -> Self {
        // B.2: SPACE-separated, priority-descending. Byte-pinned against
        // the frozen rsync 3.2.7 capture and CI lane 3 (rsync 3.4.1).
        Self {
            checksum_algos: "xxh128 xxh3 xxh64 md5 md4".to_string(),
            compression_algos: "zstd lz4 zlibx zlib".to_string(),
        }
    }
}

impl PreambleProfile {
    /// Select a profile from the endpoint host. Every host (rsync.net,
    /// Hetzner, stock SFTP+rsync, the dev fixtures) currently keeps the
    /// default that is byte-pinned in CI. The per-host hook is preserved
    /// so a future non-stock endpoint can be mapped to a reduced
    /// advertisement here without disturbing the byte-pinned path; the
    /// `with_env_overrides` knobs cover live tuning in the meantime.
    pub fn for_host(_host: &str) -> Self {
        Self::default().with_env_overrides()
    }

    /// Live-tuning escape hatch: `AEROFTP_RSYNC_CSUM_ALGOS` and
    /// `AEROFTP_RSYNC_COMPRESS_ALGOS` override the resolved profile at
    /// runtime so the exact algorithm set a stripped remote rsync
    /// wrapper accepts can be found against a live endpoint without a
    /// rebuild per attempt. No-op when unset (the common path), so
    /// production behaviour and the byte-pinned default are unchanged.
    pub fn with_env_overrides(mut self) -> Self {
        if let Ok(v) = std::env::var("AEROFTP_RSYNC_CSUM_ALGOS") {
            if !v.trim().is_empty() {
                self.checksum_algos = v.trim().to_string();
            }
        }
        if let Ok(v) = std::env::var("AEROFTP_RSYNC_COMPRESS_ALGOS") {
            if !v.trim().is_empty() {
                self.compression_algos = v.trim().to_string();
            }
        }
        self
    }
}

/// Append a hex + ASCII annotated record to a file under
/// `$AEROFTP_WIRE_DUMP_DIR`. Best-effort and env-gated: a single
/// `env::var` miss on the normal path, no panic, no behaviour change
/// when the variable is unset. Intended for isolating wire-protocol
/// drift against a real remote rsync (blocco-B methodology) and for
/// producing a concrete artifact to hand to a remote rsync operator.
fn wire_dump_append(file: &str, header: &str, bytes: &[u8]) {
    let dir = match std::env::var("AEROFTP_WIRE_DUMP_DIR") {
        Ok(d) if !d.is_empty() => d,
        _ => return,
    };
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 4 + 128);
    let _ = writeln!(out, "=== {header} ({} bytes) ===", bytes.len());
    for (i, chunk) in bytes.chunks(16).enumerate() {
        let _ = write!(out, "{:08x}  ", i * 16);
        for b in chunk {
            let _ = write!(out, "{b:02x} ");
        }
        for _ in chunk.len()..16 {
            let _ = write!(out, "   ");
        }
        let _ = write!(out, " |");
        for b in chunk {
            let c = *b;
            let _ = write!(
                out,
                "{}",
                if (0x20..0x7f).contains(&c) {
                    c as char
                } else {
                    '.'
                }
            );
        }
        let _ = writeln!(out, "|");
    }
    let _ = writeln!(out);
    let path = std::path::Path::new(&dir).join(file);
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(out.as_bytes());
    }
}

/// Record the exact client preamble (decoded fields + raw bytes) we put
/// on the wire. Env-gated via `wire_dump_append`.
fn wire_dump_client_preamble(
    protocol_version: u32,
    checksum_algos: &str,
    compression_algos: &str,
    raw: &[u8],
) {
    if std::env::var("AEROFTP_WIRE_DUMP_DIR")
        .map(|d| d.is_empty())
        .unwrap_or(true)
    {
        return;
    }
    let summary = format!(
        "protocol_version={protocol_version}\nchecksum_algos={checksum_algos:?}\ncompression_algos={compression_algos:?}",
    );
    wire_dump_append("client_preamble.txt", &summary, raw);
}

/// Record whatever the server sent before the preamble decoded (or
/// before it closed the channel). `state` distinguishes a clean decode
/// from a premature close so the artifact is self-describing.
fn wire_dump_server_response(received: &[u8], state: &str) {
    wire_dump_append(
        "server_response.txt",
        &format!("server-bytes-before-preamble state={state}"),
        received,
    );
}

/// Real-wire rsync session driver. Parameterised on the raw-capable
/// remote-shell transport so both mock and SSH paths share the machinery.
pub struct AerorsyncDriver<T: RawRemoteShellTransport> {
    transport: T,
    cancel_handle: CancelHandle,
    /// Preamble capability advertisement. Defaults to
    /// [`PreambleProfile::default`]; production overrides via
    /// [`Self::with_preamble_profile`] using [`PreambleProfile::for_host`].
    preamble_profile: PreambleProfile,

    // Populated by `perform_preamble_exchange`.
    protocol_version: u32,
    compat_flags: i32,
    checksum_seed: u32,
    negotiated_checksum_algos: String,
    negotiated_compression_algos: String,

    /// Whether this session asked the remote `rsync --server` for `-X`,
    /// captured from the [`RemoteCommandSpec`] when the stream is opened.
    ///
    /// **This is the single source of truth for xattrs on the wire.**
    /// Three separate decisions have to agree, and each one on its own is
    /// enough to desynchronise the stream if it disagrees with the others:
    /// whether `-X` goes into the server flag bundle
    /// (`compact_flags_for`), whether the file-list codec expects a
    /// trailing xattr blob on every entry
    /// (`FileListDecodeOptions::preserve_xattrs`), and whether the sender
    /// emits the out-of-band datum section after the per-file header
    /// (`xattr_datum_section_bytes`). They used to be wired independently
    /// and only agreed because all three were hard-coded off; now they all
    /// read from here, which reads from the spec.
    negotiated_xattrs: bool,

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
    /// `ndx + iflags + sum_head`: these belong to the following
    /// sum_blocks stream. Used as a prefix by `read_signature_blocks`
    /// so MSG_DATA payload bytes never get dropped on the floor.
    sig_residual_after_header: Vec<u8>,
    /// Upload path: stock rsync can answer the file-list with NDX_DONE
    /// when every file is already up to date. In that case there is no
    /// signature or delta payload to exchange, but the sender phase loop
    /// has already consumed one marker.
    upload_noop_transfer: bool,
    sender_phase_markers_seen: i32,

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
    /// wire, sized by [`Self::negotiated_file_checksum_len`] (16 bytes
    /// for xxh128 / md5 / md4, 8 for xxh3 / xxh64, 20 for sha1).
    received_file_checksum: Option<Vec<u8>>,
    /// Upload path: delta ops emitted on the wire, in emission order.
    /// Kept for test visibility: production callers should ignore this.
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
    /// Download path: stock rsync can close cleanly after receiving
    /// receiver signatures when the local baseline is already identical.
    /// In that case no delta or summary bytes follow; `finish_session`
    /// completes from local counters.
    download_clean_eof_noop: bool,
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

    /// Fix A: optional GUI progress sink. When set (only on the interactive
    /// command path), the driver reports wire bytes during the network phase
    /// via [`report_wire_progress`](Self::report_wire_progress). `None` for
    /// AeroSync and the CLI, so their hot path stays a single `is_none()`
    /// check per chunk. See `docs/dev/roadmap/APPENDIX-AERORSYNC-DELTA-REDESIGN`.
    progress_sink: Option<crate::delta_transport::DeltaProgressSink>,
    /// Throttle cursor for `report_wire_progress`: the last `transferred`
    /// value the sink was actually called with.
    last_progress_report: u64,
}

impl<T: RawRemoteShellTransport> AerorsyncDriver<T> {
    pub fn new(transport: T, cancel_handle: CancelHandle) -> Self {
        Self {
            transport,
            cancel_handle,
            preamble_profile: PreambleProfile::default(),
            protocol_version: 0,
            compat_flags: 0,
            checksum_seed: 0,
            negotiated_checksum_algos: String::new(),
            negotiated_compression_algos: String::new(),
            negotiated_xattrs: false,
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
            upload_noop_transfer: false,
            sender_phase_markers_seen: 0,
            reconstructed: None,
            received_file_checksum: None,
            emitted_delta_ops: Vec::new(),
            sent_data_bytes: 0,
            received_summary: None,
            session_stats: SessionStats::default(),
            session_role: None,
            received_raw_bytes: 0,
            summary_seed: Vec::new(),
            download_clean_eof_noop: false,
            remote_command_flavor: RemoteCommandFlavor::WrapperParity,
            progress_sink: None,
            last_progress_report: 0,
        }
    }

    /// Override the preamble capability advertisement. Production wires
    /// this from [`PreambleProfile::for_host`] so a future endpoint
    /// running a non-stock `rsync --server` wrapper can advertise a
    /// reduced, accepted algo list. The default keeps the byte-pinned
    /// stock advertisement, so callers that do not opt in are unchanged.
    pub fn with_preamble_profile(mut self, profile: PreambleProfile) -> Self {
        self.preamble_profile = profile;
        self
    }

    /// Fix A: attach an optional GUI progress sink. Only the interactive
    /// command path opts in; AeroSync and the CLI never call this, so their
    /// transfers keep `progress_sink = None` and pay no per-chunk cost.
    pub fn with_progress_sink(
        mut self,
        sink: Option<crate::delta_transport::DeltaProgressSink>,
    ) -> Self {
        self.progress_sink = sink;
        self
    }

    /// Report `transferred` wire bytes (out of `total`, a hint) to the GUI
    /// progress sink, throttled so the boxed closure (and the `transfer_event`
    /// IPC it emits) fires at most ~1% of total movement, plus the final byte.
    /// A no-op when no sink is attached (AeroSync / CLI): a single `is_none()`
    /// branch, no time syscall, nothing allocated in the hot loop.
    fn report_wire_progress(&mut self, transferred: u64, total: u64) {
        if self.progress_sink.is_none() {
            return;
        }
        let step = if total > 0 {
            (total / 100).max(256 * 1024)
        } else {
            256 * 1024
        };
        let final_tick = total > 0 && transferred >= total;
        if !final_tick && transferred < self.last_progress_report.saturating_add(step) {
            return;
        }
        self.last_progress_report = transferred;
        if let Some(sink) = self.progress_sink.as_mut() {
            sink(transferred, total);
        }
    }

    pub fn cancel_handle(&self) -> CancelHandle {
        self.cancel_handle.clone()
    }

    pub fn phase(&self) -> AerorsyncSessionPhase {
        self.phase
    }
    /// Test-only view of the configured preamble profile so the
    /// builder wiring can be regression-pinned without a live wire.
    #[cfg(test)]
    pub(crate) fn preamble_profile_for_test(&self) -> &PreambleProfile {
        &self.preamble_profile
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

    /// Block-strong algorithm for
    /// confirming rolling hits against wire signatures. Must match how
    /// the peer (or we, on the download emit path) filled
    /// `SumBlock.strong`. xxh128, xxh3, xxh64, md5, md4, and sha1 are
    /// recomputed in-tree; other winners (sha256/sha512/none, reachable
    /// only through the env override) stay `Unknown` (safer than
    /// rolling-only confirmation).
    pub(crate) fn block_strong_algo(&self) -> BlockStrongAlgo {
        match self.negotiated_checksum_algo() {
            Some(XXH128_ALGO_NAME) => BlockStrongAlgo::Xxh128 {
                seed: self.checksum_seed as u64,
            },
            Some(XXH3_ALGO_NAME) => BlockStrongAlgo::Xxh3_64 {
                seed: self.checksum_seed as u64,
            },
            Some(XXH64_ALGO_NAME) => BlockStrongAlgo::Xxh64 {
                seed: self.checksum_seed as u64,
            },
            // CLAUDE-AV-B3-17: rsync get_checksum2 CSUM_MD5 seed order.
            Some(MD5_ALGO_NAME) => BlockStrongAlgo::Md5 {
                seed: self.checksum_seed,
                proper_seed_order: self.compat_flags & CF_CHKSUM_SEED_FIX != 0,
            },
            // Y-RSC.3: builtin CSUM_MD4 appends the seed to the data;
            // CF_CHKSUM_SEED_FIX never enters that branch.
            Some(MD4_ALGO_NAME) => BlockStrongAlgo::Md4 {
                seed: self.checksum_seed,
            },
            // Y-RSC.3: sha1 exists only through rsync's OpenSSL EVP
            // path, which hashes the seed before the data.
            Some(SHA1_ALGO_NAME) => BlockStrongAlgo::Sha1 {
                seed: self.checksum_seed,
            },
            _ => BlockStrongAlgo::Unknown,
        }
    }

    fn file_checksum_kind(&self) -> FileChecksumKind {
        FileChecksumKind::from_negotiated_name(self.negotiated_checksum_algo())
    }
    pub fn negotiated_checksum_algos(&self) -> &str {
        &self.negotiated_checksum_algos
    }
    /// CLAUDE-AV-B3-12: the single checksum algorithm the two peers
    /// actually agreed on, or `None` when the lists intersect nowhere
    /// (or the peer advertised none, e.g. a server that skipped the
    /// negotiated strings entirely).
    ///
    /// [`Self::negotiated_checksum_algos`] is a misnomer worth knowing
    /// about: it holds the peer's RAW advertised list, not a winner, so
    /// "contains xxh128" is NOT the same question as "xxh128 won".
    ///
    /// Mirrors rsync 3.2.7 `compat.c::parse_negotiate_str`: `nno->saw[]`
    /// carries each name's 1-based priority taken from OUR OWN list, the
    /// scan walks the PEER's tokens, and the winner is the candidate with
    /// the best (lowest) `saw` value. That reduces exactly to the first
    /// name in our priority-ordered advertisement that the peer also
    /// advertised. Deriving it here (rather than assuming our profile's
    /// head) keeps the answer honest when `with_env_overrides` reorders
    /// or trims what we advertise.
    pub fn negotiated_checksum_algo(&self) -> Option<&str> {
        self.preamble_profile
            .checksum_algos
            .split_whitespace()
            .find(|ours| {
                self.negotiated_checksum_algos
                    .split_whitespace()
                    .any(|theirs| theirs == *ours)
            })
    }
    /// CLAUDE-AV-B3-18: byte width of both the `--checksum` file-list
    /// digest and the whole-file delta trailer for the negotiated winner.
    ///
    /// Mirrors rsync 3.2.7 `checksum.c::csum_len_for_type`. Unknown or
    /// absent negotiation retains the historical 16-byte behavior instead
    /// of guessing a new wire shape.
    pub(crate) fn negotiated_file_checksum_len(&self) -> usize {
        match self.negotiated_checksum_algo() {
            Some(XXH3_ALGO_NAME | XXH64_ALGO_NAME | "xxhash") => 8,
            Some(SHA1_ALGO_NAME) => 20,
            Some("sha256") => 32,
            Some("sha512") => 64,
            Some("none") => 1,
            // xxh128, md5, md4, and the absent-negotiation fallback all
            // share the historical 16-byte width.
            _ => A2_3_FILE_CHECKSUM_LEN,
        }
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
    /// S8j mirror of `sent_data_bytes` for the inbound direction -
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
                    "native summary/done phase not yet wired: call finish_session() explicitly",
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
                    "native summary/done phase not yet wired: call finish_session() explicitly",
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
    // because the A2.x test suite pins the sentinel behaviour: removing the
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

    /// P3-T01 W1.2: streaming-source sibling of
    /// [`drive_upload_through_delta`]. Identical session-level flow up
    /// to the delta phase; the difference is that the source bytes
    /// arrive as an `AsyncRead` instead of a fully-buffered `&[u8]`,
    /// and the delta plan is produced incrementally via
    /// [`RollingDeltaPlanProducer`] instead of a bulk
    /// `compute_delta` call. Wire output is **byte-identical** with
    /// the bulk path for the same source bytes: pinned by
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
        R: AsyncRead + AsyncSeek + Unpin + Send,
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

    /// Y-RSC.4: upload a single **symlink** entry against stock
    /// `rsync --server`.
    ///
    /// rsync transports a symlink entirely inside the file-list entry
    /// (`S_IFLNK` mode + target string, `flist.c::send_file_entry`);
    /// there is no signature, delta, or data phase for it. The remote
    /// generator creates the link straight from the flist
    /// (`generator.c::recv_generator`, `S_ISLNK` branch) and never
    /// requests a transfer, so after the flist the session goes directly
    /// to the phase-marker bookkeeping: the same shape as the no-op
    /// upload paths already pinned by the skip-notice tests.
    ///
    /// The caller must still invoke [`finish_session`] afterwards,
    /// exactly like the regular upload entry points.
    ///
    /// Errors:
    /// - `IllegalStateTransition` when `source_entry` is not a symlink
    ///   entry (`S_ISLNK(mode)` false or no target): the regular
    ///   `drive_upload_through_delta*` entry points own those.
    /// - `InvalidFrame` when the peer requests a delta transfer for the
    ///   symlink (`ITEM_TRANSFER` set): stock rsync never does; a peer
    ///   that does would next expect a token stream this driver has no
    ///   bytes for, so the session fails closed instead of deadlocking.
    ///
    /// [`finish_session`]: Self::finish_session
    pub async fn drive_upload_symlink(
        &mut self,
        command_spec: RemoteCommandSpec,
        source_entry: FileListEntry,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError> {
        match self
            .drive_upload_symlink_inner(command_spec, source_entry, bridge)
            .await
        {
            Ok(()) => Ok(()),
            Err(e) => {
                self.phase = AerorsyncSessionPhase::Failed;
                Err(e)
            }
        }
    }

    async fn drive_upload_symlink_inner(
        &mut self,
        command_spec: RemoteCommandSpec,
        mut source_entry: FileListEntry,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError> {
        if !is_symlink_mode(source_entry.mode) || source_entry.symlink_target.is_none() {
            return Err(AerorsyncError::illegal_transition(
                "drive_upload_symlink requires an S_IFLNK entry with a symlink target; \
                 use drive_upload_through_delta* for regular files",
            ));
        }
        self.session_role = Some(SessionRole::Sender);
        self.remote_command_flavor = command_spec.flavor;
        self.open_raw_stream_internal(&command_spec).await?;
        let csum_algos = self.preamble_profile.checksum_algos.clone();
        let comp_algos = self.preamble_profile.compression_algos.clone();
        self.perform_preamble_exchange(31, &csum_algos, &comp_algos)
            .await?;
        // No flist checksum for symlinks (proto >= 28, `flist.c` sends it
        // only for S_ISREG entries); the codec skips the field for
        // S_IFLNK modes, this just keeps the entry state truthful.
        source_entry.checksum = Vec::new();
        self.send_file_list_single_file(&source_entry).await?;
        self.receive_signature_phase_single_file(bridge).await?;
        if !self.upload_noop_transfer {
            return Err(AerorsyncError::invalid_frame(
                "peer requested a delta transfer (ITEM_TRANSFER) for a symlink entry; \
                 rsync transports symlinks in the file list only",
            ));
        }
        Ok(())
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

    /// P3-T01 W2.4/W2.5 + Y-RSC.5: streaming-sink sibling of
    /// [`drive_download_through_delta`].
    ///
    /// Both the signature phase and the reconstruction phase stream from
    /// `baseline` (`BaselineSource::read_block`): there is no bulk
    /// `destination_data: &[u8]` argument anymore. Peak RAM is
    /// `O(block_size + writer buffer)`, independent of baseline size
    /// (Y-RSC.5 closes the signature-phase bulk-read left open by W2.5).
    ///
    /// The reconstructed bytes do not materialise as a `Vec<u8>`: they
    /// flow into the caller-supplied `writer` (typically a
    /// `StreamingAtomicWriter`, W2.3) which retains full ownership across
    /// the call so the caller can `finalize` it (commit the temp file
    /// via rename) once the driver returns.
    ///
    /// `baseline` is the random-access source for both
    /// `send_signature_phase_from_baseline` (rolling + wire strong) and
    /// `apply_delta_streaming` (`EngineDeltaOp::CopyBlock(idx)`).
    ///
    /// `writer` is borrowed by `&mut` for the duration of the call -
    /// the caller retains ownership and is responsible for
    /// finalisation (flush + sync_all + rename) on success and for
    /// best-effort cleanup on error.
    pub async fn drive_download_through_delta_streaming(
        &mut self,
        command_spec: RemoteCommandSpec,
        baseline: &mut dyn BaselineSource,
        writer: &mut (dyn AsyncWrite + Send + Unpin),
        adapter: &dyn DeltaEngineAdapter,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError> {
        match self
            .drive_download_inner_streaming(command_spec, baseline, writer, adapter, bridge)
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
        mut source_entry: FileListEntry,
        source_data: &[u8],
        adapter: &dyn DeltaEngineAdapter,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError> {
        self.reject_symlink_entry_on_regular_upload(&source_entry)?;
        self.session_role = Some(SessionRole::Sender);
        self.remote_command_flavor = command_spec.flavor;
        self.open_raw_stream_internal(&command_spec).await?;
        // B.2: rsync wire protocol uses SPACE-separated algo lists in
        // priority-descending order. Using commas causes stock rsync
        // 3.4.1 to parse the whole list as a single unknown algorithm
        // and close the stream. Values cribbed from the frozen capture
        // `capture/artifacts_real/frozen/upload/capture_in.bin` shape.
        let csum_algos = self.preamble_profile.checksum_algos.clone();
        let comp_algos = self.preamble_profile.compression_algos.clone();
        self.perform_preamble_exchange(31, &csum_algos, &comp_algos)
            .await?;
        source_entry.checksum = self.file_checksum_kind().digest(source_data);
        self.send_file_list_single_file(&source_entry).await?;
        self.receive_signature_phase_single_file(bridge).await?;
        if !self.upload_noop_transfer {
            self.send_delta_phase_single_file(source_data, adapter)
                .await?;
        }
        Ok(())
    }

    /// P3-T01 W1.2: streaming-source twin of [`drive_upload_inner`].
    /// The pre-delta phases (preamble, file list, signature receive)
    /// are identical to the bulk path; only the final delta phase
    /// differs. See [`drive_upload_through_delta_streaming`] for the
    /// public wrapper and the parity invariant.
    async fn drive_upload_inner_streaming<R>(
        &mut self,
        command_spec: RemoteCommandSpec,
        mut source_entry: FileListEntry,
        mut source_reader: R,
        source_len: u64,
        adapter: &dyn DeltaEngineAdapter,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError>
    where
        R: AsyncRead + AsyncSeek + Unpin + Send,
    {
        self.reject_symlink_entry_on_regular_upload(&source_entry)?;
        self.session_role = Some(SessionRole::Sender);
        self.remote_command_flavor = command_spec.flavor;
        self.open_raw_stream_internal(&command_spec).await?;
        let csum_algos = self.preamble_profile.checksum_algos.clone();
        let comp_algos = self.preamble_profile.compression_algos.clone();
        self.perform_preamble_exchange(31, &csum_algos, &comp_algos)
            .await?;
        let checksum_kind = self.file_checksum_kind();
        let mut checksum_hasher = checksum_kind.streaming_hasher();
        let mut checksum_buf = vec![0u8; STREAMING_READ_CHUNK_BYTES];
        loop {
            let n = source_reader.read(&mut checksum_buf).await.map_err(|e| {
                AerorsyncError::transport(format!(
                    "drive_upload_inner_streaming: checksum read failed: {e}"
                ))
            })?;
            if n == 0 {
                break;
            }
            checksum_hasher.update(&checksum_buf[..n]);
        }
        source_entry.checksum = checksum_hasher.finish();
        source_reader.seek(SeekFrom::Start(0)).await.map_err(|e| {
            AerorsyncError::transport(format!(
                "drive_upload_inner_streaming: source rewind failed: {e}"
            ))
        })?;
        self.send_file_list_single_file(&source_entry).await?;
        self.receive_signature_phase_single_file(bridge).await?;
        if !self.upload_noop_transfer {
            self.send_delta_phase_streaming(source_reader, source_len, adapter)
                .await?;
        }
        Ok(())
    }

    /// Y-RSC.4 guard: the regular upload entry points must not carry a
    /// symlink entry. rsync transports symlinks flist-only (no
    /// signature / delta / data phases), so pushing one through the
    /// delta pipeline would wait forever on a signature header the
    /// generator never sends. Fails closed with a typed error that
    /// points the caller at [`drive_upload_symlink`].
    ///
    /// [`drive_upload_symlink`]: Self::drive_upload_symlink
    fn reject_symlink_entry_on_regular_upload(
        &self,
        source_entry: &FileListEntry,
    ) -> Result<(), AerorsyncError> {
        if is_symlink_mode(source_entry.mode) {
            return Err(AerorsyncError::illegal_transition(
                "regular upload entry points cannot carry an S_IFLNK entry; \
                 use drive_upload_symlink (symlinks have no delta phase)",
            ));
        }
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
        let csum_algos = self.preamble_profile.checksum_algos.clone();
        let comp_algos = self.preamble_profile.compression_algos.clone();
        self.perform_preamble_exchange(31, &csum_algos, &comp_algos)
            .await?;
        self.send_download_receiver_phase_prefix().await?;
        self.receive_file_list_single_file(bridge).await?;
        if self.received_entry_is_symlink() {
            return self.complete_download_symlink_flist_only().await;
        }
        self.send_signature_phase_single_file(destination_data, adapter)
            .await?;
        self.receive_delta_phase_single_file(destination_data, adapter, bridge)
            .await?;
        Ok(())
    }

    /// P3-T01 W2.4 + Y-RSC.5: streaming-sink twin of
    /// [`drive_download_inner`]. Signature send and delta receive both
    /// stream from `baseline` (no bulk `destination_data` slice).
    async fn drive_download_inner_streaming(
        &mut self,
        command_spec: RemoteCommandSpec,
        baseline: &mut dyn BaselineSource,
        writer: &mut (dyn AsyncWrite + Send + Unpin),
        adapter: &dyn DeltaEngineAdapter,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError> {
        self.session_role = Some(SessionRole::Receiver);
        self.remote_command_flavor = command_spec.flavor;
        self.open_raw_stream_internal(&command_spec).await?;
        let csum_algos = self.preamble_profile.checksum_algos.clone();
        let comp_algos = self.preamble_profile.compression_algos.clone();
        self.perform_preamble_exchange(31, &csum_algos, &comp_algos)
            .await?;
        self.send_download_receiver_phase_prefix().await?;
        self.receive_file_list_single_file(bridge).await?;
        if self.received_entry_is_symlink() {
            return self.complete_download_symlink_flist_only().await;
        }
        self.send_signature_phase_from_baseline(baseline, adapter)
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
        // The one place the spec is in hand and every production path goes
        // through. Capturing the choice here is what lets the codec and the
        // sender agree with the flags we actually sent the server.
        self.negotiated_xattrs = command_spec.preserve_xattrs;
        let stream = self
            .transport
            .open_raw_stream(command_spec.to_exec_request())
            .await?;
        self.stream = Some(stream);
        self.phase = AerorsyncSessionPhase::RawStreamOpen;
        Ok(())
    }

    /// B.2 fix: rsync wire protocol places the CLIENT first: the client
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
        // Diagnostic wire dump (env-gated, zero cost when unset): records
        // the exact client preamble we put on the wire so it can be
        // diffed against what a remote `rsync --server` expects when
        // isolating wire-protocol drift against a live endpoint.
        wire_dump_client_preamble(
            protocol_version,
            checksum_algos,
            compression_algos,
            &outbound,
        );
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
                    wire_dump_server_response(&scratch, "decoded-ok");
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
                        wire_dump_server_response(&scratch, "remote-closed-before-server-preamble");
                        return Err(AerorsyncError::transport(
                            "perform_preamble_exchange: remote closed before server preamble",
                        ));
                    }
                    scratch.extend_from_slice(&chunk);
                }
                Err(other) => {
                    // Diagnostics for the intermittent CI-only preamble
                    // desync (scratch observed starting past the 4-byte
                    // protocol-version prefix, decoding it as garbage e.g.
                    // 2015297409). The truncated/clean-EOF arms already
                    // dump; this terminal arm did not, so a flaky CI
                    // failure carried no bytes to root-cause from. Env-
                    // gated (AEROFTP_WIRE_DUMP_DIR), zero cost when unset,
                    // no behaviour change.
                    wire_dump_server_response(&scratch, "preamble-hard-fail");
                    return Err(map_realwire_error(other, "server preamble"));
                }
            }
        }
        self.phase = AerorsyncSessionPhase::ClientPreambleRecvd;
        Ok(())
    }

    /// Compute `FileListDecodeOptions` from the driver's current
    /// negotiation state. CLAUDE-AV-B3-18: callers supply the checksum
    /// width because download receives must use the negotiated width while
    /// upload sends use the width of the checksum already computed in the
    /// source entry.
    fn build_flist_options(&self, csum_len: usize) -> FileListDecodeOptions<'static> {
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
            // the negotiated checksum + uid + gid varints (with names when
            // XMIT_USER/GROUP_NAME_FOLLOWS gates them).
            always_checksum: true,
            csum_len,
            preserve_uid: true,
            preserve_gid: true,
            // Same negotiation as the `-X` in the server flag bundle, so
            // it reads from the same field rather than being asserted
            // independently. If this said `true` while the bundle omitted
            // `-X`, the decoder would eat two bytes that are not on the
            // wire and the file list would desynchronise; if it said
            // `false` while the bundle sent `-X`, it would leave those two
            // bytes behind and swallow the list terminator.
            preserve_xattrs: self.negotiated_xattrs,
            previous_name: None,
        }
    }

    async fn send_file_list_single_file(
        &mut self,
        entry: &FileListEntry,
    ) -> Result<(), AerorsyncError> {
        self.phase = AerorsyncSessionPhase::FileListSending;
        let opts = self.build_flist_options(entry.checksum.len());
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
        // S8j: remember the entry on the sender side so
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
        // CLAUDE-AV-B3-18: the file-list checksum has no length prefix.
        // A stock xxh64/xxh3 peer sends 8 bytes; assuming 16 consumed the
        // list terminator as checksum and then waited forever for another
        // MSG_DATA frame.
        let opts = self.build_flist_options(self.negotiated_file_checksum_len());
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
                    //
                    // X.2a: this list is a contract the xattr codec is
                    // written against. `decode_xattr_blob` reports a blob
                    // that straddles a frame boundary as `TruncatedBuffer`
                    // precisely so it lands here, and reserves its own
                    // `InvalidXattrField` / `XattrAbbrevUnsupported` /
                    // `XattrDatumAboveInlineLimit` for shapes that must
                    // abort. Widening this arm to swallow those would turn
                    // a hostile blob into an unbounded frame-pull loop.
                    // Retrying is safe because decoding restarts from the
                    // front of `flist_buf` and the codec keeps no state
                    // across calls.
                    Err(RealWireError::TruncatedBuffer { .. })
                    | Err(RealWireError::InvalidNameLen { .. })
                    | Err(RealWireError::InvalidAlgoListLen { .. }) => {
                        // Need more bytes: poll another Data frame below.
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

    /// Max `MSG_DATA` payload that fits the rsync multiplexed 24-bit length
    /// field (16 MiB - 1). A single logical payload larger than this is split
    /// across consecutive frames by [`write_data_frame`].
    const MSG_DATA_MAX: usize = 0x00FF_FFFF;

    /// Wrap `payload` in a single `MSG_DATA` mux frame and write it to the raw
    /// stream. `payload` must be `<= MSG_DATA_MAX`; the guard is a defensive
    /// safety net since the only callers ([`write_data_frame`] and the
    /// progress-aware delta send) already chunk to that bound.
    ///
    /// Z.4.3.f6: the header (4 B) and payload were previously emitted as
    /// two separate `write_bytes` calls, which russh turns into two
    /// `SSH_MSG_CHANNEL_DATA` packets back-to-back. Against stock
    /// `rsync --server` over an OpenSSH-side ForceCommand wrapper this
    /// occasionally produced a deadlock: the server side observed the
    /// 4-byte header packet first, started decoding the mux frame, then
    /// blocked waiting for a payload chunk that arrived in a separate
    /// SSH read pass: the receiver replied with a partial sum_head and
    /// then hung waiting for follow-up bytes that never came on the same
    /// read boundary. Trace at
    /// `docs/dev/roadmap/APPENDIX-CHECKPOINTS/2026-05-21/win11-z43-f6-trace/trace_linux.log`
    /// shows the symptom (silence for >13 s, only keepalives, no further
    /// data) after a 12-byte server response that should have been
    /// ~6800 B of signature blocks.
    ///
    /// Coalescing header + payload into a single `write_bytes` call
    /// produces one `SSH_MSG_CHANNEL_DATA` packet, which both peers
    /// agree to as a single logical mux frame on the wire. This is also
    /// strictly fewer round-trips so it is a small efficiency win on
    /// the happy path.
    async fn write_one_data_frame(&mut self, payload: &[u8]) -> Result<(), AerorsyncError> {
        if payload.len() > Self::MSG_DATA_MAX {
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
        let mut frame = Vec::with_capacity(hdr_bytes.len() + payload.len());
        frame.extend_from_slice(&hdr_bytes);
        frame.extend_from_slice(payload);
        stream.write_bytes(&frame).await?;
        self.sent_data_bytes += payload.len() as u64;
        Ok(())
    }

    /// Write a logical `MSG_DATA` payload, splitting it across consecutive mux
    /// frames when it exceeds the 24-bit length field.
    ///
    /// The common case (`payload.len() <= MSG_DATA_MAX`, i.e. <= 16 MiB) sends
    /// exactly one frame, byte-identical to the pre-chunking driver. A larger
    /// logical payload (e.g. the full compressed delta of a multi-hundred-MB
    /// brand-new file) is split into `<= MSG_DATA_MAX` frames. This is
    /// wire-safe: `MuxStreamReader` pops one `Data` frame at a time and every
    /// driver receive loop concatenates consecutive `Data` payloads before
    /// decoding, so N frames reassemble into the original logical payload.
    /// Each frame keeps its header+payload coalesced (the russh single-packet
    /// invariant documented on [`write_one_data_frame`]).
    ///
    /// Before this, an oversized payload was rejected with `InvalidFrame`,
    /// which the fallback classifier treats as a hard rejection with no classic
    /// fallback, so a brand-new upload larger than ~16 MiB failed outright.
    /// Chunking removes that failure at its source; the `InvalidFrame` guard on
    /// [`write_one_data_frame`] remains as an unreachable safety net.
    async fn write_data_frame(&mut self, payload: &[u8]) -> Result<(), AerorsyncError> {
        if payload.len() <= Self::MSG_DATA_MAX {
            return self.write_one_data_frame(payload).await;
        }
        for chunk in payload.chunks(Self::MSG_DATA_MAX) {
            self.write_one_data_frame(chunk).await?;
        }
        Ok(())
    }

    /// Size of each MSG_DATA frame on the GUI progress-aware delta send. Small
    /// enough to tick the bar smoothly, well under `MSG_DATA_MAX`; russh
    /// re-chunks to SSH packet size on the wire regardless, so the only cost is
    /// one 4-byte mux header per frame (negligible on a multi-MB delta).
    const PROGRESS_CHUNK: usize = 1024 * 1024;

    /// Send the upload delta `payload`, reporting wire-byte progress as it goes.
    ///
    /// With no progress sink attached (AeroSync / CLI) this is exactly
    /// [`write_data_frame`](Self::write_data_frame): a single frame for
    /// `<= 16 MiB`, Fix B chunking above that, byte-identical to before. With a
    /// sink (the GUI command path) it splits the payload into `PROGRESS_CHUNK`
    /// frames and reports the running wire bytes after each, so the flagship
    /// progress bar fills during the actual network send instead of jumping to
    /// 100% at the end.
    async fn write_delta_with_progress(&mut self, payload: &[u8]) -> Result<(), AerorsyncError> {
        if self.progress_sink.is_none() {
            return self.write_data_frame(payload).await;
        }
        let total = payload.len() as u64;
        let mut sent = 0u64;
        self.last_progress_report = 0;
        for chunk in payload.chunks(Self::PROGRESS_CHUNK) {
            self.write_one_data_frame(chunk).await?;
            sent += chunk.len() as u64;
            self.report_wire_progress(sent, total);
        }
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
        let (ndx, iflags, head) = match self.read_signature_header(bridge).await? {
            SignatureHeader::Transfer { ndx, iflags, head } => (ndx, iflags, head),
            SignatureHeader::NoopDone => {
                // The server answered with a bare NDX_DONE: that marker IS
                // the first phase trigger, so the echo goes out now and the
                // finish-side phase loop starts from phase 1.
                self.upload_noop_transfer = true;
                self.received_sum_head = None;
                self.received_signatures.clear();
                self.sender_phase_markers_seen = 1;
                self.emit_ndx_done_marker().await?;
                self.phase = AerorsyncSessionPhase::DeltaSent;
                return Ok(());
            }
            SignatureHeader::Skipped { .. } => {
                // The file is already up to date on the receiver. No
                // sum_head, no delta; the phase triggers are still all
                // ahead of us, so the finish-side loop starts from 0 and
                // no echo is due yet (sender.c::send_files).
                self.upload_noop_transfer = true;
                self.received_sum_head = None;
                self.received_signatures.clear();
                self.sender_phase_markers_seen = 0;
                self.phase = AerorsyncSessionPhase::DeltaSent;
                return Ok(());
            }
        };
        if ndx < 0 {
            return Err(AerorsyncError::invalid_frame(format!(
                "unexpected ndx sentinel before signature phase: {ndx}"
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
        // Reject an implausible block count before allocating: a file
        // cannot yield more than ceil(max_file_size / block_length)
        // signature blocks, so a peer-declared count above that bound is
        // malformed and must not be trusted for sizing.
        const MAX_PLAUSIBLE_FILE_SIZE: u64 = 1 << 44; // 16 TiB
        let max_plausible_blocks = if head.block_length > 0 {
            MAX_PLAUSIBLE_FILE_SIZE.div_ceil(head.block_length as u64)
        } else {
            0
        };
        if head.count as u64 > max_plausible_blocks {
            return Err(AerorsyncError::invalid_frame(format!(
                "server sum_head.count {} exceeds plausible maximum {} for block_length {}",
                head.count, max_plausible_blocks, head.block_length
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
    ) -> Result<SignatureHeader, AerorsyncError> {
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
        if ndx == NDX_DONE {
            self.summary_seed = std::mem::take(&mut buf);
            return Ok(SignatureHeader::NoopDone);
        }
        if ndx == NDX_FLIST_EOF {
            return Err(AerorsyncError::invalid_frame(format!(
                "unexpected ndx sentinel at start of signature phase: {ndx}"
            )));
        }
        // 2. iflags (u16 LE: 2 bytes)
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
        // Stock rsync only sends a sum_head when the generator asks for
        // the file (ITEM_TRANSFER set). A message without the bit is a
        // skip notice: reading 16 more bytes here would deadlock against
        // a real server, which moves straight to the phase markers.
        if iflags & ITEM_TRANSFER == 0 {
            self.summary_seed = std::mem::take(&mut buf);
            return Ok(SignatureHeader::Skipped { ndx, iflags });
        }
        // 2b. Generator xattr request section (X.2b / B4 live).
        //
        // When the peer set ITEM_REPORT_XATTR, `generator.c` calls
        // `send_xattr_request(NULL, file, f_out)` *before* `write_sum_head`.
        // That section is a run of skip varints (one per over-threshold
        // attribute the receiver still needs) terminated by a single
        // zero byte (`write_byte(f_out, 0)`). Small-only attributes still
        // emit the bare terminator. Skipping it desynchronises sum_head
        // by one byte for the small case (latent; all-zero sum_head can
        // mask it) and by more for OOB values (`count` parses as 1 with
        // `block_length` 0). See rsync 3.2.7 `xattrs.c::send_xattr_request`
        // and `sender.c::send_files` which drains it via
        // `recv_xattr_request` before `receive_sums`.
        if self.negotiated_xattrs && iflags & ITEM_REPORT_XATTR != 0 {
            loop {
                self.check_cancel("read_signature_header xattr_request")?;
                match decode_varint(&buf) {
                    Ok((skip, consumed)) => {
                        buf.drain(..consumed);
                        if skip == 0 {
                            break;
                        }
                        // Generator-side request carries only the skip
                        // (no len/datum): those arrive later on the
                        // sender response path.
                        continue;
                    }
                    Err(RealWireError::TruncatedBuffer { .. }) => {
                        let payload = self.next_data_frame(bridge).await?;
                        buf.extend_from_slice(&payload);
                    }
                    Err(other) => {
                        return Err(map_realwire_error(other, "signature xattr_request"));
                    }
                }
            }
        }
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
        // mux frames: but at this point `buf` holds POST-mux payload
        // bytes, not raw mux. We cannot re-feed it into the reader.
        // Instead we carry the residual through `read_signature_blocks`
        // via an explicit argument.
        //
        // Implementation choice: pass `buf` to `read_signature_blocks`
        // as the prefix of its own accumulator. Getters stay clean.
        self.sig_residual_after_header = std::mem::take(&mut buf);
        Ok(SignatureHeader::Transfer { ndx, iflags, head })
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
        // Do not eagerly reserve the peer-declared `count` (it is bounded
        // upstream but can still be large): cap the initial reservation
        // and let the read loop grow the vector incrementally.
        let mut out = Vec::with_capacity(count.min(4096));
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

    /// Download path (bulk): compute signatures from `destination_data`
    /// via `adapter` and emit a single mux-wrapped blob with
    /// `ndx + iflags + sum_head + count × sum_block`. Phase transitions:
    /// `FileListReceived → SumHeadSent → SumBlocksSent`.
    ///
    /// Kept for the bulk `drive_download` path and mock fixtures that
    /// inject canned signatures through `DeltaEngineAdapter`. Production
    /// downloads use [`Self::send_signature_phase_from_baseline`] (Y-RSC.5).
    async fn send_signature_phase_single_file(
        &mut self,
        destination_data: &[u8],
        adapter: &dyn DeltaEngineAdapter,
    ) -> Result<(), AerorsyncError> {
        self.phase = AerorsyncSessionPhase::SumHeadSent;
        let block_size = adapter.compute_block_size(destination_data.len() as u64);
        let engine_sigs = adapter.build_signatures(destination_data, block_size);

        // Build truncated wire SumBlocks.
        // CLAUDE-AV-B3-17: emit block strongs with the negotiated algo
        // (md5 or xxh128), not a hardcoded xxh128. Unknown winners keep
        // the historical xxh128 emit so existing fixtures do not flip;
        // confirm path still refuses Unknown, so a mismatched emit only
        // costs a full transfer rather than a silent false match.
        let s2length = A2_2_DOWNLOAD_S2LENGTH;
        let s2length_usize = s2length as usize;
        let strong_algo = self.block_strong_algo();
        let mut sum_blocks: Vec<SumBlock> = Vec::with_capacity(engine_sigs.len());
        for sig in &engine_sigs {
            let start = sig.index as usize * block_size;
            let end = start
                .saturating_add(sig.block_len as usize)
                .min(destination_data.len());
            let block = destination_data.get(start..end).unwrap_or(&[]);
            let strong_wire = self.wire_block_strong(block, strong_algo);
            let strong = strong_wire[..s2length_usize.min(strong_wire.len())].to_vec();
            sum_blocks.push(SumBlock {
                rolling: sig.rolling,
                strong,
            });
        }

        self.emit_signature_phase_payload(
            destination_data.len() as u64,
            block_size,
            sum_blocks,
            s2length,
        )
        .await
    }

    /// Y-RSC.5: streaming signature phase for production downloads.
    ///
    /// Walks `baseline` once via [`BaselineSource::read_block`]: for each
    /// block computes the Adler-32 rolling checksum (same primitive as
    /// `delta_sync::compute_signatures`) and the negotiated wire strong
    /// hash, then emits the same mux-wrapped blob as the bulk path.
    /// Peak RAM is `O(block_size)` plus the encoded payload buffer
    /// (itself proportional to block count, not file size: each sum
    /// block is a few dozen bytes).
    ///
    /// Rolling fields are byte-identical to bulk
    /// `adapter.build_signatures` / `compute_signatures` for the same
    /// content (see `compute_signatures_streaming_matches_bulk_*` and
    /// `build_signatures_streaming_matches_bulk`). Mock adapters that
    /// return canned signatures are intentionally not consulted: the
    /// streaming path always derives rolling from the baseline bytes so
    /// a large file can never be forced into RAM just to feed the
    /// adapter trait.
    async fn send_signature_phase_from_baseline(
        &mut self,
        baseline: &mut dyn BaselineSource,
        adapter: &dyn DeltaEngineAdapter,
    ) -> Result<(), AerorsyncError> {
        self.phase = AerorsyncSessionPhase::SumHeadSent;
        let file_len = baseline.len();
        let block_size = adapter.compute_block_size(file_len);
        let s2length = A2_2_DOWNLOAD_S2LENGTH;
        let s2length_usize = s2length as usize;
        let strong_algo = self.block_strong_algo();

        let n_blocks: u32 = if file_len == 0 || block_size == 0 {
            0
        } else {
            let raw = file_len.div_ceil(block_size as u64);
            u32::try_from(raw).map_err(|_| {
                AerorsyncError::invalid_frame("signature block count exceeds the u32 range")
            })?
        };

        let mut sum_blocks: Vec<SumBlock> = Vec::with_capacity(n_blocks as usize);
        for idx in 0..n_blocks {
            let block = baseline
                .read_block(idx, block_size as u32)
                .await
                .map_err(|e| {
                    AerorsyncError::transport(format!(
                        "send_signature_phase_from_baseline: read_block({idx}) failed: {e}"
                    ))
                })?;
            let rolling = crate::delta_sync::RollingChecksum::new(&block).value();
            let strong_wire = self.wire_block_strong(&block, strong_algo);
            let strong = strong_wire[..s2length_usize.min(strong_wire.len())].to_vec();
            sum_blocks.push(SumBlock { rolling, strong });
        }

        self.emit_signature_phase_payload(file_len, block_size, sum_blocks, s2length)
            .await
    }

    /// Shared wire strong-hash dispatch used by both bulk and streaming
    /// signature phases. Keeps the negotiated-algo match arms in one
    /// place so Y-RSC.3 (md4/sha1) cannot drift between the two paths.
    fn wire_block_strong(&self, block: &[u8], strong_algo: BlockStrongAlgo) -> Vec<u8> {
        match strong_algo {
            BlockStrongAlgo::Xxh128 { seed } => compute_xxh128_wire_with_seed(block, seed),
            BlockStrongAlgo::Xxh64 { .. } | BlockStrongAlgo::Xxh3_64 { .. } => {
                strong_algo.digest(block)[..8].to_vec()
            }
            // Y-RSC.3: 16-byte seeded digests (md5 proper/legacy
            // order, md4 data-then-seed).
            BlockStrongAlgo::Md5 { .. } | BlockStrongAlgo::Md4 { .. } => {
                strong_algo.digest(block)[..16].to_vec()
            }
            // Y-RSC.3: 20-byte seed-first sha1.
            BlockStrongAlgo::Sha1 { .. } => strong_algo.digest(block)[..20].to_vec(),
            BlockStrongAlgo::Sha256 | BlockStrongAlgo::Unknown => {
                compute_xxh128_wire_with_seed(block, self.checksum_seed as u64)
            }
        }
    }

    /// Encode and write the per-file signature mux payload: ndx, iflags,
    /// sum_head, sum_blocks, and the receiver phase tail. Shared by bulk
    /// and streaming signature senders after they have produced
    /// `sum_blocks`.
    ///
    /// Callers supply the already-built `sum_blocks` and the file length
    /// used for `remainder_length`.
    async fn emit_signature_phase_payload(
        &mut self,
        file_len: u64,
        block_size: usize,
        sum_blocks: Vec<SumBlock>,
        s2length: i32,
    ) -> Result<(), AerorsyncError> {
        let s2length_usize = s2length as usize;
        // Compose sum_head. Block length from the engine's choice;
        // remainder is (file_size mod block_size): identical to rsync's
        // own derivation. The modulo is computed in u64: truncating the
        // file size to i32 first wraps negative for baselines >= 2 GiB
        // and would emit a negative remainder_length, which stock rsync
        // rejects in `io.c::read_sum_head`.
        let block_length = block_size as i32;
        let remainder_length = sum_head_remainder(file_len, block_length);
        let head = SumHead {
            count: i32::try_from(sum_blocks.len()).map_err(|_| {
                AerorsyncError::invalid_frame("signature block count exceeds the int32 wire field")
            })?,
            block_length,
            checksum_length: s2length,
            remainder_length,
        };
        self.sent_sum_head = Some(head);

        // Build a single MSG_DATA payload that concatenates the per-file
        // signature header, blocks, and receiver phase tail.
        let mut payload: Vec<u8> = Vec::with_capacity(
            A2_2_DOWNLOAD_SIGNATURE_TAIL_NDX_DONE_COUNT
                + 16 /* sum_head worst */ + 4 /* ndx upper bound */ + 2 /* iflags */
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
        payload.extend_from_slice(&[0x00; A2_2_DOWNLOAD_SIGNATURE_TAIL_NDX_DONE_COUNT]);

        self.last_iflags = A2_2_DOWNLOAD_IFLAGS;
        self.write_data_frame(&payload).await?;

        self.sent_signatures = sum_blocks;
        self.phase = AerorsyncSessionPhase::SumBlocksSent;
        Ok(())
    }

    async fn send_download_receiver_phase_prefix(&mut self) -> Result<(), AerorsyncError> {
        self.write_data_frame(&[0x00; A2_2_DOWNLOAD_SIGNATURE_PREFIX_ZEROS])
            .await
    }

    /// True when the just-received single-entry file list carries a
    /// symlink (`S_ISLNK(mode)`). Only meaningful after
    /// `receive_file_list_single_file` has populated `file_list`.
    fn received_entry_is_symlink(&self) -> bool {
        self.file_list
            .first()
            .map(|entry| is_symlink_mode(entry.mode))
            .unwrap_or(false)
    }

    /// Y-RSC.4 download tail for a symlink entry: no signature, delta,
    /// or data phase.
    ///
    /// Stock rsync's generator (`generator.c::recv_generator`, `S_ISLNK`
    /// branch) creates the link straight from the flist target and never
    /// writes `ndx + iflags + sum_head` for it: the only receiver-side
    /// bytes after the flist are the same phase-marker bookkeeping the
    /// regular path appends after its signature payload. Emitting the
    /// marker tail (and nothing else) keeps the stock sender's phase
    /// loop advancing, so `finish_session`'s Receiver branch then drains
    /// the standard 3 leading NDX_DONE + SummaryFrame + trailing marker
    /// exactly like a regular download.
    ///
    /// The driver deliberately does NOT create the symlink itself: the
    /// filesystem side (atomic create + rename, `#[cfg(unix)]`) belongs
    /// to the A4 adapter, which reads the target from
    /// [`downloaded_entry`]. `reconstructed` stays `None` and no bytes
    /// reach the caller's writer.
    ///
    /// [`downloaded_entry`]: Self::downloaded_entry
    async fn complete_download_symlink_flist_only(&mut self) -> Result<(), AerorsyncError> {
        self.phase = AerorsyncSessionPhase::DeltaReceiving;
        self.write_data_frame(&[0x00; A2_2_DOWNLOAD_SIGNATURE_TAIL_NDX_DONE_COUNT])
            .await?;
        self.received_file_checksum = None;
        self.phase = AerorsyncSessionPhase::DeltaReceived;
        Ok(())
    }

    // --- A2.3 delta phase (upload: send, download: receive) --------------

    /// Upload path: compute delta via `adapter.compute_delta`, compress
    /// literals session-wide, encode the full stream (ops + END_FLAG +
    /// file_checksum trailer), and emit on the wire in a single MSG_DATA
    /// frame. Flips `committed = true` immediately before the first
    /// wire byte: the PreCommit/PostCommit boundary.
    ///
    /// Phase transitions: `SumBlocksReceiving → DeltaSending → DeltaSent`.
    /// X.2b: the out-of-band xattr datum section, which on the sender's
    /// stream sits **between the iflags echo and the sum_head echo**
    /// (measured, `06-xattr-oob-wire-evidence.md` §2). Mirrors
    /// `sender.c::send_files`, which calls the xattr datum writer right
    /// after `write_ndx_and_attrs` and before the sum_head.
    ///
    /// Gated exactly like rsync's own `preserve_xattrs && (iflags &
    /// ITEM_REPORT_XATTR)`: this session must have asked for `-X`, and the
    /// peer must have flagged this entry as carrying attributes. Both
    /// conjuncts matter. Without the first, a peer that sets the bit
    /// against a session that never negotiated xattrs could make us inject
    /// a byte the stream is not expecting, and a spurious byte here
    /// desynchronises everything after it. Without the second, we would
    /// emit a section the peer is not reading.
    ///
    /// Returns empty for every transfer that does not negotiate `-X`,
    /// which is all of them until the sender learns to read attributes off
    /// the local file (X.3). The placement is what this encodes, and the
    /// placement came from a measurement rather than a guess.
    fn xattr_datum_section_bytes(&self) -> Vec<u8> {
        if !self.negotiated_xattrs || self.last_iflags & ITEM_REPORT_XATTR == 0 {
            return Vec::new();
        }
        let pairs = self
            .file_list
            .first()
            .and_then(|e| e.xattrs.as_deref())
            .unwrap_or(&[]);
        encode_xattr_datum_section(pairs)
    }

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

        // B.2 Step 4: `block_size == 0` is the "whole file" case: the
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
            // CLAUDE-AV-B3-15: seed-aware plan for real wire; mock adapters
            // keep returning canned ops via the trait default.
            adapter.compute_delta_for_wire(
                source_data,
                &engine_sigs,
                block_size,
                self.block_strong_algo(),
            )
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
            // No compression negotiated: emit raw payloads as-is.
            pending_raw.iter().map(|p| p.to_vec()).collect()
        };
        // S8j: multi-chunk DEFLATED_DATA splitting.
        //
        // Stock rsync's `send_zstd_token` (token.c:678-776) flushes the
        // zstd output buffer whenever it reaches `MAX_DATA_COUNT`
        // (= 16383, the 14-bit length budget of the DEFLATED_DATA
        // token) and emits a fresh DEFLATED_DATA record with the rest.
        // A compressed literal larger than 16383 bytes therefore lands
        // as N consecutive DEFLATED_DATA frames on the wire; the
        // receiver's single session-wide `ZSTD_DCtx` concatenates the
        // payloads transparently: the chunk boundaries carry no
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
        // tokens: in practice governed by the driver's in-memory
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
                        // Skip zero-length blobs: `compress_zstd_literal_stream`
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

        self.session_stats.copy_blocks = u64::from(plan.copy_blocks);
        self.session_stats.matched_bytes = self
            .session_stats
            .copy_blocks
            .saturating_mul(block_size as u64);
        self.session_stats.literal_bytes = plan.literal_bytes;

        // File-level trailers are unseeded even though per-block strong
        // digests use the negotiated checksum seed.
        let file_checksum = self.file_checksum_kind().digest(source_data);

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
        // buffers": sum.count is read as a huge int from delta bytes).
        //
        // Echo values come from the receiver's signature header that
        // `read_signature_header` stashed in `last_received_ndx`,
        // `last_iflags`, and `received_sum_head`.
        let echo_head = *self.received_sum_head.as_ref().ok_or_else(|| {
            AerorsyncError::invalid_frame(
                "send_delta_phase: missing received sum_head: signature phase didn't run",
            )
        })?;
        let mut payload = Vec::with_capacity(8 + delta_bytes.len());
        payload.extend_from_slice(&encode_ndx(
            self.last_received_ndx,
            &mut self.outbound_ndx_state,
        ));
        payload.extend_from_slice(&encode_item_flags(self.last_iflags));
        // X.2b: the xattr datum section, when the peer's iflags asked for
        // one. Empty (zero bytes appended) on every path that does not
        // negotiate `-X`, so the byte-pinned delta shape is unchanged.
        payload.extend_from_slice(&self.xattr_datum_section_bytes());
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

    /// P3-T01 W1.2 / W1.3: streaming-source twin of
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
    ///   wire bytes **diverge** from the bulk path: the streaming
    ///   path emits `ceil(source_len / STREAMING_READ_CHUNK_BYTES)`
    ///   engine literals through the session-wide zstd `CCtx`, where
    ///   the bulk path emits one. The receiver's session-wide
    ///   `ZSTD_DCtx` concatenates both shapes to the same plaintext
    ///   per stock rsync's `send_zstd_token` semantics, so the
    ///   divergence is *protocol-equivalent*. The chunked emission is
    ///   what allows W1.3 to lift the 256 MiB upload-side cap without
    ///   requesting a `Vec<u8>` of `source_len` bytes: the
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

        // Drive the producer + negotiated file hasher chunk-by-chunk. The producer
        // owns the rolling window; the hasher accumulates a streaming
        // whole-file checksum of the source. Both are populated from the same
        // chunk slice so the wire trailer matches what
        // `compute_xxh128_wire(source_data)` would have produced bulk.
        let mut file_hasher = self.file_checksum_kind().streaming_hasher();
        let mut ops: Vec<EngineDeltaOp> = Vec::new();
        let mut total_source_bytes: u64 = 0;
        let mut buf = vec![0u8; STREAMING_READ_CHUNK_BYTES];

        if block_size == 0 {
            // Whole-file case: the receiver has no baseline to diff
            // against (`block_size == 0` is rsync's "send everything as
            // one literal" sentinel). The producer would silently emit
            // zero ops here, so we materialise the literal explicitly.
            //
            // P3-T01 W1.3: emit one `EngineDeltaOp::Literal` per
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
                file_hasher.update(&buf[..n]);
                total_source_bytes += n as u64;

                let mut to_consume: &[u8] = &buf[..n];
                while !to_consume.is_empty() {
                    if chunk_acc.capacity() == 0 {
                        chunk_acc.reserve_exact(STREAMING_READ_CHUNK_BYTES);
                    }
                    let space_left = STREAMING_READ_CHUNK_BYTES.saturating_sub(chunk_acc.len());
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
            // CLAUDE-AV-B3-15: wire sigs are truncated xxh128 (or other
            // negotiated) prefixes, not SHA-256. Thread the session algo.
            let mut producer = RollingDeltaPlanProducer::with_strong_algo(
                block_size,
                engine_sigs,
                self.block_strong_algo(),
            );
            loop {
                let n = source_reader.read(&mut buf).await.map_err(|e| {
                    AerorsyncError::transport(format!(
                        "send_delta_phase_streaming: source read failed: {e}"
                    ))
                })?;
                if n == 0 {
                    break;
                }
                file_hasher.update(&buf[..n]);
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

        self.session_stats.copy_blocks = ops
            .iter()
            .filter(|op| matches!(op, EngineDeltaOp::CopyBlock(_)))
            .count() as u64;
        self.session_stats.matched_bytes = self
            .session_stats
            .copy_blocks
            .saturating_mul(block_size as u64);
        self.session_stats.literal_bytes = ops
            .iter()
            .filter_map(|op| match op {
                EngineDeltaOp::Literal(bytes) => Some(bytes.len() as u64),
                EngineDeltaOp::CopyBlock(_) => None,
            })
            .sum();

        // From here on the encoding/wire-emission path is byte-for-byte
        // identical to `send_delta_phase_single_file`. Any divergence
        // would break the `streaming_send_matches_bulk_send_*` parity
        // pin below: kept in lockstep on purpose.
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

        let file_checksum = file_hasher.finish();

        let report = DeltaStreamReport {
            ops: wire_ops.clone(),
            file_checksum,
        };
        let delta_bytes = encode_delta_stream(&report);

        let echo_head = *self.received_sum_head.as_ref().ok_or_else(|| {
            AerorsyncError::invalid_frame(
                "send_delta_phase_streaming: missing received sum_head: signature phase didn't run",
            )
        })?;
        let mut payload = Vec::with_capacity(8 + delta_bytes.len());
        payload.extend_from_slice(&encode_ndx(
            self.last_received_ndx,
            &mut self.outbound_ndx_state,
        ));
        payload.extend_from_slice(&encode_item_flags(self.last_iflags));
        // X.2b: the xattr datum section, when the peer's iflags asked for
        // one. Empty (zero bytes appended) on every path that does not
        // negotiate `-X`, so the byte-pinned delta shape is unchanged.
        payload.extend_from_slice(&self.xattr_datum_section_bytes());
        payload.extend_from_slice(&encode_sum_head(&echo_head));
        payload.extend_from_slice(&delta_bytes);

        self.committed = true;
        self.emitted_delta_ops = wire_ops;
        // Fix A: report wire-byte progress to the GUI sink (if any) as the
        // delta payload streams out. Byte-identical to write_data_frame when
        // no sink is attached (AeroSync / CLI).
        self.write_delta_with_progress(&payload).await?;

        self.phase = AerorsyncSessionPhase::DeltaSent;
        Ok(())
    }

    /// Download path: drain delta stream bytes from MSG_DATA frames,
    /// decode ops + trailer, decompress literals, convert to engine ops,
    /// and apply via `adapter.apply_delta`. The reconstructed bytes are
    /// stashed in `self.reconstructed` for the A4 adapter to flush to
    /// disk via temp+rename.
    ///
    /// `committed` stays `false` throughout: A2.3 download never writes
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
        let sum_head_count = self.sent_sum_head.as_ref().map(|h| h.count);
        // CLAUDE-AV-B3-18: like the file-list checksum, the delta trailer
        // has no length prefix and must follow the negotiated winner.
        let file_checksum_len = self.negotiated_file_checksum_len();

        // Stock rsync's sender frames every file transfer as
        // `write_ndx_and_attrs` (ndx + iflags) + `write_sum_head`
        // BEFORE the token stream (`sender.c::send_files`), mirroring
        // exactly what our upload `send_delta_phase_single_file` emits.
        // Consume that prefix with the same decoder the upload-receive
        // path uses (`read_signature_header`). A leading `NDX_DONE` is
        // the genuine identical-baseline no-op: the sender skipped the
        // file, so copy the local baseline through unchanged.
        let mut buf = match self.read_signature_header(bridge).await {
            Ok(SignatureHeader::Transfer { .. }) => {
                std::mem::take(&mut self.sig_residual_after_header)
            }
            Ok(SignatureHeader::NoopDone) => {
                // residual already stashed into `summary_seed`.
                self.install_download_noop_reconstructed(destination_data);
                return Ok(());
            }
            Ok(SignatureHeader::Skipped { .. }) => {
                // Sender-side skip notice (iflags without ITEM_TRANSFER):
                // same semantics as NoopDone, the baseline is current.
                self.install_download_noop_reconstructed(destination_data);
                return Ok(());
            }
            Err(e) if e.is_clean_transport_eof() => {
                // Server closed before sending any ndx: a no-payload
                // identical no-op (nothing to reconstruct, keep local).
                self.download_clean_eof_noop = true;
                self.install_download_noop_reconstructed(destination_data);
                return Ok(());
            }
            Err(e) => return Err(e),
        };

        // Accumulate token-stream bytes until `decode_delta_stream`
        // succeeds. `buf` is seeded with the residual the prefix
        // decoder over-read from the same MSG_DATA frame(s).
        loop {
            self.check_cancel("receive_delta_phase")?;
            if !buf.is_empty() {
                match decode_delta_stream(&buf, file_checksum_len, sum_head_count) {
                    Ok((report, consumed)) => {
                        self.stash_post_delta_into_summary_seed(&buf[consumed..]);
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
            match self.next_data_frame(bridge).await {
                Ok(payload) => {
                    buf.extend_from_slice(&payload);
                    // Fix A: report wire-byte download progress to the GUI sink
                    // (no-op without one). received_raw_bytes is the compressed
                    // delta on the wire; on a real delta hit it is fewer bytes
                    // than the file, so the bar under-fills and completes at
                    // reconstruction (the end card shows the delta savings).
                    let received = self.received_raw_bytes;
                    let total_hint = self
                        .file_list
                        .first()
                        .map(|e| e.size.max(0) as u64)
                        .unwrap_or(0);
                    self.report_wire_progress(received, total_hint);
                }
                Err(e) if e.is_clean_transport_eof() => {
                    // A clean transport EOF (structured class stamped by
                    // the producing transport, Y-RSC.2) terminates a
                    // complete full/delta stream the loop-top decode
                    // could not consume earlier solely because it was
                    // still truncated: decode it now, exactly as the
                    // loop-top would. An empty `buf` here is a genuine
                    // no-payload no-op (keep the local baseline).
                    if !buf.is_empty() {
                        match decode_delta_stream(&buf, file_checksum_len, sum_head_count) {
                            Ok((report, consumed)) => {
                                self.stash_post_delta_into_summary_seed(&buf[consumed..]);
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
                                return Err(e);
                            }
                            Err(other) => {
                                return Err(map_realwire_error(other, "delta stream"));
                            }
                        }
                    }
                    self.download_clean_eof_noop = true;
                    self.install_download_noop_reconstructed(destination_data);
                    return Ok(());
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn install_reconstructed_from_wire(
        &mut self,
        destination_data: &[u8],
        adapter: &dyn DeltaEngineAdapter,
        wire_ops: Vec<DeltaOp>,
    ) -> Result<(), AerorsyncError> {
        let copy_blocks: u64 = wire_ops
            .iter()
            .filter_map(|op| match op {
                DeltaOp::CopyRun { run_length, .. } => Some(u64::from(*run_length)),
                DeltaOp::Literal { .. } => None,
            })
            .sum();
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
        self.session_stats.copy_blocks = copy_blocks;
        self.session_stats.matched_bytes = copy_blocks.saturating_mul(block_size as u64);
        let reconstructed = adapter
            .apply_delta(destination_data, &engine_ops, block_size)
            .map_err(|e| AerorsyncError::invalid_frame(format!("apply_delta: {e}")))?;
        self.reconstructed = Some(reconstructed);
        Ok(())
    }

    /// P3-T01 W2.4: streaming sibling of [`receive_delta_phase_single_file`].
    /// Identical wire-handling loop (drain MSG_DATA frames, decode delta
    /// stream, decompress literals, convert to engine ops); the only
    /// difference is the install step calls
    /// [`install_reconstructed_from_wire_streaming`] to apply the ops via
    /// `apply_delta_streaming(baseline, ops, block_size, writer)` instead
    /// of stashing a `Vec<u8>` in `self.reconstructed`.
    ///
    /// `committed` stays `false` throughout: the W2.5 caller flips its
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
        let sum_head_count = self.sent_sum_head.as_ref().map(|h| h.count);
        // CLAUDE-AV-B3-18: streaming and bulk receive paths must decode
        // the same negotiated-width trailer.
        let file_checksum_len = self.negotiated_file_checksum_len();

        // Consume the sender's `write_ndx_and_attrs` + `write_sum_head`
        // prefix (ndx + iflags + sum_head) that precedes the token
        // stream. See `receive_delta_phase_single_file` for the full
        // rationale; this is the streaming sink twin.
        let mut buf = match self.read_signature_header(bridge).await {
            Ok(SignatureHeader::Transfer { .. }) => {
                std::mem::take(&mut self.sig_residual_after_header)
            }
            Ok(SignatureHeader::NoopDone) => {
                self.install_download_noop_streaming(baseline, writer)
                    .await?;
                return Ok(());
            }
            Ok(SignatureHeader::Skipped { .. }) => {
                // Sender-side skip notice (iflags without ITEM_TRANSFER):
                // same semantics as NoopDone, the baseline is current.
                self.install_download_noop_streaming(baseline, writer)
                    .await?;
                return Ok(());
            }
            Err(e) if e.is_clean_transport_eof() => {
                self.download_clean_eof_noop = true;
                self.install_download_noop_streaming(baseline, writer)
                    .await?;
                return Ok(());
            }
            Err(e) => return Err(e),
        };

        loop {
            self.check_cancel("receive_delta_phase")?;
            if !buf.is_empty() {
                match decode_delta_stream(&buf, file_checksum_len, sum_head_count) {
                    Ok((report, consumed)) => {
                        self.stash_post_delta_into_summary_seed(&buf[consumed..]);
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
            match self.next_data_frame(bridge).await {
                Ok(payload) => {
                    buf.extend_from_slice(&payload);
                    // Fix A: report wire-byte download progress to the GUI sink
                    // (no-op without one). received_raw_bytes is the compressed
                    // delta on the wire; on a real delta hit it is fewer bytes
                    // than the file, so the bar under-fills and completes at
                    // reconstruction (the end card shows the delta savings).
                    let received = self.received_raw_bytes;
                    let total_hint = self
                        .file_list
                        .first()
                        .map(|e| e.size.max(0) as u64)
                        .unwrap_or(0);
                    self.report_wire_progress(received, total_hint);
                }
                Err(e) if e.is_clean_transport_eof() => {
                    // A clean transport EOF (structured class stamped by
                    // the producing transport, Y-RSC.2) terminates a
                    // complete full/delta stream the loop-top decode
                    // could not consume earlier solely because it was
                    // still truncated: decode it now, exactly as the
                    // loop-top would. An empty `buf` here is a genuine
                    // no-payload no-op (keep the local baseline).
                    if !buf.is_empty() {
                        match decode_delta_stream(&buf, file_checksum_len, sum_head_count) {
                            Ok((report, consumed)) => {
                                self.stash_post_delta_into_summary_seed(&buf[consumed..]);
                                self.received_file_checksum = Some(report.file_checksum.clone());
                                self.install_reconstructed_from_wire_streaming(
                                    baseline, writer, adapter, report.ops,
                                )
                                .await?;
                                self.phase = AerorsyncSessionPhase::DeltaReceived;
                                return Ok(());
                            }
                            Err(RealWireError::DeltaTokenTruncated { .. }) => {
                                // Stream genuinely incomplete at EOF: a
                                // real truncation, not a no-op. Surface
                                // the original clean-EOF error.
                                return Err(e);
                            }
                            Err(other) => {
                                return Err(map_realwire_error(other, "delta stream"));
                            }
                        }
                    }
                    self.download_clean_eof_noop = true;
                    self.install_download_noop_streaming(baseline, writer)
                        .await?;
                    return Ok(());
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Prepend the wire bytes the sender wrote AFTER the file checksum
    /// (the interleaved `NDX_DONE` markers + `SummaryFrame` + trailer)
    /// to `summary_seed`. They were pulled into the delta accumulator
    /// because they shared MSG_DATA frame(s) with the tail of the token
    /// stream; the sender then closed the channel, so `finish_session`
    /// must satisfy `drain_leading_ndx_done_download` /
    /// `receive_summary_phase` from this seed rather than the wire.
    fn stash_post_delta_into_summary_seed(&mut self, leftover: &[u8]) {
        if leftover.is_empty() {
            return;
        }
        let mut seeded = Vec::with_capacity(leftover.len() + self.summary_seed.len());
        seeded.extend_from_slice(leftover);
        seeded.extend_from_slice(&self.summary_seed);
        self.summary_seed = seeded;
    }

    fn install_download_noop_reconstructed(&mut self, destination_data: &[u8]) {
        self.reconstructed = Some(destination_data.to_vec());
        self.received_file_checksum = None;
        self.phase = AerorsyncSessionPhase::DeltaReceived;
    }

    async fn install_download_noop_streaming(
        &mut self,
        baseline: &mut dyn BaselineSource,
        writer: &mut (dyn AsyncWrite + Send + Unpin),
    ) -> Result<(), AerorsyncError> {
        let (ops, block_size) = self.download_noop_copy_ops()?;
        apply_delta_streaming(baseline, ops, block_size, writer)
            .await
            .map_err(|e| AerorsyncError::invalid_frame(format!("apply_delta_streaming: {e}")))?;
        self.received_file_checksum = None;
        self.phase = AerorsyncSessionPhase::DeltaReceived;
        Ok(())
    }

    fn download_noop_copy_ops(&self) -> Result<(Vec<EngineDeltaOp>, usize), AerorsyncError> {
        let head = self.sent_sum_head.as_ref().ok_or_else(|| {
            AerorsyncError::invalid_frame(
                "receive_delta_phase: missing local sum_head for no-op download",
            )
        })?;
        let block_size = head.block_length as usize;
        if block_size == 0 && head.count > 0 {
            return Err(AerorsyncError::invalid_frame(
                "receive_delta_phase: block_size is zero (missing local sum_head)",
            ));
        }
        let ops = (0..head.count)
            .map(|idx| EngineDeltaOp::CopyBlock(idx as u32))
            .collect();
        Ok((ops, block_size))
    }

    /// P3-T01 W2.4/W2.5: streaming sibling of
    /// [`install_reconstructed_from_wire`]. Decodes the wire ops to
    /// engine ops (same conversion as the bulk path) and dispatches to
    /// [`apply_delta_streaming`] against the caller-supplied baseline +
    /// caller-supplied writer.
    ///
    /// Errors:
    /// - `InvalidFrame` if `block_size == 0` (no `sent_sum_head` from the
    ///   signature phase: a wire-level invariant violation, identical
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
        let copy_blocks: u64 = wire_ops
            .iter()
            .filter_map(|op| match op {
                DeltaOp::CopyRun { run_length, .. } => Some(u64::from(*run_length)),
                DeltaOp::Literal { .. } => None,
            })
            .sum();
        let zstd_on = self.zstd_negotiated();
        let engine_ops = self.delta_wire_to_engine_ops(&wire_ops, zstd_on)?;
        let _ = adapter; // adapter is unused on the streaming path -
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
        self.session_stats.copy_blocks = copy_blocks;
        self.session_stats.matched_bytes = copy_blocks.saturating_mul(block_size as u64);
        apply_delta_streaming(baseline, engine_ops, block_size, writer)
            .await
            .map_err(|e| AerorsyncError::invalid_frame(format!("apply_delta_streaming: {e}")))?;
        // self.reconstructed intentionally stays None: the bytes flowed
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
                strong_len: take as u8,
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
        // zstd against every real rsync peer: the list parses as a
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
        //   `drive_*_inner`. Skip the drain entirely: the mock inbound
        //   never contains leading NDX_DONE bytes and the peek-based
        //   heuristic of the drain would misread a varlong value of 0
        //   as a marker.
        match self.session_role {
            Some(SessionRole::Receiver) => {
                if self.download_clean_eof_noop {
                    self.session_stats.bytes_sent = self.sent_data_bytes;
                    self.session_stats.bytes_received = self.received_raw_bytes;
                    return Ok(());
                }
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
                // of silently running receive semantics: that path used
                // to mask wrong-role bugs in mock tests.
                return Err(AerorsyncError::new(
                    AerorsyncErrorKind::IllegalStateTransition,
                    "finish_session called without a session_role: invoke drive_upload*/drive_download* first",
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
    ///: if it is not `0x00`, the drain is a no-op and the summary
    /// decoder sees the data unchanged.
    async fn drain_leading_ndx_done_download(
        &mut self,
        bridge: &mut dyn EventSink,
    ) -> Result<(), AerorsyncError> {
        let want = PRE_SUMMARY_NDX_DONE_COUNT_DOWNLOAD;
        let mut buf: Vec<u8> = std::mem::take(&mut self.summary_seed);
        if buf.is_empty() {
            // Pull at least one frame to peek.
            let first = self.next_data_frame(bridge).await?;
            buf.extend_from_slice(&first);
        }

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
    ///
    /// Best-effort: this is the last step of a session whose data and
    /// summary have already been fully transferred and verified. On a
    /// download the rsync sender (the server) closes the channel first,
    /// so by the time we send EOF the raw worker has often already
    /// exited; that surfaces as a `TransportFailure` ("worker dropped
    /// shutdown reply" / "channel closed"). Treating a teardown failure
    /// as fatal here would fail an otherwise byte-perfect transfer.
    /// Mirrors `read_trailing_ndx_done`'s clean-EOF tolerance and
    /// rsync's own courtesy-only connection teardown.
    async fn shutdown_raw_stream(&mut self) -> Result<(), AerorsyncError> {
        if let Some(mut stream) = self.stream.take() {
            if let Err(e) = stream.shutdown().await {
                if e.kind == AerorsyncErrorKind::TransportFailure {
                    tracing::debug!(
                        "shutdown_raw_stream: peer closed before teardown ({})",
                        e.detail
                    );
                } else {
                    self.phase = AerorsyncSessionPhase::Complete;
                    return Err(e);
                }
            }
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
        // (2) sender.c:462: final NDX_DONE once the phase loop breaks
        self.emit_ndx_done_marker().await?;
        // (3) handle_stats(-1): intentional no-op for the client sender
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
        let mut phase: i32 = self.sender_phase_markers_seen;
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
            self.sender_phase_markers_seen = phase;
            if phase > max_phase {
                break;
            }
            // sender.c:256: echo NDX_DONE back to advance the receiver's phase loop
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
        // client decided to announce them: matching rsync 3.2.7's
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
        // `session_stats` is a post-emit aggregate: it DOES include the
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
                // bytes.is_empty() is valid too: nothing to check.
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

    #[allow(dead_code)] // A2.0 surface kept for frozen-oracle pins
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

    #[allow(dead_code)] // A2.0 surface kept for frozen-oracle pins
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
        CurrentDeltaSyncBridge, DeltaEngineAdapter, EngineDeltaOp, EngineDeltaPlan,
        EngineSignatureBlock,
    };
    use crate::aerorsync::events::{classify_oob_frame, AerorsyncEvent, CollectingSink};
    use crate::aerorsync::fixtures::RealRsyncBaselineByteTranscript;
    use crate::aerorsync::mock::{MockRemoteShellTransport, MockTransportConfig};
    use crate::aerorsync::real_wire::{
        decode_client_preamble, encode_server_preamble, reassemble_msg_data, ServerPreamble,
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

    /// The `write_ndx_and_attrs` + `write_sum_head` prefix every stock
    /// rsync sender emits before the token stream on a download
    /// (`sender.c::send_files`). The driver consumes it via
    /// `read_signature_header`; the values are decode-only (the
    /// reconstruction uses the locally-sent sum_head), so one canonical
    /// valid header models the wire for every synthetic download test.
    fn download_sender_prefix() -> Vec<u8> {
        build_sig_phase_payload(
            A2_2_FIRST_FILE_NDX,
            A2_2_DOWNLOAD_IFLAGS,
            &SumHead {
                count: 0,
                block_length: 512,
                checksum_length: 2,
                remainder_length: 0,
            },
            &[],
        )
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
            strong_len: 32,
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

    // ---------------------------------------------------------------------
    // X.2b: placement and gating of the out-of-band xattr datum section.
    //
    // The section is unreachable in a real transfer today (nothing
    // negotiates `-X`), so these tests are the only thing standing between
    // the measured placement and a future session guessing at it.
    // ---------------------------------------------------------------------

    /// Driver primed as if the signature phase had just handed us the
    /// peer's per-file header, carrying `iflags`, with `entry` as the
    /// single file-list entry we sent.
    fn driver_primed_for_delta(
        negotiated_xattrs: bool,
        iflags: u16,
        xattrs: Option<Vec<crate::aerorsync::real_wire::XattrPair>>,
    ) -> AerorsyncDriver<MockRemoteShellTransport> {
        let mut d = make_driver(mock_transport_with_raw_inbound(Vec::new()));
        let mut entry = sample_file_list_entry("upload.bin");
        entry.xattrs = xattrs;
        d.file_list.push(entry);
        d.last_iflags = iflags;
        d.negotiated_xattrs = negotiated_xattrs;
        d
    }

    #[test]
    fn xattr_section_is_absent_when_the_peer_did_not_ask_for_it() {
        // No ITEM_REPORT_XATTR in the echoed iflags means no section on
        // the wire, even when the entry is loaded with attributes. This is
        // the case every transfer takes today, and the reason the delta
        // shape is byte-identical to before X.2b.
        use crate::aerorsync::real_wire::XattrPair;
        let d = driver_primed_for_delta(
            true,
            A2_2_DOWNLOAD_IFLAGS,
            Some(vec![
                XattrPair::inline("user.small", b"v1".to_vec()),
                XattrPair::inline("user.big", vec![b'B'; 64]),
            ]),
        );
        assert_eq!(
            d.xattr_datum_section_bytes(),
            Vec::<u8>::new(),
            "without the iflags bit the section must contribute zero bytes"
        );
        assert_eq!(
            A2_2_DOWNLOAD_IFLAGS & ITEM_REPORT_XATTR,
            0,
            "the driver's own download iflags must not carry the xattr bit"
        );
    }

    #[test]
    fn xattr_section_is_emitted_when_the_bit_and_the_attributes_agree() {
        // With the bit set and attributes present, the section carries the
        // over-threshold value and nothing else, terminated by the zero
        // skip. Skip 2 because the large attribute is the second of the
        // two, which is the delta encoding measured in §2.
        use crate::aerorsync::real_wire::{encode_varint, XattrPair};
        let big = vec![b'B'; 64];
        let d = driver_primed_for_delta(
            true,
            A2_2_DOWNLOAD_IFLAGS | ITEM_REPORT_XATTR,
            Some(vec![
                XattrPair::inline("user.small", b"v1".to_vec()),
                XattrPair::inline("user.big", big.clone()),
            ]),
        );
        let mut expected = encode_varint(2);
        expected.extend_from_slice(&encode_varint(64));
        expected.extend_from_slice(&big);
        expected.extend_from_slice(&encode_varint(0));
        assert_eq!(d.xattr_datum_section_bytes(), expected);
    }

    #[test]
    fn xattr_section_with_only_small_values_is_still_one_byte() {
        // The measured surprise: the section exists because the entry has
        // attributes, not because any of them is large. Emitting nothing
        // here would leave the peer one byte ahead for the rest of the
        // stream.
        use crate::aerorsync::real_wire::XattrPair;
        let d = driver_primed_for_delta(
            true,
            A2_2_DOWNLOAD_IFLAGS | ITEM_REPORT_XATTR,
            Some(vec![XattrPair::inline("user.small", b"v1".to_vec())]),
        );
        assert_eq!(d.xattr_datum_section_bytes(), vec![0x00]);
    }

    #[test]
    fn xattr_section_stays_empty_if_the_session_never_negotiated_xattrs() {
        // The conjunct that protects us from the peer. A server that sets
        // ITEM_REPORT_XATTR against a session which never sent `-X` must
        // not be able to make us inject a byte the stream is not
        // expecting: one spurious byte here desynchronises everything
        // after it. Mirrors rsync's own `preserve_xattrs && (iflags &
        // ITEM_REPORT_XATTR)`.
        use crate::aerorsync::real_wire::XattrPair;
        let d = driver_primed_for_delta(
            false,
            A2_2_DOWNLOAD_IFLAGS | ITEM_REPORT_XATTR,
            Some(vec![XattrPair::inline("user.big", vec![b'B'; 64])]),
        );
        assert_eq!(
            d.xattr_datum_section_bytes(),
            Vec::<u8>::new(),
            "the iflags bit alone must not be enough"
        );
    }

    #[tokio::test]
    async fn the_three_xattr_decisions_all_follow_the_command_spec() {
        // The guard this refactor exists for. Whether `-X` goes into the
        // server flag bundle, whether the file-list codec expects a
        // trailing blob on every entry, and whether the sender emits the
        // out-of-band section are three separate decisions that must
        // agree. Each one disagreeing on its own is enough to
        // desynchronise the stream, and they used to be wired
        // independently.
        for want_xattrs in [false, true] {
            let spec = RemoteCommandSpec::upload("/remote/target.bin").with_xattrs(want_xattrs);

            // 1. the server flag bundle
            let sends_dash_x = spec
                .to_exec_request()
                .args
                .iter()
                .any(|a| a.contains('X') && a.starts_with("-logDtp"));
            assert_eq!(sends_dash_x, want_xattrs, "flag bundle disagrees");

            let mut d = make_driver(mock_transport_with_raw_inbound(Vec::new()));
            d.open_raw_stream_internal(&spec).await.expect("open");

            // 2. the file-list codec
            assert_eq!(
                d.build_flist_options(16).preserve_xattrs,
                want_xattrs,
                "flist decode options disagree with the flag bundle"
            );

            // 3. the out-of-band section on the sender path
            let mut entry = sample_file_list_entry("upload.bin");
            entry.xattrs = Some(vec![crate::aerorsync::real_wire::XattrPair::inline(
                "user.big",
                vec![b'B'; 64],
            )]);
            d.file_list.push(entry);
            d.last_iflags = A2_2_DOWNLOAD_IFLAGS | ITEM_REPORT_XATTR;
            assert_eq!(
                !d.xattr_datum_section_bytes().is_empty(),
                want_xattrs,
                "datum section disagrees with the flag bundle"
            );
        }
    }

    #[test]
    fn item_report_xattr_is_the_measured_bit() {
        // `06-xattr-oob-wire-evidence.md` §3: the per-file shortint moves
        // from `02 a0` to `02 a1` exactly when the entry carries
        // attributes. Little-endian, that is 0xa002 -> 0xa102, a
        // difference of 0x0100.
        let without = u16::from_le_bytes([0x02, 0xa0]);
        let with = u16::from_le_bytes([0x02, 0xa1]);
        assert_eq!(with - without, ITEM_REPORT_XATTR);
        assert_eq!(ITEM_REPORT_XATTR, 1 << 8);
        assert_eq!(with & ITEM_REPORT_XATTR, ITEM_REPORT_XATTR);
        assert_eq!(without & ITEM_REPORT_XATTR, 0);
    }

    fn canonical_server_preamble_bytes() -> Vec<u8> {
        // Rsync serialises both lists as SPACE-separated (see
        // `perform_preamble_exchange` and the frozen oracle capture).
        // Using commas here hid the `zstd_negotiated` parsing bug that
        // made live uploads skip zstd compression against stock rsync.
        encode_server_preamble(&ServerPreamble {
            protocol_version: 31,
            compat_flags: 0x07,
            // CLAUDE-AV-B3-18: this shared fixture carries 16-byte file-list
            // checksums and delta trailers, so its winner must be a 16-byte
            // algorithm. Dedicated xxh64 fixtures below carry 8 bytes.
            checksum_algos: "md5".to_string(),
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

    fn decode_upload_file_and_trailer_checksums(
        outbound: &[u8],
        checksum_len: usize,
    ) -> (Vec<u8>, Vec<u8>) {
        let client = decode_client_preamble(outbound).expect("decode outbound client preamble");
        let app = reassemble_msg_data(&outbound[client.consumed..])
            .expect("reassemble outbound MSG_DATA")
            .app_stream;
        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            always_checksum: true,
            csum_len: checksum_len,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
            preserve_xattrs: false,
        };
        let (entry, mut cursor) =
            match decode_file_list_entry(&app, &opts).expect("decode outbound file-list entry") {
                (FileListDecodeOutcome::Entry(entry), consumed) => (entry, consumed),
                other => panic!("expected outbound file-list entry, got {other:?}"),
            };
        let (_, consumed) =
            decode_file_list_entry(&app[cursor..], &opts).expect("decode file-list terminator");
        cursor += consumed;

        let mut ndx_state = NdxState::default();
        let (_, consumed) =
            decode_ndx(&app[cursor..], &mut ndx_state).expect("decode NDX_FLIST_EOF");
        cursor += consumed;
        let (_, consumed) =
            decode_ndx(&app[cursor..], &mut ndx_state).expect("decode echoed file NDX");
        cursor += consumed;
        let (_, consumed) = decode_item_flags(&app[cursor..]).expect("decode echoed item flags");
        cursor += consumed;
        let (head, consumed) = decode_sum_head(&app[cursor..]).expect("decode echoed sum head");
        cursor += consumed;
        let (report, _) = decode_delta_stream(&app[cursor..], checksum_len, Some(head.count))
            .expect("decode outbound delta stream");
        (entry.checksum, report.file_checksum)
    }

    /// Build a `FileListEntry` that the encoder/decoder will round-trip
    /// under `build_flist_options` (varint flags, always_checksum on,
    /// preserve_uid/gid on with SAME_UID/SAME_GID gating uid/gid out).
    /// The flags include `XMIT_LONG_NAME` so the suffix length is encoded
    /// as a varint: which the path length (9 chars) still fits in.
    fn sample_file_list_entry(path: &str) -> FileListEntry {
        // Flags: XMIT_LONG_NAME (0x0040) | XMIT_SAME_MODE (0x0002) |
        //        XMIT_SAME_TIME (0x0080) | XMIT_SAME_UID (0x0008) |
        //        XMIT_SAME_GID (0x0010)
        //: the "all same" upload case where only the name and size are
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
            symlink_target: None,
            xattrs: None,
        }
    }

    /// Realistic FIRST-entry shape for live tests against stock rsync.
    ///
    /// `sample_file_list_entry` sets `XMIT_SAME_TIME | XMIT_SAME_MODE` with
    /// zeroed mtime/mode — legal only from the SECOND entry of a list, when
    /// a predecessor exists to be "same" as. Against a real rsync server
    /// the first entry decodes to mode 0 (a non-regular "special" file),
    /// the generator never enters the signature/delta phase, and the
    /// session deadlocks (verified 2026-07-21 by replaying the lane-3
    /// wire capture into stock rsync 3.2.7: the server stalls after
    /// `recv_file_name`; with explicit mtime/mode it proceeds exactly
    /// like the native client). Mock transports never parse the flist, so
    /// the unit suite could not catch this. Production uploads use
    /// `build_source_entry` and were never affected.
    ///
    /// Flags mirror `build_source_entry` minus the USER/GROUP_NAME_FOLLOWS
    /// pair: explicit mtime (+nsec) and mode on the wire, numeric
    /// uid/gid varints (the harness user is uid 1000).
    fn live_file_list_entry(path: &str) -> FileListEntry {
        const XMIT_MOD_NSEC: u32 = 1 << 13;
        let mtime = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        FileListEntry {
            flags: XMIT_MOD_NSEC,
            path: path.to_string(),
            size: 4096,
            mtime,
            mtime_nsec: Some(0),
            mode: 0o100_644,
            uid: Some(1000),
            uid_name: None,
            gid: Some(1000),
            gid_name: None,
            checksum: vec![0xAA; 16],
            symlink_target: None,
            xattrs: None,
        }
    }

    /// Y-RSC.4: realistic FIRST-entry shape for a symlink, mirror of
    /// [`live_file_list_entry`] with `S_IFLNK` mode, the target string,
    /// `size = strlen(target)` (rsync F_LENGTH convention), and an empty
    /// checksum (proto >= 28 sends the flist checksum only for regular
    /// files). Explicit mtime/mode: the audit 2026-07-21 §4.1 lesson,
    /// `XMIT_SAME_*` is legal only from the second entry on.
    fn symlink_file_list_entry(path: &str, target: &str) -> FileListEntry {
        const XMIT_MOD_NSEC: u32 = 1 << 13;
        let mtime = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        FileListEntry {
            flags: XMIT_MOD_NSEC,
            path: path.to_string(),
            size: target.len() as i64,
            mtime,
            mtime_nsec: Some(0),
            mode: 0o120_777,
            uid: Some(1000),
            uid_name: None,
            gid: Some(1000),
            gid_name: None,
            checksum: vec![],
            symlink_target: Some(target.to_string()),
            xattrs: None,
        }
    }

    /// Deterministic incompressible payload for live lane tests: the
    /// summary-frame assertion `bytes_sent >= source.len()` only holds
    /// when zstd (negotiated with stock rsync) cannot shrink the literal
    /// stream. A cyclic text payload compresses ~7x and the wire total
    /// legitimately drops below the source length. xorshift64 with a
    /// fixed seed keeps the run reproducible without new dependencies.
    ///
    /// Only the `#[cfg(ci_lane3)]` live tests call this; without that
    /// cfg (the default CI check job) the helper would trip `-D dead_code`
    /// after the Y-RSC.8 removal of the module-wide allow.
    #[cfg(ci_lane3)]
    fn incompressible_payload(seed: u64, len: usize) -> Vec<u8> {
        let mut state = seed | 1;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 32) as u8
            })
            .collect()
    }

    // ---- sum_head remainder regression pins (baseline >= 2 GiB) ---------

    #[test]
    fn sum_head_remainder_matches_u32_math_below_2_gib() {
        for (size, blen) in [(0u64, 700i32), (1, 700), (4096, 700), (1_048_576, 2048)] {
            let expected = if blen > 0 { (size as i32) % blen } else { 0 };
            assert_eq!(sum_head_remainder(size, blen), expected);
        }
    }

    #[test]
    fn sum_head_remainder_stays_positive_above_2_gib() {
        // 2 GiB + 123 bytes with a 700-byte block: the pre-fix `as i32`
        // cast wrapped the file size negative and produced a negative
        // remainder_length, which stock rsync rejects in read_sum_head.
        let size = (1u64 << 31) + 123;
        let r = sum_head_remainder(size, 700);
        assert!(r >= 0, "remainder must never be negative: {r}");
        assert_eq!(r, (size % 700) as i32);
        // Exact multiple of the block size wraps to zero, not to garbage.
        assert_eq!(sum_head_remainder(700u64 << 22, 700), 0);
        // Degenerate zero block length (whole-file signature) stays 0.
        assert_eq!(sum_head_remainder(size, 0), 0);
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

    #[test]
    fn preamble_profile_default_is_byte_pinned() {
        // The default advertisement is byte-pinned against the frozen
        // rsync 3.2.7 capture and CI lane 3 (rsync 3.4.1). Any change
        // here is a wire-format change and must be reviewed against the
        // frozen oracle, not silently accepted.
        let d = PreambleProfile::default();
        assert_eq!(d.checksum_algos, "xxh128 xxh3 xxh64 md5 md4");
        assert_eq!(d.compression_algos, "zstd lz4 zlibx zlib");
    }

    #[test]
    fn preamble_profile_for_host_returns_byte_pinned_default() {
        // Every host keeps the byte-pinned default today. The per-host
        // hook exists for a future non-stock endpoint but must not
        // silently deviate the default path for any current host.
        for host in [
            "rsync.net",
            "u123.your-storagebox.de",
            "127.0.0.1",
            "host.example.com",
            "",
        ] {
            assert_eq!(
                PreambleProfile::for_host(host),
                PreambleProfile::default(),
                "host {host:?} must keep the byte-pinned default profile"
            );
        }
    }

    #[test]
    fn preamble_profile_env_overrides_are_noop_when_unset() {
        // The common path (no env knobs) must leave the resolved profile
        // exactly equal to the byte-pinned default. The override branch
        // itself is exercised live, not in a parallel unit test (process
        // env is global and would race other tests).
        std::env::remove_var("AEROFTP_RSYNC_CSUM_ALGOS");
        std::env::remove_var("AEROFTP_RSYNC_COMPRESS_ALGOS");
        assert_eq!(
            PreambleProfile::default().with_env_overrides(),
            PreambleProfile::default()
        );
    }

    #[tokio::test]
    async fn driver_with_preamble_profile_stores_custom() {
        // Builder applies an arbitrary reduced profile; a
        // default-constructed driver keeps the byte-pinned default.
        let custom = PreambleProfile {
            checksum_algos: "xxh128 md5".to_string(),
            compression_algos: "zlib none".to_string(),
        };
        let d = make_driver(mock_transport()).with_preamble_profile(custom.clone());
        assert_eq!(d.preamble_profile_for_test(), &custom);
        let d2 = make_driver(mock_transport());
        assert_eq!(d2.preamble_profile_for_test(), &PreambleProfile::default());
    }

    #[tokio::test]
    async fn custom_reduced_preamble_round_trips_through_wire_encoder() {
        // A reduced advertisement must still be a legal client preamble:
        // encode then decode returns the same strings. Proves profile
        // values flow through the real wire encoder unchanged.
        use crate::aerorsync::real_wire::decode_client_preamble;
        let p = PreambleProfile {
            checksum_algos: "xxh128 md5 md4".to_string(),
            compression_algos: "zlib none".to_string(),
        };
        let mut d = make_driver(mock_transport());
        let mut sink = Vec::new();
        d.send_client_preamble(&mut sink, 31, &p.checksum_algos, &p.compression_algos)
            .await
            .unwrap();
        let decoded = decode_client_preamble(&sink).unwrap();
        assert_eq!(decoded.checksum_algos, p.checksum_algos);
        assert_eq!(decoded.compression_algos, p.compression_algos);
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
        assert_eq!(d.negotiated_checksum_algos(), "md5");
        assert_eq!(d.negotiated_compression_algos(), "none zstd");
        assert_eq!(d.phase(), AerorsyncSessionPhase::ClientPreambleRecvd);
    }

    /// CLAUDE-AV-B3-17: md5 winner maps to seeded Md5 with
    /// `proper_seed_order` from CF_CHKSUM_SEED_FIX (0x20).
    #[tokio::test]
    async fn block_strong_algo_maps_md5_with_seed_fix_flag() {
        let encoded = encode_server_preamble(&ServerPreamble {
            protocol_version: 31,
            compat_flags: 0x07 | CF_CHKSUM_SEED_FIX,
            checksum_algos: "md5".to_string(),
            compression_algos: "none".to_string(),
            checksum_seed: 0xDEAD_BEEF,
            consumed: 0,
        });
        let mut d = make_driver(mock_transport()).with_preamble_profile(PreambleProfile {
            checksum_algos: "xxh128 xxh3 xxh64 md5 md4".to_string(),
            compression_algos: "zstd none".to_string(),
        });
        d.receive_server_preamble(&encoded).await.unwrap();
        assert_eq!(d.negotiated_checksum_algo(), Some(MD5_ALGO_NAME));
        assert_eq!(
            d.block_strong_algo(),
            BlockStrongAlgo::Md5 {
                seed: 0xDEAD_BEEF,
                proper_seed_order: true,
            }
        );
    }

    /// CLAUDE-AV-B3-17: without CF_CHKSUM_SEED_FIX, md5 uses legacy
    /// seed order (data then seed).
    #[tokio::test]
    async fn block_strong_algo_maps_md5_legacy_seed_order_without_flag() {
        let encoded = encode_server_preamble(&ServerPreamble {
            protocol_version: 31,
            compat_flags: 0x07, // no CF_CHKSUM_SEED_FIX
            checksum_algos: "md5".to_string(),
            compression_algos: "none".to_string(),
            checksum_seed: 0xCAFE_BABE,
            consumed: 0,
        });
        let mut d = make_driver(mock_transport()).with_preamble_profile(PreambleProfile {
            checksum_algos: "xxh128 xxh3 xxh64 md5 md4".to_string(),
            compression_algos: "zstd none".to_string(),
        });
        d.receive_server_preamble(&encoded).await.unwrap();
        assert_eq!(
            d.block_strong_algo(),
            BlockStrongAlgo::Md5 {
                seed: 0xCAFE_BABE,
                proper_seed_order: false,
            }
        );
    }

    /// Every implemented xxhash winner maps to its seeded block-strong
    /// variant. The seed is widened from rsync's u32 wire field.
    #[tokio::test]
    async fn block_strong_algo_maps_all_xxhash_winners() {
        async fn algo(theirs: &str) -> BlockStrongAlgo {
            let encoded = encode_server_preamble(&ServerPreamble {
                protocol_version: 31,
                compat_flags: 0x07 | CF_CHKSUM_SEED_FIX,
                checksum_algos: theirs.to_string(),
                compression_algos: "none".to_string(),
                checksum_seed: 0x1111_2222,
                consumed: 0,
            });
            let mut d = make_driver(mock_transport()).with_preamble_profile(PreambleProfile {
                checksum_algos: "xxh128 xxh3 xxh64 md5 md4".to_string(),
                compression_algos: "zstd none".to_string(),
            });
            d.receive_server_preamble(&encoded).await.unwrap();
            d.block_strong_algo()
        }
        assert_eq!(
            algo("xxh128 md5").await,
            BlockStrongAlgo::Xxh128 { seed: 0x1111_2222 }
        );
        assert_eq!(
            algo("xxh64 md5").await,
            BlockStrongAlgo::Xxh64 { seed: 0x1111_2222 }
        );
        assert_eq!(
            algo("xxh3 xxh64 md5").await,
            BlockStrongAlgo::Xxh3_64 { seed: 0x1111_2222 }
        );
    }

    /// Y-RSC.3: md4 and sha1 winners map to their seeded block-strong
    /// variants. Neither carries `proper_seed_order`: the builtin
    /// CSUM_MD4 branch always appends the seed and the sha1 EVP branch
    /// always prepends it, regardless of CF_CHKSUM_SEED_FIX, so the
    /// mapping must be identical with and without the compat flag.
    #[tokio::test]
    async fn block_strong_algo_maps_md4_and_sha1_winners() {
        async fn algo(ours: &str, theirs: &str, compat_flags: i32) -> BlockStrongAlgo {
            let encoded = encode_server_preamble(&ServerPreamble {
                protocol_version: 31,
                compat_flags,
                checksum_algos: theirs.to_string(),
                compression_algos: "none".to_string(),
                checksum_seed: 0x1111_2222,
                consumed: 0,
            });
            let mut d = make_driver(mock_transport()).with_preamble_profile(PreambleProfile {
                checksum_algos: ours.to_string(),
                compression_algos: "zstd none".to_string(),
            });
            d.receive_server_preamble(&encoded).await.unwrap();
            d.block_strong_algo()
        }
        // md4 through the default advertisement (its last entry).
        for flags in [0x07, 0x07 | CF_CHKSUM_SEED_FIX] {
            assert_eq!(
                algo("xxh128 xxh3 xxh64 md5 md4", "md4", flags).await,
                BlockStrongAlgo::Md4 { seed: 0x1111_2222 },
                "md4 mapping must ignore CF_CHKSUM_SEED_FIX (flags {flags:#x})"
            );
        }
        // sha1 is not in the default advertisement; it becomes the
        // winner only under an `AEROFTP_RSYNC_CSUM_ALGOS`-shaped
        // override profile, mirrored here without touching process env.
        for flags in [0x07, 0x07 | CF_CHKSUM_SEED_FIX] {
            assert_eq!(
                algo("sha1", "xxh128 xxh3 xxh64 md5 md4 sha1 none", flags).await,
                BlockStrongAlgo::Sha1 { seed: 0x1111_2222 },
                "sha1 mapping must ignore CF_CHKSUM_SEED_FIX (flags {flags:#x})"
            );
        }
    }

    #[tokio::test]
    async fn download_signature_emit_uses_negotiated_xxh64_and_xxh3_digest_prefixes() {
        let payload = b"rsync-c-full-known-vector";
        let cases = [
            (
                XXH64_ALGO_NAME,
                vec![0x7e, 0x10, 0xc0, 0x64, 0xd4, 0x24, 0x98, 0xba],
            ),
            (
                XXH3_ALGO_NAME,
                vec![0x3a, 0x02, 0x2e, 0xf8, 0xca, 0xce, 0xd9, 0x6c],
            ),
        ];
        for (algorithm, full_digest) in cases {
            let inbound = encode_server_preamble(&ServerPreamble {
                protocol_version: 31,
                compat_flags: 0x07 | CF_CHKSUM_SEED_FIX,
                checksum_algos: algorithm.to_string(),
                compression_algos: "none".to_string(),
                checksum_seed: 0x1234_5678,
                consumed: 0,
            });
            let transport = mock_transport_with_raw_inbound(inbound);
            let mut driver = make_driver(transport).with_preamble_profile(PreambleProfile {
                checksum_algos: algorithm.to_string(),
                compression_algos: "none".to_string(),
            });
            driver
                .open_raw_stream_internal(&RemoteCommandSpec::download("/remote/target.bin"))
                .await
                .unwrap();
            driver
                .perform_preamble_exchange(31, algorithm, "none")
                .await
                .unwrap();
            let adapter = MockSigAdapter::with_fixed_signatures(
                payload.len(),
                vec![make_engine_sig(0, 0xA0A0_A0A0, 0, payload.len() as u32)],
            );
            driver
                .send_signature_phase_single_file(payload, &adapter)
                .await
                .unwrap();
            assert_eq!(driver.sent_signatures().len(), 1);
            assert_eq!(
                driver.sent_signatures()[0].strong,
                full_digest[..A2_2_DOWNLOAD_S2LENGTH as usize],
                "{algorithm} signature must emit the negotiated digest before protocol truncation"
            );
        }
    }

    /// Y-RSC.3: download-side signature emit for md4 and sha1 winners.
    /// Same shape as the xxh64/xxh3 twin above; expected full digests
    /// come from the independent python oracle (seed 0x1234_5678, md4
    /// data-then-seed, sha1 seed-then-data) so the truncated wire
    /// prefix pins both the algorithm and the seeding order.
    #[tokio::test]
    async fn download_signature_emit_uses_negotiated_md4_and_sha1_digest_prefixes() {
        let payload = b"rsync-c-full-known-vector";
        let cases = [
            (
                MD4_ALGO_NAME,
                vec![
                    0x0d, 0xb1, 0x55, 0xf2, 0x6e, 0x0a, 0x15, 0x05, 0xe4, 0x3d, 0x71, 0xe3, 0xd4,
                    0x1a, 0x85, 0x76,
                ],
            ),
            (
                SHA1_ALGO_NAME,
                vec![
                    0x74, 0x00, 0xfb, 0x71, 0x8f, 0x54, 0x61, 0xb7, 0x36, 0x9d, 0x7c, 0x4a, 0x81,
                    0x0b, 0xad, 0x6e, 0xdf, 0x85, 0x44, 0x9a,
                ],
            ),
        ];
        for (algorithm, full_digest) in cases {
            let inbound = encode_server_preamble(&ServerPreamble {
                protocol_version: 31,
                compat_flags: 0x07 | CF_CHKSUM_SEED_FIX,
                checksum_algos: algorithm.to_string(),
                compression_algos: "none".to_string(),
                checksum_seed: 0x1234_5678,
                consumed: 0,
            });
            let transport = mock_transport_with_raw_inbound(inbound);
            let mut driver = make_driver(transport).with_preamble_profile(PreambleProfile {
                checksum_algos: algorithm.to_string(),
                compression_algos: "none".to_string(),
            });
            driver
                .open_raw_stream_internal(&RemoteCommandSpec::download("/remote/target.bin"))
                .await
                .unwrap();
            driver
                .perform_preamble_exchange(31, algorithm, "none")
                .await
                .unwrap();
            let adapter = MockSigAdapter::with_fixed_signatures(
                payload.len(),
                vec![make_engine_sig(0, 0xA0A0_A0A0, 0, payload.len() as u32)],
            );
            driver
                .send_signature_phase_single_file(payload, &adapter)
                .await
                .unwrap();
            assert_eq!(driver.sent_signatures().len(), 1);
            assert_eq!(
                driver.sent_signatures()[0].strong,
                full_digest[..A2_2_DOWNLOAD_S2LENGTH as usize],
                "{algorithm} signature must emit the negotiated digest before protocol truncation"
            );
        }
    }

    /// CLAUDE-AV-B3-12. Pins rsync 3.2.7 `compat.c::parse_negotiate_str`:
    /// the winner is the first algorithm in OUR priority-ordered
    /// advertisement that the peer also advertised, NOT the head of the
    /// peer's list and NOT merely "the peer mentioned xxh128 somewhere".
    /// The download-side whole-file verify keys off this, so getting the
    /// rule wrong either disables the guard or breaks interop.
    #[tokio::test]
    async fn negotiated_checksum_algo_picks_first_of_our_list_present_in_peers() {
        async fn negotiate(ours: &str, theirs: &str) -> Option<String> {
            let encoded = encode_server_preamble(&ServerPreamble {
                protocol_version: 31,
                compat_flags: 0x07,
                checksum_algos: theirs.to_string(),
                compression_algos: "none zstd".to_string(),
                checksum_seed: 0xDEAD_BEEF,
                consumed: 0,
            });
            let mut d = make_driver(mock_transport()).with_preamble_profile(PreambleProfile {
                checksum_algos: ours.to_string(),
                compression_algos: "zstd none".to_string(),
            });
            d.receive_server_preamble(&encoded).await.unwrap();
            d.negotiated_checksum_algo().map(str::to_string)
        }

        let default_ours = "xxh128 xxh3 xxh64 md5 md4";

        // Stock rsync 3.2.7+ advertises xxh128 first: it wins, guard armed.
        assert_eq!(
            negotiate(default_ours, "xxh128 xxh3 xxh64 md5 md4").await,
            Some("xxh128".to_string())
        );
        // OUR order decides, not the peer's: the peer leads with md5 but
        // still offers xxh128, so xxh128 (our top choice) wins.
        assert_eq!(
            negotiate(default_ours, "md5 xxh128").await,
            Some("xxh128".to_string())
        );
        // The canonical fixture peer: no xxh128 on offer, so our next
        // common choice wins and the verify must stay disarmed.
        assert_eq!(
            negotiate(default_ours, "md5 xxh64").await,
            Some("xxh64".to_string())
        );
        // md5-only peer: md5 wins. This is the interop case that a naive
        // "assume xxh128" verify would have broken on every file.
        assert_eq!(
            negotiate(default_ours, "md5 md4").await,
            Some("md5".to_string())
        );
        // A peer that skipped the negotiated strings entirely.
        assert_eq!(negotiate(default_ours, "").await, None);
        // No common ground at all.
        assert_eq!(negotiate("xxh128", "md5 md4").await, None);
        // `with_env_overrides` can retune what we advertise: if we stop
        // leading with xxh128, the rule must follow our real list rather
        // than a hardcoded assumption.
        assert_eq!(
            negotiate("md5 xxh128", "xxh128 md5").await,
            Some("md5".to_string())
        );
        // Substring safety: "xxh128" must not be matched by a peer that
        // only offers "xxh12" or "xxh1284".
        assert_eq!(negotiate("xxh128", "xxh12 xxh1284").await, None);
    }

    /// CLAUDE-AV-B3-18: pin rsync 3.2.7
    /// `checksum.c::csum_len_for_type` for every name the runtime override
    /// can advertise. The xxh3 naming trap is intentional: `xxh3` is the
    /// 64-bit, 8-byte variant, while `xxh128` is 16 bytes.
    #[tokio::test]
    async fn file_checksum_len_follows_the_negotiated_algorithm() {
        let cases = [
            ("xxh128", 16),
            ("xxh3", 8),
            ("xxh64", 8),
            ("xxhash", 8),
            ("md5", 16),
            ("md4", 16),
            ("sha1", 20),
            ("sha256", 32),
            ("sha512", 64),
            ("none", 1),
        ];

        for (algorithm, expected_len) in cases {
            let encoded = encode_server_preamble(&ServerPreamble {
                protocol_version: 31,
                compat_flags: 0x07,
                checksum_algos: algorithm.to_string(),
                compression_algos: "none".to_string(),
                checksum_seed: 0,
                consumed: 0,
            });
            let mut d = make_driver(mock_transport()).with_preamble_profile(PreambleProfile {
                checksum_algos: algorithm.to_string(),
                compression_algos: "none".to_string(),
            });
            d.receive_server_preamble(&encoded).await.unwrap();
            assert_eq!(
                d.negotiated_file_checksum_len(),
                expected_len,
                "{algorithm} must use a {expected_len}-byte file checksum"
            );
        }

        let d = make_driver(mock_transport());
        assert_eq!(
            d.negotiated_file_checksum_len(),
            A2_3_FILE_CHECKSUM_LEN,
            "absent negotiation must retain the historical fallback"
        );
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
            eprintln!("frozen oracle missing: driver preamble pin skipped");
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
        // after the file list: without these bytes the test would
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
            preserve_xattrs: false,
        };
        let mut expected_entry = sample_file_list_entry("target.bin");
        expected_entry.checksum = FileChecksumKind::Md5.digest(&[]);
        let entry_bytes = encode_file_list_entry(&expected_entry, &opts);
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
    async fn write_data_frame_chunks_oversized_payload_wire_safely() {
        // Fix B regression (APPENDIX-AERORSYNC-DELTA-REDESIGN): a logical
        // MSG_DATA payload larger than the 24-bit mux length field must be
        // split into <= MSG_DATA_MAX frames whose concatenation reassembles to
        // the original. At or below the bound it must still go out as a single
        // frame, byte-identical to the pre-chunking driver. Before this an
        // oversized payload was rejected with InvalidFrame, failing every
        // brand-new upload larger than ~16 MiB outright (classic fallback
        // suppressed = data loss).
        use crate::aerorsync::real_wire::{MuxPoll, MuxStreamReader};

        const MAX: usize = AerorsyncDriver::<MockRemoteShellTransport>::MSG_DATA_MAX;

        async fn data_frames_for(payload: &[u8]) -> Vec<Vec<u8>> {
            let transport = mock_transport_with_raw_inbound(Vec::new());
            let last_raw_outbound = transport.last_raw_outbound.clone();
            let mut d = make_driver(transport);
            d.open_raw_stream_internal(&RemoteCommandSpec::upload("/remote/big.bin"))
                .await
                .expect("raw stream opens");
            d.write_data_frame(payload).await.expect("write_data_frame");
            let outbound = {
                let guard = last_raw_outbound.lock().unwrap();
                guard.as_ref().expect("raw stream was opened").clone()
            };
            let bytes = outbound.lock().unwrap().clone();
            // Reassemble through the same reader the receiver uses: it pops one
            // Data frame at a time, exactly as the driver receive loops do.
            let mut reader = MuxStreamReader::new();
            reader.feed(&bytes);
            let mut frames = Vec::new();
            while let Some(res) = reader.poll_frame() {
                match res.expect("well-formed mux frame") {
                    MuxPoll::Data(p) => frames.push(p),
                    other => panic!("expected MSG_DATA frame, got {other:?}"),
                }
            }
            frames
        }

        // Below the bound: one frame, identical bytes.
        let small = vec![0xABu8; 4096];
        let f = data_frames_for(&small).await;
        assert_eq!(f.len(), 1, "<= MSG_DATA_MAX must be a single frame");
        assert_eq!(f[0], small);

        // Exactly at the bound: still a single frame.
        let at = vec![0x5Au8; MAX];
        let f = data_frames_for(&at).await;
        assert_eq!(f.len(), 1, "== MSG_DATA_MAX must be a single frame");
        assert_eq!(f[0].len(), MAX);

        // Over the bound: 2*MAX + 12345 -> three frames, each <= MAX,
        // reassembling to the original. The positional byte pattern proves the
        // frames stay in order.
        let big_len = MAX * 2 + 12_345;
        let big: Vec<u8> = (0..big_len).map(|i| (i % 251) as u8).collect();
        let f = data_frames_for(&big).await;
        assert_eq!(f.len(), 3, "2*MAX + 12345 must be three frames");
        assert!(
            f.iter().all(|fr| !fr.is_empty() && fr.len() <= MAX),
            "every frame must fit the 24-bit length field"
        );
        assert_eq!(
            f.concat(),
            big,
            "frames must reassemble to the original payload"
        );
    }

    #[tokio::test]
    async fn write_delta_with_progress_reports_monotonic_wire_bytes() {
        // Fix A: with a progress sink attached, the upload delta send splits the
        // payload into PROGRESS_CHUNK frames and reports monotonically
        // increasing wire bytes, ending exactly at the payload size, and the
        // frames still reassemble to the original. Without a sink it stays one
        // logical payload (covered by the chunking test above).
        use crate::aerorsync::real_wire::{MuxPoll, MuxStreamReader};
        use std::sync::{Arc, Mutex};

        let calls: Arc<Mutex<Vec<(u64, u64)>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_for_sink = calls.clone();
        let sink: crate::delta_transport::DeltaProgressSink =
            Box::new(move |transferred, total| {
                calls_for_sink.lock().unwrap().push((transferred, total));
            });

        let transport = mock_transport_with_raw_inbound(Vec::new());
        let last_raw_outbound = transport.last_raw_outbound.clone();
        let mut d = make_driver(transport).with_progress_sink(Some(sink));
        d.open_raw_stream_internal(&RemoteCommandSpec::upload("/remote/big.bin"))
            .await
            .expect("raw stream opens");

        let total = AerorsyncDriver::<MockRemoteShellTransport>::PROGRESS_CHUNK * 3 + 512 * 1024;
        let payload: Vec<u8> = (0..total).map(|i| (i % 251) as u8).collect();
        d.write_delta_with_progress(&payload)
            .await
            .expect("delta send with progress");

        let reports = calls.lock().unwrap().clone();
        assert!(!reports.is_empty(), "sink must fire at least once");
        assert_eq!(
            reports.last().unwrap().0,
            total as u64,
            "final report must equal the full payload size"
        );
        assert!(
            reports.iter().all(|&(_, t)| t == total as u64),
            "reported total must stay the payload size"
        );
        assert!(
            reports.windows(2).all(|w| w[0].0 <= w[1].0),
            "reported wire bytes must be monotonically non-decreasing"
        );

        let outbound = {
            let guard = last_raw_outbound.lock().unwrap();
            guard.as_ref().expect("raw stream opened").clone()
        };
        let bytes = outbound.lock().unwrap().clone();
        let mut reader = MuxStreamReader::new();
        reader.feed(&bytes);
        let mut frames = Vec::new();
        while let Some(res) = reader.poll_frame() {
            if let MuxPoll::Data(p) = res.expect("well-formed mux frame") {
                frames.push(p);
            }
        }
        assert_eq!(
            frames.concat(),
            payload,
            "progress-chunked frames must reassemble to the original payload"
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
            preserve_xattrs: false,
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

    /// CLAUDE-AV-B3-18: exact regression for the live xxh64 hang. The
    /// server sends one complete file-list frame with an 8-byte checksum
    /// and a terminator. Reading 16 consumes the terminator as checksum,
    /// reports truncation, and waits for a second frame that never exists.
    #[tokio::test]
    async fn driver_download_accepts_xxh64_filelist_checksum_without_an_extra_frame() {
        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            always_checksum: true,
            csum_len: 8,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
            preserve_xattrs: false,
        };
        let mut entry = sample_file_list_entry("target.bin");
        entry.checksum = vec![0xA5; 8];
        let mut file_list_payload = encode_file_list_entry(&entry, &opts);
        file_list_payload.extend_from_slice(&encode_file_list_terminator(&opts));

        let mut inbound = encode_server_preamble(&ServerPreamble {
            protocol_version: 31,
            compat_flags: 0x07,
            checksum_algos: XXH64_ALGO_NAME.to_string(),
            compression_algos: "none".to_string(),
            checksum_seed: 0x1234_5678,
            consumed: 0,
        });
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &file_list_payload));

        let transport = mock_transport_with_raw_inbound(inbound);
        let mut d = make_driver(transport).with_preamble_profile(PreambleProfile {
            checksum_algos: XXH64_ALGO_NAME.to_string(),
            compression_algos: "none".to_string(),
        });
        d.session_role = Some(SessionRole::Receiver);
        d.open_raw_stream_internal(&RemoteCommandSpec::download("/remote/target.bin"))
            .await
            .unwrap();
        d.perform_preamble_exchange(31, XXH64_ALGO_NAME, "none")
            .await
            .unwrap();
        d.send_download_receiver_phase_prefix().await.unwrap();
        d.receive_file_list_single_file(&mut CollectingSink::default())
            .await
            .expect("the complete xxh64 file-list frame must decode without another read");

        assert_eq!(d.phase(), AerorsyncSessionPhase::FileListReceived);
        assert_eq!(d.file_list().len(), 1);
        assert_eq!(d.file_list()[0].checksum, vec![0xA5; 8]);
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
            preserve_xattrs: false,
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
            preserve_xattrs: false,
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
        // Empty inbound: the driver should surface a transport error
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

    /// A7: Lane 3 live integration test against a real `rsync 3.2.7`
    /// server (Docker harness at `capture/docker-compose.real-rsync.yml`,
    /// listening on `127.0.0.1:2224`).
    ///
    /// The test drives `SshRemoteShellTransport`: not the mock: through
    /// `drive_upload_through_delta` + `finish_session`, then asserts that:
    ///
    /// - upload + finish complete without errors,
    /// - phase reaches `Complete`,
    /// - `session_stats.bytes_sent` is at least the source payload size
    ///   (protocol overhead may raise it above the source length).
    ///
    /// # Gating
    ///
    /// `#[cfg(ci_lane3)]`: the test is compiled only when the
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
            eprintln!("lane 3 Docker harness not reachable on 127.0.0.1:2224: skipping");
            return;
        }

        let source_data: Vec<u8> = incompressible_payload(0xA380_5EED_0001, 1024);

        let key_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/aerorsync/capture/keys/id_ed25519");
        assert!(
            key_path.exists(),
            "ssh key not found at {key_path:?}: is the capture bundle present?"
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
            auth_password: None,
            auth_agent: false,
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

        let entry = live_file_list_entry("lane3-live.bin");
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

    /// P3-T01 W1.2: live counterpart of
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
                "lane 3 Docker harness not reachable on 127.0.0.1:2224: skipping streaming variant"
            );
            return;
        }

        let source_data: Vec<u8> = incompressible_payload(0xA380_5EED_0002, 1024);
        let source_len = source_data.len() as u64;

        let key_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/aerorsync/capture/keys/id_ed25519");
        assert!(
            key_path.exists(),
            "ssh key not found at {key_path:?}: is the capture bundle present?"
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
            auth_password: None,
            auth_agent: false,
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
        let entry = live_file_list_entry("lane3-streaming.bin");
        let entry = FileListEntry {
            size: source_len as i64,
            ..entry
        };
        let spec = RemoteCommandSpec::upload(&remote_path);

        let cursor = std::io::Cursor::new(source_data.clone());
        let upload_res = driver
            .drive_upload_through_delta_streaming(
                spec, entry, cursor, source_len, &adapter, &mut sink,
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
            preserve_xattrs: false,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);

        // A2.3: append an empty delta stream to let the driver reach
        // the stub frontier.
        let empty_delta = encode_delta_stream(&DeltaStreamReport {
            ops: Vec::new(),
            file_checksum: vec![0u8; A2_3_FILE_CHECKSUM_LEN],
        });
        // CLAUDE-AV-B3-18: this test pins xxh128 signature bytes, so use an
        // explicit xxh128 winner instead of the shared 16-byte md5 fixture.
        let mut inbound = encode_server_preamble(&ServerPreamble {
            protocol_version: 31,
            compat_flags: 0x07,
            checksum_algos: XXH128_ALGO_NAME.to_string(),
            compression_algos: "none zstd".to_string(),
            checksum_seed: 0xDEAD_BEEF,
            consumed: 0,
        });
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

        // The outbound capture must contain the receiver prefix frame
        // before the file list read, then the exact mux-wrapped
        // signature blob: first-file header, truncated strong sums, and
        // the five NDX_DONE phase markers.
        let guard = last_raw_outbound.lock().unwrap();
        let outbound_arc = guard.as_ref().expect("raw stream must have been opened");
        let outbound = outbound_arc.lock().unwrap().clone();

        let expected_prefix_frame =
            mux_frame(MuxTag::Data, &[0x00; A2_2_DOWNLOAD_SIGNATURE_PREFIX_ZEROS]);
        assert!(
            outbound
                .windows(expected_prefix_frame.len())
                .any(|w| w == expected_prefix_frame.as_slice()),
            "download receiver prefix frame must be sent before signature bytes"
        );

        let mut ndx_state = NdxState::default();
        let head = SumHead {
            count: 4,
            block_length: 1024,
            checksum_length: A2_2_DOWNLOAD_S2LENGTH,
            remainder_length: 512,
        };
        let blocks = vec![
            SumBlock {
                rolling: 0xA0A0A0A0,
                strong: compute_xxh128_wire_with_seed(
                    &destination_data[0..1024],
                    d.checksum_seed() as u64,
                )[..A2_2_DOWNLOAD_S2LENGTH as usize]
                    .to_vec(),
            },
            SumBlock {
                rolling: 0xB0B0B0B0,
                strong: compute_xxh128_wire_with_seed(
                    &destination_data[1024..2048],
                    d.checksum_seed() as u64,
                )[..A2_2_DOWNLOAD_S2LENGTH as usize]
                    .to_vec(),
            },
            SumBlock {
                rolling: 0xC0C0C0C0,
                strong: compute_xxh128_wire_with_seed(
                    &destination_data[2048..3072],
                    d.checksum_seed() as u64,
                )[..A2_2_DOWNLOAD_S2LENGTH as usize]
                    .to_vec(),
            },
            SumBlock {
                rolling: 0xD0D0D0D0,
                strong: compute_xxh128_wire_with_seed(
                    &destination_data[3072..3584],
                    d.checksum_seed() as u64,
                )[..A2_2_DOWNLOAD_S2LENGTH as usize]
                    .to_vec(),
            },
        ];
        let mut expected_payload = Vec::new();
        expected_payload.extend_from_slice(&encode_ndx(A2_2_FIRST_FILE_NDX, &mut ndx_state));
        expected_payload.extend_from_slice(&encode_item_flags(A2_2_DOWNLOAD_IFLAGS));
        expected_payload.extend_from_slice(&encode_sum_head(&head));
        for block in &blocks {
            expected_payload.extend_from_slice(&encode_sum_block(block));
        }
        expected_payload.extend_from_slice(&[0x00; A2_2_DOWNLOAD_SIGNATURE_TAIL_NDX_DONE_COUNT]);
        let expected_frame = mux_frame(MuxTag::Data, &expected_payload);

        assert!(
            outbound
                .windows(expected_frame.len())
                .any(|w| w == expected_frame.as_slice()),
            "download receiver signature frame must match frozen prefix + NDX_DONE tail"
        );
    }

    #[test]
    fn download_signature_strong_uses_seeded_xxh128_like_frozen_oracle() {
        let Some(frozen) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
            eprintln!("frozen oracle missing: download signature strong pin skipped");
            return;
        };

        let server_pre = decode_server_preamble(&frozen.download_server_to_client)
            .expect("download server preamble");
        let client_pre = decode_client_preamble(&frozen.download_client_to_server)
            .expect("download client preamble");
        let app = reassemble_msg_data(&frozen.download_client_to_server[client_pre.consumed..])
            .expect("download client app stream")
            .app_stream;

        let mut cursor = A2_2_DOWNLOAD_SIGNATURE_PREFIX_ZEROS;
        let mut ndx_state = NdxState::default();
        let (_ndx, n) = decode_ndx(&app[cursor..], &mut ndx_state).expect("file ndx");
        cursor += n;
        let (_iflags, n) = decode_item_flags(&app[cursor..]).expect("iflags");
        cursor += n;
        let (head, n) = decode_sum_head(&app[cursor..]).expect("sum head");
        cursor += n;
        let (first_block, _) = decode_sum_block(&app[cursor..], head.checksum_length as usize)
            .expect("first sum block");

        let baseline = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src/aerorsync/capture/workspace/real/local/download.bin"),
        )
        .expect("frozen download baseline");
        let expected = compute_xxh128_wire_with_seed(
            &baseline[..head.block_length as usize],
            server_pre.checksum_seed as u64,
        );

        assert_eq!(
            first_block.strong,
            expected[..head.checksum_length as usize],
            "download receiver signatures must use rsync's seeded xxh128 per-block strong bytes"
        );
    }

    #[tokio::test]
    async fn driver_upload_signature_phase_aborts_on_terminal_oob() {
        // Error frame during the signature phase: driver must bail
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
    async fn driver_upload_treats_ndx_done_signature_as_noop() {
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &[0x00; 5]));

        let transport = mock_transport_with_raw_inbound(inbound);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        d.drive_upload_through_delta(
            RemoteCommandSpec::upload("/remote/target.bin"),
            sample_file_list_entry("target.bin"),
            b"already present",
            &MockSigAdapter::default(),
            &mut sink,
        )
        .await
        .unwrap();

        assert!(d.upload_noop_transfer);
        assert!(!d.committed(), "no delta bytes are emitted for a no-op");
        assert!(d.received_sum_head().is_none());
        assert!(d.received_signatures().is_empty());
        assert!(d.emitted_delta_ops().is_empty());

        d.finish_session(&mut sink).await.unwrap();
        assert_eq!(d.phase(), AerorsyncSessionPhase::Complete);
        assert!(d.session_stats().bytes_sent > 0);
    }

    #[tokio::test]
    async fn streaming_upload_treats_ndx_done_signature_as_noop() {
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &[0x00; 5]));

        let transport = mock_transport_with_raw_inbound(inbound);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();

        d.drive_upload_through_delta_streaming(
            RemoteCommandSpec::upload("/remote/target.bin"),
            sample_file_list_entry("target.bin"),
            tokio::io::empty(),
            123,
            &MockSigAdapter::default(),
            &mut sink,
        )
        .await
        .unwrap();

        assert!(d.upload_noop_transfer);
        assert!(!d.committed());
        assert!(d.emitted_delta_ops().is_empty());
        d.finish_session(&mut sink).await.unwrap();
        assert_eq!(d.phase(), AerorsyncSessionPhase::Complete);
    }

    #[tokio::test]
    async fn driver_upload_treats_skip_notice_as_noop() {
        // Stock rsync 3.2.7 skips an up-to-date file with `ndx + iflags`
        // lacking ITEM_TRANSFER and NO sum_head (live-captured shape
        // 2026-07-21: `02 08 00` = ndx 1, iflags 0x0008). The driver must
        // treat it as a clean no-op, not a protocol violation, and the
        // finish-side phase loop consumes the remaining markers.
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &[0x02, 0x08, 0x00]));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &[0x00; 5]));

        let transport = mock_transport_with_raw_inbound(inbound);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        d.drive_upload_through_delta(
            RemoteCommandSpec::upload("/remote/target.bin"),
            sample_file_list_entry("target.bin"),
            b"already present",
            &MockSigAdapter::default(),
            &mut sink,
        )
        .await
        .unwrap();

        assert!(d.upload_noop_transfer);
        assert!(!d.committed(), "no delta bytes are emitted for a skip");
        assert!(d.received_sum_head().is_none());
        assert!(d.received_signatures().is_empty());
        assert!(d.emitted_delta_ops().is_empty());

        d.finish_session(&mut sink).await.unwrap();
        assert_eq!(d.phase(), AerorsyncSessionPhase::Complete);
    }

    // ---- Y-RSC.4 symlink tests ------------------------------------------

    #[tokio::test]
    async fn driver_upload_symlink_emits_flist_only_and_finishes_noop() {
        // A symlink upload is flist-only: the generator creates the link
        // from the entry and answers with the phase markers, never with
        // a transfer request. Scripted inbound mirrors the no-op upload
        // shape (5 NDX_DONE markers across drive + finish).
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &[0x00; 5]));

        let transport = mock_transport_with_raw_inbound(inbound);
        let last_raw_outbound = transport.last_raw_outbound.clone();
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();

        let target = "../data/target.bin";
        let entry = symlink_file_list_entry("link.lnk", target);
        d.drive_upload_symlink(
            RemoteCommandSpec::upload("/remote/link.lnk"),
            entry.clone(),
            &mut sink,
        )
        .await
        .expect("symlink upload must complete as a flist-only no-op");

        assert!(d.upload_noop_transfer, "symlinks have no delta phase");
        assert!(!d.committed(), "no delta bytes are emitted for a symlink");
        assert!(d.received_sum_head().is_none());
        assert!(d.received_signatures().is_empty());
        assert!(d.emitted_delta_ops().is_empty());

        // Pinned outbound shape: client preamble, then ONE coalesced
        // MSG_DATA frame with entry + terminator + NDX_FLIST_EOF. The
        // entry must ride the wire with the S_IFLNK mode, the raw target
        // bytes, and NO flist checksum (csum_len 0 in the driver's own
        // options because the checksum stays empty for links).
        let outbound_arc = {
            let guard = last_raw_outbound.lock().unwrap();
            guard
                .as_ref()
                .expect("raw stream must have been opened")
                .clone()
        };
        let outbound = outbound_arc.lock().unwrap().clone();
        let expected_client = encode_client_preamble(&ClientPreamble {
            protocol_version: 31,
            checksum_algos: "xxh128 xxh3 xxh64 md5 md4".to_string(),
            compression_algos: "zstd lz4 zlibx zlib".to_string(),
            consumed: 0,
        });
        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            always_checksum: true,
            csum_len: 0,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
            preserve_xattrs: false,
        };
        let mut flist_payload = encode_file_list_entry(&entry, &opts);
        flist_payload.extend_from_slice(&encode_file_list_terminator(&opts));
        let mut ndx_state = NdxState::default();
        flist_payload.extend_from_slice(&encode_ndx(NDX_FLIST_EOF, &mut ndx_state));
        let expected_flist_frame = mux_frame(MuxTag::Data, &flist_payload);
        let suffix_start = expected_client.len();
        assert_eq!(
            &outbound[suffix_start..suffix_start + expected_flist_frame.len()],
            expected_flist_frame.as_slice(),
            "symlink flist frame mismatch (entry + terminator + NDX_FLIST_EOF coalesced)"
        );
        let payload_str = String::from_utf8_lossy(&flist_payload);
        assert!(
            payload_str.contains(target),
            "encoded flist payload must carry the raw symlink target bytes"
        );

        d.finish_session(&mut sink).await.unwrap();
        assert_eq!(d.phase(), AerorsyncSessionPhase::Complete);
        assert!(d.session_stats().bytes_sent > 0);
    }

    #[tokio::test]
    async fn driver_upload_symlink_rejects_transfer_request() {
        // A peer that answers a symlink entry with ITEM_TRANSFER wants a
        // token stream this driver has no bytes for. Fail closed instead
        // of deadlocking on a delta phase that cannot happen.
        let head = SumHead {
            count: 0,
            block_length: 1024,
            checksum_length: 2,
            remainder_length: 0,
        };
        let sig_payload = build_sig_phase_payload(1, 0x8002, &head, &[]);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &sig_payload));

        let transport = mock_transport_with_raw_inbound(inbound);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        let err = d
            .drive_upload_symlink(
                RemoteCommandSpec::upload("/remote/link.lnk"),
                symlink_file_list_entry("link.lnk", "t.bin"),
                &mut sink,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind, AerorsyncErrorKind::InvalidFrame);
        assert_eq!(d.phase(), AerorsyncSessionPhase::Failed);
    }

    #[tokio::test]
    async fn driver_upload_symlink_rejects_non_symlink_entry() {
        // The dedicated entry point owns S_IFLNK entries only; a regular
        // file must keep using the delta pipeline.
        let mut d = make_driver(mock_transport());
        let mut sink = CollectingSink::default();
        let err = d
            .drive_upload_symlink(
                RemoteCommandSpec::upload("/remote/target.bin"),
                live_file_list_entry("target.bin"),
                &mut sink,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind, AerorsyncErrorKind::IllegalStateTransition);
    }

    #[tokio::test]
    async fn driver_regular_upload_paths_reject_symlink_entry() {
        // Symlinks pushed through the delta pipeline would wait forever
        // on a signature header the generator never sends. Both regular
        // entry points must refuse before any wire byte goes out.
        let entry = symlink_file_list_entry("link.lnk", "t.bin");

        let mut d = make_driver(mock_transport());
        let mut sink = CollectingSink::default();
        let err = d
            .drive_upload_through_delta(
                RemoteCommandSpec::upload("/remote/link.lnk"),
                entry.clone(),
                b"",
                &MockSigAdapter::default(),
                &mut sink,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind, AerorsyncErrorKind::IllegalStateTransition);

        let mut d = make_driver(mock_transport());
        let err = d
            .drive_upload_through_delta_streaming(
                RemoteCommandSpec::upload("/remote/link.lnk"),
                entry,
                std::io::Cursor::new(Vec::new()),
                0,
                &MockSigAdapter::default(),
                &mut sink,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind, AerorsyncErrorKind::IllegalStateTransition);
    }

    #[tokio::test]
    async fn driver_download_symlink_skips_signature_and_delta_phases() {
        use crate::aerorsync::engine_adapter::MemoryBaseline;

        // Inbound: preamble + flist(symlink entry + terminator), then
        // directly the sender's finish tail (3 leading NDX_DONE +
        // SummaryFrame + trailing marker). No sender delta prefix: the
        // generator never requests symlinks, so the sender has nothing
        // to send for them.
        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
            preserve_xattrs: false,
        };
        let target = "../rel/target.bin";
        let entry = symlink_file_list_entry("link.lnk", target);
        let entry_bytes = encode_file_list_entry(&entry, &opts);
        let term_bytes = encode_file_list_terminator(&opts);
        let mut finish_tail = vec![0x00; PRE_SUMMARY_NDX_DONE_COUNT_DOWNLOAD];
        finish_tail.extend_from_slice(&build_summary_frame_bytes(31));

        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &finish_tail));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &[0x00]));

        let transport = mock_transport_with_raw_inbound(inbound);
        let last_raw_outbound = transport.last_raw_outbound.clone();
        let mut baseline = MemoryBaseline::new(Vec::new());
        let (mut writer, captured) = MockAsyncWriter::new();
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();

        d.drive_download_through_delta_streaming(
            RemoteCommandSpec::download("/remote/link.lnk"),
            &mut baseline,
            &mut writer,
            &MockSigAdapter::default(),
            &mut sink,
        )
        .await
        .expect("symlink download must complete without signature/delta phases");

        let downloaded = d.downloaded_entry().expect("flist entry retained");
        assert!(is_symlink_mode(downloaded.mode));
        assert_eq!(downloaded.symlink_target.as_deref(), Some(target));
        assert!(
            downloaded.checksum.is_empty(),
            "symlink entries carry no flist checksum on the wire"
        );
        assert!(
            captured.lock().expect("captured lock").is_empty(),
            "no reconstruction bytes may reach the writer for a symlink"
        );
        assert!(d.reconstructed().is_none());
        assert!(d.sent_sum_head().is_none(), "no signature phase for links");
        assert!(d.received_file_checksum().is_none());
        assert_eq!(d.phase(), AerorsyncSessionPhase::DeltaReceived);

        d.finish_session(&mut sink).await.unwrap();
        assert_eq!(d.phase(), AerorsyncSessionPhase::Complete);

        // Outbound pin: client preamble, then exactly the receiver
        // housekeeping prefix (4 zero bytes), the phase-marker tail
        // (5 NDX_DONE), and the finish ACK marker: never an
        // ndx + iflags + sum_head signature request for the link.
        let outbound_arc = {
            let guard = last_raw_outbound.lock().unwrap();
            guard
                .as_ref()
                .expect("raw stream must have been opened")
                .clone()
        };
        let outbound = outbound_arc.lock().unwrap().clone();
        let expected_client = encode_client_preamble(&ClientPreamble {
            protocol_version: 31,
            checksum_algos: "xxh128 xxh3 xxh64 md5 md4".to_string(),
            compression_algos: "zstd lz4 zlibx zlib".to_string(),
            consumed: 0,
        });
        let expected_after_preamble = [
            mux_frame(MuxTag::Data, &[0x00; A2_2_DOWNLOAD_SIGNATURE_PREFIX_ZEROS]),
            mux_frame(
                MuxTag::Data,
                &[0x00; A2_2_DOWNLOAD_SIGNATURE_TAIL_NDX_DONE_COUNT],
            ),
            mux_frame(MuxTag::Data, &[0x00]),
        ]
        .concat();
        assert_eq!(
            &outbound[expected_client.len()..],
            expected_after_preamble.as_slice(),
            "symlink download outbound must be housekeeping + phase markers only"
        );
    }

    #[tokio::test]
    async fn driver_download_symlink_bulk_path_skips_phases_too() {
        // Bulk twin of the streaming test: `drive_download_through_delta`
        // must take the same flist-only branch. `reconstructed` stays
        // None on purpose: a symlink has no content stream, consumers
        // read `downloaded_entry().symlink_target` instead.
        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
            preserve_xattrs: false,
        };
        let entry = symlink_file_list_entry("link.lnk", "t/rel.bin");
        let entry_bytes = encode_file_list_entry(&entry, &opts);
        let term_bytes = encode_file_list_terminator(&opts);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));

        let transport = mock_transport_with_raw_inbound(inbound);
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();
        d.drive_download_through_delta(
            RemoteCommandSpec::download("/remote/link.lnk"),
            b"",
            &MockSigAdapter::default(),
            &mut sink,
        )
        .await
        .expect("bulk symlink download must complete after the flist");

        assert!(d.reconstructed().is_none());
        assert!(d.sent_sum_head().is_none());
        assert_eq!(
            d.downloaded_entry()
                .and_then(|e| e.symlink_target.as_deref()),
            Some("t/rel.bin")
        );
        assert_eq!(d.phase(), AerorsyncSessionPhase::DeltaReceived);
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
            preserve_xattrs: false,
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
            eprintln!("frozen oracle missing: A2.2 upload sig pin skipped");
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

        // The canonical test preamble negotiates md5. The outbound
        // capture must therefore contain the real negotiated trailer,
        // not the old hardcoded xxh128 bytes or the prototype's zero
        // placeholder.
        let expected_trailer = FileChecksumKind::Md5.digest(b"hello\0\0\0world");
        assert_eq!(expected_trailer.len(), 16);
        assert!(
            !expected_trailer.iter().all(|&b| b == 0),
            "md5 of a non-empty payload must not be all-zero"
        );
        let guard = last_raw_outbound.lock().unwrap();
        let outbound_arc = guard.as_ref().expect("raw stream must have opened");
        let outbound = outbound_arc.lock().unwrap().clone();
        assert!(
            outbound
                .windows(16)
                .any(|w| w == expected_trailer.as_slice()),
            "real negotiated md5 trailer must appear in outbound bytes"
        );
        assert!(d.sent_data_bytes() > 0);
    }

    #[tokio::test]
    async fn upload_file_list_and_trailer_follow_negotiated_algo_in_bulk_and_streaming_paths() {
        use md5::{Digest, Md5};

        let source = b"rsync-c-full-known-vector";
        let cases = [
            (XXH128_ALGO_NAME, compute_xxh128_wire(source)),
            (MD5_ALGO_NAME, Md5::digest(source).to_vec()),
            (
                XXH64_ALGO_NAME,
                vec![0x52, 0x80, 0x6e, 0xc1, 0x30, 0x3f, 0x06, 0x34],
            ),
            (
                XXH3_ALGO_NAME,
                vec![0x1b, 0x50, 0x88, 0xcd, 0xd5, 0x07, 0x4a, 0xd3],
            ),
            // Y-RSC.3: md4 and sha1 file-list digest + trailer are plain
            // UNSEEDED hashes even though the fixture preamble carries a
            // nonzero checksum_seed (0x1234_5678): `sum_init` /
            // `file_checksum` never seed the negotiated winner. Expected
            // bytes from an independent python oracle (RFC 1320 MD4 /
            // hashlib sha1), not from the crates under test.
            (
                MD4_ALGO_NAME,
                vec![
                    0x55, 0x78, 0x31, 0x98, 0xcb, 0x18, 0x55, 0xc7, 0x10, 0x84, 0x40, 0x04, 0xf6,
                    0x0d, 0x35, 0x19,
                ],
            ),
            (
                SHA1_ALGO_NAME,
                vec![
                    0xec, 0xb3, 0xea, 0x04, 0x18, 0x20, 0x2b, 0x92, 0xfd, 0x27, 0x80, 0xbe, 0x94,
                    0xed, 0xc4, 0xc4, 0xc6, 0x4b, 0x70, 0x07,
                ],
            ),
        ];

        for (algorithm, expected) in cases {
            let head = SumHead {
                count: 0,
                block_length: 0,
                checksum_length: 0,
                remainder_length: 0,
            };
            let sig_payload = build_sig_phase_payload(1, 0x8002, &head, &[]);
            let server_preamble = encode_server_preamble(&ServerPreamble {
                protocol_version: 31,
                compat_flags: 0x07 | CF_CHKSUM_SEED_FIX,
                checksum_algos: algorithm.to_string(),
                compression_algos: "none".to_string(),
                checksum_seed: 0x1234_5678,
                consumed: 0,
            });
            let profile = PreambleProfile {
                checksum_algos: algorithm.to_string(),
                compression_algos: "none".to_string(),
            };

            let mut bulk_inbound = server_preamble.clone();
            bulk_inbound.extend_from_slice(&mux_frame(MuxTag::Data, &sig_payload));
            let bulk_transport = mock_transport_with_raw_inbound(bulk_inbound);
            let bulk_outbound = bulk_transport.last_raw_outbound.clone();
            let mut bulk_driver =
                make_driver(bulk_transport).with_preamble_profile(profile.clone());
            bulk_driver
                .drive_upload_through_delta(
                    RemoteCommandSpec::upload("/remote/target.bin"),
                    sample_file_list_entry("target.bin"),
                    source,
                    &CurrentDeltaSyncBridge::new(),
                    &mut CollectingSink::default(),
                )
                .await
                .unwrap_or_else(|error| panic!("bulk {algorithm} upload failed: {error:?}"));
            let bulk_bytes = {
                let guard = bulk_outbound.lock().unwrap();
                let bytes = guard.as_ref().expect("bulk raw stream").lock().unwrap();
                bytes.clone()
            };
            let (bulk_flist, bulk_trailer) =
                decode_upload_file_and_trailer_checksums(&bulk_bytes, expected.len());
            assert_eq!(bulk_flist, expected, "bulk {algorithm} file-list digest");
            assert_eq!(bulk_trailer, expected, "bulk {algorithm} trailer");

            let mut stream_inbound = server_preamble;
            stream_inbound.extend_from_slice(&mux_frame(MuxTag::Data, &sig_payload));
            let stream_transport = mock_transport_with_raw_inbound(stream_inbound);
            let stream_outbound = stream_transport.last_raw_outbound.clone();
            let mut stream_driver = make_driver(stream_transport).with_preamble_profile(profile);
            stream_driver
                .drive_upload_through_delta_streaming(
                    RemoteCommandSpec::upload("/remote/target.bin"),
                    sample_file_list_entry("target.bin"),
                    std::io::Cursor::new(source.to_vec()),
                    source.len() as u64,
                    &CurrentDeltaSyncBridge::new(),
                    &mut CollectingSink::default(),
                )
                .await
                .unwrap_or_else(|error| panic!("streaming {algorithm} upload failed: {error:?}"));
            let stream_bytes = {
                let guard = stream_outbound.lock().unwrap();
                let bytes = guard.as_ref().expect("stream raw stream").lock().unwrap();
                bytes.clone()
            };
            let (stream_flist, stream_trailer) =
                decode_upload_file_and_trailer_checksums(&stream_bytes, expected.len());
            assert_eq!(
                stream_flist, expected,
                "streaming {algorithm} file-list digest"
            );
            assert_eq!(stream_trailer, expected, "streaming {algorithm} trailer");
        }
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
        // ~64 KiB: comfortably above `MAX_DELTA_LITERAL_LEN = 16383`
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
        // Reaches the A2.3 stub frontier: the delta phase itself must
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
                    "chunk size {} out of (0, {}]: S8j must clamp every \
                     chunk to the 14-bit DEFLATED_DATA length budget",
                    compressed_payload.len(),
                    MAX_DELTA_LITERAL_LEN
                );
            }
        }

        // Round-trip: concatenating the compressed chunks back through
        // the session DCtx must recover the original bytes: this is
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
            preserve_xattrs: false,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);

        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &download_sender_prefix()));
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
        // Download stays PreCommit: the reconstructed bytes are in RAM,
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

        // Build a 64 KiB pseudo-random logical literal: zstd cannot
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
        // wire chunks: exactly what stock rsync's `send_zstd_token`
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
            preserve_xattrs: false,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);

        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &download_sender_prefix()));
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

        // Reconstructed must equal BLK1 + logical_literal + BLK2 -
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
    async fn driver_download_delta_treats_ndx_done_as_noop_and_finishes() {
        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
            preserve_xattrs: false,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);
        let summary_bytes = build_summary_frame_bytes(31);

        let mut noop_and_tail = Vec::new();
        noop_and_tail.push(0x00);
        noop_and_tail.extend_from_slice(&[0x00; PRE_SUMMARY_NDX_DONE_COUNT_DOWNLOAD]);
        noop_and_tail.extend_from_slice(&summary_bytes);

        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &noop_and_tail));

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

        d.drive_download_through_delta(
            RemoteCommandSpec::download("/remote/target.bin"),
            &destination_data,
            &adapter,
            &mut sink,
        )
        .await
        .expect("download no-op delta succeeds");

        assert_eq!(d.reconstructed(), Some(destination_data.as_slice()));
        assert!(
            d.received_file_checksum().is_none(),
            "NDX_DONE no-op carries no file checksum trailer"
        );
        assert_eq!(d.phase(), AerorsyncSessionPhase::DeltaReceived);

        d.finish_session(&mut sink)
            .await
            .expect("summary tail after no-op delta is preserved");
        assert_eq!(d.phase(), AerorsyncSessionPhase::Complete);
        assert!(d.received_summary().is_some());
    }

    #[tokio::test]
    async fn driver_download_delta_treats_clean_eof_as_noop_and_finishes() {
        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
            preserve_xattrs: false,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);

        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));

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

        d.drive_download_through_delta(
            RemoteCommandSpec::download("/remote/target.bin"),
            &destination_data,
            &adapter,
            &mut sink,
        )
        .await
        .expect("clean EOF after signatures is a no-op download");

        assert_eq!(d.reconstructed(), Some(destination_data.as_slice()));
        assert!(d.received_file_checksum().is_none());

        d.finish_session(&mut sink)
            .await
            .expect("clean EOF no-op has no summary tail to drain");
        assert_eq!(d.phase(), AerorsyncSessionPhase::Complete);
        assert!(d.received_summary().is_none());
        assert!(d.session_stats().bytes_sent > 0);
    }

    /// Y-RSC.2 acceptance: the clean-EOF no-op classification must be
    /// invariant under a full rewording of the transport error text.
    /// Identical fixture to
    /// `driver_download_delta_treats_clean_eof_as_noop_and_finishes`, but
    /// the mock's exhaustion detail shares no substring with the
    /// historical magic strings ("remote closed (exit 0)" / "simulated
    /// remote close"). Only the structured `TransportErrorClass::CleanEof`
    /// marker can classify it; the pre-Y-RSC.2 substring matcher would
    /// have surfaced a hard transport error here.
    #[tokio::test]
    async fn driver_download_clean_eof_noop_is_invariant_to_transport_rewording() {
        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
            preserve_xattrs: false,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);

        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));

        let cfg = MockTransportConfig::healthy_upload()
            .with_raw_inbound(inbound)
            .with_raw_exhausted_detail("peer finished and went away: wording rotated for Y-RSC.2");
        let transport = MockRemoteShellTransport::new(cfg);
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

        d.drive_download_through_delta(
            RemoteCommandSpec::download("/remote/target.bin"),
            &destination_data,
            &adapter,
            &mut sink,
        )
        .await
        .expect("reworded clean EOF must still be a no-op download");

        assert_eq!(d.reconstructed(), Some(destination_data.as_slice()));
        assert!(d.received_file_checksum().is_none());

        d.finish_session(&mut sink)
            .await
            .expect("reworded clean EOF no-op has no summary tail to drain");
        assert_eq!(d.phase(), AerorsyncSessionPhase::Complete);
        assert!(d.received_summary().is_none());
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
            preserve_xattrs: false,
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
            preserve_xattrs: false,
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
        // Cancel the driver before it reaches the delta phase: the
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
            preserve_xattrs: false,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &download_sender_prefix()));
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
        // Other SessionStats fields remain at their default: A4 populates
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
        // No summary frame: but a cancel will fire before the read.
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
            preserve_xattrs: false,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);
        let summary_bytes = build_summary_frame_bytes(31);
        // S8j: real rsync 3.2.7 emits exactly 3 leading NDX_DONE
        // markers between the delta stream's file-csum trailer and the
        // SummaryFrame. `finish_session` on a Receiver-role driver now
        // drains them before decoding: replicate that shape here.
        let ndx_done_leading: Vec<u8> = vec![0x00; PRE_SUMMARY_NDX_DONE_COUNT_DOWNLOAD];
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &download_sender_prefix()));
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
        //: `SIVAL64` is the LE 64-bit writer. A future rsync version
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
            preserve_xattrs: false,
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
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &download_sender_prefix()));
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
            preserve_xattrs: false,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);

        // First byte is NDX_DONE (drain enters the strict path), second
        // byte is garbage: the drain must refuse.
        let poisoned = vec![0x00, 0xAB, 0xCD, 0xEF, 0xFE];
        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &download_sender_prefix()));
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
        // 1 sum_block): same shape as
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
            preserve_xattrs: false,
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
    // on the wire: root-cause is one of:
    //   1. the producer (W1.1) emits different ops than `compute_delta`
    //      (covered by `producer_streaming_matches_bulk_*` in
    //      `engine_adapter.rs`),
    //   2. the streaming xxh3 trailer no longer matches `xxh3_128`
    //      bulk (covered by `streaming_xxh3_matches_bulk_xxh3` below),
    //   3. the post-plan emission code (zstd compression, wire op
    //      construction, NDX/iflags/sum_head echo, payload framing) was
    //      changed in only one of the two functions: they MUST stay
    //      bit-for-bit symmetric until W1.3 lifts the upload cap.
    //
    // The tests avoid relying on `MockSigAdapter`: the bulk path now
    // calls `compute_delta` against `RealEngineAdapter`'s real rolling
    // checksum engine (see `_adapter` parameter in the streaming path,
    // currently unused: both paths derive the plan from
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
    /// the two paths diverged: start hunting there.
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

    /// Empty source against a `block_size == 0` server head: the
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
        // 5 KB pseudo-random source: rolling sums won't hit 0xAAAAAAAA.
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

    /// P3-T01 W1.3: `block_size == 0` chunked-literal pin.
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
            "block_size==0 with source > STREAMING_READ_CHUNK_BYTES MUST chunk the literal: bulk and streaming wire bytes must differ"
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
        // deferred". S8j (2026-04-26) removed that bail: the driver
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
        // with ndx / sum_head / delta frames that A2.1 does not handle -
        // the driver will surface an `InvalidFrame` error from the
        // decoder when it tries to read past the terminator as another
        // file-list entry. We accept both terminations as long as at
        // least one entry has already been absorbed.
        let Some(frozen) = RealRsyncBaselineByteTranscript::try_load_frozen() else {
            eprintln!("frozen oracle missing: A2.1 driver pin skipped");
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
        // downstream frame: both are acceptable for A2.1 so long as the
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
    // P3-T01 W2.4: drive_download_through_delta_streaming tests
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
        let compressed =
            compress_zstd_literal_stream(&[literal]).expect("zstd compress fixture literal");
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
            preserve_xattrs: false,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);

        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &download_sender_prefix()));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &delta_bytes));
        inbound
    }

    /// W2.4 test 1: happy path: streaming download decodes the wire
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
        let mut baseline = MemoryBaseline::new(b"BLK1BLK2".to_vec());

        let (mut writer, captured) = MockAsyncWriter::new();
        let mut d = make_driver(transport);

        let mut sink = CollectingSink::default();
        d.drive_download_through_delta_streaming(
            RemoteCommandSpec::download("/remote/target.bin"),
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
        // `committed` stays false on the driver: matches the bulk-path
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

    #[tokio::test]
    async fn driver_download_streaming_treats_ndx_done_as_noop_and_writes_baseline() {
        use crate::aerorsync::engine_adapter::MemoryBaseline;

        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
            preserve_xattrs: false,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);
        let summary_bytes = build_summary_frame_bytes(31);

        let mut noop_and_tail = Vec::new();
        noop_and_tail.push(0x00);
        noop_and_tail.extend_from_slice(&[0x00; PRE_SUMMARY_NDX_DONE_COUNT_DOWNLOAD]);
        noop_and_tail.extend_from_slice(&summary_bytes);

        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &noop_and_tail));

        let transport = mock_transport_with_raw_inbound(inbound);
        let adapter = MockSigAdapter::with_fixed_signatures(
            4,
            vec![
                make_engine_sig(0, 0xA0, 0x01, 4),
                make_engine_sig(1, 0xA1, 0x02, 4),
            ],
        );
        let baseline_bytes = b"BLK1BLK2".to_vec();
        let mut baseline = MemoryBaseline::new(baseline_bytes.clone());
        let (mut writer, captured) = MockAsyncWriter::new();
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();

        d.drive_download_through_delta_streaming(
            RemoteCommandSpec::download("/remote/target.bin"),
            &mut baseline,
            &mut writer,
            &adapter,
            &mut sink,
        )
        .await
        .expect("streaming no-op delta succeeds");

        let on_writer = captured.lock().expect("captured lock").clone();
        assert_eq!(on_writer, baseline_bytes);
        assert!(d.reconstructed().is_none());
        assert!(d.received_file_checksum().is_none());

        d.finish_session(&mut sink)
            .await
            .expect("summary tail after no-op delta is preserved");
        assert_eq!(d.phase(), AerorsyncSessionPhase::Complete);
    }

    #[tokio::test]
    async fn driver_download_streaming_treats_clean_eof_as_noop_and_writes_baseline() {
        use crate::aerorsync::engine_adapter::MemoryBaseline;

        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
            preserve_xattrs: false,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);

        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));

        let transport = mock_transport_with_raw_inbound(inbound);
        let adapter = MockSigAdapter::with_fixed_signatures(
            4,
            vec![
                make_engine_sig(0, 0xA0, 0x01, 4),
                make_engine_sig(1, 0xA1, 0x02, 4),
            ],
        );
        let baseline_bytes = b"BLK1BLK2".to_vec();
        let mut baseline = MemoryBaseline::new(baseline_bytes.clone());
        let (mut writer, captured) = MockAsyncWriter::new();
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();

        d.drive_download_through_delta_streaming(
            RemoteCommandSpec::download("/remote/target.bin"),
            &mut baseline,
            &mut writer,
            &adapter,
            &mut sink,
        )
        .await
        .expect("streaming clean EOF no-op succeeds");

        let on_writer = captured.lock().expect("captured lock").clone();
        assert_eq!(on_writer, baseline_bytes);
        assert!(d.reconstructed().is_none());
        assert!(d.received_file_checksum().is_none());

        d.finish_session(&mut sink)
            .await
            .expect("streaming clean EOF no-op has no summary tail");
        assert_eq!(d.phase(), AerorsyncSessionPhase::Complete);
        assert!(d.session_stats().bytes_sent > 0);
    }

    /// Y-RSC.2 acceptance, streaming twin of
    /// `driver_download_clean_eof_noop_is_invariant_to_transport_rewording`:
    /// the streaming receive path guards on the same structured class, so
    /// a reworded transport detail must not change its no-op behaviour.
    #[tokio::test]
    async fn driver_download_streaming_clean_eof_noop_is_invariant_to_transport_rewording() {
        use crate::aerorsync::engine_adapter::MemoryBaseline;

        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: true,
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
            preserve_xattrs: false,
        };
        let entry_bytes = encode_file_list_entry(&sample_file_list_entry("target.bin"), &opts);
        let term_bytes = encode_file_list_terminator(&opts);

        let mut inbound = canonical_server_preamble_bytes();
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &entry_bytes));
        inbound.extend_from_slice(&mux_frame(MuxTag::Data, &term_bytes));

        let cfg = MockTransportConfig::healthy_upload()
            .with_raw_inbound(inbound)
            .with_raw_exhausted_detail("copesetic teardown, nothing left to stream");
        let transport = MockRemoteShellTransport::new(cfg);
        let adapter = MockSigAdapter::with_fixed_signatures(
            4,
            vec![
                make_engine_sig(0, 0xA0, 0x01, 4),
                make_engine_sig(1, 0xA1, 0x02, 4),
            ],
        );
        let baseline_bytes = b"BLK1BLK2".to_vec();
        let mut baseline = MemoryBaseline::new(baseline_bytes.clone());
        let (mut writer, captured) = MockAsyncWriter::new();
        let mut d = make_driver(transport);
        let mut sink = CollectingSink::default();

        d.drive_download_through_delta_streaming(
            RemoteCommandSpec::download("/remote/target.bin"),
            &mut baseline,
            &mut writer,
            &adapter,
            &mut sink,
        )
        .await
        .expect("reworded streaming clean EOF must still be a no-op download");

        let on_writer = captured.lock().expect("captured lock").clone();
        assert_eq!(on_writer, baseline_bytes);
        assert!(d.reconstructed().is_none());
        assert!(d.received_file_checksum().is_none());

        d.finish_session(&mut sink)
            .await
            .expect("reworded streaming clean EOF no-op has no summary tail");
        assert_eq!(d.phase(), AerorsyncSessionPhase::Complete);
    }

    /// W2.4/Y-RSC.5 test 2: pin that the driver dispatches
    /// `CopyBlock(idx)` against the caller-supplied `BaselineSource`.
    /// After Y-RSC.5 the signature phase also streams from the same
    /// baseline (no bulk `destination_data` slice), so this test pins
    /// reconstruction from the baseline bytes the fixture provides.
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
        let baseline_bytes: Vec<u8> = b"XXXXYYYY".to_vec();
        let mut baseline = MemoryBaseline::new(baseline_bytes);

        let (mut writer, captured) = MockAsyncWriter::new();
        let mut d = make_driver(transport);

        let mut sink = CollectingSink::default();
        d.drive_download_through_delta_streaming(
            RemoteCommandSpec::download("/remote/target.bin"),
            &mut baseline,
            &mut writer,
            &adapter,
            &mut sink,
        )
        .await
        .expect("streaming download succeeds");

        let on_writer = captured.lock().expect("captured lock").clone();
        assert_eq!(
            &on_writer[0..8],
            b"XXXXYYYY",
            "CopyBlock dispatch must consult BaselineSource"
        );
        assert_eq!(&on_writer[8..], raw_literal.as_slice());
    }

    /// W2.4 test 3: writer that returns `BrokenPipe` on the first
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
        let mut baseline = MemoryBaseline::new(b"BLK1BLK2".to_vec());

        let mut d = make_driver(transport);
        let mut writer = FailingMockWriter;

        let mut sink = CollectingSink::default();
        let err = d
            .drive_download_through_delta_streaming(
                RemoteCommandSpec::download("/remote/target.bin"),
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

    /// W2.4 test 4: after a successful streaming download,
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
        let mut baseline = MemoryBaseline::new(b"BLK1BLK2".to_vec());

        let (mut writer, _captured) = MockAsyncWriter::new();
        let mut d = make_driver(transport);

        let mut sink = CollectingSink::default();
        d.drive_download_through_delta_streaming(
            RemoteCommandSpec::download("/remote/target.bin"),
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

    // ---- Live comparative benchmark vs native rsync (manual, #[ignore]) --
    //
    // Mirrors the native-rsync baseline runs documented in the 2026-07-21
    // AeroRsync audit (docs/dev/roadmap/APPENDIX-C-Y-D/APPENDIX-Y/reports/).
    // Requires the lane-3 Docker harness on 127.0.0.1:2224 and the bench
    // dataset under /tmp/bench (generation steps in the same report).
    // Run with:
    //   cargo test --features aerorsync --lib aerorsync_bench -- --ignored --nocapture --test-threads=1
    //
    // `--test-threads=1` is not optional for the timings: run concurrently,
    // the three scenarios contend for one container and A1 reads 9.49 s
    // instead of 1.008 s. The remote state is reset per scenario, so
    // correctness no longer depends on it, but the numbers still do.
    // Dataset generation: scripts/aerorsync-compare/make-dataset.py (seeded).
    // Skip-graceful when the harness or the dataset is absent.

    /// Wipe and recreate a remote directory over the benchmark SSH endpoint.
    ///
    /// Call ONCE at the top of each scenario, never per driver: the delta
    /// scenarios seed their own baseline and a reset between them would erase
    /// exactly what they are measuring against.
    ///
    /// Without this the suite silently lies. The remote files survive the run,
    /// so from the SECOND execution onward the "cold upload" scenario finds its
    /// target already there and measures a no-op instead: 49 bytes on the wire
    /// in ~480 ms, against 52 MB in ~1 s for a real cold upload. The label does
    /// not change, so a periodic regression suite reads a spectacular
    /// improvement where the test in fact stopped doing anything. Observed
    /// 2026-07-25 over three consecutive runs. It also creates the directory,
    /// which the scenarios used to assume: without it they fail with an opaque
    /// `remote rsync exited with code 3`.
    async fn bench_reset_remote_dir(dir: &str) -> bool {
        use crate::aerorsync::transport::{RemoteExecRequest, RemoteShellTransport};
        let Some(cfg) = bench_ssh_config() else {
            return false;
        };
        let t = crate::aerorsync::ssh_transport::SshRemoteShellTransport::new(cfg);
        let req = RemoteExecRequest {
            program: "sh".into(),
            args: vec!["-c".into(), format!("rm -rf '{dir}' && mkdir -p '{dir}'")],
            environment: Vec::new(),
        };
        match t.exec(req).await {
            Ok(out) if out.exit_code == 0 => true,
            Ok(out) => {
                eprintln!(
                    "[bench] remote reset of {dir} exited {}: {}",
                    out.exit_code,
                    String::from_utf8_lossy(&out.stderr).trim()
                );
                false
            }
            Err(e) => {
                eprintln!("[bench] remote reset of {dir} failed: {e}: skipping");
                false
            }
        }
    }

    /// The lane-3 SSH config, or `None` when the harness or the key is absent.
    fn bench_ssh_config() -> Option<crate::aerorsync::ssh_transport::SshTransportConfig> {
        use crate::aerorsync::ssh_transport::{SshHostKeyPolicy, SshTransportConfig};
        use crate::aerorsync::transport::RemoteExecRequest;

        let key_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/aerorsync/capture/keys/id_ed25519");
        if !key_path.exists() {
            eprintln!("[bench] ssh key not found: skipping");
            return None;
        }
        Some(SshTransportConfig {
            host: "127.0.0.1".into(),
            port: 2224,
            username: "testuser".into(),
            private_key_path: key_path,
            connect_timeout_ms: 10_000,
            io_timeout_ms: 120_000,
            worker_idle_poll_ms: 250,
            max_frame_size: 1 << 20,
            host_key_policy: SshHostKeyPolicy::AcceptAny,
            probe_request: RemoteExecRequest {
                program: "rsync".into(),
                args: vec!["--version".into()],
                environment: Vec::new(),
            },
            auth_password: None,
            auth_agent: false,
        })
    }

    /// Shared lane-3 transport config for the benchmark tests.
    ///
    /// Timings want `--test-threads=1`: three concurrent scenarios against one
    /// container measured A1 at 9.49 s versus 1.008 s serialized, which is
    /// contention rather than signal.
    async fn bench_lane3_driver() -> Option<(
        AerorsyncDriver<crate::aerorsync::ssh_transport::SshRemoteShellTransport>,
        CollectingSink,
    )> {
        use crate::aerorsync::ssh_transport::{
            SshHostKeyPolicy, SshRemoteShellTransport, SshTransportConfig,
        };
        use crate::aerorsync::transport::RemoteExecRequest;

        if tokio::net::TcpStream::connect("127.0.0.1:2224")
            .await
            .is_err()
        {
            eprintln!("[bench] lane 3 harness not reachable on 127.0.0.1:2224: skipping");
            return None;
        }
        let key_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/aerorsync/capture/keys/id_ed25519");
        if !key_path.exists() {
            eprintln!("[bench] ssh key not found: skipping");
            return None;
        }
        let ssh_config = SshTransportConfig {
            host: "127.0.0.1".into(),
            port: 2224,
            username: "testuser".into(),
            private_key_path: key_path,
            connect_timeout_ms: 10_000,
            io_timeout_ms: 120_000,
            worker_idle_poll_ms: 250,
            max_frame_size: 1 << 20,
            host_key_policy: SshHostKeyPolicy::AcceptAny,
            probe_request: RemoteExecRequest {
                program: "rsync".into(),
                args: vec!["--version".into()],
                environment: Vec::new(),
            },
            auth_password: None,
            auth_agent: false,
        };
        let transport = SshRemoteShellTransport::new(ssh_config);
        let driver = AerorsyncDriver::new(transport, CancelHandle::inert());
        Some((driver, CollectingSink::default()))
    }

    fn bench_read_local(path: &str) -> Option<Vec<u8>> {
        match std::fs::read(path) {
            Ok(d) => Some(d),
            Err(e) => {
                eprintln!("[bench] dataset missing at {path}: {e}: skipping");
                None
            }
        }
    }

    /// One cold upload + one 5%-changed delta upload + one redundant
    /// upload, timing each and reporting wire bytes from the driver
    /// counters. Native-rsync twin numbers are in the audit report.
    #[ignore = "manual live benchmark vs native rsync"]
    #[tokio::test]
    async fn aerorsync_bench_upload_vs_native() {
        use crate::aerorsync::engine_adapter::CurrentDeltaSyncBridge;

        if !bench_reset_remote_dir("/workspace/bench-aero/upload").await {
            return;
        }
        let Some((mut driver, mut sink)) = bench_lane3_driver().await else {
            return;
        };
        let Some(rand50) = bench_read_local("/tmp/bench/local/R_rand.bin") else {
            return;
        };
        let Some(base50) = bench_read_local("/tmp/bench/local/A_base.bin") else {
            return;
        };
        let Some(mod50) = bench_read_local("/tmp/bench/local/A_mod.bin") else {
            return;
        };
        let adapter = CurrentDeltaSyncBridge::new();

        // A1: cold upload, incompressible 50 MiB.
        let remote = "/workspace/bench-aero/upload/R.bin";
        let entry = live_file_list_entry("R.bin");
        let entry = FileListEntry {
            size: rand50.len() as i64,
            ..entry
        };
        let t0 = std::time::Instant::now();
        driver
            .drive_upload_through_delta(
                RemoteCommandSpec::upload(remote),
                entry,
                &rand50,
                &adapter,
                &mut sink,
            )
            .await
            .expect("A1 cold upload");
        driver.finish_session(&mut sink).await.expect("A1 finish");
        eprintln!(
            "[bench] A1 cold-upload-50MB-random: {:?}, wire-sent={} wire-recv={}",
            t0.elapsed(),
            driver.sent_data_bytes(),
            driver.received_raw_bytes()
        );

        // A2: delta upload, 5% scattered changes vs remote baseline.
        let (mut driver, mut sink) = bench_lane3_driver().await.unwrap();
        let remote = "/workspace/bench-aero/upload/A.bin";
        // Seed the baseline with a cold upload first (not timed).
        let entry = live_file_list_entry("A.bin");
        let entry = FileListEntry {
            size: base50.len() as i64,
            ..entry
        };
        driver
            .drive_upload_through_delta(
                RemoteCommandSpec::upload(remote),
                entry,
                &base50,
                &adapter,
                &mut sink,
            )
            .await
            .expect("A2 baseline seed");
        driver
            .finish_session(&mut sink)
            .await
            .expect("A2 seed finish");

        let (mut driver, mut sink) = bench_lane3_driver().await.unwrap();
        let entry = live_file_list_entry("A.bin");
        let entry = FileListEntry {
            size: mod50.len() as i64,
            ..entry
        };
        let t0 = std::time::Instant::now();
        driver
            .drive_upload_through_delta(
                RemoteCommandSpec::upload(remote),
                entry,
                &mod50,
                &adapter,
                &mut sink,
            )
            .await
            .expect("A2 delta upload");
        driver.finish_session(&mut sink).await.expect("A2 finish");
        eprintln!(
            "[bench] A2 delta-upload-50MB-5pct: {:?}, wire-sent={} wire-recv={}",
            t0.elapsed(),
            driver.sent_data_bytes(),
            driver.received_raw_bytes()
        );

        // A5: redundant upload (remote already holds A_mod).
        let (mut driver, mut sink) = bench_lane3_driver().await.unwrap();
        let entry = live_file_list_entry("A.bin");
        let entry = FileListEntry {
            size: mod50.len() as i64,
            ..entry
        };
        let t0 = std::time::Instant::now();
        driver
            .drive_upload_through_delta(
                RemoteCommandSpec::upload(remote),
                entry,
                &mod50,
                &adapter,
                &mut sink,
            )
            .await
            .expect("A5 redundant upload");
        driver.finish_session(&mut sink).await.expect("A5 finish");
        eprintln!(
            "[bench] A5 redundant-upload: {:?}, wire-sent={} wire-recv={}",
            t0.elapsed(),
            driver.sent_data_bytes(),
            driver.received_raw_bytes()
        );
    }

    /// Delta download: remote holds A_base, local baseline is A_mod
    /// (5% scattered changes). Verifies the reconstructed content matches
    /// the remote byte-for-byte (xxh128 whole-file trailer decoded by the
    /// driver) and reports wall time + wire bytes.
    #[ignore = "manual live benchmark vs native rsync"]
    #[tokio::test]
    async fn aerorsync_bench_download_vs_native() {
        if !bench_reset_remote_dir("/workspace/bench-aero/download").await {
            return;
        }
        use crate::aerorsync::engine_adapter::CurrentDeltaSyncBridge;

        let Some(base50) = bench_read_local("/tmp/bench/local/A_base.bin") else {
            return;
        };
        let Some(mod50) = bench_read_local("/tmp/bench/local/A_mod.bin") else {
            return;
        };
        let adapter = CurrentDeltaSyncBridge::new();

        // Seed remote B.bin with A_base (cold upload, not timed).
        let (mut driver, mut sink) = {
            let Some(x) = bench_lane3_driver().await else {
                return;
            };
            x
        };
        let remote = "/workspace/bench-aero/download/B.bin";
        let entry = live_file_list_entry("B.bin");
        let entry = FileListEntry {
            size: base50.len() as i64,
            ..entry
        };
        driver
            .drive_upload_through_delta(
                RemoteCommandSpec::upload(remote),
                entry,
                &base50,
                &adapter,
                &mut sink,
            )
            .await
            .expect("seed B.bin");
        driver.finish_session(&mut sink).await.expect("seed finish");

        // A3: delta download against the A_mod baseline.
        let (mut driver, mut sink) = bench_lane3_driver().await.unwrap();
        let t0 = std::time::Instant::now();
        driver
            .drive_download_through_delta(
                RemoteCommandSpec::download(remote),
                &mod50,
                &adapter,
                &mut sink,
            )
            .await
            .expect("A3 delta download");
        driver.finish_session(&mut sink).await.expect("A3 finish");
        let reconstructed = driver
            .reconstructed()
            .expect("bulk download reconstructs in memory")
            .to_vec();
        eprintln!(
            "[bench] A3 delta-download-50MB-5pct: {:?}, wire-sent={} wire-recv={}",
            t0.elapsed(),
            driver.sent_data_bytes(),
            driver.received_raw_bytes()
        );
        assert_eq!(
            reconstructed.len(),
            base50.len(),
            "reconstructed size mismatch"
        );
        assert_eq!(
            reconstructed, base50,
            "reconstructed content must match the remote byte-for-byte"
        );
    }

    /// Small-file batch: 20 x 256 KiB cold uploads, one SSH session per
    /// file (the per-file driver API surface). Native rsync does the same
    /// tree in ONE recursive invocation; the gap is exactly what
    /// `AerorsyncBatch` (session reuse) exists to close at the transport
    /// layer. Reports total wall time.
    #[ignore = "manual live benchmark vs native rsync"]
    #[tokio::test]
    async fn aerorsync_bench_small_batch_vs_native() {
        if !bench_reset_remote_dir("/workspace/bench-aero/small").await {
            return;
        }
        use crate::aerorsync::engine_adapter::CurrentDeltaSyncBridge;

        let adapter = CurrentDeltaSyncBridge::new();
        let t0 = std::time::Instant::now();
        let mut done = 0usize;
        for i in 0..20 {
            let path = format!("/tmp/bench/local/small/f{i:02}.bin");
            let Some(data) = bench_read_local(&path) else {
                return;
            };
            let name = format!("f{i:02}.bin");
            let Some((mut driver, mut sink)) = bench_lane3_driver().await else {
                return;
            };
            let entry = live_file_list_entry(&name);
            let entry = FileListEntry {
                size: data.len() as i64,
                ..entry
            };
            driver
                .drive_upload_through_delta(
                    RemoteCommandSpec::upload(format!("/workspace/bench-aero/small/{name}")),
                    entry,
                    &data,
                    &adapter,
                    &mut sink,
                )
                .await
                .expect("batch upload");
            driver
                .finish_session(&mut sink)
                .await
                .expect("batch finish");
            done += 1;
        }
        eprintln!(
            "[bench] A4 small-batch-20x256KiB-per-file-session: {:?} for {done} files",
            t0.elapsed()
        );
        assert_eq!(done, 20);
    }
}
