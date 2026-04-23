//! Real rsync wire format (protocol 31/32) — read-only layer.
//!
//! Sinergie 8b + 8c + 8d.
//!
//! Layer one is the protocol version handshake. 4-byte little-endian u32
//! at the very start of both directions of the SSH exec channel.
//!
//! Layer two is the capability negotiation preamble. On the server side
//! the wire shape is `version(4)` + `compat_flags(rsync varint)` +
//! `len_u8 + checksum_algos_ascii` + `len_u8 + compression_algos_ascii` +
//! `checksum_seed(4, LE u32)`, typically 71 bytes for the profile
//! `"xxh128 xxh3 xxh64 md5 md4 sha1 none"` over `"zstd lz4 zlibx zlib
//! none"` with all CF_* negotiated. On the client side the wire shape is
//! `version(4)` + `len_u8 + checksum_algos_ascii` + `len_u8 +
//! compression_algos_ascii` with no `compat_flags` and no seed, typically
//! 55 bytes for the matching profile. Early S8b drafts treated
//! `compat_flags` as a fixed 2-byte opaque block; `compat.c::read_varint`
//! in rsync 3.2.7 shows it is a proper rsync varint whose length depends
//! on which bits are set — fixed in S8d.
//!
//! Layer three is multiplex framing activated after the preamble on
//! **both** channels in remote-shell mode with protocol 31/32. Each
//! frame is a 4-byte LE u32 header; the high byte (obtained via
//! `>> 24`) equals `MPLEX_BASE + tag_code`, the low 24 bits are the
//! payload length, and `length` bytes of payload follow. `MPLEX_BASE`
//! is 7 so the first 7 enum slots used by the unmultiplexed prefix
//! cannot collide with a multiplex header.
//!
//! Early drafts of S8b assumed the client-to-server channel was raw;
//! the S8a byte oracle proved otherwise (see
//! `real_wire_client_to_server_upload_is_multiplexed_like_server_side`).
//! Both directions go through `reassemble_msg_data` symmetrically.
//!
//! Layer four (S8d) is the rsync varint / varlong primitive, used
//! everywhere in the application-level protocol above `MSG_DATA`. Layer
//! five (S8d) is the file-list entry decoder that sits on top of the
//! reassembled app stream — it is the first byte-level consumer of the
//! rsync app protocol. Every subsequent feature (signature blocks,
//! delta instructions, summary frame) piggybacks on these primitives.
//!
//! Both decoder and encoder paths are wired: the module now powers the
//! production native rsync transport (proto 31, S8j closed) via the
//! `delta_transport_impl` driver. All functions stay `pub` because the
//! driver and the live-wire test lane both reach into this primitive
//! layer directly.

#![allow(dead_code)]

use std::fmt;

/// Value added to the tag to produce the high byte of a multiplex header.
/// Matches `MPLEX_BASE` in rsync's `io.h`.
pub const MPLEX_BASE: u8 = 7;

/// Fixed size of the protocol version prefix on both channels.
pub const PROTOCOL_VERSION_LEN: usize = 4;

/// Fixed size of the server's `checksum_seed` (LE u32) right before the
/// channel switches into multiplex mode.
pub const CHECKSUM_SEED_LEN: usize = 4;

/// Fixed size of a multiplex frame header.
pub const MUX_HEADER_LEN: usize = 4;

/// Lookup table mirroring `int_byte_extra` in rsync 3.2.7 `io.c:119-125`.
/// Indexed by `first_byte / 4`, gives the number of *additional* bytes
/// that follow the first byte of a varint / varlong.
///
/// The table encodes the leading-1-bits convention: for first bytes with
/// MSB clear (`0x00..=0x7F`) the value is 0 (single-byte varint). From
/// `0x80..=0xBF` one extra byte (two-byte encoding). `0xC0..=0xDF` two
/// extras (three-byte encoding). Then, respectively, 3, 4, 5, 6.
///
/// Copy-pasted from the rsync source because a derived expression would
/// hide the exact breakpoints at `0xE0/0xF0/0xF8/0xFC` where the leading
/// 1-bit run grows from 3 to 6.
pub const INT_BYTE_EXTRA: [u8; 64] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // (00-3F)/4
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // (40-7F)/4
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, // (80-BF)/4
    2, 2, 2, 2, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 6, // (C0-FF)/4
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RealWireError {
    TruncatedBuffer {
        at: &'static str,
        needed: usize,
        available: usize,
    },
    InvalidProtocolVersion {
        value: u32,
    },
    InvalidAlgoListLen {
        declared: usize,
        available: usize,
    },
    InvalidMuxHeader {
        raw_high_byte: u8,
    },
    NonAsciiAlgoList {
        offset: usize,
        byte: u8,
    },
    /// The first byte of a varint / varlong claims more extra bytes
    /// than an int32 / int64 can hold. Matches rsync's `Overflow in
    /// read_varint()` exit path.
    VarintOverflow {
        first_byte: u8,
    },
    /// The declared size of a length-prefixed name (e.g. uid_name, file
    /// path without `XMIT_LONG_NAME`) exceeds the remaining buffer.
    InvalidNameLen {
        declared: usize,
        available: usize,
    },
    /// A non-UTF8 byte sequence was decoded where rsync guarantees ASCII
    /// (uid_name, gid_name). We surface the first offending offset.
    NonUtf8Name {
        offset: usize,
    },
    /// The decoder was asked to parse a file-list entry that relies on
    /// a previous name (via `XMIT_SAME_NAME` with `l1 > 0`) but no
    /// previous entry was provided by the caller.
    SameNameWithoutPrevious,
    /// `XMIT_SAME_NAME`'s `l1` prefix length exceeds the length of the
    /// previously-decoded name.
    SameNamePrefixTooLong {
        l1: usize,
        previous_len: usize,
    },
    /// `decode_ndx` ran out of bytes mid-encoding. `form` identifies
    /// which branch of `read_ndx` (single / three / five / negative).
    NdxTruncated {
        form: &'static str,
    },
    /// A `sum_head` field read cleanly as int32 LE but the value is
    /// outside rsync's own validation range (see `io.c::read_sum_head`).
    /// Surfaced instead of silently accepting bogus sums.
    SumHeadFieldOutOfRange {
        field: &'static str,
        value: i32,
        max: i32,
    },
    /// A delta-token record ran out of bytes mid-decoding. `at` identifies
    /// which sub-record (header / run_count / deflated_len / payload) and
    /// the byte counts surface the exact shortfall.
    DeltaTokenTruncated {
        at: &'static str,
        needed: usize,
        available: usize,
    },
    /// A COPY / COPYRUN token referenced a block index outside the
    /// `0..sum_head.count` range negotiated earlier. Matches rsync's own
    /// `token.c` behaviour of aborting on bogus block references.
    DeltaTokenOutOfRange {
        token_index: i32,
        block_count: i32,
    },
    /// A `DeltaOp::Literal`'s compressed payload could not be decoded
    /// by the zstd library. `reason` carries the underlying error
    /// message — malformed frame, truncated stream, unsupported
    /// dictionary, etc.
    ZstdDecompressionFailed {
        reason: String,
    },
}

impl fmt::Display for RealWireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RealWireError::TruncatedBuffer {
                at,
                needed,
                available,
            } => {
                write!(
                    f,
                    "truncated buffer at {at}: need {needed}, have {available}"
                )
            }
            RealWireError::InvalidProtocolVersion { value } => {
                write!(f, "invalid rsync protocol version: {value}")
            }
            RealWireError::InvalidAlgoListLen {
                declared,
                available,
            } => {
                write!(
                    f,
                    "algo-list length {declared} exceeds remaining buffer {available}"
                )
            }
            RealWireError::InvalidMuxHeader { raw_high_byte } => {
                write!(
                    f,
                    "multiplex header high byte {raw_high_byte:#04x} is below MPLEX_BASE={}",
                    MPLEX_BASE
                )
            }
            RealWireError::NonAsciiAlgoList { offset, byte } => {
                write!(
                    f,
                    "algo-list byte {byte:#04x} at offset {offset} is not printable ASCII"
                )
            }
            RealWireError::VarintOverflow { first_byte } => {
                write!(
                    f,
                    "varint overflow: first byte {first_byte:#04x} claims more than 4 extra bytes"
                )
            }
            RealWireError::InvalidNameLen {
                declared,
                available,
            } => {
                write!(
                    f,
                    "name length {declared} exceeds remaining buffer {available}"
                )
            }
            RealWireError::NonUtf8Name { offset } => {
                write!(f, "non-UTF8 byte at name offset {offset}")
            }
            RealWireError::SameNameWithoutPrevious => {
                write!(
                    f,
                    "XMIT_SAME_NAME with l1 > 0 requires a previous file-list entry"
                )
            }
            RealWireError::SameNamePrefixTooLong { l1, previous_len } => {
                write!(
                    f,
                    "XMIT_SAME_NAME prefix length {l1} exceeds previous name length {previous_len}"
                )
            }
            RealWireError::NdxTruncated { form } => {
                write!(f, "read_ndx: truncated buffer in {form} form")
            }
            RealWireError::SumHeadFieldOutOfRange { field, value, max } => {
                write!(
                    f,
                    "sum_head field `{field}` value {value} out of range (max {max})"
                )
            }
            RealWireError::DeltaTokenTruncated {
                at,
                needed,
                available,
            } => {
                write!(
                    f,
                    "delta token truncated at {at}: need {needed}, have {available}"
                )
            }
            RealWireError::DeltaTokenOutOfRange {
                token_index,
                block_count,
            } => {
                write!(
                    f,
                    "delta token {token_index} out of range for sum_head.count={block_count}"
                )
            }
            RealWireError::ZstdDecompressionFailed { reason } => {
                write!(f, "zstd decompression failed: {reason}")
            }
        }
    }
}

impl std::error::Error for RealWireError {}

/// Decoded view of the server-side preamble (bytes before multiplex kicks in).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerPreamble {
    pub protocol_version: u32,
    /// rsync `compat_flags`, the varint written by `compat.c` right
    /// after the protocol version on the server side. Individual bits
    /// are `CF_*` constants (`CF_VARINT_FLIST_FLAGS`, `CF_INC_RECURSE`,
    /// …). We return the raw i32 so callers can test bits without a
    /// duplicated `CompatFlags` enum.
    pub compat_flags: i32,
    pub checksum_algos: String,
    pub compression_algos: String,
    pub checksum_seed: u32,
    /// Exact number of bytes consumed by this preamble. Callers use this
    /// to position the multiplex cursor.
    pub consumed: usize,
}

/// Decoded view of the client-side preamble.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientPreamble {
    pub protocol_version: u32,
    pub checksum_algos: String,
    pub compression_algos: String,
    pub consumed: usize,
}

/// Recognised rsync multiplex message tags, keyed by the raw code
/// `high_byte - MPLEX_BASE`. Tag values track the canonical rsync
/// constants from `io.h` of rsync 3.2.7; unknown codes are surfaced via
/// `MuxTag::Unknown(raw)` rather than being dropped, so a future protocol
/// bump doesn't silently corrupt the demux.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuxTag {
    Data,
    ErrorXfer,
    Info,
    Error,
    Warning,
    ErrorSocket,
    Log,
    Client,
    ErrorUtf8,
    Redo,
    Stats,
    IoError,
    IoTimeout,
    Noop,
    ErrorExit,
    Success,
    Deleted,
    NoSend,
    Unknown(u8),
}

impl MuxTag {
    pub fn from_code(code: u8) -> Self {
        match code {
            0 => MuxTag::Data,
            1 => MuxTag::ErrorXfer,
            2 => MuxTag::Info,
            3 => MuxTag::Error,
            4 => MuxTag::Warning,
            5 => MuxTag::ErrorSocket,
            6 => MuxTag::Log,
            7 => MuxTag::Client,
            8 => MuxTag::ErrorUtf8,
            9 => MuxTag::Redo,
            10 => MuxTag::Stats,
            22 => MuxTag::IoError,
            33 => MuxTag::IoTimeout,
            42 => MuxTag::Noop,
            86 => MuxTag::ErrorExit,
            100 => MuxTag::Success,
            101 => MuxTag::Deleted,
            102 => MuxTag::NoSend,
            other => MuxTag::Unknown(other),
        }
    }

    pub fn code(&self) -> u8 {
        match *self {
            MuxTag::Data => 0,
            MuxTag::ErrorXfer => 1,
            MuxTag::Info => 2,
            MuxTag::Error => 3,
            MuxTag::Warning => 4,
            MuxTag::ErrorSocket => 5,
            MuxTag::Log => 6,
            MuxTag::Client => 7,
            MuxTag::ErrorUtf8 => 8,
            MuxTag::Redo => 9,
            MuxTag::Stats => 10,
            MuxTag::IoError => 22,
            MuxTag::IoTimeout => 33,
            MuxTag::Noop => 42,
            MuxTag::ErrorExit => 86,
            MuxTag::Success => 100,
            MuxTag::Deleted => 101,
            MuxTag::NoSend => 102,
            MuxTag::Unknown(raw) => raw,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MuxHeader {
    pub tag: MuxTag,
    pub length: u32,
}

impl MuxHeader {
    /// Encode the header into its 4-byte LE wire form.
    pub fn encode(&self) -> [u8; MUX_HEADER_LEN] {
        let combined: u32 =
            ((MPLEX_BASE as u32 + self.tag.code() as u32) << 24) | (self.length & 0x00FF_FFFF);
        combined.to_le_bytes()
    }

    /// Decode a 4-byte LE header. Returns `InvalidMuxHeader` if the high
    /// byte is below `MPLEX_BASE` — an early sign that the demuxer was
    /// started before the capability preamble finished.
    pub fn decode(bytes: [u8; MUX_HEADER_LEN]) -> Result<Self, RealWireError> {
        let raw = u32::from_le_bytes(bytes);
        let raw_high = (raw >> 24) as u8;
        if raw_high < MPLEX_BASE {
            return Err(RealWireError::InvalidMuxHeader {
                raw_high_byte: raw_high,
            });
        }
        let code = raw_high - MPLEX_BASE;
        let length = raw & 0x00FF_FFFF;
        Ok(MuxHeader {
            tag: MuxTag::from_code(code),
            length,
        })
    }
}

/// Decode the 4-byte protocol version prefix. Accepts 30..=40 — this is
/// intentionally permissive, covering the current 31/32 range plus a
/// small forward-compat envelope. Anything else is flagged, because an
/// unexpected protocol version at offset 0 usually means we are reading
/// the wrong channel.
pub fn decode_protocol_version(buf: &[u8]) -> Result<(u32, usize), RealWireError> {
    if buf.len() < PROTOCOL_VERSION_LEN {
        return Err(RealWireError::TruncatedBuffer {
            at: "protocol_version",
            needed: PROTOCOL_VERSION_LEN,
            available: buf.len(),
        });
    }
    let mut arr = [0u8; PROTOCOL_VERSION_LEN];
    arr.copy_from_slice(&buf[..PROTOCOL_VERSION_LEN]);
    let version = u32::from_le_bytes(arr);
    if !(30..=40).contains(&version) {
        return Err(RealWireError::InvalidProtocolVersion { value: version });
    }
    Ok((version, PROTOCOL_VERSION_LEN))
}

/// Encode the 4-byte LE protocol version.
pub fn encode_protocol_version(version: u32) -> [u8; PROTOCOL_VERSION_LEN] {
    version.to_le_bytes()
}

fn read_u8_len_prefixed_ascii(
    buf: &[u8],
    offset: usize,
    section: &'static str,
) -> Result<(String, usize), RealWireError> {
    if offset >= buf.len() {
        return Err(RealWireError::TruncatedBuffer {
            at: section,
            needed: 1,
            available: buf.len().saturating_sub(offset),
        });
    }
    let len = buf[offset] as usize;
    let start = offset + 1;
    let end = start + len;
    if end > buf.len() {
        return Err(RealWireError::InvalidAlgoListLen {
            declared: len,
            available: buf.len().saturating_sub(start),
        });
    }
    let mut out = String::with_capacity(len);
    for (i, &b) in buf[start..end].iter().enumerate() {
        // rsync uses plain ASCII for algorithm names. Reject anything
        // outside 0x20..=0x7E — a non-ASCII byte at this offset is
        // almost always a framing mistake, not a real algo name.
        if !(0x20..=0x7E).contains(&b) {
            return Err(RealWireError::NonAsciiAlgoList {
                offset: start + i,
                byte: b,
            });
        }
        out.push(b as char);
    }
    Ok((out, end - offset))
}

/// Parse the server-side preamble from the start of `buf`. Returns a
/// `ServerPreamble` whose `consumed` field gives the byte cursor at which
/// the multiplex stream begins.
pub fn decode_server_preamble(buf: &[u8]) -> Result<ServerPreamble, RealWireError> {
    let (version, mut cursor) = decode_protocol_version(buf)?;

    // compat_flags is a rsync varint written by `compat.c`. Width depends
    // on which CF_* bits are set — values up to 0x7F take one byte,
    // larger ones take two or more.
    let (compat_raw, consumed_cf) = decode_varint(&buf[cursor..])?;
    let compat_flags = compat_raw as i32;
    cursor += consumed_cf;

    let (checksum_algos, consumed_ck) =
        read_u8_len_prefixed_ascii(buf, cursor, "server_checksum_algos")?;
    cursor += consumed_ck;

    let (compression_algos, consumed_cmp) =
        read_u8_len_prefixed_ascii(buf, cursor, "server_compression_algos")?;
    cursor += consumed_cmp;

    if buf.len() < cursor + CHECKSUM_SEED_LEN {
        return Err(RealWireError::TruncatedBuffer {
            at: "checksum_seed",
            needed: CHECKSUM_SEED_LEN,
            available: buf.len().saturating_sub(cursor),
        });
    }
    let mut seed_bytes = [0u8; CHECKSUM_SEED_LEN];
    seed_bytes.copy_from_slice(&buf[cursor..cursor + CHECKSUM_SEED_LEN]);
    let checksum_seed = u32::from_le_bytes(seed_bytes);
    cursor += CHECKSUM_SEED_LEN;

    Ok(ServerPreamble {
        protocol_version: version,
        compat_flags,
        checksum_algos,
        compression_algos,
        checksum_seed,
        consumed: cursor,
    })
}

/// Parse the client-side preamble. No compat_flags, no seed.
pub fn decode_client_preamble(buf: &[u8]) -> Result<ClientPreamble, RealWireError> {
    let (version, mut cursor) = decode_protocol_version(buf)?;

    let (checksum_algos, consumed_ck) =
        read_u8_len_prefixed_ascii(buf, cursor, "client_checksum_algos")?;
    cursor += consumed_ck;

    let (compression_algos, consumed_cmp) =
        read_u8_len_prefixed_ascii(buf, cursor, "client_compression_algos")?;
    cursor += consumed_cmp;

    Ok(ClientPreamble {
        protocol_version: version,
        checksum_algos,
        compression_algos,
        consumed: cursor,
    })
}

// ----------------------------------------------------------------------------
// Sinergia 8i-encode — preamble + file checksum + sum block writers.
//
// Symmetric encoders for the read paths above. Each encoder is the
// byte-identical inverse of the matching decoder, validated by
// round-trip unit tests AND by re-encoding the frozen oracle
// preambles + sum blocks.
// ----------------------------------------------------------------------------

/// Encode `len` (must fit in u8) as a 1-byte length prefix followed by
/// the ASCII bytes of `algos`. Mirrors `read_u8_len_prefixed_ascii`.
/// Panics on non-ASCII or len > 255 — both are programming errors here.
fn write_u8_len_prefixed_ascii(out: &mut Vec<u8>, algos: &str, section: &'static str) {
    let bytes = algos.as_bytes();
    assert!(
        bytes.len() <= u8::MAX as usize,
        "{section}: algo list length {} exceeds u8 max",
        bytes.len()
    );
    for (i, b) in bytes.iter().enumerate() {
        assert!(
            (0x20..=0x7E).contains(b),
            "{section}: byte {b:#04x} at offset {i} is not printable ASCII"
        );
    }
    out.push(bytes.len() as u8);
    out.extend_from_slice(bytes);
}

/// Encode the server-side preamble. Output is byte-identical to what
/// rsync 3.2.7 writes in `compat.c::setup_protocol`.
///
/// `consumed` on the input is ignored (the encoder produces fresh
/// bytes; the round-trip path sets `consumed` from the decoder).
pub fn encode_server_preamble(preamble: &ServerPreamble) -> Vec<u8> {
    let mut out = Vec::with_capacity(preamble.consumed.max(64));
    out.extend_from_slice(&encode_protocol_version(preamble.protocol_version));
    out.extend_from_slice(&encode_varint(preamble.compat_flags));
    write_u8_len_prefixed_ascii(&mut out, &preamble.checksum_algos, "server_checksum_algos");
    write_u8_len_prefixed_ascii(
        &mut out,
        &preamble.compression_algos,
        "server_compression_algos",
    );
    out.extend_from_slice(&preamble.checksum_seed.to_le_bytes());
    out
}

/// Encode the client-side preamble (no `compat_flags`, no seed).
pub fn encode_client_preamble(preamble: &ClientPreamble) -> Vec<u8> {
    let mut out = Vec::with_capacity(preamble.consumed.max(48));
    out.extend_from_slice(&encode_protocol_version(preamble.protocol_version));
    write_u8_len_prefixed_ascii(&mut out, &preamble.checksum_algos, "client_checksum_algos");
    write_u8_len_prefixed_ascii(
        &mut out,
        &preamble.compression_algos,
        "client_compression_algos",
    );
    out
}

/// Iterator-style reader over a fully-buffered post-preamble mux stream.
/// Yields one `(MuxHeader, &[u8])` per frame. Stops at end-of-buffer;
/// returns `Err(...)` if the buffer ends mid-frame.
pub struct MuxDemuxer<'a> {
    buf: &'a [u8],
    pos: usize,
    exhausted: bool,
}

impl<'a> MuxDemuxer<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self {
            buf,
            pos: 0,
            exhausted: false,
        }
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }
}

impl<'a> Iterator for MuxDemuxer<'a> {
    type Item = Result<(MuxHeader, &'a [u8]), RealWireError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.exhausted {
            return None;
        }
        if self.pos == self.buf.len() {
            return None;
        }
        if self.buf.len() - self.pos < MUX_HEADER_LEN {
            self.exhausted = true;
            return Some(Err(RealWireError::TruncatedBuffer {
                at: "mux_header",
                needed: MUX_HEADER_LEN,
                available: self.buf.len() - self.pos,
            }));
        }
        let mut hdr_bytes = [0u8; MUX_HEADER_LEN];
        hdr_bytes.copy_from_slice(&self.buf[self.pos..self.pos + MUX_HEADER_LEN]);
        let header = match MuxHeader::decode(hdr_bytes) {
            Ok(h) => h,
            Err(e) => {
                self.exhausted = true;
                return Some(Err(e));
            }
        };
        let payload_start = self.pos + MUX_HEADER_LEN;
        let payload_end = payload_start + header.length as usize;
        if payload_end > self.buf.len() {
            self.exhausted = true;
            return Some(Err(RealWireError::TruncatedBuffer {
                at: "mux_payload",
                needed: header.length as usize,
                available: self.buf.len() - payload_start,
            }));
        }
        let payload = &self.buf[payload_start..payload_end];
        self.pos = payload_end;
        Some(Ok((header, payload)))
    }
}

/// Decoded slice of the multiplex stream: which tag carried it, and the
/// reassembled bytes. Used by `reassemble_msg_data` so callers can still
/// observe what out-of-band traffic was interleaved with the app stream
/// without having to re-decode the demuxer themselves.
///
/// `out_of_band` (kept for backward compatibility with all S8c-era
/// tests) records `(tag, length)` only — the OOB payload is dropped.
/// `oob_frames` was added in Sinergia 8h to retain the full payload
/// so the event classifier in `events::classify_oob_frame` can produce
/// typed `NativeRsyncEvent`s without re-walking the demuxer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReassemblyReport {
    /// Concatenated payload of every `MSG_DATA` frame in encounter order.
    pub app_stream: Vec<u8>,
    /// Tag + length of every non-`MSG_DATA` frame observed, in order.
    /// Zero entries when the stream is pure data. Kept for S8c parity.
    pub out_of_band: Vec<(MuxTag, u32)>,
    /// Tag + full payload of every non-`MSG_DATA` frame observed
    /// (Sinergia 8h). Same encounter order as `out_of_band` and always
    /// the same length. Feed pairs into `events::classify_oob_frame`.
    pub oob_frames: Vec<(MuxTag, Vec<u8>)>,
    /// Number of frames consumed overall (data + oob).
    pub frames_consumed: usize,
}

/// Walk every mux frame in `buf`, concatenate the `MSG_DATA` payloads
/// into one contiguous byte vector, and keep a log of out-of-band tags.
///
/// Any demux error (truncated header, truncated payload, invalid high
/// byte) aborts and is returned verbatim — partial reassembly is a
/// design mistake when the output is meant to feed a strict parser.
pub fn reassemble_msg_data(buf: &[u8]) -> Result<ReassemblyReport, RealWireError> {
    let mut app_stream = Vec::new();
    let mut out_of_band = Vec::new();
    let mut oob_frames = Vec::new();
    let mut frames_consumed = 0;
    for frame in MuxDemuxer::new(buf) {
        let (header, payload) = frame?;
        match header.tag {
            MuxTag::Data => app_stream.extend_from_slice(payload),
            other => {
                out_of_band.push((other, header.length));
                oob_frames.push((other, payload.to_vec()));
            }
        }
        frames_consumed += 1;
    }
    Ok(ReassemblyReport {
        app_stream,
        out_of_band,
        oob_frames,
        frames_consumed,
    })
}

// =============================================================================
// Sinergia 8h — Classified reassembly with OOB events.
//
// Two entry points sit on top of `reassemble_msg_data`:
//
// - `reassemble_with_events`     : full pass, classify every OOB frame.
// - `reassemble_until_terminal`  : same, but stop the moment a terminal
//                                  event (per `events::is_terminal`)
//                                  shows up — `app_stream` then ends at
//                                  the last byte BEFORE the terminal
//                                  header, never inside it.
//
// Both return `ClassifiedReassemblyReport`. A non-`None` `terminal`
// field means the stream bailed; the consumer (future S8i driver) maps
// it to a `NativeRsyncError` via `events::NativeRsyncEvent`.
//
// Design choice: the bail path returns `Ok` with `terminal: Some(...)`
// rather than synthesising a `RealWireError` variant. A remote-driven
// terminal event is structured stream state, not a parser error;
// conflating the two would force every consumer to pattern-match on
// two layers (`Result` + variant) where one suffices.
// =============================================================================

