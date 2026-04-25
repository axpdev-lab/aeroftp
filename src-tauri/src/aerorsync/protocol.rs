//! Wire-protocol types and frame codec for the Strada C native rsync prototype.
//!
//! The on-the-wire rsync protocol 31 format is NOT implemented here yet. This
//! module owns a small self-consistent envelope used for prototype tests and
//! mock-transport round trips. The intent is to decouple message shapes from
//! real wire framing until the capture-driven compatibility work starts.
//!
//! Envelope format (big-endian):
//!
//! ```text
//!   offset  size  field
//!   0       4     magic   b"RSNP"
//!   4       1     envelope_version = 1
//!   5       1     msg_type (see MessageType)
//!   6       2     flags   (bit 0 = sender, bit 1 = receiver)
//!   8       4     payload_len
//!   12      N     payload (JSON-encoded message body)
//! ```
//!
//! The payload is currently JSON for test readability. A later step replaces
//! the payload codec with the real rsync binary shape while keeping the
//! envelope as a thin dev-only wrapper.

use serde::{Deserialize, Serialize};

use crate::aerorsync::types::{AerorsyncError, FeatureFlag, ProtocolVersion, SessionRole};

pub const FRAME_MAGIC: [u8; 4] = *b"RSNP";
pub const ENVELOPE_VERSION: u8 = 1;
pub const FRAME_HEADER_SIZE: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    Hello = 0x01,
    FileMetadata = 0x02,
    SignatureBatch = 0x03,
    DeltaBatch = 0x04,
    Summary = 0x05,
    Error = 0x06,
    Done = 0x07,
}

