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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};

use crate::aerorsync::protocol::{
    DeltaInstruction as ProtocolDeltaInstruction, SignatureBlock as ProtocolSignatureBlock,
};
use crate::delta_sync;
use crate::delta_sync::{strong_hash, RollingChecksum};

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
    /// it into a typed `AerorsyncError::InvalidFrame` before returning to
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

// --- W1.1: streaming delta plan producer ---------------------------------
//
// `DeltaPlanProducer` is the chunk-fed counterpart of `compute_delta`. The
// bulk planner (`delta_sync::compute_delta`) requires the full source slice
// in memory; the producer accepts the source as a sequence of contiguous
// chunks and emits `EngineDeltaOp`s incrementally. This is the foundational
// piece of P3-T01 W1: removing the 256 MiB in-memory cap on the upload side
// requires a planner that does not materialise the source as a single Vec.
//
// Invariant: for any source slice S and any chunking strategy,
// `RollingDeltaPlanProducer` driven over S produces the exact same sequence
// of ops as `delta_sync::compute_delta(S, ...)`. Pinned by
// `producer_streaming_matches_bulk_*` tests below.

/// Aggregate counters accumulated by a `DeltaPlanProducer`. Mirrors the
/// fields of `delta_sync::DeltaResult` that downstream consumers actually
/// read (copy_blocks, literal_bytes), plus a running `source_bytes_consumed`
/// for progress UX.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineDeltaStats {
    pub copy_blocks: u32,
    pub literal_bytes: u64,
    pub source_bytes_consumed: u64,
}

/// Chunk-fed delta planner. The caller drives source bytes through
/// `drive_chunk` in arbitrary contiguous slices, then calls `finalize`
/// exactly once to drain residual literal bytes. Successive calls after
/// `finalize` are a no-op (idempotent).
pub trait DeltaPlanProducer: Send {
    /// Drive one source chunk through the planner. Emits zero or more ops
    /// into `out`. Calls MUST receive logically contiguous bytes — i.e.
    /// the concatenation of all chunks across `drive_chunk` invocations
    /// must equal the original source slice.
    fn drive_chunk(&mut self, chunk: &[u8], out: &mut Vec<EngineDeltaOp>);

    /// Drain residual ops at end of source. Must be called exactly once.
    /// Output is appended to `out`.
    fn finalize(&mut self, out: &mut Vec<EngineDeltaOp>);

    /// Aggregate stats accumulated so far.
    fn stats(&self) -> &EngineDeltaStats;
}

/// `DeltaPlanProducer` impl backed by the same rolling-Adler32 + SHA-256
/// algorithm `delta_sync::compute_delta` uses, but driven incrementally.
///
/// State machine:
/// - `source_buf` holds the sliding window of bytes not yet drained: at
///   any point, `source_buf[..pos]` are bytes already classified
///   (literal-buffered or part of an emitted CopyBlock), `source_buf[pos..]`
///   are pending. `drive_chunk` drains `..pos` at the end so the buffer
///   stays bounded by ~`block_size` (rolling window) plus any in-flight
///   chunk tail.
/// - `rolling` is `Some` when the window at `pos` has been initialised;
///   it is dropped after a CopyBlock match (pos jumps forward by
///   block_size, the next window is fresh) and re-initialised lazily.
/// - `literal_buf` accumulates unmatched bytes one by one, flushed as a
///   single `EngineDeltaOp::Literal` immediately before each `CopyBlock`
///   and again at `finalize`.
///
/// Memory footprint: `O(block_size + literal_run_length)`. The literal run
/// is unbounded for sources that never match — same as the bulk planner;
/// callers that need a hard ceiling enforce it externally (see
/// `PLAN_WINDOW_MAX_BYTES` in `delta_transport_impl`).
pub struct RollingDeltaPlanProducer {
    block_size: usize,
    signatures: Vec<EngineSignatureBlock>,
    lookup: HashMap<u32, Vec<usize>>,
    source_buf: Vec<u8>,
    pos: usize,
    rolling: Option<RollingChecksum>,
    literal_buf: Vec<u8>,
    stats: EngineDeltaStats,
    /// Tracks whether the producer has emitted any op yet. Needed to
    /// match the bulk planner's quirk of emitting a single `Literal(empty)`
    /// for an empty source: see the `source_data.len() < block_size`
    /// short-circuit in `delta_sync::compute_delta`.
    has_emitted: bool,
    finalized: bool,
}