/// Result of `reassemble_with_events` / `reassemble_until_terminal`.
///
/// `terminal` is `Some(event)` only when `reassemble_until_terminal`
/// stops early on a terminal OOB frame (e.g. `MSG_ERROR`). In that
/// case `consumed_bytes` points to the byte immediately AFTER the
/// terminating frame's header + payload — so a caller that wants to
/// inspect or re-drive the trailing bytes can resume from
/// `&buf[report.consumed_bytes..]` if it ever becomes useful.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedReassemblyReport {
    pub app_stream: Vec<u8>,
    pub events: Vec<crate::rsync_native_proto::events::NativeRsyncEvent>,
    pub terminal: Option<crate::rsync_native_proto::events::NativeRsyncEvent>,
    pub consumed_bytes: usize,
    pub frames_consumed: usize,
}

/// Walk every mux frame, classify every OOB frame into a typed
/// `NativeRsyncEvent`, never bail. `terminal` is always `None`.
///
/// Equivalent in app_stream contents to `reassemble_msg_data` —
/// pinned by the `reassemble_with_events_app_stream_matches_legacy`
/// regression test.
pub fn reassemble_with_events(buf: &[u8]) -> Result<ClassifiedReassemblyReport, RealWireError> {
    use crate::rsync_native_proto::events::classify_oob_frame;

    let mut app_stream = Vec::new();
    let mut events = Vec::new();
    let mut frames_consumed = 0;
    let mut consumed_bytes = 0;
    for frame in MuxDemuxer::new(buf) {
        let (header, payload) = frame?;
        consumed_bytes += MUX_HEADER_LEN + payload.len();
        match header.tag {
            MuxTag::Data => app_stream.extend_from_slice(payload),
            other => events.push(classify_oob_frame(other, payload)),
        }
        frames_consumed += 1;
    }
    Ok(ClassifiedReassemblyReport {
        app_stream,
        events,
        terminal: None,
        consumed_bytes,
        frames_consumed,
    })
}

/// Same as `reassemble_with_events` but stops at the first terminal
/// OOB frame (per `NativeRsyncEvent::is_terminal`). The terminating
/// event is moved into `terminal`; subsequent bytes are NOT consumed.
///
/// **Stop semantics, hardening-pinned**: `app_stream` ends at the byte
/// before the terminating frame's header. No `MSG_DATA` payload that
/// arrives AFTER the terminal frame is ever appended. `consumed_bytes`
/// includes the terminating frame's header + payload but no further
/// bytes. See `reassemble_until_terminal_does_not_consume_data_after_error`.
pub fn reassemble_until_terminal(buf: &[u8]) -> Result<ClassifiedReassemblyReport, RealWireError> {
    use crate::rsync_native_proto::events::classify_oob_frame;

    let mut app_stream = Vec::new();
    let mut events = Vec::new();
    let mut terminal = None;
    let mut frames_consumed = 0;
    let mut consumed_bytes = 0;
    for frame in MuxDemuxer::new(buf) {
        let (header, payload) = frame?;
        consumed_bytes += MUX_HEADER_LEN + payload.len();
        frames_consumed += 1;
        match header.tag {
            MuxTag::Data => app_stream.extend_from_slice(payload),
            other => {
                let event = classify_oob_frame(other, payload);
                if event.is_terminal() {
                    terminal = Some(event);
                    break;
                }
                events.push(event);
            }
        }
    }
    Ok(ClassifiedReassemblyReport {
        app_stream,
        events,
        terminal,
        consumed_bytes,
        frames_consumed,
    })
}

// =============================================================================
// Sinergia 8i — Streaming mux reader + progress counter.
//
// `reassemble_*` above operate on a fully buffered post-preamble stream —
// fine for unit tests with frozen oracle slices, wrong shape for a live
// driver that reads chunks from an SSH channel and needs to emit progress
// ticks while bytes are in flight.
//
// `MuxStreamReader` is the live counterpart: the driver feeds it whatever
// chunks the transport yields (partial frames OK) and polls for one frame
// at a time. An internal `data_bytes_consumed` counter tracks only
// `MSG_DATA` payload bytes, which is the correct denominator for
// per-file progress (OOB traffic does not count against the transfer
// total). Terminal OOB frames lock the reader — subsequent polls return
// `None` so the driver cannot accidentally process app-stream bytes that
// arrived after the remote bailed.
//
// No allocations beyond the input buffer: every popped frame detaches the
// exact header+payload slice from the reader's `Vec<u8>` and hands
// ownership to the caller. Internal buffer grows only while awaiting a
// completed frame; once a frame is returned, its bytes are drained.
// =============================================================================

/// One atomic result of polling the streaming mux reader.
///
/// `Data` carries the payload of a single `MSG_DATA` frame. The caller
/// concatenates these into the app stream for the current protocol phase.
///
/// `Oob` is a classified non-terminal out-of-band event (warnings,
/// info, state markers). The driver forwards these to the `EventSink`.
///
/// `Terminal` is a classified terminal event. The driver MUST translate
/// it via `NativeRsyncError::from_oob_event` and abort the session. After
/// `Terminal` is returned once, the reader is locked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MuxPoll {
    Data(Vec<u8>),
    Oob(crate::rsync_native_proto::events::NativeRsyncEvent),
    Terminal(crate::rsync_native_proto::events::NativeRsyncEvent),
}

/// Streaming multiplex reader. Decouples SSH read cadence from
/// frame-level consumption. Built for S8i production wiring.
#[derive(Debug, Default)]
pub struct MuxStreamReader {
    buf: Vec<u8>,
    data_bytes_consumed: u64,
    terminal_seen: bool,
}

impl MuxStreamReader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a transport-level chunk. `feed` is cheap and O(chunk.len()).
    /// The chunk may contain 0, 1 or N frames and may end mid-frame.
    pub fn feed(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// Total `MSG_DATA` payload bytes drained so far. Monotone, excludes
    /// mux header bytes and OOB payloads. Use this as the progress
    /// numerator — the denominator comes from the FileListEntry size.
    pub fn data_bytes_consumed(&self) -> u64 {
        self.data_bytes_consumed
    }

    /// Bytes still held in the internal buffer awaiting frame completion.
    /// Exposed for diagnostics; a healthy stream flushes to 0 between
    /// frame boundaries.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// `true` after a terminal OOB event has been surfaced. Subsequent
    /// `poll_frame` calls return `None`.
    pub fn terminal_seen(&self) -> bool {
        self.terminal_seen
    }

    /// Attempt to pop one complete frame. Returns `None` when:
    ///
    /// - fewer than `MUX_HEADER_LEN` bytes are buffered, OR
    /// - the header is present but the payload is still in flight, OR
    /// - a terminal event has already been observed (reader is locked).
    ///
    /// Returns `Some(Err(...))` on a malformed header — this is a hard
    /// protocol error that must abort the session. The reader is NOT
    /// auto-locked in this case (the driver decides whether to retry or
    /// bail); callers that want the locked behaviour should flip
    /// `terminal_seen` themselves or stop polling.
    pub fn poll_frame(&mut self) -> Option<Result<MuxPoll, RealWireError>> {
        use crate::rsync_native_proto::events::classify_oob_frame;

        if self.terminal_seen {
            return None;
        }
        if self.buf.len() < MUX_HEADER_LEN {
            return None;
        }
        let mut hdr_bytes = [0u8; MUX_HEADER_LEN];
        hdr_bytes.copy_from_slice(&self.buf[..MUX_HEADER_LEN]);
        let header = match MuxHeader::decode(hdr_bytes) {
            Ok(h) => h,
            Err(e) => return Some(Err(e)),
        };
        let payload_len = header.length as usize;
        if self.buf.len() < MUX_HEADER_LEN + payload_len {
            return None;
        }
        // Drain the header.
        self.buf.drain(..MUX_HEADER_LEN);
        // Collect the payload. `drain().collect()` moves the bytes into a
        // fresh Vec owned by the caller; the reader's buffer shrinks by
        // the exact amount consumed.
        let payload: Vec<u8> = self.buf.drain(..payload_len).collect();
        match header.tag {
            MuxTag::Data => {
                self.data_bytes_consumed += payload.len() as u64;
                Some(Ok(MuxPoll::Data(payload)))
            }
            other => {
                let event = classify_oob_frame(other, &payload);
                if event.is_terminal() {
                    self.terminal_seen = true;
                    Some(Ok(MuxPoll::Terminal(event)))
                } else {
                    Some(Ok(MuxPoll::Oob(event)))
                }
            }
        }
    }
}

// =============================================================================
// Section 4 — rsync varint / varlong primitives (S8d)
//
// These are rsync-specific encodings, NOT Google protobuf varints. The
// first byte carries a leading-1-bit run whose length equals the number
// of additional bytes that follow; the remaining low bits of the first
// byte combine with the subsequent bytes as little-endian payload. See
// `INT_BYTE_EXTRA` above and rsync 3.2.7 `io.c:1794-1898` for the
// canonical definition.
// =============================================================================

/// Decode a rsync varint from the start of `buf`. Returns the decoded
/// signed 32-bit value (widened to `i64` for uniform call-sites) and the
/// number of bytes consumed.
///
/// Mirrors `io.c::read_varint`. Values in `0x00..=0x7F` encode as a
/// single byte; wider values pack the high bits of the top byte into the
/// first wire byte alongside the leading-1-bit marker.
pub fn decode_varint(buf: &[u8]) -> Result<(i64, usize), RealWireError> {
    if buf.is_empty() {
        return Err(RealWireError::TruncatedBuffer {
            at: "varint_first_byte",
            needed: 1,
            available: 0,
        });
    }
    let ch = buf[0];
    let extra = INT_BYTE_EXTRA[(ch / 4) as usize] as usize;
    if extra == 0 {
        // Single-byte encoding: first byte MSB clear, value fits in
        // 7 bits so there is no sign-extension concern.
        return Ok((i64::from(ch), 1));
    }
    if extra > 4 {
        // A varint can carry at most int32, i.e. 4 extra bytes after the
        // marker. Surface the rsync overflow path rather than silently
        // producing a bogus value.
        return Err(RealWireError::VarintOverflow { first_byte: ch });
    }
    if 1 + extra > buf.len() {
        return Err(RealWireError::TruncatedBuffer {
            at: "varint_payload",
            needed: extra,
            available: buf.len() - 1,
        });
    }
    let bit: u8 = 1u8 << (8 - extra as u8);
    let mut u = [0u8; 4];
    u[..extra].copy_from_slice(&buf[1..1 + extra]);
    // Stash the high bits of the first byte (after stripping the marker)
    // into u[extra]. If extra == 4 the high bits occupy the final byte,
    // otherwise they go into the next free slot.
    if extra < 4 {
        u[extra] = ch & (bit - 1);
    }
    let raw = u32::from_le_bytes(u) as i32;
    Ok((i64::from(raw), 1 + extra))
}

/// Encode a signed 32-bit value as a rsync varint. Mirrors
/// `io.c::write_varint`.
pub fn encode_varint(x: i32) -> Vec<u8> {
    let mut b = [0u8; 5];
    let le = (x as u32).to_le_bytes();
    b[1..5].copy_from_slice(&le);

    let mut cnt: usize = 4;
    while cnt > 1 && b[cnt] == 0 {
        cnt -= 1;
    }
    let bit: u8 = 1u8 << (7 - cnt as u8 + 1);

    if b[cnt] >= bit {
        cnt += 1;
        b[0] = !(bit - 1);
    } else if cnt > 1 {
        // bit*2 fits in u8 because `cnt >= 2` implies `bit <= 0x40`.
        b[0] = b[cnt] | !(bit.wrapping_mul(2).wrapping_sub(1));
    } else {
        b[0] = b[1];
    }

    b[..cnt].to_vec()
}

/// Decode a rsync varlong from the start of `buf` with the given
/// `min_bytes` floor. Mirrors `io.c::read_varlong`.
///
/// Unlike `decode_varint`, the varlong always reads at least
/// `min_bytes` bytes, then potentially `extra` more if the first byte
/// signals a wider encoding. Used in the file-list for file size
/// (`min_bytes=3`), mtime (`min_bytes=4`), atime, crtime.
pub fn decode_varlong(buf: &[u8], min_bytes: u8) -> Result<(i64, usize), RealWireError> {
    if min_bytes == 0 || min_bytes > 8 {
        // Defensive: rsync calls varlong with min_bytes in {3, 4}; any
        // other value would indicate a caller bug.
        return Err(RealWireError::VarintOverflow { first_byte: 0 });
    }
    let min_bytes = min_bytes as usize;
    if buf.len() < min_bytes {
        return Err(RealWireError::TruncatedBuffer {
            at: "varlong_min_bytes",
            needed: min_bytes,
            available: buf.len(),
        });
    }
    let first = buf[0];
    let extra = INT_BYTE_EXTRA[(first / 4) as usize] as usize;
    if min_bytes + extra > 9 {
        return Err(RealWireError::VarintOverflow { first_byte: first });
    }
    if buf.len() < min_bytes + extra {
        return Err(RealWireError::TruncatedBuffer {
            at: "varlong_extra",
            needed: min_bytes + extra,
            available: buf.len(),
        });
    }
    // The rsync C algorithm reads `min_bytes` bytes into b2, copies
    // b2[1..min_bytes] into u.b[0..min_bytes-1], then optionally reads
    // `extra` more bytes into u.b[min_bytes-1..min_bytes-1+extra]. The
    // high-bit-stripped first byte lands at u.b[min_bytes + extra - 1].
    let mut u = [0u8; 9];
    u[..min_bytes - 1].copy_from_slice(&buf[1..min_bytes]);
    if extra > 0 {
        u[min_bytes - 1..min_bytes - 1 + extra].copy_from_slice(&buf[min_bytes..min_bytes + extra]);
        let bit: u8 = 1u8 << (8 - extra as u8);
        u[min_bytes + extra - 1] = first & (bit - 1);
    } else {
        u[min_bytes - 1] = first;
    }
    let mut arr = [0u8; 8];
    arr.copy_from_slice(&u[..8]);
    let raw = i64::from_le_bytes(arr);
    Ok((raw, min_bytes + extra))
}

/// Encode an `i64` as a rsync varlong with the given `min_bytes` floor.
/// Mirrors `io.c::write_varlong`.
pub fn encode_varlong(x: i64, min_bytes: u8) -> Vec<u8> {
    assert!(
        (1..=8).contains(&min_bytes),
        "min_bytes must be in 1..=8 per rsync io.c contract, got {min_bytes}"
    );
    let min_bytes = min_bytes as usize;
    let mut b = [0u8; 9];
    b[1..9].copy_from_slice(&(x as u64).to_le_bytes());

    let mut cnt: usize = 8;
    while cnt > min_bytes && b[cnt] == 0 {
        cnt -= 1;
    }
    let bit: u8 = 1u8 << (7 - cnt as u8 + min_bytes as u8);

    if b[cnt] >= bit {
        cnt += 1;
        b[0] = !(bit - 1);
    } else if cnt > min_bytes {
        b[0] = b[cnt] | !(bit.wrapping_mul(2).wrapping_sub(1));
    } else {
        b[0] = b[cnt];
    }

    b[..cnt].to_vec()
}

// =============================================================================
// Section 5 — File-list entry decoder (S8d)
//
// The rsync file-list is a sequence of `send_file_entry` records, each
// carrying flags + path + size + times + mode + uid/gid (optionally
// with stringified names) + optional checksum. The list ends with a
// terminator — a bare zero byte in the classic encoding, or a varint(0)
// when `CF_VARINT_FLIST_FLAGS` is negotiated.
//
// Field order mirrors `flist.c::send_file_entry` / `recv_file_entry` in
// rsync 3.2.7, restricted to protocol ≥ 31 + regular file + non-hlink.
// Device, symlink, and hardlink paths will ship in a later sinergia.
// =============================================================================

// --- XMIT flag constants (rsync.h, protocol 30+) -----------------------------

pub const XMIT_TOP_DIR: u32 = 1 << 0;
pub const XMIT_SAME_MODE: u32 = 1 << 1;
pub const XMIT_EXTENDED_FLAGS: u32 = 1 << 2;
pub const XMIT_SAME_UID: u32 = 1 << 3;
pub const XMIT_SAME_GID: u32 = 1 << 4;
pub const XMIT_SAME_NAME: u32 = 1 << 5;
pub const XMIT_LONG_NAME: u32 = 1 << 6;
pub const XMIT_SAME_TIME: u32 = 1 << 7;
pub const XMIT_SAME_RDEV_MAJOR: u32 = 1 << 8;
pub const XMIT_NO_CONTENT_DIR: u32 = 1 << 8;
pub const XMIT_HLINKED: u32 = 1 << 9;
pub const XMIT_USER_NAME_FOLLOWS: u32 = 1 << 10;
pub const XMIT_GROUP_NAME_FOLLOWS: u32 = 1 << 11;
pub const XMIT_HLINK_FIRST: u32 = 1 << 12;
pub const XMIT_IO_ERROR_ENDLIST: u32 = 1 << 12;
pub const XMIT_MOD_NSEC: u32 = 1 << 13;
pub const XMIT_SAME_ATIME: u32 = 1 << 14;

// --- Public option / outcome / entry types ----------------------------------

/// Caller-supplied context needed to decode a file-list entry. The rsync
/// wire format is not self-describing: several fields are gated by
/// command-line options (`--checksum`, `--numeric-ids`, `-o/-g`, …) and
/// by negotiated compat flags. This struct captures what is needed to
/// walk the bytes unambiguously.
#[derive(Debug, Clone)]
pub struct FileListDecodeOptions<'a> {
    /// Negotiated protocol version (31 or 32 in current transcripts).
    pub protocol: u32,
    /// `CF_VARINT_FLIST_FLAGS` was negotiated — flags are encoded as a
    /// varint instead of a 1-or-2 byte `XMIT_EXTENDED_FLAGS`-gated
    /// sequence.
    pub xfer_flags_as_varint: bool,
    /// `--checksum` is active on the wire, so each regular-file entry
    /// carries `csum_len` trailing bytes.
    pub always_checksum: bool,
    /// Length of the negotiated checksum (xxh128 = 16, md5 = 16, md4 =
    /// 16, sha1 = 20, …).
    pub csum_len: usize,
    /// `-o` / `--owner` equivalent — uid field is present when
    /// `XMIT_SAME_UID` is not set.
    pub preserve_uid: bool,
    /// `-g` / `--group` equivalent — gid field is present when
    /// `XMIT_SAME_GID` is not set.
    pub preserve_gid: bool,
    /// Last file's name, used when `XMIT_SAME_NAME` with `l1 > 0` asks
    /// us to reuse its prefix.
    pub previous_name: Option<&'a str>,
}

impl<'a> FileListDecodeOptions<'a> {
    /// Defaults tailored for the S8a frozen oracle capture: rsync 3.2.7
    /// with protocol 32, `--checksum` active, xxh128 negotiated,
    /// `CF_VARINT_FLIST_FLAGS` on.
    pub fn frozen_oracle_default() -> Self {
        Self {
            protocol: 32,
            xfer_flags_as_varint: true,
            always_checksum: true,
            csum_len: 16,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: None,
        }
    }
}

/// What `decode_file_list_entry` yielded. The caller then either
/// appends the entry to its working file-list or finalises the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileListDecodeOutcome {
    Entry(FileListEntry),
    EndOfList {
        /// IO error code carried by the terminator varint in protocol
        /// 31+ with `CF_VARINT_FLIST_FLAGS`. Zero for the happy path.
        io_error: i32,
    },
}

/// Decoded file-list entry (regular file, protocol ≥ 31 path). Device /
/// symlink / hardlink extensions are deferred — they land in a later
/// sinergia when we encounter them in a transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileListEntry {
    pub flags: u32,
    pub path: String,
    pub size: i64,
    pub mtime: i64,
    pub mtime_nsec: Option<i32>,
    pub mode: u32,
    pub uid: Option<i64>,
    pub uid_name: Option<String>,
    pub gid: Option<i64>,
    pub gid_name: Option<String>,
    /// Raw checksum bytes when `always_checksum` is active. Length
    /// equals `options.csum_len`; empty otherwise.
    pub checksum: Vec<u8>,
}

/// Read `len` bytes from `buf[offset..]` and interpret them as UTF-8.
/// Rsync guarantees ASCII for paths and owner/group names in the wild;
/// accepting UTF-8 is a strict superset that tolerates the occasional
/// non-ASCII filename without a silent replacement.
fn read_utf8_slice(buf: &[u8], offset: usize, len: usize) -> Result<String, RealWireError> {
    if offset + len > buf.len() {
        return Err(RealWireError::InvalidNameLen {
            declared: len,
            available: buf.len().saturating_sub(offset),
        });
    }
    match std::str::from_utf8(&buf[offset..offset + len]) {
        Ok(s) => Ok(s.to_string()),
        Err(e) => Err(RealWireError::NonUtf8Name {
            offset: offset + e.valid_up_to(),
        }),
    }
}

/// Decode the flags field at the start of a file-list entry. When
/// `xfer_flags_as_varint` is true, flags are a rsync varint. Otherwise,
/// it's a single byte with an optional high byte when
/// `XMIT_EXTENDED_FLAGS` is set.
fn decode_flist_flags(
    buf: &[u8],
    xfer_flags_as_varint: bool,
) -> Result<(u32, usize), RealWireError> {
    if xfer_flags_as_varint {
        let (raw, consumed) = decode_varint(buf)?;
        Ok((raw as u32, consumed))
    } else {
        if buf.is_empty() {
            return Err(RealWireError::TruncatedBuffer {
                at: "flist_flags_byte",
                needed: 1,
                available: 0,
            });
        }
        let lo = buf[0] as u32;
        if lo & XMIT_EXTENDED_FLAGS != 0 {
            if buf.len() < 2 {
                return Err(RealWireError::TruncatedBuffer {
                    at: "flist_flags_ext_byte",
                    needed: 1,
                    available: 0,
                });
            }
            let hi = buf[1] as u32;
            Ok(((hi << 8) | lo, 2))
        } else {
            Ok((lo, 1))
        }
    }
}

/// Decode a single file-list entry, or signal end-of-list if the entry
/// is the terminator. Returns `(outcome, bytes_consumed)`.
pub fn decode_file_list_entry(
    buf: &[u8],
    options: &FileListDecodeOptions,
) -> Result<(FileListDecodeOutcome, usize), RealWireError> {
    let mut cursor = 0;

    // --- 1. Flags -----------------------------------------------------------
    let (flags, consumed_flags) = decode_flist_flags(&buf[cursor..], options.xfer_flags_as_varint)?;
    cursor += consumed_flags;

    // Terminator: flags == 0 means end of file-list. For varint mode an
    // explicit io_error varint can follow; for classic mode a
    // `XMIT_EXTENDED_FLAGS | XMIT_IO_ERROR_ENDLIST` pairing carries the
    // same info — neither yet observed in the frozen oracle.
    if flags == 0 {
        return Ok((FileListDecodeOutcome::EndOfList { io_error: 0 }, cursor));
    }

    // --- 2. Name length + name ---------------------------------------------
    let mut path_bytes: Vec<u8> = Vec::new();
    if flags & XMIT_SAME_NAME != 0 {
        if cursor >= buf.len() {
            return Err(RealWireError::TruncatedBuffer {
                at: "flist_same_name_l1",
                needed: 1,
                available: 0,
            });
        }
        let l1 = buf[cursor] as usize;
        cursor += 1;
        if l1 > 0 {
            let prev = options
                .previous_name
                .ok_or(RealWireError::SameNameWithoutPrevious)?;
            if l1 > prev.len() {
                return Err(RealWireError::SameNamePrefixTooLong {
                    l1,
                    previous_len: prev.len(),
                });
            }
            path_bytes.extend_from_slice(&prev.as_bytes()[..l1]);
        }
    }

    let l2: usize = if flags & XMIT_LONG_NAME != 0 {
        let (raw, consumed) = decode_varint(&buf[cursor..])?;
        cursor += consumed;
        if raw < 0 {
            return Err(RealWireError::InvalidNameLen {
                declared: 0,
                available: 0,
            });
        }
        raw as usize
    } else {
        if cursor >= buf.len() {
            return Err(RealWireError::TruncatedBuffer {
                at: "flist_name_len",
                needed: 1,
                available: 0,
            });
        }
        let n = buf[cursor] as usize;
        cursor += 1;
        n
    };

    if cursor + l2 > buf.len() {
        return Err(RealWireError::InvalidNameLen {
            declared: l2,
            available: buf.len().saturating_sub(cursor),
        });
    }
    path_bytes.extend_from_slice(&buf[cursor..cursor + l2]);
    cursor += l2;

    let path = match String::from_utf8(path_bytes) {
        Ok(s) => s,
        Err(e) => {
            return Err(RealWireError::NonUtf8Name {
                offset: e.utf8_error().valid_up_to(),
            });
        }
    };

    // --- 3. Size (varlong, min_bytes=3) ------------------------------------
    let (size, consumed_size) = decode_varlong(&buf[cursor..], 3)?;
    cursor += consumed_size;

    // --- 4. mtime (varlong, min_bytes=4) unless XMIT_SAME_TIME -------------
    let mtime: i64 = if flags & XMIT_SAME_TIME != 0 {
        0
    } else if options.protocol >= 30 {
        let (m, consumed) = decode_varlong(&buf[cursor..], 4)?;
        cursor += consumed;
        m
    } else {
        // Pre-30 fallback — not expected in this sinergia.
        if cursor + 4 > buf.len() {
            return Err(RealWireError::TruncatedBuffer {
                at: "flist_mtime_legacy",
                needed: 4,
                available: buf.len().saturating_sub(cursor),
            });
        }
        let mut a = [0u8; 4];
        a.copy_from_slice(&buf[cursor..cursor + 4]);
        cursor += 4;
        i64::from(i32::from_le_bytes(a))
    };

    // --- 5. Mtime nanoseconds (protocol ≥ 31, XMIT_MOD_NSEC) --------------
    let mtime_nsec: Option<i32> = if options.protocol >= 31 && (flags & XMIT_MOD_NSEC != 0) {
        let (n, consumed) = decode_varint(&buf[cursor..])?;
        cursor += consumed;
        Some(n as i32)
    } else {
        None
    };

    // --- 6. Mode (u32 LE) unless XMIT_SAME_MODE ----------------------------
    let mode: u32 = if flags & XMIT_SAME_MODE != 0 {
        0
    } else {
        if cursor + 4 > buf.len() {
            return Err(RealWireError::TruncatedBuffer {
                at: "flist_mode",
                needed: 4,
                available: buf.len().saturating_sub(cursor),
            });
        }
        let mut a = [0u8; 4];
        a.copy_from_slice(&buf[cursor..cursor + 4]);
        cursor += 4;
        u32::from_le_bytes(a)
    };

    // --- 7. uid (varint) + optional USER_NAME_FOLLOWS ----------------------
    let (uid, uid_name) = if options.preserve_uid && (flags & XMIT_SAME_UID == 0) {
        let (uid_raw, consumed) = decode_varint(&buf[cursor..])?;
        cursor += consumed;
        let name = if flags & XMIT_USER_NAME_FOLLOWS != 0 {
            if cursor >= buf.len() {
                return Err(RealWireError::TruncatedBuffer {
                    at: "flist_uid_name_len",
                    needed: 1,
                    available: 0,
                });
            }
            let name_len = buf[cursor] as usize;
            cursor += 1;
            let s = read_utf8_slice(buf, cursor, name_len)?;
            cursor += name_len;
            Some(s)
        } else {
            None
        };
        (Some(uid_raw), name)
    } else {
        (None, None)
    };

    // --- 8. gid (varint) + optional GROUP_NAME_FOLLOWS ---------------------
    let (gid, gid_name) = if options.preserve_gid && (flags & XMIT_SAME_GID == 0) {
        let (gid_raw, consumed) = decode_varint(&buf[cursor..])?;
        cursor += consumed;
        let name = if flags & XMIT_GROUP_NAME_FOLLOWS != 0 {
            if cursor >= buf.len() {
                return Err(RealWireError::TruncatedBuffer {
                    at: "flist_gid_name_len",
                    needed: 1,
                    available: 0,
                });
            }
            let name_len = buf[cursor] as usize;
            cursor += 1;
            let s = read_utf8_slice(buf, cursor, name_len)?;
            cursor += name_len;
            Some(s)
        } else {
            None
        };
        (Some(gid_raw), name)
    } else {
        (None, None)
    };

    // --- 9. Checksum (always_checksum active) ------------------------------
    let checksum = if options.always_checksum && options.csum_len > 0 {
        if cursor + options.csum_len > buf.len() {
            return Err(RealWireError::TruncatedBuffer {
                at: "flist_checksum",
                needed: options.csum_len,
                available: buf.len().saturating_sub(cursor),
            });
        }
        let v = buf[cursor..cursor + options.csum_len].to_vec();
        cursor += options.csum_len;
        v
    } else {
        Vec::new()
    };

    Ok((
        FileListDecodeOutcome::Entry(FileListEntry {
            flags,
            path,
            size,
            mtime,
            mtime_nsec,
            mode,
            uid,
            uid_name,
            gid,
            gid_name,
            checksum,
        }),
        cursor,
    ))
}

