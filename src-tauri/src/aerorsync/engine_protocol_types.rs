//! Minimal protocol-shaped types retained after Y-RSC.8 (RSNP stack retirement).
//!
//! The legacy RSNP envelope codec, Hello, and frame framing lived in
//! `protocol.rs` and were deleted with the SessionDriver/server stack.
//! `engine_adapter` still needs these two shapes for the
//! protocol-engine `From` / `TryFrom` bridges used by unit tests and by
//! any residual wire-planning helpers (`engine_ops_to_wire`).

/// Destination-side signature block as presented to the engine bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureBlock {
    pub index: u32,
    pub rolling: u32,
    pub strong: [u8; 32],
    pub block_len: u32,
}

/// Wire-level delta instruction stream, including the framing-only
/// `EndOfFile` terminator that has no engine counterpart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeltaInstruction {
    CopyBlock { index: u32 },
    Literal { data: Vec<u8> },
    EndOfFile,
}