impl RollingDeltaPlanProducer {
    /// Build a producer from a known block size + destination signatures.
    /// Mirrors the input shape of `delta_sync::compute_delta` so callers
    /// can swap one for the other transparently.
    pub fn new(block_size: usize, signatures: Vec<EngineSignatureBlock>) -> Self {
        let mut lookup: HashMap<u32, Vec<usize>> = HashMap::new();
        for (i, sig) in signatures.iter().enumerate() {
            lookup.entry(sig.rolling).or_default().push(i);
        }
        Self {
            block_size,
            signatures,
            lookup,
            source_buf: Vec::new(),
            pos: 0,
            rolling: None,
            literal_buf: Vec::new(),
            stats: EngineDeltaStats::default(),
            has_emitted: false,
            finalized: false,
        }
    }

    fn flush_literal(&mut self, out: &mut Vec<EngineDeltaOp>) {
        if !self.literal_buf.is_empty() {
            self.stats.literal_bytes += self.literal_buf.len() as u64;
            let literal = std::mem::take(&mut self.literal_buf);
            out.push(EngineDeltaOp::Literal(literal));
            self.has_emitted = true;
        }
    }

    /// Linear scan over candidates with matching rolling checksum. Returns
    /// the signature index (not the block index) of the first hit.
    fn find_match(&self, rolling_val: u32) -> Option<usize> {
        let candidates = self.lookup.get(&rolling_val)?;
        let window = &self.source_buf[self.pos..self.pos + self.block_size];
        let strong = strong_hash(window);
        candidates
            .iter()
            .find(|&&idx| self.signatures[idx].strong == strong)
            .copied()
    }
}

impl DeltaPlanProducer for RollingDeltaPlanProducer {
    fn drive_chunk(&mut self, chunk: &[u8], out: &mut Vec<EngineDeltaOp>) {
        if self.finalized || chunk.is_empty() {
            return;
        }
        self.source_buf.extend_from_slice(chunk);
        self.stats.source_bytes_consumed += chunk.len() as u64;

        loop {
            // Lazily initialise the rolling window after a match (or at
            // the very start). Bail out if there are not enough bytes
            // for a full block_size window — wait for the next chunk.
            if self.rolling.is_none() {
                if self.source_buf.len() - self.pos >= self.block_size {
                    let window = &self.source_buf[self.pos..self.pos + self.block_size];
                    self.rolling = Some(RollingChecksum::new(window));
                } else {
                    break;
                }
            }

            let rolling_val = self
                .rolling
                .as_ref()
                .expect("rolling initialised above")
                .value();

            if let Some(sig_idx) = self.find_match(rolling_val) {
                self.flush_literal(out);
                let copy_idx = self.signatures[sig_idx].index;
                out.push(EngineDeltaOp::CopyBlock(copy_idx));
                self.stats.copy_blocks += 1;
                self.has_emitted = true;
                self.pos += self.block_size;
                self.rolling = None;
                continue;
            }

            // No match — to roll forward by 1 we need both `pos`
            // (byte to drop) and `pos + block_size` (byte to add) in the
            // buffer. If the +block_size byte is missing, wait for more.
            if self.source_buf.len() - self.pos < self.block_size + 1 {
                break;
            }
            let old_byte = self.source_buf[self.pos];
            let new_byte = self.source_buf[self.pos + self.block_size];
            self.literal_buf.push(old_byte);
            self.rolling
                .as_mut()
                .expect("rolling initialised above")
                .roll(old_byte, new_byte);
            self.pos += 1;
        }

        if self.pos > 0 {
            self.source_buf.drain(..self.pos);
            self.pos = 0;
        }
    }

    fn finalize(&mut self, out: &mut Vec<EngineDeltaOp>) {
        if self.finalized {
            return;
        }
        // Tail bytes that never reached a full block_size window survive
        // in `source_buf` — bulk semantics emit them as the trailing
        // literal.
        if self.source_buf.len() > self.pos {
            self.literal_buf
                .extend_from_slice(&self.source_buf[self.pos..]);
        }
        self.flush_literal(out);
        // Bulk-planner parity: an entirely empty source produces a single
        // `Literal(empty)` (see `compute_delta` short-circuit). Match it
        // so `streaming_ops == bulk_ops` holds bit-for-bit.
        if !self.has_emitted && self.stats.source_bytes_consumed == 0 {
            out.push(EngineDeltaOp::Literal(Vec::new()));
            self.has_emitted = true;
        }
        self.source_buf.clear();
        self.pos = 0;
        self.rolling = None;
        self.finalized = true;
    }