// ----------------------------------------------------------------------------
// Sinergia 8i-encode — file_list_entry writer.
//
// Mirror of `decode_file_list_entry`. Field order, gating semantics, and
// every length-prefix shape MUST match `flist.c::send_file_entry` in
// rsync 3.2.7. Validated by round-trip tests against the decoder AND
// by re-encoding the frozen oracle's flist entry byte-for-byte.
//
// **SAME_NAME prefix semantics**: when `entry.flags & XMIT_SAME_NAME`
// is set, the encoder computes `l1` as the longest common byte prefix
// between `entry.path` and `options.previous_name`. The remaining
// suffix length `l2 = entry.path.len() - l1` is emitted via `l2` length
// prefix (varint if `XMIT_LONG_NAME` else 1-byte) followed by the raw
// suffix bytes. This is the exact mirror of `flist.c:480` `for (l1=0;
// fname[l1]==lastname[l1] && (fname[l1] || lastname[l1]); l1++) {}`
// modulo the rsync `<255` clamp on classic-flag mode.
// ----------------------------------------------------------------------------

/// Compute `(l1, l2_bytes)` for a file-list entry given the previous
/// name and the SAME_NAME flag. Mirrors `flist.c:480-486`. When
/// SAME_NAME is unset, `l1` is always 0 and the full path is the
/// suffix.
fn compute_flist_name_split<'a>(
    entry_path: &'a str,
    previous_name: Option<&str>,
    same_name: bool,
) -> (usize, &'a [u8]) {
    if !same_name {
        return (0, entry_path.as_bytes());
    }
    let prev = previous_name.unwrap_or("");
    let entry_bytes = entry_path.as_bytes();
    let prev_bytes = prev.as_bytes();
    let max_common = entry_bytes.len().min(prev_bytes.len()).min(255);
    let mut l1 = 0;
    while l1 < max_common && entry_bytes[l1] == prev_bytes[l1] {
        l1 += 1;
    }
    (l1, &entry_bytes[l1..])
}

/// Encode a `FileListEntry` using the same option set the decoder
/// would consume. Returns the byte sequence written to the wire.
///
/// **Caller contract**: `entry.flags` carries the XMIT_* gating
/// decisions (SAME_NAME, LONG_NAME, MOD_NSEC, USER_NAME_FOLLOWS, …).
/// The encoder honours those flags exactly — it does NOT recompute
/// SAME_TIME/SAME_MODE/SAME_UID/SAME_GID from entry deltas, because
/// the decision matrix lives one layer up (the planner /
/// flist-builder, which knows the full file list and previous-entry
/// context). For SAME_NAME, the encoder DOES compute `l1` from
/// `entry.path` vs `options.previous_name` since that is the only
/// well-defined choice given the path.
pub fn encode_file_list_entry(entry: &FileListEntry, options: &FileListDecodeOptions) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);

    // --- 1. Flags ---------------------------------------------------------
    if options.xfer_flags_as_varint {
        out.extend_from_slice(&encode_varint(entry.flags as i32));
    } else {
        let lo = (entry.flags & 0xFF) as u8;
        let needs_ext = (entry.flags & XMIT_EXTENDED_FLAGS) != 0;
        if needs_ext {
            out.push(lo);
            out.push(((entry.flags >> 8) & 0xFF) as u8);
        } else {
            out.push(lo);
        }
    }

    // --- 2. Name length + suffix bytes -----------------------------------
    let same_name = (entry.flags & XMIT_SAME_NAME) != 0;
    let (l1, suffix) = compute_flist_name_split(&entry.path, options.previous_name, same_name);
    if same_name {
        out.push(l1 as u8);
    }
    if (entry.flags & XMIT_LONG_NAME) != 0 {
        out.extend_from_slice(&encode_varint(suffix.len() as i32));
    } else {
        out.push(suffix.len() as u8);
    }
    out.extend_from_slice(suffix);

    // --- 3. Size (varlong, min_bytes=3) ----------------------------------
    out.extend_from_slice(&encode_varlong(entry.size, 3));

    // --- 4. mtime (varlong, min_bytes=4) unless XMIT_SAME_TIME -----------
    if (entry.flags & XMIT_SAME_TIME) == 0 {
        if options.protocol >= 30 {
            out.extend_from_slice(&encode_varlong(entry.mtime, 4));
        } else {
            // Pre-30 path mirrors the legacy 4-byte LE fallback in
            // the decoder. Truncating cast is the historic behaviour.
            out.extend_from_slice(&(entry.mtime as i32).to_le_bytes());
        }
    }

    // --- 5. mtime nsec (protocol ≥ 31, XMIT_MOD_NSEC) --------------------
    if options.protocol >= 31 && (entry.flags & XMIT_MOD_NSEC) != 0 {
        let n = entry.mtime_nsec.unwrap_or(0);
        out.extend_from_slice(&encode_varint(n));
    }

    // --- 6. Mode (u32 LE) unless XMIT_SAME_MODE --------------------------
    if (entry.flags & XMIT_SAME_MODE) == 0 {
        out.extend_from_slice(&entry.mode.to_le_bytes());
    }

    // --- 7. uid + optional name ------------------------------------------
    if options.preserve_uid && (entry.flags & XMIT_SAME_UID) == 0 {
        let uid_value = entry.uid.unwrap_or(0);
        out.extend_from_slice(&encode_varint(uid_value as i32));
        if (entry.flags & XMIT_USER_NAME_FOLLOWS) != 0 {
            let name = entry.uid_name.as_deref().unwrap_or("");
            assert!(
                name.len() <= u8::MAX as usize,
                "uid_name length {} exceeds u8 wire encoding",
                name.len()
            );
            out.push(name.len() as u8);
            out.extend_from_slice(name.as_bytes());
        }
    }

    // --- 8. gid + optional name ------------------------------------------
    if options.preserve_gid && (entry.flags & XMIT_SAME_GID) == 0 {
        let gid_value = entry.gid.unwrap_or(0);
        out.extend_from_slice(&encode_varint(gid_value as i32));
        if (entry.flags & XMIT_GROUP_NAME_FOLLOWS) != 0 {
            let name = entry.gid_name.as_deref().unwrap_or("");
            assert!(
                name.len() <= u8::MAX as usize,
                "gid_name length {} exceeds u8 wire encoding",
                name.len()
            );
            out.push(name.len() as u8);
            out.extend_from_slice(name.as_bytes());
        }
    }

    // --- 9. Checksum (always_checksum) ------------------------------------
    if options.always_checksum && options.csum_len > 0 {
        assert_eq!(
            entry.checksum.len(),
            options.csum_len,
            "checksum length mismatch: entry has {} bytes, options.csum_len = {}",
            entry.checksum.len(),
            options.csum_len
        );
        out.extend_from_slice(&entry.checksum);
    }

    out
}

/// Encode a file-list terminator. In `xfer_flags_as_varint` mode this
/// is `varint(0)` (single zero byte); in classic mode this is also a
/// single zero byte. Symmetric to the `flags == 0` early return in
/// `decode_file_list_entry`.
pub fn encode_file_list_terminator(options: &FileListDecodeOptions) -> Vec<u8> {
    if options.xfer_flags_as_varint {
        encode_varint(0)
    } else {
        vec![0u8]
    }
}

// ---------------------------------------------------------------------------
// Sinergia 8e — file-index (ndx), item flags, and signature block decoder.
//
// After the file-list terminator, rsync's generator and sender interleave
// a per-file header (`write_ndx` + `write_shortint(iflags)`) with a
// `write_sum_head` (four int32 LE fields) and `count` pairs of
// (rolling checksum u32 LE, truncated strong checksum `s2length` bytes).
// Phase transitions are signalled via `NDX_DONE` single-byte markers, and
// the sender announces the end of the file-list phase with `NDX_FLIST_EOF`.
//
// Wire shape validated byte-for-byte against the frozen oracle's
// `upload_server_to_client` stream:
//   [write_ndx(1)] 02
//   [write_shortint(ITEM_TRANSFER|ITEM_REPORT_CHANGE)] 02 80
//   [write_sum_head(count=375, blen=700, s2len=2, rem=344)] 77 01 00 00
//                                                            bc 02 00 00
//                                                            02 00 00 00
//                                                            58 01 00 00
//   [375 * (rolling u32 LE, strong 2B)] 2250 bytes
//   [5 * write_ndx(NDX_DONE)] 00 00 00 00 00
// Total 2274 bytes, exactly what `reassemble_msg_data` delivers.
// ---------------------------------------------------------------------------

/// Sentinel ndx values from `rsync.h`. Negative-on-wire values that the
/// sender / generator use to signal phase transitions and end-of-stream.
pub const NDX_DONE: i32 = -1;
pub const NDX_FLIST_EOF: i32 = -2;
pub const NDX_DEL_STATS: i32 = -3;
pub const NDX_FLIST_OFFSET: i32 = -101;

/// Rsync `sum_head` validation bounds from `io.c::read_sum_head` +
/// `rsync.h` constants. Matching exactly keeps overflow errors
/// surface-compatible with the reference implementation.
pub const SUM_HEAD_MAX_BLOCK_LEN_PROTO30PLUS: i32 = 131_072;
pub const SUM_HEAD_MAX_DIGEST_LEN: i32 = 20;

/// State carried between successive `decode_ndx` calls on the **same**
/// direction. The rsync ndx stream is diff-encoded against two rolling
/// baselines (positive and negative indices) — a fresh state starts at
/// `(prev_positive = -1, prev_negative = 1)` per `io.c::read_ndx`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NdxState {
    prev_positive: i32,
    prev_negative: i32,
}

impl NdxState {
    /// Fresh baselines matching rsync's static initialisers.
    pub fn new() -> Self {
        Self {
            prev_positive: -1,
            prev_negative: 1,
        }
    }

    /// Current positive baseline. Exposed for white-box testing; a
    /// production driver never needs to peek at it.
    pub fn prev_positive(&self) -> i32 {
        self.prev_positive
    }

    /// Current negative baseline (stored as a positive magnitude, as
    /// rsync does internally).
    pub fn prev_negative(&self) -> i32 {
        self.prev_negative
    }
}

impl Default for NdxState {
    fn default() -> Self {
        Self::new()
    }
}

/// Decoded `sum_head` — four int32 LE values, validated against
/// `read_sum_head`'s bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SumHead {
    /// Number of signature blocks that follow.
    pub count: i32,
    /// Block length used for rolling checksum windowing.
    pub block_length: i32,
    /// Bytes of strong checksum transmitted per block. Often
    /// truncated below the native digest length to save bandwidth.
    pub checksum_length: i32,
    /// Size of the final partial block (0..block_length).
    pub remainder_length: i32,
}

/// Decoded signature block — 4-byte LE rolling checksum + `strong_len`
/// bytes of strong checksum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SumBlock {
    pub rolling: u32,
    pub strong: Vec<u8>,
}

/// Decode one ndx value and update the caller's baselines. Returns the
/// consumed byte count. Matches `io.c::read_ndx` for protocol >= 30:
/// single byte (diff 1..=253), three-byte form (`0xFE + u16 BE diff`),
/// five-byte form (`0xFE + 4-byte absolute int32 with high bit set`),
/// negative prefix `0xFF` followed by one of the above on the negative
/// baseline, or single-byte 0 meaning `NDX_DONE`.
pub fn decode_ndx(buf: &[u8], state: &mut NdxState) -> Result<(i32, usize), RealWireError> {
    if buf.is_empty() {
        return Err(RealWireError::NdxTruncated { form: "prefix" });
    }

    let first = buf[0];

    // Special single-byte case: 0 = NDX_DONE. `read_ndx` returns this
    // directly without touching either baseline.
    if first == 0 {
        return Ok((NDX_DONE, 1));
    }

    // A 0xFF prefix says "the remainder is a diff against the
    // **negative** baseline". Consume the prefix byte and then continue
    // with the standard 1/3/5-byte form on the second byte.
    let (payload, negate, consumed_prefix) = if first == 0xFF {
        if buf.len() < 2 {
            return Err(RealWireError::NdxTruncated {
                form: "negative_prefix",
            });
        }
        (&buf[1..], true, 1usize)
    } else {
        (buf, false, 0usize)
    };

    let (raw, consumed_body) = decode_ndx_body(payload, negate, state)?;
    Ok((raw, consumed_prefix + consumed_body))
}

fn decode_ndx_body(
    buf: &[u8],
    negate: bool,
    state: &mut NdxState,
) -> Result<(i32, usize), RealWireError> {
    if buf.is_empty() {
        return Err(RealWireError::NdxTruncated {
            form: "body_first_byte",
        });
    }
    let first = buf[0];

    // Helper: look up which baseline to update and finalise the value.
    fn finalize(state: &mut NdxState, negate: bool, num: i32) -> i32 {
        if negate {
            state.prev_negative = num;
            -num
        } else {
            state.prev_positive = num;
            num
        }
    }

    if first == 0xFE {
        // Either 3-byte (0xFE + hi8 + lo8 diff) or 5-byte (0xFE +
        // (abs >> 24) | 0x80) + abs LE lower 24 bits). The
        // discriminator is the high bit of the second byte.
        if buf.len() < 3 {
            return Err(RealWireError::NdxTruncated { form: "fe_prefix" });
        }
        let second = buf[1];
        if (second & 0x80) != 0 {
            // 5-byte form: b[1] carries abs >> 24 with 0x80 bit,
            // b[2]=abs low byte, b[3]=abs mid byte, b[4]=abs high byte.
            if buf.len() < 5 {
                return Err(RealWireError::NdxTruncated { form: "fe_5byte" });
            }
            let top = u32::from(second & 0x7F);
            let lo = u32::from(buf[2]);
            let mid = u32::from(buf[3]);
            let hi = u32::from(buf[4]);
            // Reassemble: `lo` + `mid << 8` + `hi << 16` + `top << 24`.
            let abs = lo | (mid << 8) | (hi << 16) | (top << 24);
            let num = abs as i32;
            Ok((finalize(state, negate, num), 5))
        } else {
            // 3-byte form: diff = (b[1] << 8) | b[2], applied on top of
            // the selected baseline. No sign-extension.
            let hi = u32::from(second);
            let lo = u32::from(buf[2]);
            let diff = (hi << 8) | lo;
            let baseline = if negate {
                state.prev_negative
            } else {
                state.prev_positive
            };
            let num = baseline.wrapping_add(diff as i32);
            Ok((finalize(state, negate, num), 3))
        }
    } else {
        // Single-byte diff (1..=253). `first` is always interpreted as
        // unsigned here, added on top of the selected baseline.
        let diff = u32::from(first);
        let baseline = if negate {
            state.prev_negative
        } else {
            state.prev_positive
        };
        let num = baseline.wrapping_add(diff as i32);
        Ok((finalize(state, negate, num), 1))
    }
}

/// Encode an ndx value against the caller's rolling state. Mirrors
/// `io.c::write_ndx`. Used by the production driver
/// (`delta_transport_impl`) to frame outgoing ndx values and by the
/// test lane for round-trip assertions.
pub fn encode_ndx(ndx: i32, state: &mut NdxState) -> Vec<u8> {
    // NDX_DONE is always a single zero byte, no baseline mutation.
    if ndx == NDX_DONE {
        return vec![0];
    }
    let mut out: Vec<u8> = Vec::with_capacity(6);
    let (abs, diff, is_negative) = if ndx >= 0 {
        let diff = ndx.wrapping_sub(state.prev_positive);
        state.prev_positive = ndx;
        (ndx, diff, false)
    } else {
        out.push(0xFF);
        let abs = -ndx;
        let diff = abs.wrapping_sub(state.prev_negative);
        state.prev_negative = abs;
        (abs, diff, true)
    };
    let _ = is_negative; // handled by the 0xFF prefix above

    if diff > 0 && diff < 0xFE {
        out.push(diff as u8);
    } else if !(0..=0x7FFF).contains(&diff) {
        // 5-byte form with the 32-bit absolute ndx, high bit set on top.
        out.push(0xFE);
        let abs_u = abs as u32;
        out.push(((abs_u >> 24) as u8) | 0x80);
        out.push((abs_u & 0xFF) as u8);
        out.push(((abs_u >> 8) & 0xFF) as u8);
        out.push(((abs_u >> 16) & 0xFF) as u8);
    } else {
        // 3-byte form: 0xFE + hi8 + lo8 of the diff.
        out.push(0xFE);
        out.push(((diff >> 8) & 0xFF) as u8);
        out.push((diff & 0xFF) as u8);
    }
    out
}

/// Decode a `write_shortint(iflags)` — 2 bytes little-endian u16.
pub fn decode_item_flags(buf: &[u8]) -> Result<(u16, usize), RealWireError> {
    if buf.len() < 2 {
        return Err(RealWireError::TruncatedBuffer {
            at: "item_flags",
            needed: 2,
            available: buf.len(),
        });
    }
    let lo = u16::from(buf[0]);
    let hi = u16::from(buf[1]);
    Ok(((hi << 8) | lo, 2))
}

/// Encode iflags as 2-byte LE u16. Mirrors `write_shortint`.
pub fn encode_item_flags(flags: u16) -> [u8; 2] {
    flags.to_le_bytes()
}

/// Decode a `write_sum_head` — four int32 LE fields with bounds
/// validation matching `io.c::read_sum_head` (count >= 0, 0 <= blength
/// <= MAX_BLOCK_SIZE, 0 <= s2length <= MAX_DIGEST_LEN, 0 <= remainder
/// <= blength).
pub fn decode_sum_head(buf: &[u8]) -> Result<(SumHead, usize), RealWireError> {
    if buf.len() < 16 {
        return Err(RealWireError::TruncatedBuffer {
            at: "sum_head",
            needed: 16,
            available: buf.len(),
        });
    }
    let count = i32::from_le_bytes(buf[0..4].try_into().unwrap());
    let block_length = i32::from_le_bytes(buf[4..8].try_into().unwrap());
    let checksum_length = i32::from_le_bytes(buf[8..12].try_into().unwrap());
    let remainder_length = i32::from_le_bytes(buf[12..16].try_into().unwrap());

    if count < 0 {
        return Err(RealWireError::SumHeadFieldOutOfRange {
            field: "count",
            value: count,
            max: i32::MAX,
        });
    }
    if !(0..=SUM_HEAD_MAX_BLOCK_LEN_PROTO30PLUS).contains(&block_length) {
        return Err(RealWireError::SumHeadFieldOutOfRange {
            field: "block_length",
            value: block_length,
            max: SUM_HEAD_MAX_BLOCK_LEN_PROTO30PLUS,
        });
    }
    if !(0..=SUM_HEAD_MAX_DIGEST_LEN).contains(&checksum_length) {
        return Err(RealWireError::SumHeadFieldOutOfRange {
            field: "checksum_length",
            value: checksum_length,
            max: SUM_HEAD_MAX_DIGEST_LEN,
        });
    }
    if !(0..=block_length).contains(&remainder_length) {
        return Err(RealWireError::SumHeadFieldOutOfRange {
            field: "remainder_length",
            value: remainder_length,
            max: block_length,
        });
    }

    Ok((
        SumHead {
            count,
            block_length,
            checksum_length,
            remainder_length,
        },
        16,
    ))
}

/// Encode a SumHead as 4×int32 LE. For unit-test round-trip only.
pub fn encode_sum_head(head: &SumHead) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&head.count.to_le_bytes());
    out[4..8].copy_from_slice(&head.block_length.to_le_bytes());
    out[8..12].copy_from_slice(&head.checksum_length.to_le_bytes());
    out[12..16].copy_from_slice(&head.remainder_length.to_le_bytes());
    out
}

/// Decode a single signature block: u32 LE rolling checksum followed by
/// exactly `strong_len` bytes of strong checksum. Caller passes
/// `strong_len` from a previously-decoded `SumHead.checksum_length`.
pub fn decode_sum_block(buf: &[u8], strong_len: usize) -> Result<(SumBlock, usize), RealWireError> {
    let needed = 4usize
        .checked_add(strong_len)
        .ok_or(RealWireError::SumHeadFieldOutOfRange {
            field: "checksum_length",
            value: strong_len as i32,
            max: SUM_HEAD_MAX_DIGEST_LEN,
        })?;
    if buf.len() < needed {
        return Err(RealWireError::TruncatedBuffer {
            at: "sum_block",
            needed,
            available: buf.len(),
        });
    }
    let rolling = u32::from_le_bytes(buf[0..4].try_into().unwrap());
    let strong = buf[4..4 + strong_len].to_vec();
    Ok((SumBlock { rolling, strong }, needed))
}

/// Encode a single signature block (Sinergia 8i-encode). Inverse of
/// `decode_sum_block`. The strong-checksum slice MUST already be the
/// per-`SumHead.checksum_length` length the receiver agreed to —
/// truncation is a caller responsibility (see `checksum.c::sum_init`
/// in rsync 3.2.7).
pub fn encode_sum_block(block: &SumBlock) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + block.strong.len());
    out.extend_from_slice(&block.rolling.to_le_bytes());
    out.extend_from_slice(&block.strong);
    out
}

/// Encode a file-level strong checksum (Sinergia 8i-encode). It is the
/// raw `checksum_length` bytes appended after `END_FLAG` by
/// `match.c::match_sums`. No length prefix; the receiver knows the
/// negotiated length up front. This helper is a no-op transformation
/// retained for symmetry + intent clarity at call sites.
pub fn encode_file_checksum(bytes: &[u8]) -> Vec<u8> {
    bytes.to_vec()
}

// =============================================================================
// Sezione 7 — delta instruction stream (match_sums → send_*_token output)
// =============================================================================
//
// Rsync 3.2.7 emits the delta stream through one of three sibling functions:
//   - `token.c::simple_send_token`     (no compression negotiated)
//   - `token.c::send_deflated_token`   (zlib, CPRES_ZLIB/CPRES_ZLIBX)
//   - `token.c::send_zstd_token`       (zstd, CPRES_ZSTD — proto 31+ when
//                                       negotiation elects it)
//
// The **uncompressed** path uses a simple 4-byte-integer framing — every
// record is a single `write_int` and the data follows raw. The **compressed**
// paths (zlib + zstd) share an IDENTICAL **outer** byte framing, documented
// in `token.c` lines 321-327 as the `END_FLAG / TOKEN_* / DEFLATED_DATA /
// TOKENRUN_* / TOKEN_REL / TOKENRUN_REL` tag set. What changes between the
// two compressed variants is only the payload bytes inside a DEFLATED_DATA
// record (a self-contained zlib stream or a self-contained zstd frame).
// The outer tag parsing is bit-identical.
//
// The decoder below deals with the **compressed outer framing** only —
// appropriate for the Strada C frozen oracle (proto 31 + zstd negotiated via
// `CF_VARINT_FLIST_FLAGS + negotiate_the_strings`). The payload inside each
// `Literal` is returned as opaque `compressed_payload: Vec<u8>`; real zstd
// decoding is performed downstream in the driver (`delta_transport_impl`)
// where the decompressor owns window state across frames. The
// `simple_send_token` (uncompressed) shape is intentionally not implemented
// here — it is unreachable via the frozen oracle and would require a
// separate capture lane before we can wire it.
//
// After the tag stream a single `END_FLAG=0x00` marks end-of-tokens for the
// current file. Immediately after the END_FLAG the sender writes the
// `xfer_sum_len`-byte file-level strong checksum RAW, with no length prefix
// (`match.c::match_sums` line 423). The length is negotiated upfront (see
// `checksum.c::csum_len_for_type`) and defaults to 16 for MD5 / xxh128 /
// xxh3 derivatives — in the frozen oracle profile the negotiated algo is
// xxh128 (host + server both xxh3-linked), so `checksum_length = 16`.
//
// Wire tags (matches `token.c:321-327`):
//   END_FLAG       0x00            end-of-tokens for this file
//   TOKEN_LONG     0x20            absolute 32-bit token_index (int32 LE follows)
//   TOKENRUN_LONG  0x21            absolute token_index + 16-bit run_count
//   DEFLATED_DATA  0x40 | hi6(len) compressed literal: (len=(hi6<<8)|lo8), lo8 byte follows, then `len` raw bytes
//                                  valid len range: 1..=16383
//   TOKEN_REL      0x80 | rel6     relative offset in [0,63] from last token_index, single-block match
//   TOKENRUN_REL   0xC0 | rel6     relative offset + 16-bit run_count little-endian
//
// State carried across successive records on the SAME file is the
// `last_token_end` value — rsync's `last_run_end + run_len` — used to
// resolve relative tokens. A fresh `DeltaStreamState` starts at 0.

/// Outer token framing tags from `token.c:321-327`. Copied as runtime
/// constants for use in pattern matching and error messages.
pub const TOKEN_END_FLAG: u8 = 0x00;
pub const TOKEN_LONG: u8 = 0x20;
pub const TOKENRUN_LONG: u8 = 0x21;
pub const TOKEN_DEFLATED_DATA: u8 = 0x40;
pub const TOKEN_REL: u8 = 0x80;
pub const TOKENRUN_REL: u8 = 0xC0;

/// Maximum literal payload size in a single DEFLATED_DATA record — 14 bits
/// spread as `hi6 << 8 | lo8`.
pub const MAX_DELTA_LITERAL_LEN: usize = 16_383;

