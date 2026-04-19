//! Bridge definitions between the native rsync protocol layer and the existing
//! algorithmic delta engine.
//!
//! Sinergia 2 introduced the type-level bridges (`From<ProtocolSignatureBlock>
//! for EngineSignatureBlock`, `TryFrom<ProtocolDeltaInstruction> for
//! EngineDeltaOp`). Sinergia 4 fills in the behavior:
//!
//!   - inverse conversions (engine → protocol) so the receiver side can
//!     publish locally-computed signatures to the wire, and so a locally-
//!     computed engine delta plan can be serialised for upload
//!   - `DeltaEngineAdapter` implemented on `CurrentDeltaSyncBridge` by
//!     delegating to `crate::delta_sync`, the production delta engine
//!
//! Separation of concerns: this module DOES translate between prototype
//! types and the production engine, but DOES NOT replicate any algorithm.
//! If `delta_sync.rs` changes, only the small conversion block here needs
//! maintenance — the rest of the prototype stays stable.

use crate::delta_sync;
use crate::rsync_native_proto::protocol::{
    DeltaInstruction as ProtocolDeltaInstruction, SignatureBlock as ProtocolSignatureBlock,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineSignatureBlock {
    pub index: u32,
    pub rolling: u32,
    pub strong: [u8; 32],
    pub block_len: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineDeltaOp {
    CopyBlock(u32),
    Literal(Vec<u8>),
}

// --- protocol → engine (Sinergia 2) ---------------------------------------

impl From<ProtocolSignatureBlock> for EngineSignatureBlock {
    fn from(sb: ProtocolSignatureBlock) -> Self {
        Self {
            index: sb.index,
            rolling: sb.rolling,
            strong: sb.strong,
            block_len: sb.block_len,
        }
    }
}

/// Errors that can arise converting a wire-level `DeltaInstruction` into an
/// engine-level `EngineDeltaOp`.
///
/// The wire protocol has one extra variant (`EndOfFile`) that marks the end of
/// the delta instruction stream. The engine plan represents end-of-stream
/// implicitly via the length of `EngineDeltaPlan::ops`, so `EndOfFile` has no
/// engine counterpart and must be consumed by the caller as a framing marker
/// rather than converted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeltaInstructionConversionError {
    /// Tried to convert `DeltaInstruction::EndOfFile`, which is a framing
    /// marker and not a delta operation the engine can execute.
    EndOfFileIsFramingMarker,
}

impl std::fmt::Display for DeltaInstructionConversionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EndOfFileIsFramingMarker => f.write_str(
                "DeltaInstruction::EndOfFile is a framing marker; \
                 drain it as end-of-stream instead of converting",
            ),
        }
    }
}

impl std::error::Error for DeltaInstructionConversionError {}

impl TryFrom<ProtocolDeltaInstruction> for EngineDeltaOp {
    type Error = DeltaInstructionConversionError;

    fn try_from(di: ProtocolDeltaInstruction) -> Result<Self, Self::Error> {
        match di {
            ProtocolDeltaInstruction::CopyBlock { index } => Ok(Self::CopyBlock(index)),
            ProtocolDeltaInstruction::Literal { data } => Ok(Self::Literal(data)),
            ProtocolDeltaInstruction::EndOfFile => {
                Err(DeltaInstructionConversionError::EndOfFileIsFramingMarker)
            }
        }
    }
}

// --- engine → protocol (Sinergia 4) ---------------------------------------

impl From<EngineSignatureBlock> for ProtocolSignatureBlock {
    fn from(eb: EngineSignatureBlock) -> Self {
        Self {
            index: eb.index,
            rolling: eb.rolling,
            strong: eb.strong,
            block_len: eb.block_len,
        }
    }
}

impl From<EngineDeltaOp> for ProtocolDeltaInstruction {
    fn from(op: EngineDeltaOp) -> Self {
        match op {
            EngineDeltaOp::CopyBlock(i) => Self::CopyBlock { index: i },
            EngineDeltaOp::Literal(d) => Self::Literal { data: d },
        }
    }
}

/// Wrap a complete engine delta stream into a wire-ready sequence of
/// `DeltaInstruction`s terminated by `EndOfFile` — which is required by the
/// driver's pre-flight validation on `UploadPlan.delta_instructions`.
pub fn engine_ops_to_wire(ops: Vec<EngineDeltaOp>) -> Vec<ProtocolDeltaInstruction> {
    let mut out: Vec<ProtocolDeltaInstruction> = ops
        .into_iter()
        .map(ProtocolDeltaInstruction::from)
        .collect();
    out.push(ProtocolDeltaInstruction::EndOfFile);
    out
}

// --- engine ↔ delta_sync::BlockSignature ----------------------------------
// These conversions stay private to the module: the real engine type name
// and field shape is an implementation detail the rest of the prototype
// should not have to know.

impl From<delta_sync::BlockSignature> for EngineSignatureBlock {
    fn from(bs: delta_sync::BlockSignature) -> Self {
        Self {
            index: bs.index,
            rolling: bs.rolling,
            strong: bs.strong,
            block_len: bs.size,
        }
    }
}