    fn stats(&self) -> &EngineDeltaStats {
        &self.stats
    }
}

// --- W2.1: streaming download baseline source ----------------------------
//
// `BaselineSource` is the random-access read counterpart of the streaming
// upload producer (`DeltaPlanProducer`). Where the producer turns a chunk
// stream of *source* bytes into delta ops, the baseline source feeds the
// download-side `apply_delta_streaming` (W2.2) the *destination* bytes
// referenced by `EngineDeltaOp::CopyBlock(idx)` without ever materialising
// the destination as a single `Vec<u8>`.
//
// Why a trait: in-memory tests (`MemoryBaseline`) and live downloads
// (`FileBaseline` over `tokio::fs::File`) share the same caller — the
// streaming `apply_delta` — but differ in storage. The trait is the seam
// that lets unit tests pin parity with the bulk `apply_delta` byte-for-byte
// while production code reads from a file handle without buffering.
//
// Memory bound: `O(read_block_size)` for the returned `Vec<u8>` plus the
// implementation's own state (a single `tokio::fs::File` handle for
// `FileBaseline`, a `Vec<u8>` for `MemoryBaseline`). No dependence on the
// total baseline length once the source is opened.
//
// Semantic alignment with `delta_sync::apply_delta`: a `CopyBlock(idx)` in
// the bulk path slices `dest_data[idx*block_size .. min((idx+1)*block_size,
// dest_data.len())]`. `BaselineSource::read_block(idx, block_size)` returns
// the same range. The tail block (when `len % block_size != 0`) returns a
// `Vec<u8>` shorter than `block_size`. Out-of-range reads (`idx*block_size
// > len()`) error out — the caller is expected to skip them, mirroring the
// bulk path's `if offset >= dest_data.len() { return Err(...) }`.

/// Random-access read interface over the download-side baseline. Used by
/// `apply_delta_streaming` (W2.2) to satisfy `EngineDeltaOp::CopyBlock(idx)`
/// without holding the entire baseline in memory.
#[async_trait]
pub trait BaselineSource: Send {
    /// Total length of the baseline in bytes. Stable for the lifetime of
    /// the source.
    fn len(&self) -> u64;

    /// `true` iff `len() == 0`.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Random-access read of the block at `block_idx` assuming a stride of
    /// `block_size` bytes per block. Returns up to `block_size` bytes; the
    /// returned `Vec<u8>` is shorter than `block_size` for the tail block
    /// when the baseline length is not a multiple of `block_size`.
    ///
    /// Errors:
    /// - `InvalidInput` if `block_idx as u64 * block_size as u64` strictly
    ///   exceeds `len()`. (Equality is allowed and yields an empty `Vec`.)
    /// - `InvalidInput` if `block_size == 0` and `block_idx > 0` (offset
    ///   computation would alias to zero).
    /// - I/O error from the underlying source on read failure.
    ///
    /// Wire alignment: the returned bytes are byte-identical to
    /// `delta_sync::apply_delta`'s `dest_data[offset..end]` slice. Pinned
    /// by the W2.1 unit tests.
    async fn read_block(
        &mut self,
        block_idx: u32,
        block_size: u32,
    ) -> std::io::Result<Vec<u8>>;
}

/// Compute the `(offset, len)` pair for `read_block(block_idx, block_size)`
/// against a baseline of length `total_len`. Shared between the file-backed
/// and in-memory implementations so the slicing semantics never drift.
///
/// Returns `Ok((offset, len))` on success. `len` may be zero (block_size 0,
/// or offset == total_len for a probe at the boundary). Errors as documented
/// on `BaselineSource::read_block`.
fn baseline_block_bounds(
    block_idx: u32,
    block_size: u32,
    total_len: u64,
) -> std::io::Result<(u64, usize)> {
    if block_size == 0 {
        if block_idx == 0 {
            return Ok((0, 0));
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "BaselineSource::read_block: block_size == 0 with block_idx {block_idx} > 0"
            ),
        ));
    }
    let offset = block_idx as u64 * block_size as u64;
    if offset > total_len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "BaselineSource::read_block: offset {offset} > baseline len {total_len} \
                 (block_idx={block_idx}, block_size={block_size})"
            ),
        ));
    }
    let end = (offset + block_size as u64).min(total_len);
    let len = (end - offset) as usize;
    Ok((offset, len))
}