/// One decoded delta instruction. The compressed path's three distinct
/// encodings are normalised into two op variants here — `CopyRun` with
/// `run_length == 1` is equivalent to a single `Copy` and the caller gets
/// to decide whether to coalesce at their layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeltaOp {
    /// One or more consecutive matched blocks starting at
    /// `start_token_index`. `run_length >= 1`.
    CopyRun {
        start_token_index: i32,
        run_length: u16,
    },
    /// An opaque compressed literal payload. The bytes are the raw
    /// contents of a self-contained zlib stream or zstd frame — the
    /// outer framing does NOT tell us which compressor produced them;
    /// the caller is expected to know from the negotiated algo.
    Literal { compressed_payload: Vec<u8> },
}

/// State carried between successive `decode_delta_op` calls on the **same**
/// file. Tracks the "last_run_end" baseline used to interpret relative
/// tokens (`TOKEN_REL` / `TOKENRUN_REL`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeltaStreamState {
    /// The token index one past the end of the most recently decoded
    /// CopyRun. Starts at 0 per `token.c::send_deflated_token` (static
    /// `last_run_end = 0` after token_init).
    last_run_end: i32,
}

impl DeltaStreamState {
    pub fn new() -> Self {
        Self { last_run_end: 0 }
    }

    /// Current last_run_end baseline. White-box access for tests.
    pub fn last_run_end(&self) -> i32 {
        self.last_run_end
    }
}

/// Outcome of a single `decode_delta_op` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeltaOpOutcome {
    /// A regular delta op (CopyRun or Literal).
    Op(DeltaOp),
    /// The `END_FLAG=0x00` sentinel — no more delta ops follow for this
    /// file. The caller should then read the file-level strong checksum
    /// via `decode_file_checksum`.
    EndFlag,
}

/// Full decoded report of one file's delta stream plus the trailing
/// file-level strong checksum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeltaStreamReport {
    pub ops: Vec<DeltaOp>,
    /// Raw bytes of the file-level strong checksum (xxh128 / MD5 / SHA1 /
    /// xxh3-derived — negotiated upfront, not tagged on the wire).
    pub file_checksum: Vec<u8>,
}

/// Decode one delta-stream record. Mutates `state.last_run_end` for
/// TOKEN_REL / TOKENRUN_REL / TOKEN_LONG / TOKENRUN_LONG variants.
/// Returns either an `Op` or the `EndFlag` sentinel plus the byte count
/// consumed. The END_FLAG case returns `1` — the caller should then call
/// `decode_file_checksum` against the following bytes.
pub fn decode_delta_op(
    buf: &[u8],
    state: &mut DeltaStreamState,
) -> Result<(DeltaOpOutcome, usize), RealWireError> {
    if buf.is_empty() {
        return Err(RealWireError::DeltaTokenTruncated {
            at: "tag",
            needed: 1,
            available: 0,
        });
    }
    let tag = buf[0];

    // END_FLAG — single byte, no state mutation.
    if tag == TOKEN_END_FLAG {
        return Ok((DeltaOpOutcome::EndFlag, 1));
    }

    // DEFLATED_DATA: tag = 0x40 | hi6(len); second byte = low 8 bits of len.
    // Total len = (hi6 << 8) | lo8, valid range 1..=16383.
    if (tag & 0xC0) == TOKEN_DEFLATED_DATA {
        if buf.len() < 2 {
            return Err(RealWireError::DeltaTokenTruncated {
                at: "deflated_len",
                needed: 2,
                available: buf.len(),
            });
        }
        let hi6 = usize::from(tag & 0x3F);
        let lo8 = usize::from(buf[1]);
        let len = (hi6 << 8) | lo8;
        if len == 0 {
            // rsync never emits a zero-length DEFLATED_DATA. Treat as a
            // malformed record rather than silently accepting.
            return Err(RealWireError::DeltaTokenTruncated {
                at: "deflated_len_zero",
                needed: 1,
                available: 0,
            });
        }
        let payload_start = 2usize;
        let payload_end = payload_start + len;
        if buf.len() < payload_end {
            return Err(RealWireError::DeltaTokenTruncated {
                at: "deflated_payload",
                needed: len,
                available: buf.len() - payload_start,
            });
        }
        let payload = buf[payload_start..payload_end].to_vec();
        return Ok((
            DeltaOpOutcome::Op(DeltaOp::Literal {
                compressed_payload: payload,
            }),
            payload_end,
        ));
    }

    // TOKENRUN_REL: tag = 0xC0 | rel6; next 2 bytes = run_count LE u16.
    // Relative forms must come BEFORE the TOKEN_REL test because 0xC0 also
    // has the 0x80 bit set.
    if (tag & 0xC0) == TOKENRUN_REL {
        if buf.len() < 3 {
            return Err(RealWireError::DeltaTokenTruncated {
                at: "tokenrun_rel",
                needed: 3,
                available: buf.len(),
            });
        }
        let rel = i32::from(tag & 0x3F);
        let run = u16::from_le_bytes([buf[1], buf[2]]);
        let start = state.last_run_end.wrapping_add(rel);
        state.last_run_end = start.wrapping_add(i32::from(run));
        return Ok((
            DeltaOpOutcome::Op(DeltaOp::CopyRun {
                start_token_index: start,
                run_length: run,
            }),
            3,
        ));
    }

    // TOKEN_REL: tag = 0x80 | rel6, single block match, no payload.
    if (tag & 0xC0) == TOKEN_REL {
        let rel = i32::from(tag & 0x3F);
        let start = state.last_run_end.wrapping_add(rel);
        state.last_run_end = start.wrapping_add(1);
        return Ok((
            DeltaOpOutcome::Op(DeltaOp::CopyRun {
                start_token_index: start,
                run_length: 1,
            }),
            1,
        ));
    }

    // TOKENRUN_LONG: 0x21 + int32 LE absolute token + u16 LE run.
    if tag == TOKENRUN_LONG {
        if buf.len() < 7 {
            return Err(RealWireError::DeltaTokenTruncated {
                at: "tokenrun_long",
                needed: 7,
                available: buf.len(),
            });
        }
        let start = i32::from_le_bytes(buf[1..5].try_into().unwrap());
        let run = u16::from_le_bytes([buf[5], buf[6]]);
        state.last_run_end = start.wrapping_add(i32::from(run));
        return Ok((
            DeltaOpOutcome::Op(DeltaOp::CopyRun {
                start_token_index: start,
                run_length: run,
            }),
            7,
        ));
    }

    // TOKEN_LONG: 0x20 + int32 LE absolute token, single block.
    if tag == TOKEN_LONG {
        if buf.len() < 5 {
            return Err(RealWireError::DeltaTokenTruncated {
                at: "token_long",
                needed: 5,
                available: buf.len(),
            });
        }
        let start = i32::from_le_bytes(buf[1..5].try_into().unwrap());
        state.last_run_end = start.wrapping_add(1);
        return Ok((
            DeltaOpOutcome::Op(DeltaOp::CopyRun {
                start_token_index: start,
                run_length: 1,
            }),
            5,
        ));
    }

    // Any other tag byte is unknown — rsync itself exits on unknown tags.
    Err(RealWireError::DeltaTokenTruncated {
        at: "unknown_tag",
        needed: 1,
        available: 1,
    })
}

/// Decode the file-level strong checksum that follows `END_FLAG`.
/// The length is not encoded on the wire — it comes from the earlier
/// algorithm negotiation (commonly 16 bytes for MD5 / xxh128, 20 for
/// SHA1). Returns the checksum bytes and the consumed byte count.
pub fn decode_file_checksum(buf: &[u8], len: usize) -> Result<(Vec<u8>, usize), RealWireError> {
    if buf.len() < len {
        return Err(RealWireError::DeltaTokenTruncated {
            at: "file_checksum",
            needed: len,
            available: buf.len(),
        });
    }
    Ok((buf[..len].to_vec(), len))
}

// ----------------------------------------------------------------------------
// Sinergia 8i-encode — delta op + delta stream writers.
//
// Mirror of `decode_delta_op` / `decode_delta_stream`. For each
// `CopyRun`, the encoder picks between the REL and LONG forms based
// on whether `start_token_index - state.last_run_end` fits in 0..=63
// (the 6-bit field of TOKEN_REL / TOKENRUN_REL). For `Literal`, the
// payload length (must be in 1..=MAX_DELTA_LITERAL_LEN) is split into
// `hi6 << 8 | lo8` and stored in the 2-byte header. State is mutated
// the same way the decoder does so a re-encode of a decoded ops vec
// produces an identical wire trail.
// ----------------------------------------------------------------------------

/// Encode a single delta op into its wire form. `state.last_run_end`
/// is updated for every `CopyRun` variant, mirroring the decoder.
///
/// Panics on a `Literal` whose payload is empty or larger than
/// `MAX_DELTA_LITERAL_LEN` — both are caller programming errors
/// (rsync never emits them; the decoder rejects them).
pub fn encode_delta_op(op: &DeltaOp, state: &mut DeltaStreamState) -> Vec<u8> {
    match op {
        DeltaOp::Literal { compressed_payload } => {
            let len = compressed_payload.len();
            assert!(
                (1..=MAX_DELTA_LITERAL_LEN).contains(&len),
                "Literal payload length {} outside valid 1..={} range",
                len,
                MAX_DELTA_LITERAL_LEN
            );
            let hi6 = ((len >> 8) & 0x3F) as u8;
            let lo8 = (len & 0xFF) as u8;
            let mut out = Vec::with_capacity(2 + len);
            out.push(TOKEN_DEFLATED_DATA | hi6);
            out.push(lo8);
            out.extend_from_slice(compressed_payload);
            out
        }
        DeltaOp::CopyRun {
            start_token_index,
            run_length,
        } => {
            let rel = start_token_index.wrapping_sub(state.last_run_end);
            let use_rel = (0..=63).contains(&rel);
            let new_end = start_token_index.wrapping_add(i32::from(*run_length));
            state.last_run_end = new_end;
            if use_rel && *run_length == 1 {
                // TOKEN_REL: single byte 0x80 | rel6
                vec![TOKEN_REL | (rel as u8)]
            } else if use_rel {
                // TOKENRUN_REL: 0xC0 | rel6 + 2-byte run LE
                let mut out = Vec::with_capacity(3);
                out.push(TOKENRUN_REL | (rel as u8));
                out.extend_from_slice(&run_length.to_le_bytes());
                out
            } else if *run_length == 1 {
                // TOKEN_LONG: 0x20 + 4-byte int32 LE
                let mut out = Vec::with_capacity(5);
                out.push(TOKEN_LONG);
                out.extend_from_slice(&start_token_index.to_le_bytes());
                out
            } else {
                // TOKENRUN_LONG: 0x21 + 4-byte int32 LE + 2-byte run LE
                let mut out = Vec::with_capacity(7);
                out.push(TOKENRUN_LONG);
                out.extend_from_slice(&start_token_index.to_le_bytes());
                out.extend_from_slice(&run_length.to_le_bytes());
                out
            }
        }
    }
}

/// Encode a complete delta stream: every op via `encode_delta_op`,
/// then the `END_FLAG` sentinel, then the raw `file_checksum` bytes.
/// Symmetric to `decode_delta_stream`.
pub fn encode_delta_stream(report: &DeltaStreamReport) -> Vec<u8> {
    let mut state = DeltaStreamState::new();
    let mut out = Vec::with_capacity(report.ops.len() * 4 + 1 + report.file_checksum.len());
    for op in &report.ops {
        out.extend_from_slice(&encode_delta_op(op, &mut state));
    }
    out.push(TOKEN_END_FLAG);
    out.extend_from_slice(&report.file_checksum);
    out
}

/// One-shot decoder: iterate delta tokens until `END_FLAG`, then read the
/// trailing file checksum of length `checksum_len`. Validates COPY
/// targets against `sum_head.count` if provided; pass `None` to skip the
/// range check (useful when driving the decoder against hand-assembled
/// fixtures where no sum_head precedes the stream).
pub fn decode_delta_stream(
    buf: &[u8],
    checksum_len: usize,
    sum_head_count: Option<i32>,
) -> Result<(DeltaStreamReport, usize), RealWireError> {
    let mut state = DeltaStreamState::new();
    let mut cursor = 0usize;
    let mut ops: Vec<DeltaOp> = Vec::new();

    loop {
        let (outcome, consumed) = decode_delta_op(&buf[cursor..], &mut state)?;
        cursor += consumed;
        match outcome {
            DeltaOpOutcome::EndFlag => break,
            DeltaOpOutcome::Op(op) => {
                if let (
                    Some(count),
                    DeltaOp::CopyRun {
                        start_token_index,
                        run_length,
                    },
                ) = (sum_head_count, &op)
                {
                    let end = start_token_index
                        .checked_add(i32::from(*run_length))
                        .ok_or(RealWireError::DeltaTokenOutOfRange {
                            token_index: *start_token_index,
                            block_count: count,
                        })?;
                    if *start_token_index < 0 || end > count {
                        return Err(RealWireError::DeltaTokenOutOfRange {
                            token_index: *start_token_index,
                            block_count: count,
                        });
                    }
                }
                ops.push(op);
            }
        }
    }

    let (file_checksum, csum_consumed) = decode_file_checksum(&buf[cursor..], checksum_len)?;
    cursor += csum_consumed;

    Ok((DeltaStreamReport { ops, file_checksum }, cursor))
}

/// Decompress every `DeltaOp::Literal.compressed_payload` of a session
/// in wire order and return the concatenated raw bytes.
///
/// Rsync's `send_zstd_token` (`token.c` lines 678-770 in rsync 3.2.7)
/// calls `ZSTD_compressStream2` with `ZSTD_e_continue` or
/// `ZSTD_e_flush` but NEVER `ZSTD_e_end`. Each `DEFLATED_DATA` record
/// is therefore a **flush block** inside a single streaming zstd
/// context shared across the whole session (per file, per direction).
/// The receiver (`recv_zstd_token`, token.c:780+) uses a matching
/// `zstd_dctx` preserved across calls.
///
/// Implication for this decoder: calling a single-frame API like
/// `zstd::stream::decode_all` on ONE literal fails with
/// "incomplete frame" — the frame epilogue never ships. Callers MUST
/// feed the payloads in emission order through one streaming
/// decoder; this helper does that by concatenating the compressed
/// chunks and handing the result to `zstd::stream::Decoder`, which
/// processes back-to-back blocks transparently.
///
/// Available only when the `proto_native_rsync` feature is enabled
/// (the `zstd` crate is an optional dependency gated behind it).
pub fn decompress_zstd_literal_stream(payloads: &[&[u8]]) -> Result<Vec<u8>, RealWireError> {
    // A session-wide zstd context mirrors `recv_zstd_token`'s
    // `zstd_dctx` (token.c:778+ — a single static DCtx across all
    // DEFLATED_DATA records of the session). Feeding one payload at a
    // time through `decompress_stream` matches the receiver's own
    // ZSTD_decompressStream loop — and crucially does NOT require the
    // frame to be terminated, since the sender's encoder never emits
    // the frame epilogue (`send_zstd_token` uses ZSTD_e_continue or
    // ZSTD_e_flush, never ZSTD_e_end).
    let total: usize = payloads.iter().map(|p| p.len()).sum();
    if total == 0 {
        return Ok(Vec::new());
    }

    use zstd::zstd_safe::{DCtx, InBuffer, OutBuffer};

    let mut ctx = DCtx::create();
    let mut out: Vec<u8> = Vec::new();
    // Pre-allocate an output staging buffer sized to one zstd stream
    // block. Each call to `decompress_stream` writes up to this many
    // bytes; we copy the produced bytes into `out` after each call.
    let staging_capacity = DCtx::out_size().max(1 << 14);
    let mut staging: Vec<u8> = vec![0u8; staging_capacity];

    for payload in payloads {
        if payload.is_empty() {
            continue;
        }
        let mut input = InBuffer::around(payload);
        // Loop until the current payload is fully consumed — each
        // `decompress_stream` call may produce 0 to `staging_capacity`
        // output bytes depending on how much of the frame is ready.
        while input.pos < payload.len() {
            let mut output = OutBuffer::around(&mut staging[..]);
            ctx.decompress_stream(&mut output, &mut input)
                .map_err(|code| {
                    let reason = zstd::zstd_safe::get_error_name(code).to_string();
                    RealWireError::ZstdDecompressionFailed { reason }
                })?;
            out.extend_from_slice(output.as_slice());
        }
    }

    Ok(out)
}

/// Decompress a sequence of compressed literal payloads in wire order
/// and return one `Vec<u8>` **per input payload** (preserving the
/// boundaries between them). Uses the same session-wide `DCtx` as
/// `decompress_zstd_literal_stream`, so the decompressed bytes are
/// identical; the only difference is the output shape.
///
/// Intended use: the native rsync driver's download path needs to know
/// which decompressed bytes belong to which `DeltaOp::Literal` in order
/// to interleave them with `CopyRun` ops when building the
/// `EngineDeltaOp` stream for the delta engine. The flat-concatenated
/// shape of `decompress_zstd_literal_stream` loses that mapping.
///
/// Empty input payloads produce empty `Vec<u8>` outputs (retaining the
/// 1:1 input/output mapping for tidy indexing).
pub fn decompress_zstd_literal_stream_boundaries(
    payloads: &[&[u8]],
) -> Result<Vec<Vec<u8>>, RealWireError> {
    use zstd::zstd_safe::{DCtx, InBuffer, OutBuffer};

    let mut ctx = DCtx::create();
    let staging_capacity = DCtx::out_size().max(1 << 14);
    let mut staging: Vec<u8> = vec![0u8; staging_capacity];
    let mut results: Vec<Vec<u8>> = Vec::with_capacity(payloads.len());

    for payload in payloads {
        let mut this_out: Vec<u8> = Vec::new();
        if !payload.is_empty() {
            let mut input = InBuffer::around(payload);
            while input.pos < payload.len() {
                let mut output = OutBuffer::around(&mut staging[..]);
                ctx.decompress_stream(&mut output, &mut input)
                    .map_err(|code| {
                        let reason = zstd::zstd_safe::get_error_name(code).to_string();
                        RealWireError::ZstdDecompressionFailed { reason }
                    })?;
                this_out.extend_from_slice(output.as_slice());
            }
        }
        results.push(this_out);
    }

    Ok(results)
}

// ----------------------------------------------------------------------------
// Sinergia 8i-encode — Fase 4: zstd literal stream compressor (mirror of
// `decompress_zstd_literal_stream`).
//
// Produces one compressed `Vec<u8>` per input payload, suitable for
// embedding inside a `DeltaOp::Literal` DEFLATED_DATA record. Mirrors
// `send_zstd_token` (`token.c:678-770` in rsync 3.2.7):
//
//   - one session-wide `ZSTD_CCtx` shared across every payload,
//   - `compress_stream2` called with `EndDirective::Continue` while the
//     payload still has unconsumed bytes,
//   - one final `compress_stream2` call with `EndDirective::Flush` per
//     payload boundary (the `flush = ZSTD_e_flush` branch in
//     token.c:741),
//   - **NEVER** `EndDirective::End` — the frame epilogue is never
//     written, the receiver's `recv_zstd_token` does not expect it.
//
// Round-trip pinned: feeding the output of this function into
// `decompress_zstd_literal_stream` MUST yield the original
// concatenated input bytes.
// ----------------------------------------------------------------------------

/// Compress a sequence of literal payloads through a single
/// session-wide `ZSTD_CCtx`. Returns one compressed `Vec<u8>` per
/// non-empty input payload, in encounter order.
///
/// Empty input payloads are skipped silently — they yield no
/// DEFLATED_DATA record (consistent with `send_zstd_token` not
/// emitting an empty token, see token.c:691 `if (nb)` guard).
///
/// Available only when `proto_native_rsync` is enabled.
pub fn compress_zstd_literal_stream(payloads: &[&[u8]]) -> Result<Vec<Vec<u8>>, RealWireError> {
    use zstd::zstd_safe::zstd_sys::ZSTD_EndDirective;
    use zstd::zstd_safe::{CCtx, CParameter, InBuffer, OutBuffer};

    let mut ctx = CCtx::create();
    // Match rsync's negotiated default level. `send_zstd_token` honours
    // `--compress-level` via `ZSTD_c_compressionLevel` set in `setup_zstd`
    // (token.c:608+). Default 3 is the rsync default for `--zstd` without
    // an explicit level. Round-trip semantics are insensitive to the
    // exact level — any level decodes via the same DCtx loop.
    ctx.set_parameter(CParameter::CompressionLevel(3))
        .map_err(|code| RealWireError::ZstdDecompressionFailed {
            reason: format!(
                "set CompressionLevel: {}",
                zstd::zstd_safe::get_error_name(code)
            ),
        })?;

    let staging_capacity = CCtx::out_size().max(1 << 14);
    let mut staging: Vec<u8> = vec![0u8; staging_capacity];

    let mut blobs: Vec<Vec<u8>> = Vec::with_capacity(payloads.len());
    for payload in payloads {
        if payload.is_empty() {
            continue;
        }
        let mut blob: Vec<u8> = Vec::new();
        let mut input = InBuffer::around(payload);

        // Continue mode: feed the payload until the encoder has read
        // every byte. May or may not produce output bytes during this
        // pass (zstd buffers internally until block boundaries).
        while input.pos < payload.len() {
            let mut output = OutBuffer::around(&mut staging[..]);
            ctx.compress_stream2(&mut output, &mut input, ZSTD_EndDirective::ZSTD_e_continue)
                .map_err(|code| RealWireError::ZstdDecompressionFailed {
                    reason: format!(
                        "compress_stream2 Continue: {}",
                        zstd::zstd_safe::get_error_name(code)
                    ),
                })?;
            blob.extend_from_slice(output.as_slice());
        }

        // Flush mode: drain the encoder so this payload's bytes land in
        // a DEFLATED_DATA-shippable block. Loop until `compress_stream2`
        // returns 0 (no more buffered bytes pending). MUST NOT use
        // `EndDirective::End` — the receiver's `recv_zstd_token` does
        // not expect a frame epilogue.
        let empty: &[u8] = &[];
        loop {
            let mut empty_in = InBuffer::around(empty);
            let mut output = OutBuffer::around(&mut staging[..]);
            let remaining = ctx
                .compress_stream2(&mut output, &mut empty_in, ZSTD_EndDirective::ZSTD_e_flush)
                .map_err(|code| RealWireError::ZstdDecompressionFailed {
                    reason: format!(
                        "compress_stream2 Flush: {}",
                        zstd::zstd_safe::get_error_name(code)
                    ),
                })?;
            blob.extend_from_slice(output.as_slice());
            if remaining == 0 {
                break;
            }
        }

        blobs.push(blob);
    }

    Ok(blobs)
}

// =============================================================================
// Sezione 8 — End-of-session summary frame (S8g)
// =============================================================================
//
// Rsync 3.2.7 `main.c::handle_stats` (lines 323-386) is the centralised
// point where per-session totals are written to / read from the wire. Of
// the 5 call sites in `main.c` the one that reaches the wire in a
// client<->server transfer is `handle_stats(f_out)` at line 960 — invoked
// by `do_server_sender` right after the final `io_flush(FULL_FLUSH)` of
// `send_files`. Lookups on the other side: `handle_stats(f_in)` at line
// 1056 in `do_recv` reads the same frame on the client receiver.
//
// Critical side-effect: `handle_stats` is ONLY emitted by the
// server-sender path (see line 340 `if (am_server) if (am_sender) {...}`).
// Server-receiver returns silently. Client-sender writes to `batch_fd` if
// `--write-batch` is active (line 374-381) but never to the wire socket.
// Therefore the summary frame appears EXCLUSIVELY on the
// server->client direction of a `download` (server acts as sender).
// Neither direction of an upload carries it.
//
// Protocol gates (`main.c:346-353` + `io.h:46` inline):
//   - proto < 29  → 3 fields only (total_read, total_written, total_size)
//   - proto = 29  → 5 fields, all encoded as `write_longint`
//   - proto >= 30 → 5 fields, all encoded as `write_varlong(_, x, 3)`
//
// Field order on the wire is always: total_read, total_written,
// total_size, flist_buildtime, flist_xfertime. The reader side in
// `main.c::handle_stats` (line 364-370) swaps the first two in memory
// because "read/write meaning swaps when switching from sender to
// receiver" — this is an in-memory semantic swap by the CLIENT, not a
// wire reordering. On the wire the sender-side order is canonical.
//
// Session-level framing around the summary (proto >= 31, per
// `main.c::read_final_goodbye` line 883-895):
//   1. `send_files` emits its final `NDX_DONE` marking end-of-transfer.
//   2. `handle_stats` emits the summary varlong/longint sequence.
//   3. The client reads the summary, then sends `NDX_DONE` back.
//   4. The server reads the client's NDX_DONE, then writes one more
//      `NDX_DONE` via `write_ndx(f_out, NDX_DONE)` (line 887).
//   5. The client reads the trailing NDX_DONE and exits.
// Decoding the whole tail of a download-server-to-client app stream
// therefore looks like: `...delta...file_csum...NDX_DONE...summary...NDX_DONE`.
//
// Wire formats used:
//   - `write_longint` (`io.c:1867` + SIVAL): 4 bytes LE if
//     `0 <= x <= 0x7FFFFFFF`; otherwise 12 bytes = FF FF FF FF sentinel,
//     then int64 split into low32/high32 LE. Mirrored by `read_longint`
//     (`io.c:1867` readback).
//   - `write_varlong(_, x, 3)`: already decoded by `decode_varlong` from
//     S8d. Minimum 3 bytes, maximum 9 bytes, first byte's leading-1s
//     signal extra-byte count.
//
// The frozen oracle profile is proto 31 (negotiated downwards from the
// server's proto 32), so the on-wire format is 5 × varlong with
// min_bytes=3. The `proto<30` branches are implemented for symmetry and
// round-trip unit tests, but have no live fixture yet.

/// Minimum bytes floor used by `write_varlong30(_, _, 3)` when serialising
/// stats fields in proto >= 30. See `main.c::handle_stats` and
/// `io.h::write_varlong30`.
pub const SUMMARY_VARLONG_MIN_BYTES: u8 = 3;

/// Protocol version at which `handle_stats` started emitting the extra
/// `flist_buildtime` + `flist_xfertime` fields (`main.c:350`).
pub const PROTOCOL_SUMMARY_ADDS_FLIST_TIMES: u32 = 29;

/// Protocol version at which the 5 summary fields switched from
/// `write_longint` to `write_varlong(_, _, 3)` (`io.h:46`).
pub const PROTOCOL_SUMMARY_SWITCHES_TO_VARLONG: u32 = 30;