impl From<&EngineSignatureBlock> for delta_sync::BlockSignature {
    fn from(eb: &EngineSignatureBlock) -> Self {
        Self {
            index: eb.index,
            rolling: eb.rolling,
            strong: eb.strong,
            size: eb.block_len,
        }
    }
}

impl From<delta_sync::DeltaOp> for EngineDeltaOp {
    fn from(op: delta_sync::DeltaOp) -> Self {
        match op {
            delta_sync::DeltaOp::CopyBlock(i) => Self::CopyBlock(i),
            delta_sync::DeltaOp::Literal(d) => Self::Literal(d),
        }
    }
}

// --- engine plan + adapter trait ------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct EngineDeltaPlan {
    pub ops: Vec<EngineDeltaOp>,
    pub copy_blocks: u32,
    pub literal_bytes: u64,
    /// Sum of literal bytes plus instruction overhead. Not the same as
    /// `literal_bytes`: lets callers size down buffers and decide whether
    /// the delta is worth streaming at all.
    pub total_delta_bytes: u64,
    /// 1.0 means "native delta is as large as full copy". Higher is better.
    pub savings_ratio: f64,
    /// Engine's own recommendation: `true` iff `savings_ratio` crosses its
    /// internal threshold (currently 0.20 of file size).
    pub should_use_delta: bool,
}

pub trait DeltaEngineAdapter: Send + Sync {
    fn compute_block_size(&self, file_size: u64) -> usize;

    fn build_signatures(
        &self,
        destination_data: &[u8],
        block_size: usize,
    ) -> Vec<EngineSignatureBlock>;

    fn compute_delta(
        &self,
        source_data: &[u8],
        destination_signatures: &[EngineSignatureBlock],
        block_size: usize,
    ) -> EngineDeltaPlan;

    /// Reconstruct a file from the destination data and a delta instruction
    /// stream. Used by the download-with-engine driver path. `block_size` is
    /// the block size the signatures were computed with.
    ///
    /// Returns the raw underlying engine error as a string — the driver wraps
    /// it into a typed `NativeRsyncError::InvalidFrame` before returning to
    /// callers.
    fn apply_delta(
        &self,
        destination_data: &[u8],
        ops: &[EngineDeltaOp],
        block_size: usize,
    ) -> Result<Vec<u8>, String>;
}

/// The production bridge: delegates every call to `crate::delta_sync`.
#[derive(Debug, Default)]
pub struct CurrentDeltaSyncBridge;

impl CurrentDeltaSyncBridge {
    pub fn new() -> Self {
        Self
    }
}

impl DeltaEngineAdapter for CurrentDeltaSyncBridge {
    fn compute_block_size(&self, file_size: u64) -> usize {
        delta_sync::compute_block_size(file_size)
    }

    fn build_signatures(
        &self,
        destination_data: &[u8],
        block_size: usize,
    ) -> Vec<EngineSignatureBlock> {
        let table = delta_sync::compute_signatures(destination_data, block_size);
        table.signatures.into_iter().map(Into::into).collect()
    }

    fn compute_delta(
        &self,
        source_data: &[u8],
        destination_signatures: &[EngineSignatureBlock],
        block_size: usize,
    ) -> EngineDeltaPlan {
        // Reconstruct the engine's SignatureTable from the engine-form input.
        // `file_size` is recovered as the sum of per-block lengths — which is
        // exact because `delta_sync::compute_signatures` always produces full
        // `block_size` blocks except for a possibly shorter tail.
        let signatures: Vec<delta_sync::BlockSignature> = destination_signatures
            .iter()
            .map(delta_sync::BlockSignature::from)
            .collect();
        let file_size: u64 = destination_signatures
            .iter()
            .map(|s| s.block_len as u64)
            .sum();
        let table = delta_sync::SignatureTable {
            block_size,
            file_size,
            signatures,
        };
        let (ops, result) = delta_sync::compute_delta(source_data, &table);
        EngineDeltaPlan {
            ops: ops.into_iter().map(Into::into).collect(),
            copy_blocks: result.copy_blocks,
            literal_bytes: result.literal_bytes,
            total_delta_bytes: result.total_delta_bytes,
            savings_ratio: result.savings_ratio,
            should_use_delta: result.should_use_delta,
        }
    }

    fn apply_delta(
        &self,
        destination_data: &[u8],
        ops: &[EngineDeltaOp],
        block_size: usize,
    ) -> Result<Vec<u8>, String> {
        // Convert prototype ops back to engine ops, then delegate.
        let wire_ops: Vec<delta_sync::DeltaOp> = ops
            .iter()
            .cloned()
            .map(|op| match op {
                EngineDeltaOp::CopyBlock(i) => delta_sync::DeltaOp::CopyBlock(i),
                EngineDeltaOp::Literal(b) => delta_sync::DeltaOp::Literal(b),
            })
            .collect();
        delta_sync::apply_delta(destination_data, &wire_ops, block_size)
    }
}