/// `BaselineSource` backed by a `tokio::fs::File` opened in read-only mode.
/// The file handle is held open for the lifetime of the source; reads use
/// `seek` + `read_exact` without any internal buffering or caching.
///
/// Trade-off: HDD with high seek latency may experience tail-bound reads
/// when the delta plan walks `CopyBlock` indices out of order. A bounded
/// LRU cache of `K` recent blocks is a cheap follow-up if profiling shows
/// it matters; the trait shape supports a `CachedBaseline<S>` decorator
/// without changes here.
pub struct FileBaseline {
    file: tokio::fs::File,
    len: u64,
    path: PathBuf,
}

impl FileBaseline {
    /// Open `path` for read and snapshot its length via `metadata`. The
    /// returned source is positioned arbitrarily — every `read_block` does
    /// its own `seek` so concurrent or out-of-order reads are safe.
    pub async fn open(path: &Path) -> std::io::Result<Self> {
        let file = tokio::fs::File::open(path).await?;
        let metadata = file.metadata().await?;
        Ok(Self {
            file,
            len: metadata.len(),
            path: path.to_path_buf(),
        })
    }

    /// Path the source was opened from. Diagnostic only — the source does
    /// not re-open this path on retry.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[async_trait]
impl BaselineSource for FileBaseline {
    fn len(&self) -> u64 {
        self.len
    }

    async fn read_block(
        &mut self,
        block_idx: u32,
        block_size: u32,
    ) -> std::io::Result<Vec<u8>> {
        let (offset, len) = baseline_block_bounds(block_idx, block_size, self.len)?;
        if len == 0 {
            return Ok(Vec::new());
        }
        self.file.seek(SeekFrom::Start(offset)).await?;
        let mut buf = vec![0u8; len];
        self.file.read_exact(&mut buf).await?;
        Ok(buf)
    }
}

/// `BaselineSource` backed by an in-memory byte buffer. Intended for unit
/// tests that need to pin parity with `delta_sync::apply_delta` without
/// touching the filesystem; can also serve as a small-file fast path if
/// callers ever need it (no current production user).
pub struct MemoryBaseline {
    data: Vec<u8>,
}

impl MemoryBaseline {
    pub fn new(data: Vec<u8>) -> Self {
        Self { data }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }
}

#[async_trait]
impl BaselineSource for MemoryBaseline {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }

    async fn read_block(
        &mut self,
        block_idx: u32,
        block_size: u32,
    ) -> std::io::Result<Vec<u8>> {
        let (offset, len) = baseline_block_bounds(block_idx, block_size, self.len())?;
        if len == 0 {
            return Ok(Vec::new());
        }
        let start = offset as usize;
        Ok(self.data[start..start + len].to_vec())
    }
}

#[cfg(test)]
mod baseline_source_tests {
    use super::*;
    use std::io::Write;

    fn deterministic_payload(len: usize) -> Vec<u8> {
        // Same generator pattern as the producer pseudo-random tests so the
        // baseline content is reproducible and not byte-uniform.
        (0..len)
            .map(|i| ((i.wrapping_mul(2654435761)) & 0xFF) as u8)
            .collect()
    }