/// Decoded end-of-session statistics. Missing optional fields are
/// `None` only when `protocol_version < 29` — for proto >= 29 both
/// `flist_*` fields are always written on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryFrame {
    /// Cumulative bytes read by the sender (`stats.total_read` at end
    /// of session).
    pub total_read: i64,
    /// Cumulative bytes written by the sender.
    pub total_written: i64,
    /// Sum of the logical size of all files in the transfer
    /// (`stats.total_size`). For a single-file transfer this equals
    /// the plain file size in bytes.
    pub total_size: i64,
    /// Time spent building the file list, in seconds. `Some` for
    /// proto >= 29, `None` otherwise.
    pub flist_buildtime: Option<i64>,
    /// Time spent transferring the file list, in seconds. `Some` for
    /// proto >= 29, `None` otherwise.
    pub flist_xfertime: Option<i64>,
}

/// Decode the summary frame that follows `send_files`' final NDX_DONE on
/// the server-to-client direction of a download. Returns the parsed
/// frame plus the number of bytes consumed from `buf`.
///
/// Protocol 30+ uses 5 × `write_varlong(_, _, 3)`. Protocol 29 uses 5
/// × `write_longint`. Protocol 28 and older use 3 × `write_longint`.
///
/// This function only parses the summary itself — the surrounding
/// NDX_DONE markers (`main.c::read_final_goodbye`) belong to the caller.
pub fn decode_summary_frame(
    buf: &[u8],
    protocol_version: u32,
) -> Result<(SummaryFrame, usize), RealWireError> {
    let with_flist_times = protocol_version >= PROTOCOL_SUMMARY_ADDS_FLIST_TIMES;
    let use_varlong = protocol_version >= PROTOCOL_SUMMARY_SWITCHES_TO_VARLONG;

    let mut cursor = 0usize;
    let mut read_field = |field: &'static str| -> Result<i64, RealWireError> {
        let slice = &buf[cursor..];
        let (value, consumed) = if use_varlong {
            decode_varlong(slice, SUMMARY_VARLONG_MIN_BYTES).map_err(|e| {
                // Retag varlong's generic TruncatedBuffer with a
                // field-specific `at` so error messages can pinpoint
                // which summary field tripped.
                match e {
                    RealWireError::TruncatedBuffer {
                        needed, available, ..
                    } => RealWireError::TruncatedBuffer {
                        at: field,
                        needed,
                        available,
                    },
                    other => other,
                }
            })?
        } else {
            decode_longint(slice, field)?
        };
        cursor += consumed;
        Ok(value)
    };

    let total_read = read_field("summary_total_read")?;
    let total_written = read_field("summary_total_written")?;
    let total_size = read_field("summary_total_size")?;
    let (flist_buildtime, flist_xfertime) = if with_flist_times {
        (
            Some(read_field("summary_flist_buildtime")?),
            Some(read_field("summary_flist_xfertime")?),
        )
    } else {
        (None, None)
    };

    Ok((
        SummaryFrame {
            total_read,
            total_written,
            total_size,
            flist_buildtime,
            flist_xfertime,
        },
        cursor,
    ))
}

/// Encode a `SummaryFrame` for the given protocol version. Mirrors
/// `main.c::handle_stats` write path. Used only for round-trip unit
/// tests — `real_wire.rs` is decode-oriented.
pub fn encode_summary_frame(frame: &SummaryFrame, protocol_version: u32) -> Vec<u8> {
    let with_flist_times = protocol_version >= PROTOCOL_SUMMARY_ADDS_FLIST_TIMES;
    let use_varlong = protocol_version >= PROTOCOL_SUMMARY_SWITCHES_TO_VARLONG;

    let mut out: Vec<u8> = Vec::new();
    let mut push = |value: i64| {
        if use_varlong {
            out.extend_from_slice(&encode_varlong(value, SUMMARY_VARLONG_MIN_BYTES));
        } else {
            out.extend_from_slice(&encode_longint(value));
        }
    };

    push(frame.total_read);
    push(frame.total_written);
    push(frame.total_size);
    if with_flist_times {
        // Mirror rsync: for proto >= 29 both fields are always written.
        // We serialise 0 when the caller left them `None` (shouldn't
        // happen for valid inputs, but avoids panics on malformed
        // frames).
        push(frame.flist_buildtime.unwrap_or(0));
        push(frame.flist_xfertime.unwrap_or(0));
    }
    out
}

/// Decode a `write_longint` field. Mirrors `io.c::read_longint`:
/// reads a 4-byte little-endian signed int; if the value is
/// `-1` (0xFFFFFFFF) it reads 8 more bytes and returns their combined
/// `low | (high << 32)` value. Otherwise the initial int32 widened to
/// `i64` is returned.
///
/// The `field` tag is threaded through so a truncated-buffer error
/// can pinpoint which summary slot tripped.
fn decode_longint(buf: &[u8], field: &'static str) -> Result<(i64, usize), RealWireError> {
    if buf.len() < 4 {
        return Err(RealWireError::TruncatedBuffer {
            at: field,
            needed: 4,
            available: buf.len(),
        });
    }
    let low = i32::from_le_bytes(buf[0..4].try_into().unwrap());
    if low != -1 {
        return Ok((i64::from(low), 4));
    }
    if buf.len() < 12 {
        return Err(RealWireError::TruncatedBuffer {
            at: field,
            needed: 12,
            available: buf.len(),
        });
    }
    let lo = u32::from_le_bytes(buf[4..8].try_into().unwrap());
    let hi = u32::from_le_bytes(buf[8..12].try_into().unwrap());
    let combined = (u64::from(lo)) | (u64::from(hi) << 32);
    Ok((combined as i64, 12))
}