impl MessageType {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x01 => Some(Self::Hello),
            0x02 => Some(Self::FileMetadata),
            0x03 => Some(Self::SignatureBatch),
            0x04 => Some(Self::DeltaBatch),
            0x05 => Some(Self::Summary),
            0x06 => Some(Self::Error),
            0x07 => Some(Self::Done),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelloMessage {
    pub protocol: ProtocolVersion,
    pub role: SessionRole,
    pub features: Vec<FeatureFlag>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileMetadataMessage {
    pub path: String,
    pub size: u64,
    pub mode: u32,
    pub modified_unix_secs: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignatureBlock {
    pub index: u32,
    pub rolling: u32,
    #[serde(with = "serde_bytes_32")]
    pub strong: [u8; 32],
    pub block_len: u32,
}

/// Wire-level signature batch. Carries the `block_size` the destination side
/// used so the consumer can recompute a delta without guessing from block
/// lengths. Required for the engine-mode driver path, which feeds
/// `block_size` straight into `DeltaEngineAdapter::compute_delta`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignatureBatchMessage {
    pub block_size: u32,
    pub blocks: Vec<SignatureBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DeltaInstruction {
    CopyBlock { index: u32 },
    Literal { data: Vec<u8> },
    EndOfFile,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SummaryMessage {
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub literal_bytes: u64,
    pub matched_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorMessage {
    pub code: u16,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum WireMessage {
    Hello(HelloMessage),
    FileMetadata(FileMetadataMessage),
    SignatureBatch(SignatureBatchMessage),
    DeltaBatch(Vec<DeltaInstruction>),
    Summary(SummaryMessage),
    Error(ErrorMessage),
    Done,
}

impl WireMessage {
    pub fn message_type(&self) -> MessageType {
        match self {
            WireMessage::Hello(_) => MessageType::Hello,
            WireMessage::FileMetadata(_) => MessageType::FileMetadata,
            WireMessage::SignatureBatch(_) => MessageType::SignatureBatch,
            WireMessage::DeltaBatch(_) => MessageType::DeltaBatch,
            WireMessage::Summary(_) => MessageType::Summary,
            WireMessage::Error(_) => MessageType::Error,
            WireMessage::Done => MessageType::Done,
        }
    }

    pub fn role_flag_bits(&self) -> u16 {
        match self {
            WireMessage::Hello(h) => h.role.as_flag_bit(),
            _ => 0,
        }
    }
}

pub trait FrameCodec {
    fn encode(&self, message: &WireMessage) -> Result<Vec<u8>, AerorsyncError>;
    fn decode(&self, raw: &[u8]) -> Result<WireMessage, AerorsyncError>;
}

#[derive(Debug, Clone)]
pub struct AerorsyncFrameCodec {
    pub max_frame_size: usize,
}

impl AerorsyncFrameCodec {
    pub fn new(max_frame_size: usize) -> Self {
        Self { max_frame_size }
    }

    fn validate_header_slice(
        &self,
        raw: &[u8],
    ) -> Result<(usize, MessageType, u16), AerorsyncError> {
        if raw.len() < FRAME_HEADER_SIZE {
            return Err(AerorsyncError::invalid_frame(format!(
                "frame too short: {} bytes (need at least {})",
                raw.len(),
                FRAME_HEADER_SIZE
            )));
        }
        if raw[0..4] != FRAME_MAGIC {
            return Err(AerorsyncError::invalid_frame(
                "bad magic: not an RSNP envelope",
            ));
        }
        if raw[4] != ENVELOPE_VERSION {
            return Err(AerorsyncError::unsupported_version(format!(
                "envelope version {} not supported (expected {})",
                raw[4], ENVELOPE_VERSION
            )));
        }
        let msg_type = MessageType::from_u8(raw[5]).ok_or_else(|| {
            AerorsyncError::invalid_frame(format!("unknown message type byte: {:#x}", raw[5]))
        })?;
        let flags = u16::from_be_bytes([raw[6], raw[7]]);
        let payload_len = u32::from_be_bytes([raw[8], raw[9], raw[10], raw[11]]) as usize;

        if FRAME_HEADER_SIZE + payload_len > self.max_frame_size {
            return Err(AerorsyncError::invalid_frame(format!(
                "frame size {} exceeds max {}",
                FRAME_HEADER_SIZE + payload_len,
                self.max_frame_size
            )));
        }
        if raw.len() < FRAME_HEADER_SIZE + payload_len {
            return Err(AerorsyncError::invalid_frame(format!(
                "truncated frame: declared {} bytes of payload but got {}",
                payload_len,
                raw.len().saturating_sub(FRAME_HEADER_SIZE)
            )));
        }
        Ok((payload_len, msg_type, flags))
    }

    fn write_header(
        &self,
        msg: &WireMessage,
        payload_len: usize,
    ) -> Result<[u8; FRAME_HEADER_SIZE], AerorsyncError> {
        if FRAME_HEADER_SIZE + payload_len > self.max_frame_size {
            return Err(AerorsyncError::invalid_frame(format!(
                "outgoing frame size {} exceeds max {}",
                FRAME_HEADER_SIZE + payload_len,
                self.max_frame_size
            )));
        }
        let mut hdr = [0u8; FRAME_HEADER_SIZE];
        hdr[0..4].copy_from_slice(&FRAME_MAGIC);
        hdr[4] = ENVELOPE_VERSION;
        hdr[5] = msg.message_type() as u8;
        hdr[6..8].copy_from_slice(&msg.role_flag_bits().to_be_bytes());
        let len_bytes = (payload_len as u32).to_be_bytes();
        hdr[8..12].copy_from_slice(&len_bytes);
        Ok(hdr)
    }
}

impl FrameCodec for AerorsyncFrameCodec {
    fn encode(&self, message: &WireMessage) -> Result<Vec<u8>, AerorsyncError> {
        let payload = serde_json::to_vec(message).map_err(|e| {
            AerorsyncError::invalid_frame(format!("payload serialization failed: {e}"))
        })?;
        let hdr = self.write_header(message, payload.len())?;
        let mut out = Vec::with_capacity(FRAME_HEADER_SIZE + payload.len());
        out.extend_from_slice(&hdr);
        out.extend_from_slice(&payload);
        Ok(out)
    }

    fn decode(&self, raw: &[u8]) -> Result<WireMessage, AerorsyncError> {
        let (payload_len, msg_type, _flags) = self.validate_header_slice(raw)?;
        let payload = &raw[FRAME_HEADER_SIZE..FRAME_HEADER_SIZE + payload_len];
        let msg: WireMessage = serde_json::from_slice(payload).map_err(|e| {
            AerorsyncError::invalid_frame(format!("payload deserialization failed: {e}"))
        })?;
        if msg.message_type() != msg_type {
            return Err(AerorsyncError::invalid_frame(format!(
                "header type {:?} does not match payload type {:?}",
                msg_type,
                msg.message_type()
            )));
        }
        Ok(msg)
    }
}

// 32-byte array serde helper (serde doesn't derive for fixed-size > 32 by default
// but 32 works on recent versions; we keep this explicit for portability).
mod serde_bytes_32 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(bytes)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let v = <Vec<u8>>::deserialize(d)?;
        if v.len() != 32 {
            return Err(serde::de::Error::custom(format!(
                "expected 32 bytes, got {}",
                v.len()
            )));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&v);
        Ok(out)
    }
}