    fn write_temp_file(name: &str, payload: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("aerorsync-w2-1-{}-{}", name, std::process::id()));
        let mut f = std::fs::File::create(&path).expect("create temp file");
        f.write_all(payload).expect("write temp file");
        f.sync_all().expect("sync temp file");
        path
    }

    /// `MemoryBaseline` and `FileBaseline` produce byte-identical reads for
    /// every block index against the same payload. This is the canonical
    /// W2.1 parity: the file-backed impl never deviates from the in-memory
    /// reference.
    #[tokio::test]
    async fn baselines_read_identical_blocks_in_order() {
        let block_size: u32 = 1024;
        let block_count = 16;
        let payload = deterministic_payload(block_count * block_size as usize);

        let mut mem = MemoryBaseline::new(payload.clone());
        let path = write_temp_file("identical-in-order", &payload);
        let mut file = FileBaseline::open(&path).await.expect("open file baseline");

        for idx in 0..block_count as u32 {
            let from_mem = mem.read_block(idx, block_size).await.unwrap();
            let from_file = file.read_block(idx, block_size).await.unwrap();
            assert_eq!(from_mem, from_file, "block {idx} mem vs file diverges");
            assert_eq!(from_mem.len(), block_size as usize);
        }
        assert_eq!(mem.len(), payload.len() as u64);
        assert_eq!(file.len(), payload.len() as u64);
        std::fs::remove_file(&path).ok();
    }

    /// Random-access reads (reverse order, then arbitrary jumps) match the
    /// in-memory reference and the bulk `apply_delta` slicing.
    #[tokio::test]
    async fn file_baseline_supports_random_access() {
        let block_size: u32 = 512;
        let block_count = 32;
        let payload = deterministic_payload(block_count * block_size as usize);
        let path = write_temp_file("random-access", &payload);
        let mut file = FileBaseline::open(&path).await.unwrap();

        // Reverse order
        for idx in (0..block_count as u32).rev() {
            let buf = file.read_block(idx, block_size).await.unwrap();
            let want = &payload[idx as usize * block_size as usize
                ..(idx as usize + 1) * block_size as usize];
            assert_eq!(buf, want, "reverse-order block {idx} mismatch");
        }

        // Arbitrary jumps
        for &idx in &[7u32, 0, 31, 15, 4, 28, 8, 8, 0] {
            let buf = file.read_block(idx, block_size).await.unwrap();
            let want = &payload[idx as usize * block_size as usize
                ..(idx as usize + 1) * block_size as usize];
            assert_eq!(buf, want, "jump block {idx} mismatch");
        }
        std::fs::remove_file(&path).ok();
    }

    /// The tail block (when `len % block_size != 0`) returns a `Vec<u8>`
    /// shorter than `block_size`. This mirrors `delta_sync::apply_delta`'s
    /// `min((idx+1)*block_size, dest_data.len())` clamp.
    #[tokio::test]
    async fn baseline_tail_block_is_truncated() {
        let block_size: u32 = 512;
        let payload_len = 4 * block_size as usize + 137; // tail = 137 B
        let payload = deterministic_payload(payload_len);

        let mut mem = MemoryBaseline::new(payload.clone());
        let path = write_temp_file("tail-block", &payload);
        let mut file = FileBaseline::open(&path).await.unwrap();

        let tail_idx = 4u32;
        let mem_tail = mem.read_block(tail_idx, block_size).await.unwrap();
        let file_tail = file.read_block(tail_idx, block_size).await.unwrap();
        assert_eq!(mem_tail.len(), 137, "mem tail must be 137 bytes");
        assert_eq!(file_tail.len(), 137, "file tail must be 137 bytes");
        assert_eq!(mem_tail, file_tail);
        assert_eq!(&mem_tail[..], &payload[4 * block_size as usize..]);
        std::fs::remove_file(&path).ok();
    }

    /// Reading at `block_idx == 0` with `len == 0` and an empty baseline
    /// returns an empty `Vec`, not an error. Symmetric with
    /// `apply_delta`'s tolerance for empty `dest_data` when the only ops
    /// are `Literal`.
    #[tokio::test]
    async fn baseline_empty_file_block_zero_returns_empty() {
        let mut mem = MemoryBaseline::new(Vec::new());
        let path = write_temp_file("empty", &[]);
        let mut file = FileBaseline::open(&path).await.unwrap();

        assert_eq!(mem.len(), 0);
        assert!(mem.is_empty());
        assert_eq!(file.len(), 0);
        assert!(file.is_empty());

        let mem_empty = mem.read_block(0, 1024).await.unwrap();
        let file_empty = file.read_block(0, 1024).await.unwrap();
        assert!(mem_empty.is_empty(), "empty mem read must be empty Vec");
        assert!(file_empty.is_empty(), "empty file read must be empty Vec");
        std::fs::remove_file(&path).ok();
    }

    /// `block_idx` whose offset exceeds `len()` is an `InvalidInput` error.
    /// Caller is expected to never produce such ops; the source is the
    /// last line of defence against a corrupt delta plan.
    #[tokio::test]
    async fn baseline_oob_block_idx_errors() {
        let block_size: u32 = 256;
        let payload = deterministic_payload(2 * block_size as usize);

        let mut mem = MemoryBaseline::new(payload.clone());
        let err = mem.read_block(10, block_size).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);

        let path = write_temp_file("oob", &payload);
        let mut file = FileBaseline::open(&path).await.unwrap();
        let err = file.read_block(10, block_size).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        std::fs::remove_file(&path).ok();
    }

    /// `block_size == 0` is permitted only for `block_idx == 0` and yields
    /// an empty buffer. With `block_idx > 0`, it errors instead of silently
    /// aliasing every index to offset 0.
    #[tokio::test]
    async fn baseline_block_size_zero_special_case() {
        let payload = deterministic_payload(1024);
        let mut mem = MemoryBaseline::new(payload.clone());
        assert!(mem.read_block(0, 0).await.unwrap().is_empty());
        let err = mem.read_block(1, 0).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);

        let path = write_temp_file("bs-zero", &payload);
        let mut file = FileBaseline::open(&path).await.unwrap();
        assert!(file.read_block(0, 0).await.unwrap().is_empty());
        let err = file.read_block(1, 0).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        std::fs::remove_file(&path).ok();
    }

    /// Pin the slicing parity with `delta_sync::apply_delta`: for every
    /// block of a synthetic file, `BaselineSource::read_block` returns the
    /// exact same bytes the bulk `apply_delta` would emit for a single
    /// `CopyBlock(idx)` op. This is the invariant W2.2 will rely on to
    /// claim wire parity between bulk and streaming download.
    #[tokio::test]
    async fn baseline_block_matches_apply_delta_copyblock_slice() {
        let block_size: u32 = 1024;
        // Non-multiple length so the tail block exercises the truncation.
        let payload = deterministic_payload(7 * block_size as usize + 333);
        let mut mem = MemoryBaseline::new(payload.clone());

        let bulk_block_count = (payload.len() as u64).div_ceil(block_size as u64) as u32;

        for idx in 0..bulk_block_count {
            // `apply_delta` for `CopyBlock(idx)` produces:
            //   dest_data[idx*block_size .. min((idx+1)*block_size, len)]
            let offset = idx as usize * block_size as usize;
            let end = ((idx as usize + 1) * block_size as usize).min(payload.len());
            let bulk_slice = &payload[offset..end];
            let from_baseline = mem.read_block(idx, block_size).await.unwrap();
            assert_eq!(
                from_baseline,
                bulk_slice,
                "block {idx}: streaming baseline diverges from apply_delta slice"
            );
        }
    }

    /// `baseline_block_bounds` is the shared math behind both impls. Pin
    /// it directly so any future regression there surfaces here, not only
    /// through the impl tests above.
    #[test]
    fn baseline_block_bounds_math() {
        // Standard mid-file block.
        assert_eq!(baseline_block_bounds(2, 512, 4096).unwrap(), (1024, 512));
        // Tail block with truncation.
        assert_eq!(baseline_block_bounds(7, 512, 7 * 512 + 100).unwrap(), (3584, 100));
        // Boundary read at offset == len returns empty.
        assert_eq!(baseline_block_bounds(8, 512, 4096).unwrap(), (4096, 0));
        // OOB block_idx errors.
        assert!(baseline_block_bounds(9, 512, 4096).is_err());
        // block_size == 0 special case.
        assert_eq!(baseline_block_bounds(0, 0, 4096).unwrap(), (0, 0));
        assert!(baseline_block_bounds(1, 0, 4096).is_err());
    }
}