/// Encode an `i64` as `write_longint`. 4-byte LE if the value fits in
/// `0..=0x7FFFFFFF`, otherwise 12 bytes = `FF FF FF FF` sentinel then
/// low32 LE then high32 LE. Used only for round-trip unit tests.
fn encode_longint(value: i64) -> Vec<u8> {
    if (0..=0x7FFFFFFF).contains(&value) {
        let as_i32 = value as i32;
        return as_i32.to_le_bytes().to_vec();
    }
    let mut out = Vec::with_capacity(12);
    out.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
    let bits = value as u64;
    out.extend_from_slice(&(bits as u32).to_le_bytes());
    out.extend_from_slice(&((bits >> 32) as u32).to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------------
    // Protocol version round-trips
    // -------------------------------------------------------------------------

    #[test]
    fn protocol_version_round_trip_31() {
        let bytes = encode_protocol_version(31);
        assert_eq!(bytes, [0x1F, 0, 0, 0]);
        let (v, n) = decode_protocol_version(&bytes).unwrap();
        assert_eq!((v, n), (31, 4));
    }

    #[test]
    fn protocol_version_round_trip_32() {
        let bytes = encode_protocol_version(32);
        assert_eq!(bytes, [0x20, 0, 0, 0]);
        let (v, n) = decode_protocol_version(&bytes).unwrap();
        assert_eq!((v, n), (32, 4));
    }

    #[test]
    fn protocol_version_rejects_out_of_range() {
        let bytes = encode_protocol_version(999);
        let err = decode_protocol_version(&bytes).unwrap_err();
        assert!(matches!(err, RealWireError::InvalidProtocolVersion { .. }));
    }

    #[test]
    fn protocol_version_rejects_truncated() {
        let err = decode_protocol_version(&[0x1F, 0]).unwrap_err();
        assert!(matches!(
            err,
            RealWireError::TruncatedBuffer {
                at: "protocol_version",
                ..
            }
        ));
    }

    // -------------------------------------------------------------------------
    // Multiplex header
    // -------------------------------------------------------------------------

    #[test]
    fn mux_header_msg_data_round_trip() {
        let header = MuxHeader {
            tag: MuxTag::Data,
            length: 2269,
        };
        let wire = header.encode();
        // Shape: tag_raw = MPLEX_BASE + 0 = 7; length = 0x0008DD; LE u32 = 0xDD 0x08 0x00 0x07.
        assert_eq!(wire, [0xDD, 0x08, 0x00, 0x07]);
        let decoded = MuxHeader::decode(wire).unwrap();
        assert_eq!(decoded, header);
    }

    #[test]
    fn mux_header_other_tags_round_trip() {
        for tag in [MuxTag::Info, MuxTag::Warning, MuxTag::Error, MuxTag::Stats] {
            let header = MuxHeader { tag, length: 17 };
            let wire = header.encode();
            let decoded = MuxHeader::decode(wire).unwrap();
            assert_eq!(decoded, header);
        }
    }

    #[test]
    fn mux_header_unknown_tag_preserves_raw_code() {
        let header = MuxHeader {
            tag: MuxTag::Unknown(123),
            length: 5,
        };
        let wire = header.encode();
        let decoded = MuxHeader::decode(wire).unwrap();
        match decoded.tag {
            MuxTag::Unknown(raw) => assert_eq!(raw, 123),
            other => panic!("expected Unknown(123), got {:?}", other),
        }
    }

    #[test]
    fn mux_header_rejects_high_byte_below_base() {
        // high byte 0 = classic "no mux yet" signal (e.g. reading into
        // the capability preamble). Detected as InvalidMuxHeader.
        let err = MuxHeader::decode([0x00, 0x00, 0x00, 0x00]).unwrap_err();
        assert!(matches!(
            err,
            RealWireError::InvalidMuxHeader { raw_high_byte: 0 }
        ));
    }

    // -------------------------------------------------------------------------
    // Client preamble
    // -------------------------------------------------------------------------

    #[test]
    fn client_preamble_parses_observed_31_profile() {
        // Shape: version(4) + 0x1E + "xxh128 xxh3 xxh64 md5 md4 sha1" (30) +
        //        0x13 + "zstd lz4 zlibx zlib" (19) = 55 bytes.
        let mut buf = Vec::new();
        buf.extend_from_slice(&encode_protocol_version(31));
        buf.push(30);
        buf.extend_from_slice(b"xxh128 xxh3 xxh64 md5 md4 sha1");
        buf.push(19);
        buf.extend_from_slice(b"zstd lz4 zlibx zlib");
        assert_eq!(buf.len(), 55);

        let preamble = decode_client_preamble(&buf).unwrap();
        assert_eq!(preamble.protocol_version, 31);
        assert_eq!(preamble.checksum_algos, "xxh128 xxh3 xxh64 md5 md4 sha1");
        assert_eq!(preamble.compression_algos, "zstd lz4 zlibx zlib");
        assert_eq!(preamble.consumed, 55);
    }

    // -------------------------------------------------------------------------
    // Server preamble
    // -------------------------------------------------------------------------

    #[test]
    fn server_preamble_parses_observed_32_profile() {
        // compat_flags on the wire is a rsync varint, not a fixed 2-byte
        // block. In the frozen transcript it decodes as 0x01FF (all nine
        // `CF_*` bits set), and happens to encode as a 2-byte varint.
        let mut buf = Vec::new();
        buf.extend_from_slice(&encode_protocol_version(32));
        buf.extend_from_slice(&[0x81, 0xFF]); // varint(0x01FF) observed
        buf.push(35);
        buf.extend_from_slice(b"xxh128 xxh3 xxh64 md5 md4 sha1 none");
        buf.push(24);
        buf.extend_from_slice(b"zstd lz4 zlibx zlib none");
        buf.extend_from_slice(&0x69E2_7A9Au32.to_le_bytes()); // seed from frozen capture
        assert_eq!(buf.len(), 71);

        let preamble = decode_server_preamble(&buf).unwrap();
        assert_eq!(preamble.protocol_version, 32);
        assert_eq!(preamble.compat_flags, 0x01FF);
        assert_eq!(
            preamble.checksum_algos,
            "xxh128 xxh3 xxh64 md5 md4 sha1 none"
        );
        assert_eq!(preamble.compression_algos, "zstd lz4 zlibx zlib none");
        assert_eq!(preamble.checksum_seed, 0x69E2_7A9A);
        assert_eq!(preamble.consumed, 71);
    }

    #[test]
    fn server_preamble_rejects_non_ascii_algo_byte() {
        // Length 3 with a NUL in the middle — should be surfaced with
        // offset so the caller can point at the offending byte. The
        // compat_flags varint is the single byte 0x00.
        let mut buf = Vec::new();
        buf.extend_from_slice(&encode_protocol_version(32));
        buf.push(0x00); // varint(0)
        buf.push(3);
        buf.extend_from_slice(&[b'a', 0x00, b'b']);
        // compression_algos section (len 0) + 4-byte seed — even though the
        // checksum_algos section will already error out, finish the frame
        // so decode_server_preamble doesn't trip on a truncation first.
        buf.push(0);
        buf.extend_from_slice(&[0u8; 4]);
        let err = decode_server_preamble(&buf).unwrap_err();
        assert!(
            matches!(err, RealWireError::NonAsciiAlgoList { .. }),
            "expected NonAsciiAlgoList, got {err:?}"
        );
    }

    #[test]
    fn server_preamble_reads_one_byte_varint_compat_flags() {
        // A simpler handshake where only CF_INC_RECURSE is set — varint
        // fits in a single byte (0x01), so the preamble is 70 bytes
        // instead of 71. Locks the S8d fix: S8b would have read the
        // first byte of the checksum_algos length as compat_flags' high
        // byte, corrupting the rest of the parse.
        let mut buf = Vec::new();
        buf.extend_from_slice(&encode_protocol_version(32));
        buf.push(0x01); // varint(1) = CF_INC_RECURSE
        buf.push(5);
        buf.extend_from_slice(b"xxh64");
        buf.push(4);
        buf.extend_from_slice(b"zstd");
        buf.extend_from_slice(&0x1234_5678u32.to_le_bytes());
        assert_eq!(buf.len(), 4 + 1 + 1 + 5 + 1 + 4 + 4);

        let preamble = decode_server_preamble(&buf).unwrap();
        assert_eq!(preamble.compat_flags, 0x01);
        assert_eq!(preamble.checksum_algos, "xxh64");
        assert_eq!(preamble.compression_algos, "zstd");
        assert_eq!(preamble.checksum_seed, 0x1234_5678);
        assert_eq!(preamble.consumed, buf.len());
    }

    // -------------------------------------------------------------------------
    // Demuxer
    // -------------------------------------------------------------------------

    #[test]
    fn demuxer_reads_sequence_of_msg_data_frames() {
        // Simulated tail of upload/capture_out.bin:
        //   MSG_DATA len=1  -> 0x00
        //   MSG_DATA len=3  -> 0x00 0x00 0x00
        //   MSG_DATA len=1  -> 0x00
        let mut buf = Vec::new();
        for (len, payload_byte) in [(1u32, 0x00), (3u32, 0x00), (1u32, 0x00)] {
            buf.extend_from_slice(
                &MuxHeader {
                    tag: MuxTag::Data,
                    length: len,
                }
                .encode(),
            );
            for _ in 0..len {
                buf.push(payload_byte);
            }
        }
        let demuxer = MuxDemuxer::new(&buf);
        let frames: Vec<_> = demuxer.map(|r| r.unwrap()).collect();
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].0.length, 1);
        assert_eq!(frames[1].0.length, 3);
        assert_eq!(frames[2].0.length, 1);
        for (hdr, _) in &frames {
            assert_eq!(hdr.tag, MuxTag::Data);
        }
    }

    #[test]
    fn demuxer_flags_truncated_payload_on_final_frame() {
        let mut buf = Vec::new();
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::Data,
                length: 5,
            }
            .encode(),
        );
        buf.extend_from_slice(&[0xAA, 0xBB]); // only 2 of 5 payload bytes
        let mut demuxer = MuxDemuxer::new(&buf);
        let result = demuxer.next().unwrap();
        assert!(matches!(
            result,
            Err(RealWireError::TruncatedBuffer {
                at: "mux_payload",
                ..
            })
        ));
        // Subsequent next() returns None — iterator is exhausted once
        // an error is surfaced.
        assert!(demuxer.next().is_none());
    }

    #[test]
    fn demuxer_flags_truncated_header_when_less_than_4_bytes_remain() {
        let buf = [0x07, 0x00]; // 2 bytes, not enough for a header
        let mut demuxer = MuxDemuxer::new(&buf);
        let result = demuxer.next().unwrap();
        assert!(matches!(
            result,
            Err(RealWireError::TruncatedBuffer {
                at: "mux_header",
                ..
            })
        ));
    }

    // -------------------------------------------------------------------------
    // Reassembly
    // -------------------------------------------------------------------------

    #[test]
    fn reassembly_concatenates_two_msg_data_payloads() {
        let mut buf = Vec::new();
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::Data,
                length: 3,
            }
            .encode(),
        );
        buf.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::Data,
                length: 2,
            }
            .encode(),
        );
        buf.extend_from_slice(&[0xDD, 0xEE]);

        let report = reassemble_msg_data(&buf).unwrap();
        assert_eq!(report.app_stream, vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE]);
        assert!(report.out_of_band.is_empty());
        assert_eq!(report.frames_consumed, 2);
    }

    #[test]
    fn reassembly_filters_out_of_band_frames_but_records_them() {
        let mut buf = Vec::new();
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::Data,
                length: 2,
            }
            .encode(),
        );
        buf.extend_from_slice(&[0x01, 0x02]);
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::Info,
                length: 4,
            }
            .encode(),
        );
        buf.extend_from_slice(b"info");
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::Data,
                length: 1,
            }
            .encode(),
        );
        buf.extend_from_slice(&[0x03]);
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::Warning,
                length: 3,
            }
            .encode(),
        );
        buf.extend_from_slice(b"WRN");

        let report = reassemble_msg_data(&buf).unwrap();
        assert_eq!(report.app_stream, vec![0x01, 0x02, 0x03]);
        assert_eq!(
            report.out_of_band,
            vec![(MuxTag::Info, 4), (MuxTag::Warning, 3)]
        );
        assert_eq!(report.frames_consumed, 4);
    }

    #[test]
    fn reassembly_propagates_demuxer_errors() {
        let mut buf = Vec::new();
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::Data,
                length: 5,
            }
            .encode(),
        );
        buf.extend_from_slice(&[0xAA, 0xBB]); // short — only 2 of 5 bytes
        let err = reassemble_msg_data(&buf).unwrap_err();
        assert!(matches!(
            err,
            RealWireError::TruncatedBuffer {
                at: "mux_payload",
                ..
            }
        ));
    }

    #[test]
    fn reassembly_of_empty_buffer_is_empty() {
        let report = reassemble_msg_data(&[]).unwrap();
        assert!(report.app_stream.is_empty());
        assert!(report.out_of_band.is_empty());
        assert!(report.oob_frames.is_empty());
        assert_eq!(report.frames_consumed, 0);
    }

    #[test]
    fn reassembly_oob_frames_carry_full_payload_alongside_legacy_lengths() {
        // S8h regression — `oob_frames` MUST carry the same (tag, payload)
        // sequence that `out_of_band` describes by length only. A drift
        // between the two would silently break event classification.
        let mut buf = Vec::new();
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::Data,
                length: 2,
            }
            .encode(),
        );
        buf.extend_from_slice(&[0xFE, 0xED]);
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::Info,
                length: 5,
            }
            .encode(),
        );
        buf.extend_from_slice(b"hello");
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::Warning,
                length: 3,
            }
            .encode(),
        );
        buf.extend_from_slice(b"WRN");

        let report = reassemble_msg_data(&buf).unwrap();
        assert_eq!(report.app_stream, vec![0xFE, 0xED]);
        assert_eq!(
            report.out_of_band,
            vec![(MuxTag::Info, 5), (MuxTag::Warning, 3)]
        );
        assert_eq!(
            report.oob_frames,
            vec![
                (MuxTag::Info, b"hello".to_vec()),
                (MuxTag::Warning, b"WRN".to_vec()),
            ]
        );
        assert_eq!(report.out_of_band.len(), report.oob_frames.len());
        assert_eq!(report.frames_consumed, 3);
    }

    // -------------------------------------------------------------------------
    // Sinergia 8h — Classified reassembly with OOB events.
    // -------------------------------------------------------------------------

    #[test]
    fn reassemble_with_events_app_stream_matches_legacy() {
        // Invariant: the app_stream produced by `reassemble_with_events`
        // equals the one produced by `reassemble_msg_data` on the same
        // buffer. Classification is purely additive.
        let mut buf = Vec::new();
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::Data,
                length: 2,
            }
            .encode(),
        );
        buf.extend_from_slice(&[0x01, 0x02]);
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::Info,
                length: 4,
            }
            .encode(),
        );
        buf.extend_from_slice(b"info");
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::Data,
                length: 1,
            }
            .encode(),
        );
        buf.extend_from_slice(&[0x03]);

        let legacy = reassemble_msg_data(&buf).unwrap();
        let new = reassemble_with_events(&buf).unwrap();
        assert_eq!(legacy.app_stream, new.app_stream);
        assert_eq!(legacy.frames_consumed, new.frames_consumed);
        assert_eq!(new.events.len(), 1);
        assert!(new.terminal.is_none());
        assert_eq!(new.consumed_bytes, buf.len());
    }

    #[test]
    fn reassemble_with_events_collects_every_oob_in_order() {
        let mut buf = Vec::new();
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::Info,
                length: 3,
            }
            .encode(),
        );
        buf.extend_from_slice(b"one");
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::Warning,
                length: 3,
            }
            .encode(),
        );
        buf.extend_from_slice(b"two");
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::Data,
                length: 4,
            }
            .encode(),
        );
        buf.extend_from_slice(b"DATA");

        let report = reassemble_with_events(&buf).unwrap();
        assert_eq!(report.app_stream, b"DATA");
        assert_eq!(report.events.len(), 2);
        assert!(report.terminal.is_none());
        // Order preserved: Info before Warning.
        assert!(matches!(
            &report.events[0],
            crate::rsync_native_proto::events::NativeRsyncEvent::Info { message } if message == "one"
        ));
        assert!(matches!(
            &report.events[1],
            crate::rsync_native_proto::events::NativeRsyncEvent::Warning { message } if message == "two"
        ));
    }

    #[test]
    fn reassemble_until_terminal_does_not_consume_data_after_error() {
        // HARDENING — pin S8h's stop semantics. A `MSG_DATA` frame that
        // sits AFTER a terminal `MSG_ERROR` MUST NOT be appended to
        // `app_stream`. consumed_bytes stops at the end of the Error
        // frame's header + payload, not at end-of-buffer.
        let mut buf = Vec::new();
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::Data,
                length: 2,
            }
            .encode(),
        );
        buf.extend_from_slice(b"OK");
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::Warning,
                length: 2,
            }
            .encode(),
        );
        buf.extend_from_slice(b"hm");
        let error_header_offset = buf.len();
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::Error,
                length: 5,
            }
            .encode(),
        );
        buf.extend_from_slice(b"BOOM!");
        let consumed_at_terminal = buf.len();
        // These MUST NOT be touched.
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::Data,
                length: 6,
            }
            .encode(),
        );
        buf.extend_from_slice(b"AFTER!");

        let report = reassemble_until_terminal(&buf).unwrap();
        assert_eq!(report.app_stream, b"OK");
        assert_eq!(report.events.len(), 1);
        assert!(matches!(
            &report.events[0],
            crate::rsync_native_proto::events::NativeRsyncEvent::Warning { .. }
        ));
        assert!(matches!(
            report.terminal.as_ref().unwrap(),
            crate::rsync_native_proto::events::NativeRsyncEvent::Error { message } if message == "BOOM!"
        ));
        // 3 frames consumed: Data, Warning, Error (NOT the trailing Data).
        assert_eq!(report.frames_consumed, 3);
        assert_eq!(report.consumed_bytes, consumed_at_terminal);
        assert!(consumed_at_terminal > error_header_offset);
        assert!(consumed_at_terminal < buf.len());
    }

    #[test]
    fn reassemble_until_terminal_full_pass_when_no_terminal_present() {
        // No terminal frame -> terminal: None, consumed_bytes == buf.len(),
        // every event in `events` (warnings, info, etc.).
        let mut buf = Vec::new();
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::Data,
                length: 2,
            }
            .encode(),
        );
        buf.extend_from_slice(b"AB");
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::Warning,
                length: 1,
            }
            .encode(),
        );
        buf.extend_from_slice(b"!");
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::IoError,
                length: 4,
            }
            .encode(),
        );
        buf.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]);

        let report = reassemble_until_terminal(&buf).unwrap();
        assert_eq!(report.app_stream, b"AB");
        assert_eq!(report.events.len(), 2);
        assert!(report.terminal.is_none());
        assert_eq!(report.consumed_bytes, buf.len());
    }

    #[test]
    fn reassemble_until_terminal_bail_on_error_exit_with_nonzero_code() {
        // ErrorExit semantics: code > 0 is terminal. Pinned vs
        // events::is_terminal source-of-truth.
        let mut buf = Vec::new();
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::Data,
                length: 1,
            }
            .encode(),
        );
        buf.extend_from_slice(b"x");
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::ErrorExit,
                length: 4,
            }
            .encode(),
        );
        buf.extend_from_slice(&[0x17, 0x00, 0x00, 0x00]); // 23 = RERR_PARTIAL

        let report = reassemble_until_terminal(&buf).unwrap();
        assert!(matches!(
            report.terminal.as_ref().unwrap(),
            crate::rsync_native_proto::events::NativeRsyncEvent::ErrorExit { code: Some(23) }
        ));
    }

    #[test]
    fn reassemble_until_terminal_does_not_bail_on_error_exit_zero() {
        // ErrorExit with code 0 (cleanup signal) is NOT terminal. Pinned
        // by the same dual-payload rules in events::classify_oob_frame.
        let mut buf = Vec::new();
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::ErrorExit,
                length: 0,
            }
            .encode(),
        );
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::Data,
                length: 3,
            }
            .encode(),
        );
        buf.extend_from_slice(b"OK!");

        let report = reassemble_until_terminal(&buf).unwrap();
        assert!(report.terminal.is_none());
        assert_eq!(report.app_stream, b"OK!");
    }

    #[test]
    fn classified_reassembly_propagates_demux_errors_verbatim() {
        // A truncated header inside reassemble_with_events / _until_terminal
        // surfaces the same `RealWireError::TruncatedBuffer` the legacy
        // path would. No silent recovery, no partial classification.
        let mut buf = Vec::new();
        buf.extend_from_slice(
            &MuxHeader {
                tag: MuxTag::Data,
                length: 5,
            }
            .encode(),
        );
        buf.extend_from_slice(&[0xAA, 0xBB]); // short
        let err1 = reassemble_with_events(&buf).unwrap_err();
        let err2 = reassemble_until_terminal(&buf).unwrap_err();
        assert!(matches!(
            err1,
            RealWireError::TruncatedBuffer {
                at: "mux_payload",
                ..
            }
        ));
        assert!(matches!(
            err2,
            RealWireError::TruncatedBuffer {
                at: "mux_payload",
                ..
            }
        ));
    }

    // -------------------------------------------------------------------------
    // Varint / varlong primitives (S8d)
    //
    // Golden pairs derived from rsync 3.2.7 `io.c::write_varint`. Encoder
    // emits `b[0..cnt]` where cnt is the index of the topmost non-zero
    // byte in the LE layout (stored at b[1..5]) plus one extra when the
    // topmost byte would collide with the marker bits.
    // -------------------------------------------------------------------------

    #[test]
    fn varint_round_trip_golden_pairs() {
        let golden: &[(i32, &[u8])] = &[
            (0, &[0x00]),
            (1, &[0x01]),
            (127, &[0x7F]),
            (128, &[0x80, 0x80]),
            (255, &[0x80, 0xFF]),
            (256, &[0x81, 0x00]),
            (1024, &[0x84, 0x00]),
            (65_535, &[0xC0, 0xFF, 0xFF]),
            (65_536, &[0xC1, 0x00, 0x00]),
            (262_144, &[0xC4, 0x00, 0x00]),
            (i32::MAX, &[0xF0, 0xFF, 0xFF, 0xFF, 0x7F]),
        ];
        for &(value, bytes) in golden {
            let encoded = encode_varint(value);
            assert_eq!(encoded.as_slice(), bytes, "encode_varint({value}) mismatch");
            let (decoded, consumed) = decode_varint(bytes).unwrap();
            assert_eq!(decoded as i32, value, "decode_varint({bytes:?}) mismatch");
            assert_eq!(consumed, bytes.len(), "varint consumed {value}");
        }
    }

    #[test]
    fn varint_decodes_negative_i32_via_sign_extension() {
        let encoded = encode_varint(-1);
        // -1 LE is [0xFF; 4]; topmost byte is 0xFF >= 0x10 so cnt
        // extends to 5. First byte = ~(0x10 - 1) = 0xF0.
        assert_eq!(encoded, vec![0xF0, 0xFF, 0xFF, 0xFF, 0xFF]);
        let (decoded, _) = decode_varint(&encoded).unwrap();
        assert_eq!(decoded as i32, -1);
    }

    #[test]
    fn varint_empty_buffer_is_truncated() {
        let err = decode_varint(&[]).unwrap_err();
        assert!(matches!(
            err,
            RealWireError::TruncatedBuffer {
                at: "varint_first_byte",
                ..
            }
        ));
    }

    #[test]
    fn varint_truncated_payload() {
        // 0xC0 claims 2 extras but we only give 1.
        let err = decode_varint(&[0xC0, 0x01]).unwrap_err();
        assert!(matches!(
            err,
            RealWireError::TruncatedBuffer {
                at: "varint_payload",
                ..
            }
        ));
    }

    #[test]
    fn varint_overflow_rejected() {
        // 0xFE has extra=5 per INT_BYTE_EXTRA; int32 can carry at most 4.
        let err = decode_varint(&[0xFE, 0, 0, 0, 0, 0]).unwrap_err();
        assert!(matches!(err, RealWireError::VarintOverflow { .. }));
    }

    #[test]
    fn varlong_round_trip_size_min_bytes_3() {
        // Golden for file-size field: size = 262_144 with min_bytes = 3.
        // cnt lands at 3 (the topmost non-zero byte of the LE layout) and
        // is equal to min_bytes, so no marker bit is set.
        let encoded = encode_varlong(262_144, 3);
        assert_eq!(encoded, vec![0x04, 0x00, 0x00]);
        let (decoded, consumed) = decode_varlong(&encoded, 3).unwrap();
        assert_eq!(decoded, 262_144);
        assert_eq!(consumed, 3);
    }

    #[test]
    fn varlong_round_trip_mtime_min_bytes_4() {
        // A plausible mtime from the frozen oracle (bytes 75..79 of the
        // upload client-to-server stream, after reassembly).
        let mtime: i64 = 0x69E2_7F58;
        let encoded = encode_varlong(mtime, 4);
        // Single-byte-first-byte path: cnt == 4 == min_bytes. Result is
        // [topmost_byte, b[1], b[2], b[3]] with no marker.
        assert_eq!(encoded, vec![0x69, 0x58, 0x7F, 0xE2]);
        let (decoded, consumed) = decode_varlong(&encoded, 4).unwrap();
        assert_eq!(decoded, mtime);
        assert_eq!(consumed, 4);
    }

    #[test]
    fn varlong_extends_when_value_exceeds_min_bytes_floor() {
        // 2^40 + 1 requires 6 bytes total on wire with min_bytes=4.
        let value: i64 = (1i64 << 40) + 1;
        let encoded = encode_varlong(value, 4);
        let (decoded, consumed) = decode_varlong(&encoded, 4).unwrap();
        assert_eq!(decoded, value);
        assert_eq!(consumed, encoded.len());
        assert!(
            encoded.len() >= 5,
            "value 2^40 + 1 must take at least 5 bytes under min_bytes=4"
        );
    }

    #[test]
    fn varlong_truncated_min_bytes() {
        let err = decode_varlong(&[0x01, 0x02], 3).unwrap_err();
        assert!(matches!(
            err,
            RealWireError::TruncatedBuffer {
                at: "varlong_min_bytes",
                ..
            }
        ));
    }

    // -------------------------------------------------------------------------
    // File-list entry decoder (S8d)
    // -------------------------------------------------------------------------

    fn frozen_oracle_opts<'a>(previous_name: Option<&'a str>) -> FileListDecodeOptions<'a> {
        FileListDecodeOptions {
            previous_name,
            ..FileListDecodeOptions::frozen_oracle_default()
        }
    }

    #[test]
    fn decode_file_list_entry_happy_path_varint_flags() {
        // Hand-assembled entry matching the frozen oracle's layout:
        // flags varint carrying USER_NAME_FOLLOWS|GROUP_NAME_FOLLOWS|
        // MOD_NSEC, name "sample.bin" (10 bytes), size 262_144,
        // mtime 0x69E2_7F58, nsec 958_829_505, mode 0o100_664 (0x81B4),
        // uid 1000 + "axpnet", gid 1000 + "axpnet", 16-byte checksum.
        let mut buf: Vec<u8> = Vec::new();
        let flags = XMIT_USER_NAME_FOLLOWS | XMIT_GROUP_NAME_FOLLOWS | XMIT_MOD_NSEC;
        buf.extend_from_slice(&encode_varint(flags as i32));
        buf.push(10); // l2 (name length, classic byte because !XMIT_LONG_NAME)
        buf.extend_from_slice(b"sample.bin");
        buf.extend_from_slice(&encode_varlong(262_144, 3));
        buf.extend_from_slice(&encode_varlong(0x69E2_7F58, 4));
        buf.extend_from_slice(&encode_varint(958_829_505));
        buf.extend_from_slice(&0x81B4u32.to_le_bytes());
        buf.extend_from_slice(&encode_varint(1000));
        buf.push(6);
        buf.extend_from_slice(b"axpnet");
        buf.extend_from_slice(&encode_varint(1000));
        buf.push(6);
        buf.extend_from_slice(b"axpnet");
        buf.extend_from_slice(&[0xAA; 16]); // xxh128 checksum

        let opts = frozen_oracle_opts(None);
        let (outcome, consumed) = decode_file_list_entry(&buf, &opts).unwrap();
        assert_eq!(consumed, buf.len());
        let entry = match outcome {
            FileListDecodeOutcome::Entry(e) => e,
            _ => panic!("expected Entry, got {outcome:?}"),
        };
        assert_eq!(entry.path, "sample.bin");
        assert_eq!(entry.size, 262_144);
        assert_eq!(entry.mtime, 0x69E2_7F58);
        assert_eq!(entry.mtime_nsec, Some(958_829_505));
        assert_eq!(entry.mode, 0x81B4);
        assert_eq!(entry.uid, Some(1000));
        assert_eq!(entry.uid_name.as_deref(), Some("axpnet"));
        assert_eq!(entry.gid, Some(1000));
        assert_eq!(entry.gid_name.as_deref(), Some("axpnet"));
        assert_eq!(entry.checksum.len(), 16);
        assert!(entry.checksum.iter().all(|&b| b == 0xAA));
    }

    #[test]
    fn decode_file_list_entry_recognises_terminator_varint_zero() {
        let opts = frozen_oracle_opts(None);
        let buf = [0x00u8]; // varint(0)
        let (outcome, consumed) = decode_file_list_entry(&buf, &opts).unwrap();
        assert_eq!(consumed, 1);
        assert_eq!(outcome, FileListDecodeOutcome::EndOfList { io_error: 0 });
    }

    #[test]
    fn decode_file_list_entry_classic_flags_mode_single_byte() {
        // Classic encoding (xfer_flags_as_varint=false) with no flags
        // requiring a high byte. Entry: "A.txt" (5), size 7, mtime 100,
        // mode 0644, no uid/gid, no checksum. A bare 0x00 would be the
        // terminator, so we set XMIT_TOP_DIR to get a non-zero flag byte.
        let single_byte_flags: u8 = XMIT_TOP_DIR as u8;
        let mut buf: Vec<u8> = Vec::new();
        buf.push(single_byte_flags);
        buf.push(5); // name length
        buf.extend_from_slice(b"A.txt");
        buf.extend_from_slice(&encode_varlong(7, 3));
        buf.extend_from_slice(&encode_varlong(100, 4));
        buf.extend_from_slice(&0o100_644u32.to_le_bytes());

        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: false,
            always_checksum: false,
            csum_len: 0,
            preserve_uid: false,
            preserve_gid: false,
            previous_name: None,
        };
        let (outcome, consumed) = decode_file_list_entry(&buf, &opts).unwrap();
        assert_eq!(consumed, buf.len());
        let entry = match outcome {
            FileListDecodeOutcome::Entry(e) => e,
            _ => panic!("expected Entry"),
        };
        assert_eq!(entry.path, "A.txt");
        assert_eq!(entry.size, 7);
        assert_eq!(entry.mtime, 100);
        assert_eq!(entry.mode, 0o100_644);
        assert_eq!(entry.uid, None);
        assert_eq!(entry.gid, None);
        assert!(entry.checksum.is_empty());
    }

    #[test]
    fn decode_file_list_entry_classic_flags_uses_ext_byte_when_bit_set() {
        // XMIT_EXTENDED_FLAGS signals a high byte follows. Use MOD_NSEC
        // (bit 13 = 0x2000) so the high byte carries 0x20.
        let mut buf: Vec<u8> = Vec::new();
        let flags_lo = XMIT_EXTENDED_FLAGS as u8 | XMIT_TOP_DIR as u8;
        buf.push(flags_lo);
        buf.push(0x20); // high byte — only XMIT_MOD_NSEC (0x2000 >> 8 = 0x20)
        buf.push(1);
        buf.extend_from_slice(b"x");
        buf.extend_from_slice(&encode_varlong(1, 3));
        buf.extend_from_slice(&encode_varlong(0, 4));
        buf.extend_from_slice(&encode_varint(42));
        buf.extend_from_slice(&0o100_600u32.to_le_bytes());

        let opts = FileListDecodeOptions {
            protocol: 31,
            xfer_flags_as_varint: false,
            always_checksum: false,
            csum_len: 0,
            preserve_uid: false,
            preserve_gid: false,
            previous_name: None,
        };
        let (outcome, _) = decode_file_list_entry(&buf, &opts).unwrap();
        let entry = match outcome {
            FileListDecodeOutcome::Entry(e) => e,
            _ => panic!("expected Entry"),
        };
        assert_eq!(entry.path, "x");
        assert_eq!(entry.mtime_nsec, Some(42));
        assert_eq!(
            entry.flags & XMIT_MOD_NSEC,
            XMIT_MOD_NSEC,
            "high byte must land in the returned flags"
        );
    }

    #[test]
    fn decode_file_list_entry_same_name_without_previous_errors() {
        let mut buf: Vec<u8> = Vec::new();
        // varint flags with XMIT_SAME_NAME set, l1 = 3 — and no
        // previous_name in the options.
        let flags = XMIT_SAME_NAME;
        buf.extend_from_slice(&encode_varint(flags as i32));
        buf.push(3); // l1

        let opts = frozen_oracle_opts(None);
        let err = decode_file_list_entry(&buf, &opts).unwrap_err();
        assert!(matches!(err, RealWireError::SameNameWithoutPrevious));
    }

    #[test]
    fn decode_file_list_entry_same_name_reuses_previous_prefix() {
        let mut buf: Vec<u8> = Vec::new();
        let flags =
            XMIT_SAME_NAME | XMIT_SAME_UID | XMIT_SAME_GID | XMIT_SAME_TIME | XMIT_SAME_MODE;
        buf.extend_from_slice(&encode_varint(flags as i32));
        buf.push(4); // l1 — reuse "/tmp" prefix
        buf.push(3); // l2 — " .a"
        buf.extend_from_slice(b"/.a");
        buf.extend_from_slice(&encode_varlong(0, 3)); // size 0

        let opts = FileListDecodeOptions {
            protocol: 32,
            xfer_flags_as_varint: true,
            always_checksum: false,
            csum_len: 0,
            preserve_uid: true,
            preserve_gid: true,
            previous_name: Some("/tmp"),
        };
        let (outcome, _consumed) = decode_file_list_entry(&buf, &opts).unwrap();
        let entry = match outcome {
            FileListDecodeOutcome::Entry(e) => e,
            _ => panic!("expected Entry"),
        };
        assert_eq!(entry.path, "/tmp/.a");
        assert_eq!(entry.size, 0);
    }

    // -------------------------------------------------------------------------
    // Sinergia 8e — ndx / item_flags / sum_head / sum_block
    // -------------------------------------------------------------------------

    #[test]
    fn decode_ndx_single_byte_positive_form_round_trips() {
        // Fresh state: prev_positive=-1. Single-byte diff=2 encodes ndx=1.
        let mut enc_state = NdxState::new();
        let bytes = encode_ndx(1, &mut enc_state);
        assert_eq!(bytes, vec![0x02]);

        let mut dec_state = NdxState::new();
        let (ndx, consumed) = decode_ndx(&bytes, &mut dec_state).unwrap();
        assert_eq!(ndx, 1);
        assert_eq!(consumed, 1);
        assert_eq!(dec_state.prev_positive(), 1);
    }

    #[test]
    fn decode_ndx_three_byte_form_encodes_medium_diff() {
        // diff > 0x7FFF forces 5-byte form; diff in [254, 0x7FFF] uses 3-byte.
        // Start at -1, send ndx=300: diff=301 → 3-byte form.
        let mut enc_state = NdxState::new();
        let bytes = encode_ndx(300, &mut enc_state);
        assert_eq!(bytes[0], 0xFE);
        assert_eq!(bytes.len(), 3);
        // diff=301 = 0x012D → hi=0x01, lo=0x2D.
        assert_eq!(bytes[1], 0x01);
        assert_eq!(bytes[2], 0x2D);

        let mut dec_state = NdxState::new();
        let (ndx, consumed) = decode_ndx(&bytes, &mut dec_state).unwrap();
        assert_eq!(ndx, 300);
        assert_eq!(consumed, 3);
        assert_eq!(dec_state.prev_positive(), 300);
    }

    #[test]
    fn decode_ndx_five_byte_form_encodes_large_absolute() {
        // ndx = 0x12345 requires 5-byte form from baseline -1 (diff > 0x7FFF).
        let mut enc_state = NdxState::new();
        let target: i32 = 0x12345;
        let bytes = encode_ndx(target, &mut enc_state);
        assert_eq!(bytes[0], 0xFE);
        assert_eq!(bytes.len(), 5);
        // 5-byte form encodes abs, not diff: b[1] = (abs>>24)|0x80 = 0x80.
        assert_eq!(bytes[1], 0x80);
        assert_eq!(bytes[2], (target & 0xFF) as u8);
        assert_eq!(bytes[3], ((target >> 8) & 0xFF) as u8);
        assert_eq!(bytes[4], ((target >> 16) & 0xFF) as u8);

        let mut dec_state = NdxState::new();
        let (ndx, consumed) = decode_ndx(&bytes, &mut dec_state).unwrap();
        assert_eq!(ndx, target);
        assert_eq!(consumed, 5);
    }

    #[test]
    fn decode_ndx_negative_prefix_matches_write_ndx_for_flist_eof() {
        // NDX_FLIST_EOF (-2) from a fresh state: prefix 0xFF, then diff=1
        // against prev_negative=1 (abs=2, diff=2-1=1 → single byte).
        let mut enc_state = NdxState::new();
        let bytes = encode_ndx(NDX_FLIST_EOF, &mut enc_state);
        assert_eq!(bytes, vec![0xFF, 0x01]);

        let mut dec_state = NdxState::new();
        let (ndx, consumed) = decode_ndx(&bytes, &mut dec_state).unwrap();
        assert_eq!(ndx, NDX_FLIST_EOF);
        assert_eq!(consumed, 2);
        assert_eq!(dec_state.prev_negative(), 2);
    }

    #[test]
    fn decode_ndx_done_marker_is_single_zero_byte_no_state_change() {
        // NDX_DONE is written as a single 0 byte with neither baseline
        // touched (matching `io.c::read_ndx`).
        let mut enc_state = NdxState::new();
        let bytes = encode_ndx(NDX_DONE, &mut enc_state);
        assert_eq!(bytes, vec![0x00]);
        assert_eq!(enc_state.prev_positive(), -1);
        assert_eq!(enc_state.prev_negative(), 1);

        let mut dec_state = NdxState::new();
        let (ndx, consumed) = decode_ndx(&bytes, &mut dec_state).unwrap();
        assert_eq!(ndx, NDX_DONE);
        assert_eq!(consumed, 1);
        assert_eq!(dec_state.prev_positive(), -1);
        assert_eq!(dec_state.prev_negative(), 1);
    }

    #[test]
    fn decode_item_flags_round_trips_known_bits() {
        // ITEM_TRANSFER | ITEM_REPORT_CHANGE = 0x8002 — the flag profile
        // observed at the start of every per-file header in the frozen
        // oracle (both upload and download directions).
        let iflags = 0x8002u16;
        let enc = encode_item_flags(iflags);
        assert_eq!(enc, [0x02, 0x80]);
        let (dec, consumed) = decode_item_flags(&enc).unwrap();
        assert_eq!(dec, iflags);
        assert_eq!(consumed, 2);
    }

    #[test]
    fn decode_item_flags_truncated_buffer() {
        let err = decode_item_flags(&[0x02]).unwrap_err();
        assert!(matches!(
            err,
            RealWireError::TruncatedBuffer {
                at: "item_flags",
                ..
            }
        ));
    }

    #[test]
    fn decode_sum_head_accepts_frozen_oracle_profile() {
        // The exact 16 bytes that follow the per-file header in the frozen
        // upload server->client stream: count=375, blength=700,
        // s2length=2 (truncated), remainder=344.
        let bytes: [u8; 16] = [
            0x77, 0x01, 0x00, 0x00, // count = 375
            0xBC, 0x02, 0x00, 0x00, // blength = 700
            0x02, 0x00, 0x00, 0x00, // s2length = 2
            0x58, 0x01, 0x00, 0x00, // remainder = 344
        ];
        let (head, consumed) = decode_sum_head(&bytes).unwrap();
        assert_eq!(consumed, 16);
        assert_eq!(head.count, 375);
        assert_eq!(head.block_length, 700);
        assert_eq!(head.checksum_length, 2);
        assert_eq!(head.remainder_length, 344);
        // Encoder parity.
        assert_eq!(encode_sum_head(&head), bytes);
    }

    #[test]
    fn decode_sum_head_rejects_blength_above_max_block_size() {
        let bad = SumHead {
            count: 1,
            block_length: SUM_HEAD_MAX_BLOCK_LEN_PROTO30PLUS + 1,
            checksum_length: 16,
            remainder_length: 0,
        };
        let bytes = encode_sum_head(&bad);
        let err = decode_sum_head(&bytes).unwrap_err();
        assert!(matches!(
            err,
            RealWireError::SumHeadFieldOutOfRange {
                field: "block_length",
                ..
            }
        ));
    }

    #[test]
    fn decode_sum_head_rejects_remainder_exceeding_blength() {
        let bad = SumHead {
            count: 1,
            block_length: 100,
            checksum_length: 16,
            remainder_length: 200,
        };
        let bytes = encode_sum_head(&bad);
        let err = decode_sum_head(&bytes).unwrap_err();
        assert!(matches!(
            err,
            RealWireError::SumHeadFieldOutOfRange {
                field: "remainder_length",
                ..
            }
        ));
    }

    #[test]
    fn decode_sum_block_with_truncated_strong_len_two() {
        // rsync truncates the strong checksum to save bandwidth when the
        // block count is small. s2length=2 is what the frozen oracle uses.
        let buf: &[u8] = &[
            0xAB, 0xCD, 0xEF, 0x01, // rolling LE = 0x01EFCDAB
            0x42, 0x99, // strong (2 bytes)
            0xFF, // extra byte past the block — must not be consumed
        ];
        let (block, consumed) = decode_sum_block(buf, 2).unwrap();
        assert_eq!(consumed, 6);
        assert_eq!(block.rolling, 0x01EF_CDAB);
        assert_eq!(block.strong, vec![0x42, 0x99]);
    }

    #[test]
    fn decode_sum_block_truncation_reports_needed_vs_available() {
        let buf: &[u8] = &[0x00, 0x00, 0x00, 0x00, 0x01]; // 4 rolling + 1 strong
        let err = decode_sum_block(buf, 16).unwrap_err();
        let (needed, available) = match err {
            RealWireError::TruncatedBuffer {
                needed, available, ..
            } => (needed, available),
            other => panic!("unexpected error variant: {other:?}"),
        };
        assert_eq!(needed, 20);
        assert_eq!(available, 5);
    }

    // S8e hardening — edge cases surfaced while reading `io.c::write_ndx`
    // against `io.c::read_ndx`. Not on the frozen oracle's happy path but
    // they pin the decoder's contract so future changes can't drift.

    #[test]
    fn encode_ndx_diff_zero_uses_three_byte_form_per_rsync_comment() {
        // rsync comment: "A diff of 254 - 32767 or 0 is sent as a 0xFE +
        // a two-byte diff". Re-emitting the same ndx yields diff=0 and
        // must land in the 3-byte form with both diff bytes zero.
        let mut state = NdxState::new();
        // First call sets prev_positive = 7 (diff=8 from baseline -1).
        let _ = encode_ndx(7, &mut state);
        assert_eq!(state.prev_positive(), 7);
        // Second call: diff = 7 - 7 = 0 → 3-byte form `FE 00 00`.
        let bytes = encode_ndx(7, &mut state);
        assert_eq!(bytes, vec![0xFE, 0x00, 0x00]);

        // And decode must round-trip back to 7 without mutating the
        // baseline (the value is already there).
        let mut dec_state = NdxState::new();
        let _ = encode_ndx(7, &mut dec_state); // bring decoder baseline to 7
        let (ndx, consumed) = decode_ndx(&bytes, &mut dec_state).unwrap();
        assert_eq!(ndx, 7);
        assert_eq!(consumed, 3);
        assert_eq!(dec_state.prev_positive(), 7);
    }

    #[test]
    fn decode_ndx_ff_zero_sequence_returns_negated_baseline() {
        // `FF 00` is not a sequence rsync ever emits (NDX_DONE is the
        // unprefixed 0x00), but the decoder's contract when faced with
        // it is "single-byte diff 0 on the negative baseline". Pin the
        // observable outcome so a refactor that accidentally short-circuits
        // on `buf[1] == 0` inside the negative branch can't slip past.
        let mut state = NdxState::new();
        let (ndx, consumed) = decode_ndx(&[0xFF, 0x00], &mut state).unwrap();
        // Fresh negative baseline is 1, diff=0 → num=1, return -1.
        assert_eq!(ndx, -1);
        assert_eq!(consumed, 2);
        // prev_negative is unchanged (still 1, set by the finalize step).
        assert_eq!(state.prev_negative(), 1);
        // prev_positive is untouched.
        assert_eq!(state.prev_positive(), -1);
    }

    // -------------------------------------------------------------------------
    // Sezione 7 — delta instruction stream
    // -------------------------------------------------------------------------

    #[test]
    fn decode_delta_op_end_flag_returns_sentinel_without_state_change() {
        let mut state = DeltaStreamState::new();
        let (outcome, consumed) = decode_delta_op(&[TOKEN_END_FLAG], &mut state).unwrap();
        assert_eq!(outcome, DeltaOpOutcome::EndFlag);
        assert_eq!(consumed, 1);
        // END_FLAG never updates last_run_end.
        assert_eq!(state.last_run_end(), 0);
    }

    #[test]
    fn decode_delta_op_tokenrun_rel_matches_frozen_oracle_header() {
        // The first record the sender emits on the frozen upload client
        // stream is `C0 0A 00`: TOKENRUN_REL with rel=0, run=10.
        // With fresh state (last_run_end=0) this resolves to
        // CopyRun { start=0, run=10 } and advances last_run_end to 10.
        let mut state = DeltaStreamState::new();
        let (outcome, consumed) = decode_delta_op(&[0xC0, 0x0A, 0x00], &mut state).unwrap();
        assert_eq!(
            outcome,
            DeltaOpOutcome::Op(DeltaOp::CopyRun {
                start_token_index: 0,
                run_length: 10,
            })
        );
        assert_eq!(consumed, 3);
        assert_eq!(state.last_run_end(), 10);
    }

    #[test]
    fn decode_delta_op_token_rel_advances_last_run_end_by_one() {
        // TOKEN_REL with rel=5 after last_run_end=10 → start=15, run=1,
        // last_run_end advances to 16.
        let mut state = DeltaStreamState { last_run_end: 10 };
        let tag = TOKEN_REL | 5;
        let (outcome, consumed) = decode_delta_op(&[tag], &mut state).unwrap();
        assert_eq!(
            outcome,
            DeltaOpOutcome::Op(DeltaOp::CopyRun {
                start_token_index: 15,
                run_length: 1,
            })
        );
        assert_eq!(consumed, 1);
        assert_eq!(state.last_run_end(), 16);
    }

    #[test]
    fn decode_delta_op_deflated_data_parses_len_and_payload() {
        // `41 1D 28 B5 2F FD ... (285 bytes total payload)` — frozen oracle
        // shape for the first LITERAL. hi6=1, lo8=0x1D → len=285.
        let mut payload_bytes: Vec<u8> = vec![0x41, 0x1D];
        // Fabricate a deterministic 285-byte payload (content doesn't matter
        // here — the decoder returns it opaque).
        payload_bytes.extend((0..285u16).map(|i| (i & 0xFF) as u8));

        let mut state = DeltaStreamState::new();
        let (outcome, consumed) = decode_delta_op(&payload_bytes, &mut state).unwrap();

        match outcome {
            DeltaOpOutcome::Op(DeltaOp::Literal { compressed_payload }) => {
                assert_eq!(compressed_payload.len(), 285);
                assert_eq!(compressed_payload[0], 0); // first byte of fabricated payload
                assert_eq!(compressed_payload[255], 0xFF);
                assert_eq!(compressed_payload[256], 0); // wraps on our counter
            }
            other => panic!("expected Literal, got {:?}", other),
        }
        assert_eq!(consumed, 2 + 285);
        // Literals do not mutate last_run_end.
        assert_eq!(state.last_run_end(), 0);
    }

    #[test]
    fn decode_delta_op_deflated_data_zero_length_is_rejected() {
        // rsync never emits DEFLATED_DATA with len=0. Treat as malformed.
        let mut state = DeltaStreamState::new();
        let err = decode_delta_op(&[0x40, 0x00], &mut state).unwrap_err();
        assert!(matches!(
            err,
            RealWireError::DeltaTokenTruncated {
                at: "deflated_len_zero",
                ..
            }
        ));
    }

    #[test]
    fn decode_delta_op_deflated_payload_truncation_reports_needed() {
        // Declare len=10 but supply only 3 payload bytes.
        let mut state = DeltaStreamState::new();
        let err = decode_delta_op(&[0x40, 0x0A, 0x11, 0x22, 0x33], &mut state).unwrap_err();
        match err {
            RealWireError::DeltaTokenTruncated {
                at,
                needed,
                available,
            } => {
                assert_eq!(at, "deflated_payload");
                assert_eq!(needed, 10);
                assert_eq!(available, 3);
            }
            other => panic!("unexpected error: {:?}", other),
        }
    }

    #[test]
    fn decode_delta_op_tokenrun_long_uses_absolute_index() {
        // TOKENRUN_LONG (0x21) + int32 LE absolute + u16 LE run
        // token_index = 0x0001_1223 = 70179, run = 0x0005 = 5.
        let mut state = DeltaStreamState::new();
        let wire = [0x21, 0x23, 0x12, 0x01, 0x00, 0x05, 0x00];
        let (outcome, consumed) = decode_delta_op(&wire, &mut state).unwrap();
        assert_eq!(
            outcome,
            DeltaOpOutcome::Op(DeltaOp::CopyRun {
                start_token_index: 70_179,
                run_length: 5,
            })
        );
        assert_eq!(consumed, 7);
        // last_run_end = start + run
        assert_eq!(state.last_run_end(), 70_184);
    }

    #[test]
    fn decode_delta_op_token_long_uses_absolute_index_single_block() {
        let mut state = DeltaStreamState::new();
        let wire = [0x20, 0x2A, 0x00, 0x00, 0x00]; // token_index = 42
        let (outcome, consumed) = decode_delta_op(&wire, &mut state).unwrap();
        assert_eq!(
            outcome,
            DeltaOpOutcome::Op(DeltaOp::CopyRun {
                start_token_index: 42,
                run_length: 1,
            })
        );
        assert_eq!(consumed, 5);
        assert_eq!(state.last_run_end(), 43);
    }

    #[test]
    fn decode_delta_op_empty_buffer_truncates_at_tag() {
        let mut state = DeltaStreamState::new();
        let err = decode_delta_op(&[], &mut state).unwrap_err();
        match err {
            RealWireError::DeltaTokenTruncated {
                at,
                needed,
                available,
            } => {
                assert_eq!(at, "tag");
                assert_eq!(needed, 1);
                assert_eq!(available, 0);
            }
            other => panic!("unexpected error: {:?}", other),
        }
    }

    #[test]
    fn decode_delta_op_tokenrun_rel_truncated_body_is_rejected() {
        let mut state = DeltaStreamState::new();
        // TOKENRUN_REL tag with only one following byte (needs two for
        // the u16 run_count).
        let err = decode_delta_op(&[0xC0, 0x05], &mut state).unwrap_err();
        match err {
            RealWireError::DeltaTokenTruncated {
                at,
                needed,
                available,
            } => {
                assert_eq!(at, "tokenrun_rel");
                assert_eq!(needed, 3);
                assert_eq!(available, 2);
            }
            other => panic!("unexpected error: {:?}", other),
        }
        // State must NOT have been mutated on error.
        assert_eq!(state.last_run_end(), 0);
    }

    #[test]
    fn decode_delta_stream_iterates_and_captures_file_checksum() {
        // Fabricated short stream: TOKENRUN_REL rel=0 run=3, then
        // TOKEN_REL rel=0 (single block match), then END_FLAG, then a
        // 4-byte file checksum.
        let mut wire: Vec<u8> = Vec::new();
        wire.extend_from_slice(&[0xC0, 0x03, 0x00]); // run of 3 @ 0
        wire.push(TOKEN_REL); // rel=0 single match → start=3
        wire.push(TOKEN_END_FLAG);
        wire.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);

        let (report, consumed) = decode_delta_stream(&wire, 4, Some(10)).unwrap();
        assert_eq!(report.ops.len(), 2);
        assert_eq!(
            report.ops[0],
            DeltaOp::CopyRun {
                start_token_index: 0,
                run_length: 3
            }
        );
        assert_eq!(
            report.ops[1],
            DeltaOp::CopyRun {
                start_token_index: 3,
                run_length: 1
            }
        );
        assert_eq!(report.file_checksum, vec![0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(consumed, 3 + 1 + 1 + 4);
    }

    #[test]
    fn decode_delta_stream_rejects_copy_past_sum_head_count() {
        // A run that ends at 11 with sum_head.count=10 must be rejected.
        let wire = [0xC0, 0x0B, 0x00, TOKEN_END_FLAG];
        let err = decode_delta_stream(&wire, 0, Some(10)).unwrap_err();
        match err {
            RealWireError::DeltaTokenOutOfRange {
                token_index,
                block_count,
            } => {
                assert_eq!(token_index, 0);
                assert_eq!(block_count, 10);
            }
            other => panic!("unexpected error: {:?}", other),
        }
    }

    #[test]
    fn decode_delta_stream_accepts_exact_count_boundary() {
        // Boundary: run of 10 starting at 0 with sum_head.count=10 is
        // valid (ends exactly at count). One past that (end=11) was
        // rejected by the previous test.
        let wire = [0xC0, 0x0A, 0x00, TOKEN_END_FLAG];
        let (report, consumed) = decode_delta_stream(&wire, 0, Some(10)).unwrap();
        assert_eq!(report.ops.len(), 1);
        assert_eq!(consumed, 4);
    }

    #[test]
    fn decode_delta_stream_empty_deltas_still_reads_file_checksum() {
        // Stream with only END_FLAG + 16-byte checksum. rsync may emit
        // this when every block matches AND nothing is literal — a
        // pathological case that pins the "zero ops" code path.
        let mut wire = vec![TOKEN_END_FLAG];
        wire.extend_from_slice(&[0xAB; 16]);
        let (report, consumed) = decode_delta_stream(&wire, 16, None).unwrap();
        assert!(report.ops.is_empty());
        assert_eq!(report.file_checksum, vec![0xAB; 16]);
        assert_eq!(consumed, 1 + 16);
    }

    #[test]
    fn decode_file_checksum_truncation_reports_needed() {
        let err = decode_file_checksum(&[0x11, 0x22], 16).unwrap_err();
        match err {
            RealWireError::DeltaTokenTruncated {
                at,
                needed,
                available,
            } => {
                assert_eq!(at, "file_checksum");
                assert_eq!(needed, 16);
                assert_eq!(available, 2);
            }
            other => panic!("unexpected error: {:?}", other),
        }
    }

    #[test]
    fn decode_delta_op_deflated_data_max_length_round_trips() {
        // Hardening: the 14-bit len field maxes out at 16383. Pin the
        // top boundary so a future refactor that truncates len to 8192
        // (say, a naive mask change) fails loudly.
        let len = MAX_DELTA_LITERAL_LEN;
        let hi6 = (len >> 8) as u8;
        let lo8 = (len & 0xFF) as u8;
        let tag = TOKEN_DEFLATED_DATA | hi6;
        let mut wire = vec![tag, lo8];
        wire.extend(std::iter::repeat_n(0xAA, len));

        let mut state = DeltaStreamState::new();
        let (outcome, consumed) = decode_delta_op(&wire, &mut state).unwrap();
        match outcome {
            DeltaOpOutcome::Op(DeltaOp::Literal { compressed_payload }) => {
                assert_eq!(compressed_payload.len(), MAX_DELTA_LITERAL_LEN);
                assert!(compressed_payload.iter().all(|&b| b == 0xAA));
            }
            other => panic!("expected Literal, got {:?}", other),
        }
        assert_eq!(consumed, 2 + MAX_DELTA_LITERAL_LEN);
    }

    // -------------------------------------------------------------------------
    // Section 8 — Summary frame (S8g)
    // -------------------------------------------------------------------------

    #[test]
    fn summary_frame_proto31_round_trip_small_values() {
        // Canonical proto 31 frame with small positive values — every
        // varlong fits in the 3-byte floor.
        let frame = SummaryFrame {
            total_read: 42,
            total_written: 1_000,
            total_size: 262_144,
            flist_buildtime: Some(0),
            flist_xfertime: Some(3),
        };
        let wire = encode_summary_frame(&frame, 31);
        let (decoded, consumed) = decode_summary_frame(&wire, 31).unwrap();
        assert_eq!(decoded, frame);
        assert_eq!(consumed, wire.len());
    }

    #[test]
    fn summary_frame_proto31_round_trip_large_values() {
        // Force varlong extra bytes: value > 2^31 triggers min_bytes + extra.
        let frame = SummaryFrame {
            total_read: 8_589_934_592,    // 2^33
            total_written: 4_294_967_296, // 2^32
            total_size: 10_737_418_240,   // 10 GiB
            flist_buildtime: Some(250),
            flist_xfertime: Some(500),
        };
        let wire = encode_summary_frame(&frame, 31);
        let (decoded, consumed) = decode_summary_frame(&wire, 31).unwrap();
        assert_eq!(decoded, frame);
        assert_eq!(consumed, wire.len());
    }

    #[test]
    fn summary_frame_proto31_zero_size_is_accepted() {
        // `total_size == 0` is legitimate for an empty transfer and must
        // decode without error.
        let frame = SummaryFrame {
            total_read: 0,
            total_written: 0,
            total_size: 0,
            flist_buildtime: Some(0),
            flist_xfertime: Some(0),
        };
        let wire = encode_summary_frame(&frame, 31);
        let (decoded, _) = decode_summary_frame(&wire, 31).unwrap();
        assert_eq!(decoded, frame);
    }

    #[test]
    fn summary_frame_proto31_pins_field_order() {
        // Hardening: if a future refactor swaps the order of writes,
        // this test catches it. Each field gets a distinct value.
        let frame = SummaryFrame {
            total_read: 111,
            total_written: 222,
            total_size: 333,
            flist_buildtime: Some(444),
            flist_xfertime: Some(555),
        };
        let wire = encode_summary_frame(&frame, 31);
        let (decoded, _) = decode_summary_frame(&wire, 31).unwrap();
        assert_eq!(decoded.total_read, 111);
        assert_eq!(decoded.total_written, 222);
        assert_eq!(decoded.total_size, 333);
        assert_eq!(decoded.flist_buildtime, Some(444));
        assert_eq!(decoded.flist_xfertime, Some(555));
    }

    #[test]
    fn summary_frame_proto31_truncated_third_field_pinpoints_total_size() {
        let frame = SummaryFrame {
            total_read: 5,
            total_written: 5,
            total_size: 5,
            flist_buildtime: Some(0),
            flist_xfertime: Some(0),
        };
        let wire = encode_summary_frame(&frame, 31);
        // Keep only the first two varlongs (each 3 bytes at min_bytes=3).
        let truncated = &wire[..6];
        let err = decode_summary_frame(truncated, 31).unwrap_err();
        assert!(
            matches!(
                err,
                RealWireError::TruncatedBuffer {
                    at: "summary_total_size",
                    ..
                }
            ),
            "expected truncated-buffer pointing at summary_total_size, got {:?}",
            err
        );
    }

    #[test]
    fn summary_frame_proto31_empty_buffer_reports_first_field() {
        let err = decode_summary_frame(&[], 31).unwrap_err();
        assert!(
            matches!(
                err,
                RealWireError::TruncatedBuffer {
                    at: "summary_total_read",
                    ..
                }
            ),
            "expected truncation on first field, got {:?}",
            err
        );
    }

    #[test]
    fn summary_frame_proto31_exact_buffer_has_no_slack() {
        let frame = SummaryFrame {
            total_read: 1,
            total_written: 2,
            total_size: 3,
            flist_buildtime: Some(4),
            flist_xfertime: Some(5),
        };
        let wire = encode_summary_frame(&frame, 31);
        let (_, consumed) = decode_summary_frame(&wire, 31).unwrap();
        // Hardening: consumed must exactly match the buffer length —
        // a reader that over- or under-consumes would desync the next
        // frame.
        assert_eq!(consumed, wire.len());
    }

    #[test]
    fn summary_frame_proto32_behaves_like_proto31() {
        // Proto 32 doesn't change `handle_stats` — wire format must be
        // bit-identical to proto 31.
        let frame = SummaryFrame {
            total_read: 100,
            total_written: 200,
            total_size: 300,
            flist_buildtime: Some(400),
            flist_xfertime: Some(500),
        };
        let wire31 = encode_summary_frame(&frame, 31);
        let wire32 = encode_summary_frame(&frame, 32);
        assert_eq!(wire31, wire32);
        let (decoded, _) = decode_summary_frame(&wire32, 32).unwrap();
        assert_eq!(decoded, frame);
    }

    #[test]
    fn summary_frame_proto29_uses_longint_with_flist_times() {
        // Proto 29 still has the 2 extra fields but serialises via
        // write_longint (4 or 12 bytes per field).
        let frame = SummaryFrame {
            total_read: 42,
            total_written: 100,
            total_size: 262_144,
            flist_buildtime: Some(1),
            flist_xfertime: Some(2),
        };
        let wire = encode_summary_frame(&frame, 29);
        // All 5 values fit in i32 positive → 4 bytes each → 20 bytes total.
        assert_eq!(wire.len(), 20);
        let (decoded, consumed) = decode_summary_frame(&wire, 29).unwrap();
        assert_eq!(decoded, frame);
        assert_eq!(consumed, 20);
    }

    #[test]
    fn summary_frame_proto28_omits_flist_times() {
        let frame = SummaryFrame {
            total_read: 42,
            total_written: 100,
            total_size: 262_144,
            flist_buildtime: None,
            flist_xfertime: None,
        };
        let wire = encode_summary_frame(&frame, 28);
        // 3 × 4-byte longint = 12 bytes.
        assert_eq!(wire.len(), 12);
        let (decoded, consumed) = decode_summary_frame(&wire, 28).unwrap();
        assert_eq!(decoded, frame);
        assert_eq!(consumed, 12);
        assert_eq!(decoded.flist_buildtime, None);
        assert_eq!(decoded.flist_xfertime, None);
    }

    #[test]
    fn summary_frame_proto29_large_value_uses_12_byte_longint() {
        // Value > 0x7FFFFFFF triggers the 12-byte write_longint branch.
        let frame = SummaryFrame {
            total_read: 8_589_934_592, // 2^33
            total_written: 0,
            total_size: 0,
            flist_buildtime: Some(0),
            flist_xfertime: Some(0),
        };
        let wire = encode_summary_frame(&frame, 29);
        // total_read = 12 bytes, four small fields = 4 bytes each,
        // total 12 + 4*4 = 28 bytes.
        assert_eq!(wire.len(), 28);
        // total_read wire: FF FF FF FF + low32(2^33) LE + high32(2^33) LE.
        assert_eq!(wire[0..4], [0xFF, 0xFF, 0xFF, 0xFF]);
        let low = u32::from_le_bytes(wire[4..8].try_into().unwrap());
        let high = u32::from_le_bytes(wire[8..12].try_into().unwrap());
        assert_eq!(low, 0);
        assert_eq!(high, 2);
        let (decoded, _) = decode_summary_frame(&wire, 29).unwrap();
        assert_eq!(decoded.total_read, 8_589_934_592);
    }

    #[test]
    fn summary_frame_proto30_uses_varlong_gate() {
        // Proto 30 must already use varlong (same as proto 31+). This
        // test pins the exact protocol at which the encoding switches.
        let frame = SummaryFrame {
            total_read: 42,
            total_written: 42,
            total_size: 42,
            flist_buildtime: Some(42),
            flist_xfertime: Some(42),
        };
        let wire_30 = encode_summary_frame(&frame, 30);
        let wire_29 = encode_summary_frame(&frame, 29);
        // Proto 30 uses varlong(min=3) → 3 bytes per small value → 15 bytes.
        // Proto 29 uses longint → 4 bytes per small value → 20 bytes.
        assert_eq!(wire_30.len(), 15);
        assert_eq!(wire_29.len(), 20);
        let (decoded_30, _) = decode_summary_frame(&wire_30, 30).unwrap();
        let (decoded_29, _) = decode_summary_frame(&wire_29, 29).unwrap();
        assert_eq!(decoded_30, frame);
        assert_eq!(decoded_29, frame);
    }

    #[test]
    fn summary_frame_longint_negative_sentinel_round_trip() {
        // Hardening for the write_longint sentinel branch: a negative
        // value like -1 triggers the 12-byte encoding because
        // `-1 < 0` fails the fast-path guard.
        let frame = SummaryFrame {
            total_read: -1,
            total_written: 0,
            total_size: 0,
            flist_buildtime: Some(0),
            flist_xfertime: Some(0),
        };
        let wire = encode_summary_frame(&frame, 29);
        // total_read = 12 bytes longint; other 4 fields = 4 bytes each.
        assert_eq!(wire.len(), 28);
        let (decoded, _) = decode_summary_frame(&wire, 29).unwrap();
        assert_eq!(decoded.total_read, -1);
    }

    // -------------------------------------------------------------------------
    // zstd literal stream decompression (S8f-bis)
    // -------------------------------------------------------------------------

    #[test]
    fn decompress_zstd_literal_stream_round_trip_single_complete_frame_ascii() {
        // A caller-supplied single-element slice with a complete zstd
        // frame (`encode_all` terminates with `ZSTD_e_end`) must
        // round-trip. This is the synthetic-test path — NOT what
        // rsync emits on the wire (see token.c:741 / ZSTD_e_flush
        // without ZSTD_e_end).
        let original: Vec<u8> = b"real-live-upload sample payload".to_vec();
        let compressed = zstd::stream::encode_all(&original[..], 3).unwrap();
        // Pin the frame magic so a silent library change to a different
        // container (e.g. bare deflate) fails loudly.
        assert_eq!(&compressed[..4], &[0x28, 0xB5, 0x2F, 0xFD]);
        let decompressed = decompress_zstd_literal_stream(&[compressed.as_slice()]).unwrap();
        assert_eq!(decompressed, original);
    }

    #[test]
    fn decompress_zstd_literal_stream_round_trip_empty_frame() {
        let original: Vec<u8> = Vec::new();
        let compressed = zstd::stream::encode_all(&original[..], 3).unwrap();
        let decompressed = decompress_zstd_literal_stream(&[compressed.as_slice()]).unwrap();
        assert!(decompressed.is_empty());
    }

    #[test]
    fn decompress_zstd_literal_stream_round_trip_patterned_bytes() {
        // The incrementing-byte pattern used by the frozen oracle
        // fixture. zstd should shrink this aggressively since the
        // pattern is deterministic, then restore it exactly.
        let original: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
        let compressed = zstd::stream::encode_all(&original[..], 3).unwrap();
        assert!(
            compressed.len() < original.len(),
            "zstd must reduce a highly-patterned buffer"
        );
        let decompressed = decompress_zstd_literal_stream(&[compressed.as_slice()]).unwrap();
        assert_eq!(decompressed, original);
    }

    #[test]
    fn decompress_zstd_literal_stream_concatenates_multiple_input_slices() {
        // Hardening: splitting one zstd frame across multiple input
        // slices must yield the same output as feeding it contiguous.
        // This pins the stream-aware behaviour (single shared
        // decoder context) that the frozen oracle needs.
        let original: Vec<u8> = (0..512u16).map(|i| (i % 256) as u8).collect();
        let compressed = zstd::stream::encode_all(&original[..], 3).unwrap();
        assert!(compressed.len() > 8);
        let mid = compressed.len() / 2;
        let split: [&[u8]; 2] = [&compressed[..mid], &compressed[mid..]];
        let decompressed = decompress_zstd_literal_stream(&split).unwrap();
        assert_eq!(decompressed, original);
    }

    #[test]
    fn decompress_zstd_literal_stream_rejects_malformed_frame() {
        // Random bytes that don't start with the zstd magic.
        let garbage: &[u8] = &[0xAA, 0xBB, 0xCC, 0xDD, 0x01, 0x02];
        let err = decompress_zstd_literal_stream(&[garbage]).unwrap_err();
        assert!(matches!(err, RealWireError::ZstdDecompressionFailed { .. }));
    }

    #[test]
    fn decompress_zstd_literal_stream_accepts_unfinished_frame() {
        // Hardening: rsync's `send_zstd_token` (token.c:741) calls
        // `ZSTD_compressStream2` with `ZSTD_e_flush` but NEVER
        // `ZSTD_e_end` — the frame epilogue is never shipped. The
        // stream-aware decoder MUST tolerate a prefix of a frame
        // that has only seen flushes; a pedantic "frame not
        // terminated" error here would break every real capture.
        let original: Vec<u8> = (0..=99u8).collect();
        let compressed = zstd::stream::encode_all(&original[..], 3).unwrap();
        // Drop the final 3 bytes to simulate the missing epilogue.
        // On a stream-aware decoder we still get the body out.
        let unfinished = &compressed[..compressed.len() - 3];
        let out = decompress_zstd_literal_stream(&[unfinished])
            .expect("unfinished frame must not error on stream-aware decoder");
        // We accept any prefix of the original (bounded by
        // flush-block granularity); it must be byte-equal to the
        // corresponding prefix of `original`.
        assert!(
            !out.is_empty(),
            "at least one flush block should have produced output"
        );
        assert_eq!(out, original[..out.len()]);
    }

    #[test]
    fn decompress_zstd_literal_stream_empty_input_is_empty_output() {
        // No payloads at all must yield an empty Vec, not an error.
        // `zstd::stream::read::Decoder::new` rejects a zero-byte init,
        // so the helper must short-circuit on an empty slice list.
        let out = decompress_zstd_literal_stream(&[]).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn summary_frame_proto28_truncated_reports_correct_field() {
        let frame = SummaryFrame {
            total_read: 1,
            total_written: 2,
            total_size: 3,
            flist_buildtime: None,
            flist_xfertime: None,
        };
        let wire = encode_summary_frame(&frame, 28);
        // Keep only the first longint (4 bytes). Second field should
        // fail with `summary_total_written`.
        let err = decode_summary_frame(&wire[..4], 28).unwrap_err();
        assert!(
            matches!(
                err,
                RealWireError::TruncatedBuffer {
                    at: "summary_total_written",
                    ..
                }
            ),
            "expected truncation on total_written, got {:?}",
            err
        );
    }

    // -------------------------------------------------------------------------
    // Sinergia 8i-encode — Fase 1: preamble + sum_block + file_csum encoders.
    // -------------------------------------------------------------------------

    #[test]
    fn server_preamble_round_trip_typical_proto31_profile() {
        let original = ServerPreamble {
            protocol_version: 31,
            compat_flags: 0x07,
            checksum_algos: "xxh128 xxh3 xxh64 md5 md4 sha1 none".to_string(),
            compression_algos: "zstd lz4 zlibx zlib none".to_string(),
            checksum_seed: 0xDEAD_BEEF,
            consumed: 0,
        };
        let bytes = encode_server_preamble(&original);
        let decoded = decode_server_preamble(&bytes).unwrap();
        assert_eq!(decoded.protocol_version, original.protocol_version);
        assert_eq!(decoded.compat_flags, original.compat_flags);
        assert_eq!(decoded.checksum_algos, original.checksum_algos);
        assert_eq!(decoded.compression_algos, original.compression_algos);
        assert_eq!(decoded.checksum_seed, original.checksum_seed);
        assert_eq!(decoded.consumed, bytes.len());
    }

    #[test]
    fn server_preamble_round_trip_minimal_compat_flags() {
        // Single-byte compat_flags varint (0x00) — pin the boundary
        // where varint encoder emits exactly one byte.
        let original = ServerPreamble {
            protocol_version: 30,
            compat_flags: 0,
            checksum_algos: "md5".to_string(),
            compression_algos: "none".to_string(),
            checksum_seed: 0,
            consumed: 0,
        };
        let bytes = encode_server_preamble(&original);
        let decoded = decode_server_preamble(&bytes).unwrap();
        assert_eq!(decoded.consumed, bytes.len());
        assert_eq!(decoded.compat_flags, 0);
    }

    #[test]
    fn client_preamble_round_trip_typical_profile() {
        let original = ClientPreamble {
            protocol_version: 31,
            checksum_algos: "xxh128 xxh3 xxh64 md5 md4 sha1 none".to_string(),
            compression_algos: "zstd lz4 zlibx zlib none".to_string(),
            consumed: 0,
        };
        let bytes = encode_client_preamble(&original);
        let decoded = decode_client_preamble(&bytes).unwrap();
        assert_eq!(decoded.protocol_version, 31);
        assert_eq!(decoded.checksum_algos, original.checksum_algos);
        assert_eq!(decoded.compression_algos, original.compression_algos);
        assert_eq!(decoded.consumed, bytes.len());
    }

    #[test]
    fn sum_block_round_trip_with_2byte_strong_checksum() {
        // Frozen oracle profile: checksum_length = 2.
        let original = SumBlock {
            rolling: 0xCAFE_BABE,
            strong: vec![0xAB, 0xCD],
        };
        let bytes = encode_sum_block(&original);
        assert_eq!(bytes.len(), 4 + 2);
        let (decoded, consumed) = decode_sum_block(&bytes, 2).unwrap();
        assert_eq!(decoded, original);
        assert_eq!(consumed, bytes.len());
    }

    #[test]
    fn sum_block_round_trip_with_16byte_strong_checksum() {
        let original = SumBlock {
            rolling: 0,
            strong: (0..16).collect(),
        };
        let bytes = encode_sum_block(&original);
        let (decoded, _) = decode_sum_block(&bytes, 16).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn file_checksum_round_trip_is_byte_identical() {
        let csum: Vec<u8> = (0..16).collect();
        let encoded = encode_file_checksum(&csum);
        assert_eq!(encoded, csum);
    }

    #[test]
    #[should_panic(expected = "is not printable ASCII")]
    fn write_u8_len_prefixed_ascii_rejects_non_ascii_at_caller_boundary() {
        // HARDENING — a non-ASCII byte in the algo list is a programmer
        // error caught at encode time, never silently smuggled.
        let mut out = Vec::new();
        write_u8_len_prefixed_ascii(&mut out, "zstd\u{00FF}", "test");
    }

    #[test]
    #[should_panic(expected = "exceeds u8 max")]
    fn write_u8_len_prefixed_ascii_rejects_overlong_list() {
        let mut out = Vec::new();
        let long = "a".repeat(256);
        write_u8_len_prefixed_ascii(&mut out, &long, "test");
    }

    // -------------------------------------------------------------------------
    // Sinergia 8i-encode — Fase 2: file_list_entry round-trip.
    // -------------------------------------------------------------------------

    fn frozen_oracle_options_for_test<'a>(prev: Option<&'a str>) -> FileListDecodeOptions<'a> {
        let mut opts = FileListDecodeOptions::frozen_oracle_default();
        opts.previous_name = prev;
        opts
    }

    fn assert_flist_entry_round_trip(entry: FileListEntry, opts: &FileListDecodeOptions) {
        let bytes = encode_file_list_entry(&entry, opts);
        let (outcome, consumed) =
            decode_file_list_entry(&bytes, opts).expect("entry must decode after encode");
        assert_eq!(
            consumed,
            bytes.len(),
            "decoder should consume exactly the encoded slice"
        );
        match outcome {
            FileListDecodeOutcome::Entry(decoded) => {
                assert_eq!(decoded.flags, entry.flags, "flags drift");
                assert_eq!(decoded.path, entry.path, "path drift");
                assert_eq!(decoded.size, entry.size, "size drift");
                assert_eq!(decoded.mtime, entry.mtime, "mtime drift");
                assert_eq!(decoded.mtime_nsec, entry.mtime_nsec, "mtime_nsec drift");
                assert_eq!(decoded.mode, entry.mode, "mode drift");
                assert_eq!(decoded.uid, entry.uid, "uid drift");
                assert_eq!(decoded.uid_name, entry.uid_name, "uid_name drift");
                assert_eq!(decoded.gid, entry.gid, "gid drift");
                assert_eq!(decoded.gid_name, entry.gid_name, "gid_name drift");
                assert_eq!(decoded.checksum, entry.checksum, "checksum drift");
            }
            other => panic!("expected Entry, got {other:?}"),
        }
    }

    fn baseline_entry() -> FileListEntry {
        FileListEntry {
            // varint flags: includes USER+GROUP names, MOD_NSEC, LONG_NAME bit
            // off (path < 255 bytes), no SAME_*.
            flags: XMIT_USER_NAME_FOLLOWS | XMIT_GROUP_NAME_FOLLOWS | XMIT_MOD_NSEC,
            path: "upload.bin".to_string(),
            size: 262_144,
            mtime: 1_700_000_000,
            mtime_nsec: Some(123_456_789),
            mode: 0o100_644,
            uid: Some(1000),
            uid_name: Some("axpnet".to_string()),
            gid: Some(1000),
            gid_name: Some("axpnet".to_string()),
            checksum: vec![0xAB; 16],
        }
    }

    #[test]
    fn encode_file_list_entry_round_trip_baseline() {
        let opts = frozen_oracle_options_for_test(None);
        assert_flist_entry_round_trip(baseline_entry(), &opts);
    }

    #[test]
    fn encode_file_list_entry_round_trip_with_long_name() {
        let mut entry = baseline_entry();
        entry.flags |= XMIT_LONG_NAME;
        entry.path = "a".repeat(300);
        let opts = frozen_oracle_options_for_test(None);
        assert_flist_entry_round_trip(entry, &opts);
    }

    #[test]
    fn encode_file_list_entry_round_trip_with_same_name_full_prefix_match() {
        let mut entry = baseline_entry();
        entry.flags |= XMIT_SAME_NAME;
        entry.path = "upload.bin".to_string();
        let opts = frozen_oracle_options_for_test(Some("upload.bin"));
        // l1 should equal full path length, suffix length 0.
        assert_flist_entry_round_trip(entry, &opts);
    }

    #[test]
    fn encode_file_list_entry_round_trip_with_same_name_partial_prefix() {
        let mut entry = baseline_entry();
        entry.flags |= XMIT_SAME_NAME;
        entry.path = "upload.bin".to_string();
        // Common prefix "upload." (7 bytes), suffix "bin".
        let opts = frozen_oracle_options_for_test(Some("upload.txt"));
        assert_flist_entry_round_trip(entry, &opts);
    }

    #[test]
    fn encode_file_list_entry_round_trip_same_time_omits_mtime() {
        let mut entry = baseline_entry();
        entry.flags |= XMIT_SAME_TIME;
        entry.mtime = 0; // gated out, value MUST be 0 to match decoder behaviour
        let opts = frozen_oracle_options_for_test(None);
        assert_flist_entry_round_trip(entry, &opts);
    }

    #[test]
    fn encode_file_list_entry_round_trip_same_mode_omits_mode_field() {
        let mut entry = baseline_entry();
        entry.flags |= XMIT_SAME_MODE;
        entry.mode = 0;
        let opts = frozen_oracle_options_for_test(None);
        assert_flist_entry_round_trip(entry, &opts);
    }

    #[test]
    fn encode_file_list_entry_round_trip_no_uid_no_gid_preservation() {
        // HARDENING: flags must be non-zero to avoid triggering the
        // terminator path. XMIT_TOP_DIR = 1 << 0 is harmless here and
        // the decoder ignores it for regular files. The point of this
        // test is the decoder option toggle, not the flag value.
        let entry = FileListEntry {
            flags: XMIT_TOP_DIR,
            path: "x.bin".to_string(),
            size: 42,
            mtime: 1_700_000_000,
            mtime_nsec: None,
            mode: 0o100_600,
            uid: None,
            uid_name: None,
            gid: None,
            gid_name: None,
            checksum: vec![0; 16],
        };
        let mut opts = FileListDecodeOptions::frozen_oracle_default();
        opts.preserve_uid = false;
        opts.preserve_gid = false;
        assert_flist_entry_round_trip(entry, &opts);
    }

    #[test]
    fn encode_file_list_entry_round_trip_no_checksum() {
        let mut entry = baseline_entry();
        entry.checksum = Vec::new();
        let mut opts = FileListDecodeOptions::frozen_oracle_default();
        opts.always_checksum = false;
        opts.csum_len = 0;
        opts.previous_name = None;
        assert_flist_entry_round_trip(entry, &opts);
    }

    #[test]
    fn encode_file_list_entry_round_trip_classic_flags_no_extended() {
        let entry = FileListEntry {
            // Lower-byte flags only, no XMIT_EXTENDED_FLAGS bit.
            flags: XMIT_SAME_GID,
            path: "small.txt".to_string(),
            size: 100,
            mtime: 1_700_000_000,
            mtime_nsec: None,
            mode: 0o100_644,
            uid: Some(1000),
            uid_name: None,
            gid: None, // gated out by SAME_GID
            gid_name: None,
            checksum: vec![],
        };
        let mut opts = FileListDecodeOptions::frozen_oracle_default();
        opts.xfer_flags_as_varint = false;
        opts.always_checksum = false;
        opts.csum_len = 0;
        opts.preserve_gid = true;
        opts.previous_name = None;
        assert_flist_entry_round_trip(entry, &opts);
    }

    #[test]
    fn encode_file_list_entry_round_trip_classic_flags_with_extended() {
        let entry = FileListEntry {
            flags: XMIT_EXTENDED_FLAGS | XMIT_USER_NAME_FOLLOWS | XMIT_MOD_NSEC,
            path: "ext.txt".to_string(),
            size: 100,
            mtime: 1_700_000_000,
            mtime_nsec: Some(0),
            mode: 0o100_644,
            uid: Some(1000),
            uid_name: Some("u".to_string()),
            gid: Some(1000),
            gid_name: None,
            checksum: vec![],
        };
        let mut opts = FileListDecodeOptions::frozen_oracle_default();
        opts.xfer_flags_as_varint = false;
        opts.always_checksum = false;
        opts.csum_len = 0;
        opts.previous_name = None;
        assert_flist_entry_round_trip(entry, &opts);
    }

    #[test]
    fn encode_file_list_terminator_round_trip_varint_mode() {
        let mut opts = FileListDecodeOptions::frozen_oracle_default();
        opts.xfer_flags_as_varint = true;
        let bytes = encode_file_list_terminator(&opts);
        assert_eq!(bytes, vec![0x00]);
        let (outcome, consumed) = decode_file_list_entry(&bytes, &opts).unwrap();
        assert!(matches!(
            outcome,
            FileListDecodeOutcome::EndOfList { io_error: 0 }
        ));
        assert_eq!(consumed, 1);
    }

    #[test]
    fn encode_file_list_terminator_round_trip_classic_mode() {
        let mut opts = FileListDecodeOptions::frozen_oracle_default();
        opts.xfer_flags_as_varint = false;
        let bytes = encode_file_list_terminator(&opts);
        assert_eq!(bytes, vec![0x00]);
    }

    #[test]
    fn compute_flist_name_split_handles_disjoint_paths() {
        let (l1, suffix) = compute_flist_name_split("foo.txt", Some("bar.txt"), true);
        assert_eq!(l1, 0);
        assert_eq!(suffix, b"foo.txt");
    }

    #[test]
    fn compute_flist_name_split_caps_at_255_bytes() {
        let prev = "x".repeat(300);
        let entry = "x".repeat(300);
        let (l1, suffix) = compute_flist_name_split(&entry, Some(&prev), true);
        assert_eq!(l1, 255);
        assert_eq!(suffix.len(), 45);
    }

    // -------------------------------------------------------------------------
    // Sinergia 8i-encode — Fase 3: delta op + delta stream round-trip.
    // -------------------------------------------------------------------------

    fn round_trip_delta_op(op: DeltaOp, expected_form: &'static str) {
        let mut enc_state = DeltaStreamState::new();
        let bytes = encode_delta_op(&op, &mut enc_state);
        // Pin which form the encoder selected.
        let form = match bytes[0] {
            b if b == TOKEN_END_FLAG => "END_FLAG",
            b if (b & 0xC0) == TOKEN_DEFLATED_DATA => "DEFLATED_DATA",
            b if (b & 0xC0) == TOKENRUN_REL => "TOKENRUN_REL",
            b if (b & 0xC0) == TOKEN_REL => "TOKEN_REL",
            TOKENRUN_LONG => "TOKENRUN_LONG",
            TOKEN_LONG => "TOKEN_LONG",
            _ => "UNKNOWN",
        };
        assert_eq!(form, expected_form, "encoder picked the wrong wire form");

        let mut dec_state = DeltaStreamState::new();
        let (outcome, consumed) = decode_delta_op(&bytes, &mut dec_state).unwrap();
        assert_eq!(
            consumed,
            bytes.len(),
            "decoder did not consume entire encoded slice"
        );
        assert_eq!(
            enc_state, dec_state,
            "state divergence between encoder and decoder"
        );
        match (op, outcome) {
            (a, DeltaOpOutcome::Op(b)) => assert_eq!(a, b, "op round-trip drift"),
            _ => panic!("unexpected outcome"),
        }
    }

    #[test]
    fn encode_delta_op_picks_token_rel_for_small_offset_run_one() {
        round_trip_delta_op(
            DeltaOp::CopyRun {
                start_token_index: 5,
                run_length: 1,
            },
            "TOKEN_REL",
        );
    }

    #[test]
    fn encode_delta_op_picks_tokenrun_rel_for_small_offset_run_many() {
        round_trip_delta_op(
            DeltaOp::CopyRun {
                start_token_index: 10,
                run_length: 7,
            },
            "TOKENRUN_REL",
        );
    }

    #[test]
    fn encode_delta_op_picks_token_long_for_large_offset_run_one() {
        round_trip_delta_op(
            DeltaOp::CopyRun {
                start_token_index: 100_000,
                run_length: 1,
            },
            "TOKEN_LONG",
        );
    }

    #[test]
    fn encode_delta_op_picks_tokenrun_long_for_large_offset_run_many() {
        round_trip_delta_op(
            DeltaOp::CopyRun {
                start_token_index: 100_000,
                run_length: 17,
            },
            "TOKENRUN_LONG",
        );
    }

    #[test]
    fn encode_delta_op_literal_fits_in_two_byte_header() {
        round_trip_delta_op(
            DeltaOp::Literal {
                compressed_payload: vec![0xAB; 100],
            },
            "DEFLATED_DATA",
        );
    }

    #[test]
    fn encode_delta_op_literal_at_max_size_round_trips() {
        round_trip_delta_op(
            DeltaOp::Literal {
                compressed_payload: vec![0xCD; MAX_DELTA_LITERAL_LEN],
            },
            "DEFLATED_DATA",
        );
    }

    #[test]
    #[should_panic(expected = "outside valid 1..=16383 range")]
    fn encode_delta_op_literal_zero_length_panics_caller_error() {
        let mut state = DeltaStreamState::new();
        encode_delta_op(
            &DeltaOp::Literal {
                compressed_payload: Vec::new(),
            },
            &mut state,
        );
    }

    #[test]
    #[should_panic(expected = "outside valid 1..=16383 range")]
    fn encode_delta_op_literal_overlong_panics_caller_error() {
        let mut state = DeltaStreamState::new();
        encode_delta_op(
            &DeltaOp::Literal {
                compressed_payload: vec![0; MAX_DELTA_LITERAL_LEN + 1],
            },
            &mut state,
        );
    }

    #[test]
    fn encode_delta_op_state_advances_to_run_end() {
        let mut state = DeltaStreamState::new();
        let _ = encode_delta_op(
            &DeltaOp::CopyRun {
                start_token_index: 50,
                run_length: 10,
            },
            &mut state,
        );
        assert_eq!(state.last_run_end(), 60);
    }

    #[test]
    fn encode_delta_op_chain_of_relative_ops_uses_correct_baselines() {
        let mut state = DeltaStreamState::new();
        // First: rel 0, run 1 => last_run_end = 1
        let bytes = encode_delta_op(
            &DeltaOp::CopyRun {
                start_token_index: 0,
                run_length: 1,
            },
            &mut state,
        );
        assert_eq!(bytes, vec![TOKEN_REL]);
        assert_eq!(state.last_run_end(), 1);
        // Second: rel 5, run 1 => start_token_index = 6, last_run_end = 7
        let bytes = encode_delta_op(
            &DeltaOp::CopyRun {
                start_token_index: 6,
                run_length: 1,
            },
            &mut state,
        );
        assert_eq!(bytes, vec![TOKEN_REL | 5]);
        assert_eq!(state.last_run_end(), 7);
        // Third: same relative op decoded should match symmetric position.
    }

    #[test]
    fn encode_delta_stream_full_round_trip_matches_decoder() {
        let report = DeltaStreamReport {
            ops: vec![
                DeltaOp::Literal {
                    compressed_payload: vec![0x01; 10],
                },
                DeltaOp::CopyRun {
                    start_token_index: 0,
                    run_length: 3,
                },
                DeltaOp::CopyRun {
                    start_token_index: 100,
                    run_length: 1,
                },
                DeltaOp::Literal {
                    compressed_payload: vec![0x02; 50],
                },
                DeltaOp::CopyRun {
                    start_token_index: 200_000,
                    run_length: 5,
                },
            ],
            file_checksum: vec![0xFE; 16],
        };
        let bytes = encode_delta_stream(&report);
        let (decoded, consumed) = decode_delta_stream(&bytes, 16, None).unwrap();
        assert_eq!(consumed, bytes.len());
        assert_eq!(decoded, report);
    }

    // -------------------------------------------------------------------------
    // Sinergia 8i-encode — Fase 4: zstd literal stream compress round-trip.
    // -------------------------------------------------------------------------

    #[test]
    fn compress_zstd_literal_stream_empty_input_yields_empty_output() {
        let blobs = compress_zstd_literal_stream(&[]).unwrap();
        assert!(blobs.is_empty());
    }

    #[test]
    fn compress_zstd_literal_stream_skips_empty_payloads_silently() {
        // Mirrors token.c:691 `if (nb)` guard — empty payloads do not
        // produce DEFLATED_DATA records.
        let blobs = compress_zstd_literal_stream(&[&[][..], &[][..]]).unwrap();
        assert!(blobs.is_empty());
    }

    #[test]
    fn compress_zstd_literal_stream_single_payload_round_trip_matches_input() {
        let original = b"hello rsync zstd literal stream payload!".repeat(8);
        let blobs = compress_zstd_literal_stream(&[&original[..]]).unwrap();
        assert_eq!(blobs.len(), 1);
        assert!(!blobs[0].is_empty(), "compressed output must be non-empty");
        // First blob carries the ZSTD frame magic 28 B5 2F FD.
        assert_eq!(
            &blobs[0][..4],
            &[0x28, 0xB5, 0x2F, 0xFD],
            "first blob must begin with ZSTD frame magic"
        );
        // Round-trip via the decoder (also session-wide).
        let blob_refs: Vec<&[u8]> = blobs.iter().map(|v| v.as_slice()).collect();
        let decoded = decompress_zstd_literal_stream(&blob_refs).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn compress_zstd_literal_stream_multi_payload_round_trip_concatenates_input() {
        // Two separate payloads through the same session -> one
        // streaming context across both. After decompression the
        // concatenation MUST equal the input concatenation.
        let p1 = b"alpha alpha alpha alpha alpha alpha alpha alpha alpha".repeat(4);
        let p2 = b"beta beta beta beta beta beta beta beta beta beta beta".repeat(4);
        let blobs = compress_zstd_literal_stream(&[&p1[..], &p2[..]]).unwrap();
        assert_eq!(blobs.len(), 2, "one blob per non-empty input payload");
        // First blob carries the magic, second is a continuation block.
        assert_eq!(&blobs[0][..4], &[0x28, 0xB5, 0x2F, 0xFD]);
        assert_ne!(
            &blobs[1][..4],
            &[0x28, 0xB5, 0x2F, 0xFD],
            "second blob is a continuation, no fresh frame magic"
        );

        let blob_refs: Vec<&[u8]> = blobs.iter().map(|v| v.as_slice()).collect();
        let decoded = decompress_zstd_literal_stream(&blob_refs).unwrap();
        let mut expected = p1.clone();
        expected.extend_from_slice(&p2);
        assert_eq!(decoded, expected);
    }

    #[test]
    fn compress_zstd_literal_stream_each_payload_is_independently_decodable_in_session() {
        // HARDENING — pin the per-payload boundary semantics: feeding
        // ONLY the first compressed blob to the session decoder must
        // yield exactly the first payload's bytes (because we flushed
        // at the boundary). A drift to ZSTD_e_continue (no flush)
        // would make the first blob undecodable on its own.
        let p1: Vec<u8> = (0..2_000_u32).map(|i| (i % 251) as u8).collect();
        let p2: Vec<u8> = (0..1_500_u32).map(|i| ((i + 7) % 251) as u8).collect();
        let blobs = compress_zstd_literal_stream(&[&p1[..], &p2[..]]).unwrap();
        assert_eq!(blobs.len(), 2);

        // Decode just the first blob.
        let only_first = decompress_zstd_literal_stream(&[&blobs[0][..]]).unwrap();
        assert_eq!(only_first, p1, "first blob must decode to p1 in isolation");
    }

    // ==========================================================================
    // Sinergia 8i — MuxStreamReader (streaming demuxer + progress counter)
    // ==========================================================================

    use crate::rsync_native_proto::events::NativeRsyncEvent;

    fn frame(tag: MuxTag, payload: &[u8]) -> Vec<u8> {
        let hdr = MuxHeader {
            tag,
            length: payload.len() as u32,
        };
        let mut out = hdr.encode().to_vec();
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn mux_stream_reader_empty_yields_none() {
        let mut r = MuxStreamReader::new();
        assert!(r.poll_frame().is_none());
        assert_eq!(r.data_bytes_consumed(), 0);
    }

    #[test]
    fn mux_stream_reader_partial_header_yields_none() {
        let mut r = MuxStreamReader::new();
        r.feed(&[0x07]); // 1 byte of header
        assert!(r.poll_frame().is_none());
        assert_eq!(r.buffered(), 1);
    }

    #[test]
    fn mux_stream_reader_partial_payload_yields_none() {
        let mut r = MuxStreamReader::new();
        let mut frm = frame(MuxTag::Data, b"hello world");
        // Feed header + only part of payload.
        frm.truncate(MUX_HEADER_LEN + 5);
        r.feed(&frm);
        assert!(r.poll_frame().is_none());
        assert_eq!(r.buffered(), MUX_HEADER_LEN + 5);
        assert_eq!(r.data_bytes_consumed(), 0);
    }

    #[test]
    fn mux_stream_reader_pops_single_data_frame_and_counts_payload() {
        let mut r = MuxStreamReader::new();
        r.feed(&frame(MuxTag::Data, b"abc"));
        let got = r.poll_frame().unwrap().unwrap();
        assert_eq!(got, MuxPoll::Data(b"abc".to_vec()));
        assert_eq!(r.data_bytes_consumed(), 3);
        assert_eq!(r.buffered(), 0);
        assert!(r.poll_frame().is_none());
    }

    #[test]
    fn mux_stream_reader_counter_excludes_oob_payload() {
        let mut r = MuxStreamReader::new();
        r.feed(&frame(MuxTag::Info, b"chatter"));
        r.feed(&frame(MuxTag::Data, b"real"));
        // First poll: OOB Info (non-terminal)
        match r.poll_frame().unwrap().unwrap() {
            MuxPoll::Oob(NativeRsyncEvent::Info { message }) => {
                assert_eq!(message, "chatter");
            }
            other => panic!("expected Oob(Info), got {other:?}"),
        }
        assert_eq!(r.data_bytes_consumed(), 0, "OOB must NOT count");
        // Second poll: Data
        match r.poll_frame().unwrap().unwrap() {
            MuxPoll::Data(d) => assert_eq!(d, b"real"),
            other => panic!("expected Data, got {other:?}"),
        }
        assert_eq!(r.data_bytes_consumed(), 4);
    }

    #[test]
    fn mux_stream_reader_feeds_chunks_across_frame_boundary() {
        // Split one frame across two feeds — must still pop correctly.
        let mut r = MuxStreamReader::new();
        let frm = frame(MuxTag::Data, b"chunked_payload");
        let mid = MUX_HEADER_LEN + 3; // header + first 3 bytes of payload
        r.feed(&frm[..mid]);
        assert!(r.poll_frame().is_none());
        r.feed(&frm[mid..]);
        match r.poll_frame().unwrap().unwrap() {
            MuxPoll::Data(d) => assert_eq!(d, b"chunked_payload"),
            other => panic!("expected Data, got {other:?}"),
        }
        assert_eq!(r.data_bytes_consumed(), 15);
    }

    #[test]
    fn mux_stream_reader_multiple_frames_in_one_chunk() {
        let mut r = MuxStreamReader::new();
        let mut big = Vec::new();
        big.extend_from_slice(&frame(MuxTag::Data, b"one"));
        big.extend_from_slice(&frame(MuxTag::Data, b"two"));
        big.extend_from_slice(&frame(MuxTag::Info, b"info"));
        big.extend_from_slice(&frame(MuxTag::Data, b"three"));
        r.feed(&big);
        // Pop in order.
        match r.poll_frame().unwrap().unwrap() {
            MuxPoll::Data(d) => assert_eq!(d, b"one"),
            other => panic!("{other:?}"),
        }
        match r.poll_frame().unwrap().unwrap() {
            MuxPoll::Data(d) => assert_eq!(d, b"two"),
            other => panic!("{other:?}"),
        }
        match r.poll_frame().unwrap().unwrap() {
            MuxPoll::Oob(NativeRsyncEvent::Info { .. }) => {}
            other => panic!("{other:?}"),
        }
        match r.poll_frame().unwrap().unwrap() {
            MuxPoll::Data(d) => assert_eq!(d, b"three"),
            other => panic!("{other:?}"),
        }
        assert!(r.poll_frame().is_none());
        assert_eq!(r.data_bytes_consumed(), 3 + 3 + 5);
    }

    #[test]
    fn mux_stream_reader_terminal_locks_further_polls_hardening() {
        // HARDENING — after a terminal event, even if the buffer holds a
        // complete subsequent Data frame, `poll_frame` MUST return None.
        // The driver cannot be allowed to process app-stream bytes that
        // arrived after the remote bailed. Mirrors the bail semantics of
        // `reassemble_until_terminal`.
        let mut r = MuxStreamReader::new();
        r.feed(&frame(MuxTag::Data, b"before"));
        r.feed(&frame(MuxTag::Error, b"remote boom\n"));
        r.feed(&frame(MuxTag::Data, b"after-bail"));

        // Data before terminal
        match r.poll_frame().unwrap().unwrap() {
            MuxPoll::Data(d) => assert_eq!(d, b"before"),
            other => panic!("{other:?}"),
        }
        // Terminal
        match r.poll_frame().unwrap().unwrap() {
            MuxPoll::Terminal(NativeRsyncEvent::Error { message }) => {
                assert_eq!(message, "remote boom");
            }
            other => panic!("{other:?}"),
        }
        // Lock: even though buffer still holds a Data frame, poll returns None.
        assert!(r.terminal_seen());
        assert!(r.poll_frame().is_none());
        assert!(r.buffered() > 0, "buffer residue preserved for inspection");
        assert_eq!(
            r.data_bytes_consumed(),
            6,
            "counter frozen at terminal boundary"
        );
    }

    #[test]
    fn mux_stream_reader_malformed_header_returns_error() {
        let mut r = MuxStreamReader::new();
        // High byte below MPLEX_BASE (7) — invalid per `MuxHeader::decode`.
        r.feed(&[0x00, 0x00, 0x00, 0x00]);
        match r.poll_frame().unwrap() {
            Err(RealWireError::InvalidMuxHeader { .. }) => {}
            other => panic!("expected InvalidMuxHeader, got {other:?}"),
        }
    }

    #[test]
    fn mux_stream_reader_counter_monotone_under_interleaved_feed_poll() {
        // HARDENING — stress: interleave small feeds and polls. Counter
        // must be monotone and match the sum of MSG_DATA payload lengths.
        let mut r = MuxStreamReader::new();
        let mut big = Vec::new();
        let mut expected = 0u64;
        for i in 0..20 {
            let payload: Vec<u8> = (0..=i).map(|b| b as u8).collect();
            big.extend_from_slice(&frame(MuxTag::Data, &payload));
            expected += payload.len() as u64;
            // Sprinkle a non-terminal OOB between data frames.
            if i % 3 == 0 {
                big.extend_from_slice(&frame(MuxTag::Info, b"tick"));
            }
        }
        // Feed in tiny 7-byte chunks and poll after each feed.
        let mut last_counter = 0u64;
        for chunk in big.chunks(7) {
            r.feed(chunk);
            while let Some(result) = r.poll_frame() {
                let _ = result.unwrap();
                let cur = r.data_bytes_consumed();
                assert!(cur >= last_counter, "counter went backwards");
                last_counter = cur;
            }
        }
        assert_eq!(r.data_bytes_consumed(), expected);
    }

    #[test]
    fn mux_stream_reader_error_exit_zero_is_oob_not_terminal() {
        // Regression: ErrorExit with code 0 is non-terminal per events.rs
        // policy. MuxStreamReader MUST classify it as Oob, not Terminal —
        // otherwise a cleanup signal would wrongly lock the reader.
        let mut r = MuxStreamReader::new();
        r.feed(&frame(MuxTag::ErrorExit, &[0u8, 0, 0, 0]));
        match r.poll_frame().unwrap().unwrap() {
            MuxPoll::Oob(NativeRsyncEvent::ErrorExit { code: Some(0) }) => {}
            other => panic!("expected Oob(ErrorExit(0)), got {other:?}"),
        }
        assert!(!r.terminal_seen());
    }
}