#[cfg(test)]
mod producer_tests {
    use super::*;
    use crate::delta_sync::{compute_delta, compute_signatures};

    fn engine_sigs_from_dest(dest: &[u8], block_size: usize) -> Vec<EngineSignatureBlock> {
        let table = compute_signatures(dest, block_size);
        table.signatures.into_iter().map(Into::into).collect()
    }

    fn ops_to_engine(ops: Vec<delta_sync::DeltaOp>) -> Vec<EngineDeltaOp> {
        ops.into_iter().map(Into::into).collect()
    }

    /// Drive `source` through the producer using fixed-size chunks.
    /// `chunk_size == 0` is interpreted as "single chunk = full source".
    fn run_producer(
        block_size: usize,
        signatures: Vec<EngineSignatureBlock>,
        source: &[u8],
        chunk_size: usize,
    ) -> (Vec<EngineDeltaOp>, EngineDeltaStats) {
        let mut producer = RollingDeltaPlanProducer::new(block_size, signatures);
        let mut out = Vec::new();
        if chunk_size == 0 {
            producer.drive_chunk(source, &mut out);
        } else {
            for chunk in source.chunks(chunk_size) {
                producer.drive_chunk(chunk, &mut out);
            }
        }
        producer.finalize(&mut out);
        (out, producer.stats().clone())
    }

    fn bulk_ops(source: &[u8], dest: &[u8], block_size: usize) -> Vec<EngineDeltaOp> {
        let table = compute_signatures(dest, block_size);
        let (ops, _) = compute_delta(source, &table);
        ops_to_engine(ops)
    }

    #[test]
    fn producer_empty_source_matches_bulk_literal_empty() {
        let block_size = 512;
        let dest = vec![0u8; 4096];
        let sigs = engine_sigs_from_dest(&dest, block_size);
        let bulk = bulk_ops(&[], &dest, block_size);

        let (streaming, stats) = run_producer(block_size, sigs, &[], 0);
        assert_eq!(streaming, bulk, "empty source must match bulk shape");
        assert_eq!(stats.source_bytes_consumed, 0);
        assert_eq!(stats.copy_blocks, 0);
        assert_eq!(stats.literal_bytes, 0);
    }

    #[test]
    fn producer_smaller_than_block_matches_bulk_single_literal() {
        let block_size = 512;
        let dest = vec![0xAAu8; 4096];
        let sigs = engine_sigs_from_dest(&dest, block_size);
        let source = b"too small for a full block".to_vec();
        let bulk = bulk_ops(&source, &dest, block_size);

        let (streaming, stats) = run_producer(block_size, sigs, &source, 0);
        assert_eq!(streaming, bulk);
        assert_eq!(stats.source_bytes_consumed, source.len() as u64);
    }

    #[test]
    fn producer_identical_source_dest_emits_only_copy_blocks() {
        let block_size = 512;
        let mut dest = vec![0u8; 8 * block_size];
        for (i, b) in dest.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let source = dest.clone();
        let sigs = engine_sigs_from_dest(&dest, block_size);
        let bulk = bulk_ops(&source, &dest, block_size);

        let (streaming, stats) = run_producer(block_size, sigs, &source, 0);
        assert_eq!(streaming, bulk, "identical source/dest must match bulk");
        assert!(
            streaming
                .iter()
                .all(|op| matches!(op, EngineDeltaOp::CopyBlock(_))),
            "identical source must produce only CopyBlocks"
        );
        assert_eq!(stats.copy_blocks, 8);
        assert_eq!(stats.literal_bytes, 0);
    }

    #[test]
    fn producer_disjoint_source_dest_emits_only_literal() {
        let block_size = 512;
        let dest = vec![0xAAu8; 8 * block_size];
        let mut source = vec![0u8; 8 * block_size];
        for (i, b) in source.iter_mut().enumerate() {
            // Bytes deliberately chosen so no window matches the dest
            // signature (all 0xAA blocks). 0x55 + position-based jitter
            // never coincides with a 0xAA block.
            *b = 0x55u8.wrapping_add((i as u8) & 0x0F);
        }
        let sigs = engine_sigs_from_dest(&dest, block_size);
        let bulk = bulk_ops(&source, &dest, block_size);

        let (streaming, stats) = run_producer(block_size, sigs, &source, 0);
        assert_eq!(streaming, bulk, "disjoint source/dest must match bulk");
        assert!(
            streaming
                .iter()
                .all(|op| matches!(op, EngineDeltaOp::Literal(_))),
            "disjoint source must produce only Literals"
        );
        assert_eq!(stats.copy_blocks, 0);
    }

    #[test]
    fn producer_chunk_boundary_invariant_synthetic() {
        // Build a source that mixes matched blocks (from dest) with
        // unmatched literal runs of various lengths. Then drive it
        // through the producer with several chunk sizes and verify the
        // ops are byte-for-byte equal to the bulk planner.
        let block_size = 512;
        let mut dest = Vec::new();
        for blk in 0..6u32 {
            let mut block = vec![0u8; block_size];
            for (i, b) in block.iter_mut().enumerate() {
                *b = ((blk as usize + i) % 251) as u8;
            }
            dest.extend(&block);
        }
        let mut source = Vec::new();
        // matched block 0
        source.extend(&dest[0..block_size]);
        // 100 bytes of literal
        source.extend(std::iter::repeat_n(0xEE, 100));
        // matched block 3 (out of order)
        source.extend(&dest[3 * block_size..4 * block_size]);
        // 7-byte literal tail
        source.extend(b"TAILEND");
        let sigs = engine_sigs_from_dest(&dest, block_size);
        let bulk = bulk_ops(&source, &dest, block_size);

        for &chunk_size in &[1usize, 2, 7, 13, 64, 100, 256, 512, 1024, 4096] {
            let (streaming, _) = run_producer(block_size, sigs.clone(), &source, chunk_size);
            assert_eq!(
                streaming, bulk,
                "chunk_size={chunk_size} must produce the same ops as bulk"
            );
        }
    }

    #[test]
    fn producer_chunk_boundary_invariant_pseudo_random() {
        // Deterministic pseudo-random source + destination, no matches
        // expected most of the time. Pin: the streaming ops match the
        // bulk ops for every chunk size we try.
        let block_size = 256;
        let dest_len = 16 * block_size;
        let mut dest = vec![0u8; dest_len];
        for (i, b) in dest.iter_mut().enumerate() {
            *b = ((i.wrapping_mul(2654435761)) & 0xFF) as u8;
        }
        let mut source = vec![0u8; dest_len + 137];
        for (i, b) in source.iter_mut().enumerate() {
            *b = ((i.wrapping_mul(40503) ^ 0xA5) & 0xFF) as u8;
        }
        // Splice in 2 matched blocks to exercise the match path.
        source[block_size..2 * block_size].copy_from_slice(&dest[2 * block_size..3 * block_size]);
        source[5 * block_size..6 * block_size].copy_from_slice(&dest[7 * block_size..8 * block_size]);
        let sigs = engine_sigs_from_dest(&dest, block_size);
        let bulk = bulk_ops(&source, &dest, block_size);

        for &chunk_size in &[1usize, 3, 17, 63, 128, 256, 257, 1024, 1 << 20] {
            let (streaming, _) = run_producer(block_size, sigs.clone(), &source, chunk_size);
            assert_eq!(
                streaming, bulk,
                "pseudo-random chunk_size={chunk_size} must match bulk"
            );
        }
    }

    #[test]
    fn producer_stats_match_bulk_counters() {
        let block_size = 512;
        let dest = vec![0u8; 4 * block_size];
        let sigs_engine = engine_sigs_from_dest(&dest, block_size);
        // Source with 2 matched blocks (idx 0, 2) and a 60-byte literal tail.
        let mut source = Vec::new();
        source.extend(&dest[0..block_size]);
        source.extend(&dest[2 * block_size..3 * block_size]);
        source.extend(std::iter::repeat_n(0xCC, 60));
        let bulk_table = compute_signatures(&dest, block_size);
        let (_, bulk_result) = compute_delta(&source, &bulk_table);

        let (_, stats) = run_producer(block_size, sigs_engine, &source, 64);
        assert_eq!(stats.copy_blocks, bulk_result.copy_blocks);
        assert_eq!(stats.literal_bytes, bulk_result.literal_bytes);
        assert_eq!(stats.source_bytes_consumed, source.len() as u64);
    }

    #[test]
    fn producer_finalize_is_idempotent() {
        let block_size = 512;
        let dest = vec![0u8; 4096];
        let sigs = engine_sigs_from_dest(&dest, block_size);
        let mut producer = RollingDeltaPlanProducer::new(block_size, sigs);
        let mut out = Vec::new();
        producer.drive_chunk(b"short", &mut out);
        producer.finalize(&mut out);
        let after_first = out.clone();
        // Second finalize + post-finalize drive_chunk are no-ops.
        producer.finalize(&mut out);
        producer.drive_chunk(b"ignored", &mut out);
        assert_eq!(out, after_first);
    }
}
