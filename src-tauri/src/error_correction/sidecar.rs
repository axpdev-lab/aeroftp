//! Unified detached Error-Correction sidecar: the `.aerocorrect` format.
//!
//! One detached parity format for *any* protected byte stream (Ehud's #276 call:
//! a single EC sidecar format for any file). It supersedes the two earlier
//! formats, the vault's `.aerovault.rec` and AeroSync's `.aerorec`, which differed
//! only in their framing: the vault carried three region payloads (header /
//! manifest / data) and AeroSync carried windowed file segments. Both are just
//! "N protected windows over a byte stream", so this format expresses both:
//! AeroSync writes one full-file segment (or windows for large files), the vault
//! writes one segment per region. The Reed-Solomon codec itself
//! (`super::compute_error_correction_shards_grid` / `reconstruct_from_error_correction`)
//! is unchanged and shared.
//!
//! Binding is by content (the SHA-256 of the whole protected stream), so the
//! sidecar is self-describing and works for any file. Higher layers add their own
//! checks on top: AeroSync compares the expected remote path/size, and the vault
//! re-verifies recovered bytes against its authenticated header MAC / manifest
//! `cipher_hash` before persisting, so a foreign or corrupt sidecar can only make a
//! repair FAIL, never overwrite good data.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::ERROR_CORRECTION_SHARD_CKSUM_LEN;
use super::{
    error_correction_grid, ERROR_CORRECTION_MAX_PCT, ERROR_CORRECTION_MAX_SHARD,
    ERROR_CORRECTION_MIN_SHARD,
};

/// Magic for a unified detached recovery file: "AEROCORR".
pub(crate) const AEROCORRECT_MAGIC: &[u8; 8] = b"AEROCORR";
/// On-disk format version written today. v2 is the self-healing format (#276 Ehud
/// point 2): the small critical metadata that locates everything is stored redundantly
/// so a lightly-corrupted sidecar still recovers. Bump only on a wire-incompatible
/// change.
pub(crate) const AEROCORRECT_VERSION: u8 = 2;
/// Legacy version still parsed for read-back (the pre-#276 wholesale-checksum framing).
/// We write v2 and read both, so any v1 sidecars left from earlier dev runs keep working.
const AEROCORRECT_VERSION_V1: u8 = 1;
/// Conventional sidecar extension: `<file>` -> `<file>.aerocorrect`.
pub(crate) const AEROCORRECT_EXTENSION: &str = ".aerocorrect";

const BINDING_DOMAIN: &[u8] = b"aerocorrect-binding-v1";
const BINDING_LEN: usize = 32;

// ── v2 self-healing framing ────────────────────────────────────────────────
// On-disk layout:
//   magic(8) | version(1) | segment_count ×3 (12)                     = V2_PREFIX_LEN
//   directory copy 1 (DIR_LEN) | blake3(copy1)(32)
//   directory copy 2 (DIR_LEN) | blake3(copy2)(32)
//   directory copy 3 (DIR_LEN) | blake3(copy3)(32)
//   parity region: every segment's AVEC payload, concatenated in segment order
// One directory copy:
//   total_len(8) | binding_id(32) | content_sha256(32)               = DIR_FIXED_LEN
//   per segment: window_offset(8) | window_len(8) | avec_len(8)
//                | avec geometry header copy(32)                     = DIR_PER_SEG_LEN
//
// The directory is the locator. It is tiny (DIR_LEN < 1 KiB even for a 1 GiB file at
// 64 MiB windows) and stored in triplicate with a per-copy checksum, so any single
// corrupted copy is detected and the read falls back to a good copy. segment_count is
// kept in three fixed slots so the copy boundaries themselves survive a stray flip. The
// bulk parity carries NO wholesale envelope checksum: every shard (data and parity)
// already carries its own checksum and `reconstruct_from_error_correction` treats a
// mismatching shard as an RS erasure, so rotted parity is routed around at repair time.
// Safety is unchanged: a repair only persists after the rebuilt bytes re-verify against
// the authenticated content hash / header MAC / manifest cipher_hash, so a corrupt or
// foreign sidecar can only make a repair FAIL, never overwrite good data.
const DIRECTORY_COPIES: usize = 3;
const COPY_CKSUM_LEN: usize = 32;
/// Copy of an AVEC payload's 32-byte geometry header, kept in the directory so a rotted
/// in-payload header is healed before reconstruction (the per-shard checksums then cover
/// the rest of the payload).
const GEOM_LEN: usize = 32;
/// magic(8) + version(1) + segment_count ×3 (12).
const V2_PREFIX_LEN: usize = 8 + 1 + 4 * DIRECTORY_COPIES;
/// One directory copy's fixed prefix: total_len(8) + binding(32) + content_sha256(32).
const DIR_FIXED_LEN: usize = 8 + BINDING_LEN + 32;
/// One directory copy's per-segment record: window_offset(8) + window_len(8)
/// + avec_len(8) + geometry header copy(32).
const DIR_PER_SEG_LEN: usize = 8 + 8 + 8 + GEOM_LEN;
/// Per-sidecar fixed framing bytes in v2 (prefix + the three directory fixed prefixes and
/// their per-copy checksums), independent of segment count.
const V2_SIDECAR_FIXED_LEN: usize =
    V2_PREFIX_LEN + DIRECTORY_COPIES * (DIR_FIXED_LEN + COPY_CKSUM_LEN);
/// Per-segment directory framing bytes in v2 (the per-segment record, replicated across
/// the three directory copies). That segment's parity (AVEC) bytes are added on top.
const V2_SEGMENT_FRAMING_LEN: usize = DIRECTORY_COPIES * DIR_PER_SEG_LEN;
/// Upper bound on the total bytes of the three directory copies, so a forged segment count
/// can never make the streaming reader buffer an attacker-sized locator region. 64 MiB
/// covers a directory for a multi-TiB file (far above the 1 GiB EC file cap).
const MAX_DIRECTORY_REGION: usize = 64 * 1024 * 1024;

// ── v1 (legacy, read-only) framing ─────────────────────────────────────────
const CHECKSUM_LEN: usize = 32;
/// v1 magic(8) + version(1) + binding(32) + content_sha256(32) + total_len(8)
/// + segment_count(4).
const HEADER_LEN: usize = 8 + 1 + BINDING_LEN + 32 + 8 + 4;
/// v1 per-segment fixed header: window_offset(8) + window_len(8) + avec_len(8).
const SEGMENT_HEADER_LEN: usize = 24;

/// One protected window over the source byte stream plus its parity (AVEC) blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AeroCorrectSegment {
    pub(crate) window_offset: u64,
    pub(crate) window_len: u64,
    pub(crate) avec_bytes: Vec<u8>,
}

/// In-memory view of a `.aerocorrect` sidecar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AeroCorrectSidecar {
    pub(crate) binding_id: [u8; BINDING_LEN],
    pub(crate) content_sha256: [u8; 32],
    pub(crate) total_len: u64,
    pub(crate) segments: Vec<AeroCorrectSegment>,
}

/// Domain-separated binding id, derived purely from the protected content's
/// SHA-256. Path and size (AeroSync concerns) live in the higher layer, not here.
pub(crate) fn aerocorrect_binding_id(content_sha256: &[u8; 32]) -> [u8; BINDING_LEN] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(BINDING_DOMAIN);
    hasher.update(content_sha256);
    let digest = hasher.finalize();
    let mut out = [0u8; BINDING_LEN];
    out.copy_from_slice(digest.as_bytes());
    out
}

/// Append a `<file>.aerocorrect` suffix to a path string.
pub(crate) fn aerocorrect_sidecar_path(path: &str) -> String {
    format!("{path}{AEROCORRECT_EXTENSION}")
}

impl AeroCorrectSidecar {
    pub(crate) fn new(
        content_sha256: [u8; 32],
        total_len: u64,
        segments: Vec<AeroCorrectSegment>,
    ) -> Self {
        Self {
            binding_id: aerocorrect_binding_id(&content_sha256),
            content_sha256,
            total_len,
            segments,
        }
    }

    /// Encode one directory copy (the locator): the fixed prefix plus, per segment, its
    /// window placement, AVEC length, and a copy of the AVEC payload's 32-byte geometry
    /// header (so a rotted in-payload header can be healed from here on read).
    fn encode_directory(&self) -> Vec<u8> {
        let mut d = Vec::with_capacity(DIR_FIXED_LEN + DIR_PER_SEG_LEN * self.segments.len());
        d.extend_from_slice(&self.total_len.to_le_bytes());
        d.extend_from_slice(&self.binding_id);
        d.extend_from_slice(&self.content_sha256);
        for segment in &self.segments {
            d.extend_from_slice(&segment.window_offset.to_le_bytes());
            d.extend_from_slice(&segment.window_len.to_le_bytes());
            d.extend_from_slice(&(segment.avec_bytes.len() as u64).to_le_bytes());
            let mut geom = [0u8; GEOM_LEN];
            let g = segment.avec_bytes.len().min(GEOM_LEN);
            geom[..g].copy_from_slice(&segment.avec_bytes[..g]);
            d.extend_from_slice(&geom);
        }
        d
    }

    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let n = self.segments.len();
        let dir = self.encode_directory();
        let parity_len: usize = self.segments.iter().map(|s| s.avec_bytes.len()).sum();
        let total = V2_PREFIX_LEN + DIRECTORY_COPIES * (dir.len() + COPY_CKSUM_LEN) + parity_len;
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(AEROCORRECT_MAGIC);
        out.push(AEROCORRECT_VERSION);
        let count = (n as u32).to_le_bytes();
        for _ in 0..DIRECTORY_COPIES {
            out.extend_from_slice(&count);
        }
        let cksum = blake3::hash(&dir);
        for _ in 0..DIRECTORY_COPIES {
            out.extend_from_slice(&dir);
            out.extend_from_slice(cksum.as_bytes());
        }
        for segment in &self.segments {
            out.extend_from_slice(&segment.avec_bytes);
        }
        out
    }

    pub(crate) fn from_bytes(data: &[u8]) -> Result<Self, String> {
        if data.len() < AEROCORRECT_MAGIC.len() + 1 {
            return Err("aerocorrect sidecar too short".to_string());
        }
        if &data[..AEROCORRECT_MAGIC.len()] != AEROCORRECT_MAGIC {
            return Err("bad aerocorrect sidecar magic".to_string());
        }
        match data[AEROCORRECT_MAGIC.len()] {
            AEROCORRECT_VERSION => Self::from_bytes_v2(data),
            AEROCORRECT_VERSION_V1 => Self::from_bytes_v1(data),
            version => Err(format!("unsupported aerocorrect sidecar version {version}")),
        }
    }

    /// Parse the self-healing v2 framing: recover the locator from its replicated copies,
    /// then read each segment's parity from the contiguous parity region (splicing the
    /// directory's geometry-header copy over a possibly-rotted in-payload header).
    fn from_bytes_v2(data: &[u8]) -> Result<Self, String> {
        if data.len() < V2_PREFIX_LEN {
            return Err("aerocorrect sidecar too short".to_string());
        }
        let counts = read_count_slots(&data[9..V2_PREFIX_LEN]);
        let mut last_err = "aerocorrect sidecar directory unrecoverable".to_string();
        for n in candidate_counts(&counts) {
            if n == 0 {
                continue;
            }
            let layout = match V2Layout::for_count(n as usize, data.len() as u64) {
                Some(l) => l,
                None => continue, // candidate count cannot physically fit this file
            };
            let dir = match recover_directory(data, &layout) {
                Ok(d) => d,
                Err(e) => {
                    last_err = e;
                    continue;
                }
            };
            return parse_v2_directory(&dir, &layout, data);
        }
        Err(last_err)
    }

    /// Parse the legacy v1 framing (wholesale-checksum envelope). Read-only back-compat.
    fn from_bytes_v1(data: &[u8]) -> Result<Self, String> {
        if data.len() < HEADER_LEN + CHECKSUM_LEN {
            return Err("aerocorrect sidecar too short".to_string());
        }
        let body_end = data.len() - CHECKSUM_LEN;
        let actual = blake3::hash(&data[..body_end]);
        if actual.as_bytes() != &data[body_end..] {
            return Err("aerocorrect sidecar integrity check failed".to_string());
        }

        let mut off = AEROCORRECT_MAGIC.len() + 1;
        let mut binding_id = [0u8; BINDING_LEN];
        binding_id.copy_from_slice(&data[off..off + BINDING_LEN]);
        off += BINDING_LEN;
        let mut content_sha256 = [0u8; 32];
        content_sha256.copy_from_slice(&data[off..off + 32]);
        off += 32;
        let total_len = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
        off += 8;
        let segment_count = u32::from_le_bytes(data[off..off + 4].try_into().unwrap()) as usize;
        off += 4;
        if segment_count == 0 {
            return Err("aerocorrect sidecar has no segments".to_string());
        }
        let max_segments = (body_end - off) / SEGMENT_HEADER_LEN;
        if segment_count > max_segments {
            return Err(format!(
                "aerocorrect sidecar segment count {segment_count} exceeds buffer capacity {max_segments}"
            ));
        }

        let mut segments = Vec::with_capacity(segment_count);
        for _ in 0..segment_count {
            if off + SEGMENT_HEADER_LEN > body_end {
                return Err("aerocorrect sidecar truncated reading segment header".to_string());
            }
            let window_offset = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
            off += 8;
            let window_len = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
            off += 8;
            let avec_len_u64 = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
            off += 8;
            validate_segment_avec_len(window_len, avec_len_u64)?;
            let avec_len = usize::try_from(avec_len_u64)
                .map_err(|_| "aerocorrect sidecar segment length exceeds usize".to_string())?;
            if off.checked_add(avec_len).is_none_or(|end| end > body_end) {
                return Err("aerocorrect sidecar segment length mismatch".to_string());
            }
            let avec_bytes = data[off..off + avec_len].to_vec();
            off += avec_len;
            segments.push(AeroCorrectSegment {
                window_offset,
                window_len,
                avec_bytes,
            });
        }

        if off != body_end {
            return Err("aerocorrect sidecar has unexpected trailing bytes".to_string());
        }

        Ok(Self {
            binding_id,
            content_sha256,
            total_len,
            segments,
        })
    }

    /// Confirm this sidecar belongs to content with `content_sha256`. A mismatch
    /// means a wrong/foreign sidecar; the binding alone never authorizes a write
    /// (the caller still re-verifies recovered bytes).
    pub(crate) fn verify_binding(&self, content_sha256: &[u8; 32]) -> Result<(), String> {
        if &self.content_sha256 == content_sha256
            && self.binding_id == aerocorrect_binding_id(content_sha256)
        {
            Ok(())
        } else {
            Err("aerocorrect sidecar does not match this content (binding mismatch)".to_string())
        }
    }
}

// ── v2 locator recovery (shared by the in-memory parse and the streaming reader) ──

/// The three little-endian `u32` segment-count slots from the v2 prefix.
fn read_count_slots(bytes: &[u8]) -> [u32; DIRECTORY_COPIES] {
    let mut out = [0u32; DIRECTORY_COPIES];
    for (i, slot) in out.iter_mut().enumerate() {
        let base = i * 4;
        *slot = u32::from_le_bytes(bytes[base..base + 4].try_into().unwrap());
    }
    out
}

/// Candidate segment counts to try, in order: a 2-of-3 majority first (the common case,
/// where at most one slot rotted), then any remaining distinct values as fallbacks for
/// the rare all-three-differ case. Deduplicated.
fn candidate_counts(counts: &[u32; DIRECTORY_COPIES]) -> Vec<u32> {
    let mut order: Vec<u32> = Vec::new();
    for &c in counts {
        if counts.iter().filter(|&&x| x == c).count() >= 2 && !order.contains(&c) {
            order.push(c);
        }
    }
    for &c in counts {
        if !order.contains(&c) {
            order.push(c);
        }
    }
    order
}

/// Byte offsets of the v2 regions for a candidate segment count, validated against the
/// physical file size so a forged/garbled count can never drive an out-of-bounds read or
/// an oversized allocation.
#[derive(Debug, Clone, Copy)]
struct V2Layout {
    n: usize,
    dir_len: usize,
    /// First byte of directory copy `i`: `copy_base(i)`.
    copies_base: usize,
    parity_start: usize,
    parity_len: u64,
}

impl V2Layout {
    fn for_count(n: usize, file_len: u64) -> Option<Self> {
        let dir_len = DIR_FIXED_LEN.checked_add(DIR_PER_SEG_LEN.checked_mul(n)?)?;
        let copy_stride = dir_len.checked_add(COPY_CKSUM_LEN)?;
        let copies_span = copy_stride.checked_mul(DIRECTORY_COPIES)?;
        // The locator is tiny in practice (< 3 MiB even for a multi-TiB file). Reject a
        // crafted count whose directory copies would exceed this ceiling so the streaming
        // reader never buffers an attacker-sized region in memory.
        if copies_span > MAX_DIRECTORY_REGION {
            return None;
        }
        let parity_start = V2_PREFIX_LEN.checked_add(copies_span)?;
        let parity_len = (file_len).checked_sub(parity_start as u64)?;
        Some(Self {
            n,
            dir_len,
            copies_base: V2_PREFIX_LEN,
            parity_start,
            parity_len,
        })
    }

    fn copy_base(&self, i: usize) -> usize {
        self.copies_base + i * (self.dir_len + COPY_CKSUM_LEN)
    }
}

/// Recover the locator (one directory copy) for `layout` from `data`. First take any copy
/// whose stored checksum matches (an intact copy heals a damaged sibling). If none match,
/// fall back to a byte-wise majority vote across the three copies; accept that
/// reconstruction only if it is internally consistent (its `binding_id` equals
/// `BLAKE3(content_sha256)`, a 2^-256 coincidence under random rot), so a garbled vote is
/// never trusted. Returns the directory bytes or an error when the locator is too damaged
/// to recover (beyond the self-heal budget -> the caller fails closed).
fn recover_directory(data: &[u8], layout: &V2Layout) -> Result<Vec<u8>, String> {
    let mut copies: Vec<&[u8]> = Vec::with_capacity(DIRECTORY_COPIES);
    for i in 0..DIRECTORY_COPIES {
        let base = layout.copy_base(i);
        let cksum_end = base + layout.dir_len + COPY_CKSUM_LEN;
        if cksum_end > data.len() {
            continue;
        }
        let dir = &data[base..base + layout.dir_len];
        let stored = &data[base + layout.dir_len..cksum_end];
        if blake3::hash(dir).as_bytes() == stored {
            return Ok(dir.to_vec());
        }
        copies.push(dir);
    }
    // No intact copy: reconstruct by per-byte majority across the copies we could read,
    // then accept only if the result is internally consistent (a garbled vote is rejected).
    if copies.len() >= 2 {
        let mut voted = vec![0u8; layout.dir_len];
        for (b, out) in voted.iter_mut().enumerate() {
            let mut best_val = copies[0][b];
            let mut best_count = 0usize;
            for c in &copies {
                let val = c[b];
                let count = copies.iter().filter(|x| x[b] == val).count();
                if count > best_count {
                    best_count = count;
                    best_val = val;
                }
            }
            *out = best_val;
        }
        if directory_is_self_consistent(&voted) {
            return Ok(voted);
        }
    }
    Err("aerocorrect sidecar locator is too corrupt to recover (all directory copies failed their checksum)".to_string())
}

/// A directory blob is internally consistent when its `binding_id` is the domain-separated
/// hash of its own `content_sha256`. Used to validate a majority-voted reconstruction.
fn directory_is_self_consistent(dir: &[u8]) -> bool {
    if dir.len() < DIR_FIXED_LEN {
        return false;
    }
    let mut binding_id = [0u8; BINDING_LEN];
    binding_id.copy_from_slice(&dir[8..8 + BINDING_LEN]);
    let mut content_sha256 = [0u8; 32];
    content_sha256.copy_from_slice(&dir[8 + BINDING_LEN..DIR_FIXED_LEN]);
    binding_id == aerocorrect_binding_id(&content_sha256)
}

/// Per-segment metadata decoded from a recovered v2 directory: the window placement, the
/// parity payload's offset (relative to the parity region) and length, and the protected
/// copy of its 32-byte AVEC geometry header.
struct V2SegMeta {
    window_offset: u64,
    window_len: u64,
    avec_offset: u64,
    avec_len: u64,
    geom: [u8; GEOM_LEN],
}

/// A fully decoded, bounds-checked v2 directory (the locator).
struct V2Directory {
    binding_id: [u8; BINDING_LEN],
    content_sha256: [u8; 32],
    total_len: u64,
    segments: Vec<V2SegMeta>,
}

/// Decode and bounds-check every field of a recovered v2 directory against the parity
/// region size, returning the binding, content hash, total length, and per-segment
/// metadata. Shared by the in-memory parse and the streaming reader so both enforce the
/// same anti-forgery bounds; neither touches the parity bytes here.
fn parse_v2_directory_fields(dir: &[u8], layout: &V2Layout) -> Result<V2Directory, String> {
    if dir.len() != DIR_FIXED_LEN + DIR_PER_SEG_LEN * layout.n {
        return Err("aerocorrect directory length mismatch".to_string());
    }
    let total_len = u64::from_le_bytes(dir[0..8].try_into().unwrap());
    let mut binding_id = [0u8; BINDING_LEN];
    binding_id.copy_from_slice(&dir[8..8 + BINDING_LEN]);
    let mut content_sha256 = [0u8; 32];
    content_sha256.copy_from_slice(&dir[8 + BINDING_LEN..DIR_FIXED_LEN]);

    let mut metas = Vec::with_capacity(layout.n);
    let mut parity_used: u64 = 0;
    for i in 0..layout.n {
        let base = DIR_FIXED_LEN + i * DIR_PER_SEG_LEN;
        let window_offset = u64::from_le_bytes(dir[base..base + 8].try_into().unwrap());
        let window_len = u64::from_le_bytes(dir[base + 8..base + 16].try_into().unwrap());
        let avec_len = u64::from_le_bytes(dir[base + 16..base + 24].try_into().unwrap());
        let mut geom = [0u8; GEOM_LEN];
        geom.copy_from_slice(&dir[base + 24..base + 24 + GEOM_LEN]);
        validate_segment_avec_len(window_len, avec_len)?;
        let avec_offset = parity_used;
        let end = avec_offset
            .checked_add(avec_len)
            .filter(|end| *end <= layout.parity_len)
            .ok_or("aerocorrect sidecar segment length mismatch")?;
        parity_used = end;
        metas.push(V2SegMeta {
            window_offset,
            window_len,
            avec_offset,
            avec_len,
            geom,
        });
    }
    if parity_used != layout.parity_len {
        return Err(format!(
            "aerocorrect sidecar parity region is {} bytes but segments declare {parity_used}",
            layout.parity_len
        ));
    }
    Ok(V2Directory {
        binding_id,
        content_sha256,
        total_len,
        segments: metas,
    })
}

/// Parse a recovered v2 directory plus the parity region into an in-memory sidecar,
/// healing each AVEC payload's geometry header from the directory copy.
fn parse_v2_directory(
    dir: &[u8],
    layout: &V2Layout,
    data: &[u8],
) -> Result<AeroCorrectSidecar, String> {
    let decoded = parse_v2_directory_fields(dir, layout)?;
    let mut segments = Vec::with_capacity(decoded.segments.len());
    for meta in decoded.segments {
        let avec_len = usize::try_from(meta.avec_len)
            .map_err(|_| "aerocorrect sidecar segment length exceeds usize".to_string())?;
        let seg_start = layout.parity_start + meta.avec_offset as usize;
        let mut avec_bytes = data[seg_start..seg_start + avec_len].to_vec();
        // Heal the in-payload geometry header from the directory's protected copy.
        let g = avec_bytes.len().min(GEOM_LEN);
        avec_bytes[..g].copy_from_slice(&meta.geom[..g]);
        segments.push(AeroCorrectSegment {
            window_offset: meta.window_offset,
            window_len: meta.window_len,
            avec_bytes,
        });
    }
    Ok(AeroCorrectSidecar {
        binding_id: decoded.binding_id,
        content_sha256: decoded.content_sha256,
        total_len: decoded.total_len,
        segments,
    })
}

/// Exact serialized size (in bytes) of a single-full-segment `.aerocorrect`
/// sidecar for a `content_len`-byte stream at overhead level `pct`, derived from
/// the same v2 fixed-grid geometry WITHOUT allocating or hashing.
///
/// Source of truth for the sync-doctor cost preview. A flat `size * pct / 100`
/// underestimates the real sidecar by orders of magnitude for small files (the
/// `ERROR_CORRECTION_MIN_SHARD` floor forces >= 4 KiB parity shards) and
/// under-reports at the `ERROR_CORRECTION_MAX_SHARD` cliff.
#[cfg(test)]
pub(crate) fn estimate_single_segment_sidecar_len(content_len: u64, pct: u32) -> u64 {
    (V2_SIDECAR_FIXED_LEN + V2_SEGMENT_FRAMING_LEN) as u64 + avec_len_for(content_len, pct)
}

/// Serialized AVEC parity payload bytes for ONE protected window of `content_len`
/// bytes at overhead level `pct`, derived from the same v2 fixed-grid geometry
/// WITHOUT allocating or hashing. An empty window has an empty AVEC payload.
pub(crate) fn avec_len_for(content_len: u64, pct: u32) -> u64 {
    avec_len_for_checked(content_len, pct).unwrap_or(u64::MAX)
}

fn avec_len_for_checked(content_len: u64, pct: u32) -> Option<u64> {
    if content_len == 0 {
        return Some(0);
    }
    let (k, p) = error_correction_grid(pct);
    let (k, p) = (k as u64, p as u64);
    let s = content_len.div_ceil(k).clamp(
        ERROR_CORRECTION_MIN_SHARD as u64,
        ERROR_CORRECTION_MAX_SHARD as u64,
    );
    let num_data = content_len.div_ceil(s);
    let num_groups = num_data.div_ceil(k);
    let num_parity = num_groups.checked_mul(p)?;
    let checksum_len = num_data
        .checked_add(num_parity)?
        .checked_mul(ERROR_CORRECTION_SHARD_CKSUM_LEN as u64)?;
    let parity_len = num_parity.checked_mul(s)?;
    32u64.checked_add(checksum_len)?.checked_add(parity_len)
}

fn validate_segment_avec_len(window_len: u64, avec_len: u64) -> Result<(), String> {
    let max_len = avec_len_for_checked(window_len, ERROR_CORRECTION_MAX_PCT)
        .ok_or("aerocorrect sidecar segment AVEC length geometry overflows")?;
    if avec_len > max_len {
        return Err(format!(
            "aerocorrect sidecar segment AVEC length {avec_len} exceeds maximum {max_len} for window length {window_len}"
        ));
    }
    Ok(())
}

/// Default protected-window size for windowed (large-file) `.aerocorrect` sidecars.
/// A stream is tiled into windows of at most this many bytes; each window carries its
/// own independent RS parity segment, so generation/verification/repair touch at most
/// one window of plaintext at a time (bounded memory regardless of total file size).
/// 64 MiB balances per-window memory against per-file segment-table overhead.
pub(crate) const AEROCORRECT_WINDOW_SIZE: u64 = 64 * 1024 * 1024;

/// Tile a `total_len`-byte stream into `(window_offset, window_len)` windows of at
/// most `window` bytes, in order, contiguous, covering exactly `[0, total_len)`. A
/// zero-length stream yields one empty window `(0, 0)` so the sidecar always has at
/// least one segment (the format rejects a zero-segment file). `window` is treated as
/// at least 1 to avoid a zero step.
pub(crate) fn aerocorrect_windows(total_len: u64, window: u64) -> Vec<(u64, u64)> {
    if total_len == 0 {
        return vec![(0, 0)];
    }
    let w = window.max(1);
    let mut out = Vec::with_capacity((total_len.div_ceil(w)) as usize);
    let mut off = 0u64;
    while off < total_len {
        let len = w.min(total_len - off);
        out.push((off, len));
        off += len;
    }
    out
}

/// Exact serialized size of the windowed `.aerocorrect` sidecar for a `total_len`-byte
/// stream at overhead `pct`, tiled at `window`. Equals one per-sidecar frame plus, per
/// window, the per-segment header and that window's AVEC payload. Reduces to
/// `estimate_single_segment_sidecar_len` when the stream fits in a single window.
pub(crate) fn estimate_windowed_sidecar_len(total_len: u64, pct: u32, window: u64) -> u64 {
    let mut total = V2_SIDECAR_FIXED_LEN as u64;
    let segment_len = |len| (V2_SEGMENT_FRAMING_LEN as u64).saturating_add(avec_len_for(len, pct));
    if total_len == 0 {
        return total.saturating_add(segment_len(0));
    }
    let w = window.max(1);
    let full_windows = total_len / w;
    let tail = total_len % w;
    if full_windows > 0 {
        total = total.saturating_add(full_windows.saturating_mul(segment_len(w)));
    }
    if tail > 0 {
        total = total.saturating_add(segment_len(tail));
    }
    total
}

/// Confirm a parsed sidecar's segments tile their stream in order, contiguously, with
/// no gaps or overlaps, starting at 0 and covering exactly `total_len`. Windowed
/// streaming verify/repair reads the file sequentially window by window, so a sidecar
/// whose windows do not tile cleanly (a forged or foreign layout) is rejected before
/// any repair touches the file.
pub(crate) fn validate_window_tiling(
    segments: &[AeroCorrectSegment],
    total_len: u64,
) -> Result<(), String> {
    validate_window_tiling_iter(
        segments.iter().map(|s| (s.window_offset, s.window_len)),
        total_len,
    )
}

/// `validate_window_tiling` over any `(window_offset, window_len)` iterator, so the
/// file-backed streaming reader can validate its segment directory without
/// materializing `AeroCorrectSegment`s.
pub(crate) fn validate_window_tiling_iter(
    windows: impl IntoIterator<Item = (u64, u64)>,
    total_len: u64,
) -> Result<(), String> {
    let mut expected = 0u64;
    for (i, (window_offset, window_len)) in windows.into_iter().enumerate() {
        if window_offset != expected {
            return Err(format!(
                "aerocorrect window {i} starts at {window_offset} but expected {expected} (non-contiguous tiling)"
            ));
        }
        expected = expected
            .checked_add(window_len)
            .ok_or("aerocorrect window tiling overflows")?;
    }
    if expected != total_len {
        return Err(format!(
            "aerocorrect windows cover {expected} bytes but total_len is {total_len}"
        ));
    }
    Ok(())
}

/// I/O buffer for the streaming integrity hash pass. Bounds the read syscall size; not
/// the parity window (which is read whole per segment via `read_segment_avec`).
const SIDECAR_HASH_CHUNK: usize = 1024 * 1024;

/// One segment's location inside the sidecar file: the window it protects plus the byte
/// range of its parity (AVEC) payload. The parity bytes themselves are NOT loaded. For v2
/// sidecars `geom` carries the directory's protected copy of the payload's geometry header
/// (`None` for v1, where the in-payload header is trusted directly and must not be touched).
#[derive(Debug, Clone, Copy)]
pub(crate) struct AeroCorrectSegmentRef {
    pub(crate) window_offset: u64,
    pub(crate) window_len: u64,
    avec_file_offset: u64,
    avec_len: u64,
    geom: Option<[u8; GEOM_LEN]>,
}

/// File-backed streaming reader for a `.aerocorrect` sidecar. `open` parses the fixed
/// header and the segment directory (offsets/lengths only) and verifies the whole-file
/// integrity checksum in a single streaming pass, without ever holding a parity payload
/// in memory. Per-window parity is then read on demand via `read_segment_avec`, so the
/// repair path's memory is bounded to one window's parity regardless of sidecar size.
/// This is the streaming counterpart of `AeroCorrectSidecar::from_bytes`.
pub(crate) struct AeroCorrectSidecarReader {
    file: File,
    binding_id: [u8; BINDING_LEN],
    pub(crate) content_sha256: [u8; 32],
    pub(crate) total_len: u64,
    segments: Vec<AeroCorrectSegmentRef>,
}

impl AeroCorrectSidecarReader {
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        let mut file = File::open(path)
            .map_err(|e| format!("open aerocorrect sidecar {}: {e}", path.display()))?;
        let file_len = file
            .metadata()
            .map_err(|e| format!("stat aerocorrect sidecar {}: {e}", path.display()))?
            .len();
        if file_len < (AEROCORRECT_MAGIC.len() + 1) as u64 {
            return Err("aerocorrect sidecar too short".to_string());
        }
        let mut prefix = [0u8; AEROCORRECT_MAGIC.len() + 1];
        file.read_exact(&mut prefix)
            .map_err(|e| format!("read aerocorrect magic: {e}"))?;
        if &prefix[..AEROCORRECT_MAGIC.len()] != AEROCORRECT_MAGIC {
            return Err("bad aerocorrect sidecar magic".to_string());
        }
        match prefix[AEROCORRECT_MAGIC.len()] {
            AEROCORRECT_VERSION => Self::open_v2(file, file_len),
            AEROCORRECT_VERSION_V1 => Self::open_v1(file, file_len),
            version => Err(format!("unsupported aerocorrect sidecar version {version}")),
        }
    }

    /// Streaming open of the self-healing v2 framing: read only the prefix and the (tiny)
    /// directory-copies region into memory, recover the locator, and build segment refs
    /// pointing at the on-disk parity. The parity bytes are never loaded here, so memory
    /// stays bounded to the locator regardless of file size.
    fn open_v2(mut file: File, file_len: u64) -> Result<Self, String> {
        if file_len < V2_PREFIX_LEN as u64 {
            return Err("aerocorrect sidecar too short".to_string());
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|e| format!("seek aerocorrect prefix: {e}"))?;
        let mut prefix = [0u8; V2_PREFIX_LEN];
        file.read_exact(&mut prefix)
            .map_err(|e| format!("read aerocorrect prefix: {e}"))?;
        let counts = read_count_slots(&prefix[9..V2_PREFIX_LEN]);

        let mut last_err = "aerocorrect sidecar directory unrecoverable".to_string();
        for n in candidate_counts(&counts) {
            if n == 0 {
                continue;
            }
            let layout = match V2Layout::for_count(n as usize, file_len) {
                Some(l) => l,
                None => continue,
            };
            // Read just the directory-copies region (bounded by MAX_DIRECTORY_REGION).
            let copies_span = layout.parity_start - layout.copies_base;
            let mut copies_buf = vec![0u8; copies_span];
            file.seek(SeekFrom::Start(layout.copies_base as u64))
                .map_err(|e| format!("seek aerocorrect directory: {e}"))?;
            if file.read_exact(&mut copies_buf).is_err() {
                continue;
            }
            // recover_directory addresses copies at absolute offsets, so wrap the region
            // in a zero-prefixed view of the right length for it to index into.
            let mut head = vec![0u8; layout.parity_start];
            head[layout.copies_base..].copy_from_slice(&copies_buf);
            let dir = match recover_directory(&head, &layout) {
                Ok(d) => d,
                Err(e) => {
                    last_err = e;
                    continue;
                }
            };
            let decoded = match parse_v2_directory_fields(&dir, &layout) {
                Ok(v) => v,
                Err(e) => {
                    last_err = e;
                    continue;
                }
            };
            let segments = decoded
                .segments
                .into_iter()
                .map(|m| AeroCorrectSegmentRef {
                    window_offset: m.window_offset,
                    window_len: m.window_len,
                    avec_file_offset: layout.parity_start as u64 + m.avec_offset,
                    avec_len: m.avec_len,
                    geom: Some(m.geom),
                })
                .collect();
            return Ok(Self {
                file,
                binding_id: decoded.binding_id,
                content_sha256: decoded.content_sha256,
                total_len: decoded.total_len,
                segments,
            });
        }
        Err(last_err)
    }

    /// Streaming open of the legacy v1 framing (wholesale-checksum envelope, read-only).
    fn open_v1(mut file: File, file_len: u64) -> Result<Self, String> {
        if file_len < (HEADER_LEN + CHECKSUM_LEN) as u64 {
            return Err("aerocorrect sidecar too short".to_string());
        }
        let body_end = file_len - CHECKSUM_LEN as u64;

        file.seek(SeekFrom::Start(0))
            .map_err(|e| format!("seek aerocorrect header: {e}"))?;
        let mut header = [0u8; HEADER_LEN];
        file.read_exact(&mut header)
            .map_err(|e| format!("read aerocorrect header: {e}"))?;
        let mut off = AEROCORRECT_MAGIC.len() + 1;
        let mut binding_id = [0u8; BINDING_LEN];
        binding_id.copy_from_slice(&header[off..off + BINDING_LEN]);
        off += BINDING_LEN;
        let mut content_sha256 = [0u8; 32];
        content_sha256.copy_from_slice(&header[off..off + 32]);
        off += 32;
        let total_len = u64::from_le_bytes(header[off..off + 8].try_into().unwrap());
        off += 8;
        let segment_count = u32::from_le_bytes(header[off..off + 4].try_into().unwrap()) as usize;
        if segment_count == 0 {
            return Err("aerocorrect sidecar has no segments".to_string());
        }
        let max_segments = ((body_end - HEADER_LEN as u64) / SEGMENT_HEADER_LEN as u64) as usize;
        if segment_count > max_segments {
            return Err(format!(
                "aerocorrect sidecar segment count {segment_count} exceeds buffer capacity {max_segments}"
            ));
        }

        let mut segments = Vec::with_capacity(segment_count);
        let mut pos = HEADER_LEN as u64;
        for _ in 0..segment_count {
            if pos + SEGMENT_HEADER_LEN as u64 > body_end {
                return Err("aerocorrect sidecar truncated reading segment header".to_string());
            }
            file.seek(SeekFrom::Start(pos))
                .map_err(|e| format!("seek aerocorrect segment header: {e}"))?;
            let mut seg_hdr = [0u8; SEGMENT_HEADER_LEN];
            file.read_exact(&mut seg_hdr)
                .map_err(|e| format!("read aerocorrect segment header: {e}"))?;
            let window_offset = u64::from_le_bytes(seg_hdr[0..8].try_into().unwrap());
            let window_len = u64::from_le_bytes(seg_hdr[8..16].try_into().unwrap());
            let avec_len = u64::from_le_bytes(seg_hdr[16..24].try_into().unwrap());
            let avec_file_offset = pos + SEGMENT_HEADER_LEN as u64;
            validate_segment_avec_len(window_len, avec_len)?;
            if avec_file_offset
                .checked_add(avec_len)
                .is_none_or(|end| end > body_end)
            {
                return Err("aerocorrect sidecar segment length mismatch".to_string());
            }
            segments.push(AeroCorrectSegmentRef {
                window_offset,
                window_len,
                avec_file_offset,
                avec_len,
                geom: None,
            });
            pos = avec_file_offset + avec_len;
        }
        if pos != body_end {
            return Err("aerocorrect sidecar has unexpected trailing bytes".to_string());
        }

        // Verify the whole-file integrity checksum in one streaming pass (bounded memory).
        let mut stored = [0u8; CHECKSUM_LEN];
        file.seek(SeekFrom::Start(body_end))
            .map_err(|e| format!("seek aerocorrect checksum: {e}"))?;
        file.read_exact(&mut stored)
            .map_err(|e| format!("read aerocorrect checksum: {e}"))?;
        file.seek(SeekFrom::Start(0))
            .map_err(|e| format!("seek aerocorrect body: {e}"))?;
        let mut hasher = blake3::Hasher::new();
        let mut remaining = body_end;
        let mut buf = vec![0u8; SIDECAR_HASH_CHUNK];
        while remaining > 0 {
            let n = remaining.min(buf.len() as u64) as usize;
            file.read_exact(&mut buf[..n])
                .map_err(|e| format!("read aerocorrect body for checksum: {e}"))?;
            hasher.update(&buf[..n]);
            remaining -= n as u64;
        }
        if hasher.finalize().as_bytes() != &stored {
            return Err("aerocorrect sidecar integrity check failed".to_string());
        }

        Ok(Self {
            file,
            binding_id,
            content_sha256,
            total_len,
            segments,
        })
    }

    /// Confirm this sidecar belongs to content with `content_sha256` (same semantics as
    /// `AeroCorrectSidecar::verify_binding`).
    pub(crate) fn verify_binding(&self, content_sha256: &[u8; 32]) -> Result<(), String> {
        if &self.content_sha256 == content_sha256
            && self.binding_id == aerocorrect_binding_id(content_sha256)
        {
            Ok(())
        } else {
            Err("aerocorrect sidecar does not match this content (binding mismatch)".to_string())
        }
    }

    pub(crate) fn segments(&self) -> &[AeroCorrectSegmentRef] {
        &self.segments
    }

    /// Read one segment's parity (AVEC) bytes on demand. Only this window's parity is
    /// resident; callers process windows one at a time for bounded memory. For v2 sidecars
    /// the in-payload geometry header is healed from the directory's protected copy before
    /// the bytes are returned, so a rotted header cannot block reconstruction.
    pub(crate) fn read_segment_avec(&mut self, index: usize) -> Result<Vec<u8>, String> {
        let seg = *self
            .segments
            .get(index)
            .ok_or_else(|| format!("aerocorrect segment index {index} out of range"))?;
        let off = seg.avec_file_offset;
        let len = usize::try_from(seg.avec_len)
            .map_err(|_| "aerocorrect segment length exceeds usize".to_string())?;
        self.file
            .seek(SeekFrom::Start(off))
            .map_err(|e| format!("seek aerocorrect segment avec: {e}"))?;
        let mut buf = vec![0u8; len];
        self.file
            .read_exact(&mut buf)
            .map_err(|e| format!("read aerocorrect segment avec: {e}"))?;
        if let Some(geom) = seg.geom {
            let g = buf.len().min(GEOM_LEN);
            buf[..g].copy_from_slice(&geom[..g]);
        }
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::super::compute_error_correction_shards_grid;
    use super::*;
    use sha2::{Digest, Sha256};

    fn sample(len: usize) -> Vec<u8> {
        let mut seed = *blake3::hash(b"aerocorrect-seed").as_bytes();
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            seed = *blake3::hash(&seed).as_bytes();
            out.extend_from_slice(&seed);
        }
        out.truncate(len);
        out
    }

    fn sha256(data: &[u8]) -> [u8; 32] {
        let d = Sha256::digest(data);
        let mut out = [0u8; 32];
        out.copy_from_slice(&d);
        out
    }

    fn single_segment(data: &[u8], pct: u32) -> AeroCorrectSidecar {
        let (k, p) = error_correction_grid(pct);
        let (avec, _shards, _prot, _ovh) = compute_error_correction_shards_grid(&[data], k, p);
        AeroCorrectSidecar::new(
            sha256(data),
            data.len() as u64,
            vec![AeroCorrectSegment {
                window_offset: 0,
                window_len: data.len() as u64,
                avec_bytes: avec,
            }],
        )
    }

    #[test]
    fn round_trips_and_binds() {
        let data = sample(96 * 1024 + 17);
        let sc = single_segment(&data, 15);
        let bytes = sc.to_bytes();
        let parsed = AeroCorrectSidecar::from_bytes(&bytes).expect("parse");
        assert_eq!(parsed, sc);
        assert_eq!(parsed.to_bytes(), bytes);
        parsed
            .verify_binding(&sha256(&data))
            .expect("binding holds");
    }

    #[test]
    fn rejects_wrong_content_binding() {
        let data = sample(64 * 1024);
        let sc = single_segment(&data, 20);
        let other = sample(64 * 1024 + 1);
        assert!(sc.verify_binding(&sha256(&other)).is_err());
    }

    #[test]
    fn rejects_bad_magic_version_and_truncation() {
        let data = sample(40_000);
        let bytes = single_segment(&data, 20).to_bytes();
        // bad magic
        let mut bad = bytes.clone();
        bad[0] ^= 0xFF;
        assert!(AeroCorrectSidecar::from_bytes(&bad).is_err());
        // bad version
        let mut badv = bytes.clone();
        badv[8] = 0xFE;
        assert!(AeroCorrectSidecar::from_bytes(&badv).is_err());
        // truncated parity region: the directory still parses but the declared parity no
        // longer matches the shrunken file.
        assert!(AeroCorrectSidecar::from_bytes(&bytes[..bytes.len() - 1]).is_err());
        // forged huge segment count (all three count slots) cannot drive a giant allocation.
        let mut forged = bytes.clone();
        for i in 0..DIRECTORY_COPIES {
            let off = 9 + i * 4;
            forged[off..off + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        }
        assert!(AeroCorrectSidecar::from_bytes(&forged).is_err());
    }

    #[test]
    fn estimate_matches_real_single_segment_len() {
        for &len in &[0usize, 1, 100, 4096, 4097, 50_000, 1_000_000] {
            for &pct in &[7u32, 15, 20, 25, 30, 50] {
                let data = sample(len);
                let real = single_segment(&data, pct).to_bytes().len() as u64;
                let est = estimate_single_segment_sidecar_len(len as u64, pct);
                assert_eq!(est, real, "estimate != real len={len} pct={pct}");
            }
        }
        let big = sample(10 * 1024 * 1024);
        let real = single_segment(&big, 50).to_bytes().len() as u64;
        assert_eq!(
            estimate_single_segment_sidecar_len(big.len() as u64, 50),
            real
        );
    }

    /// Build a real windowed sidecar the way the windowed generator does: one segment
    /// per window, each carrying that window's own AVEC parity.
    fn windowed(data: &[u8], pct: u32, window: u64) -> AeroCorrectSidecar {
        let (k, p) = error_correction_grid(pct);
        let segments = aerocorrect_windows(data.len() as u64, window)
            .into_iter()
            .map(|(off, len)| {
                let w = &data[off as usize..(off + len) as usize];
                let (avec, _s, _prot, _ovh) = compute_error_correction_shards_grid(&[w], k, p);
                AeroCorrectSegment {
                    window_offset: off,
                    window_len: len,
                    avec_bytes: avec,
                }
            })
            .collect();
        AeroCorrectSidecar::new(sha256(data), data.len() as u64, segments)
    }

    #[test]
    fn windows_tile_contiguously_and_cover_total() {
        assert_eq!(aerocorrect_windows(0, 64), vec![(0, 0)]);
        assert_eq!(aerocorrect_windows(64, 64), vec![(0, 64)]);
        assert_eq!(aerocorrect_windows(65, 64), vec![(0, 64), (64, 1)]);
        assert_eq!(
            aerocorrect_windows(200, 64),
            vec![(0, 64), (64, 64), (128, 64), (192, 8)]
        );
        // Tiling always validates against its own total.
        for &total in &[0u64, 1, 63, 64, 65, 200, 4096] {
            let segs: Vec<_> = aerocorrect_windows(total, 64)
                .into_iter()
                .map(|(o, l)| AeroCorrectSegment {
                    window_offset: o,
                    window_len: l,
                    avec_bytes: vec![],
                })
                .collect();
            validate_window_tiling(&segs, total).expect("self-tiling must validate");
        }
    }

    #[test]
    fn estimate_windowed_matches_real_multi_window() {
        // Force multiple windows with a small window size so the test stays cheap.
        let window = 50_000u64;
        for &len in &[0usize, 1, 49_999, 50_000, 50_001, 130_000, 200_003] {
            for &pct in &[7u32, 20, 50] {
                let data = sample(len);
                let real = windowed(&data, pct, window).to_bytes().len() as u64;
                let est = estimate_windowed_sidecar_len(len as u64, pct, window);
                assert_eq!(est, real, "windowed estimate != real len={len} pct={pct}");
            }
        }
    }

    #[test]
    fn estimate_windowed_handles_huge_lengths_without_materializing_windows() {
        assert_eq!(estimate_windowed_sidecar_len(u64::MAX, 50, 1), u64::MAX);
    }

    #[test]
    fn multi_window_round_trips_and_rejects_bad_tiling() {
        let window = 40_000u64;
        let data = sample(135_000);
        let sc = windowed(&data, 20, window);
        assert!(sc.segments.len() >= 3, "expected several windows");
        let parsed = AeroCorrectSidecar::from_bytes(&sc.to_bytes()).expect("parse");
        assert_eq!(parsed, sc);
        validate_window_tiling(&parsed.segments, data.len() as u64).expect("good tiling");
        // A gap in the tiling is rejected.
        let mut holey = parsed.clone();
        holey.segments[1].window_offset += 1;
        assert!(validate_window_tiling(&holey.segments, data.len() as u64).is_err());
        // Windows that under-cover the declared total are rejected.
        assert!(validate_window_tiling(&parsed.segments, data.len() as u64 + 1).is_err());
    }

    #[test]
    fn streaming_reader_matches_in_memory_parse() {
        let window = 40_000u64;
        let data = sample(135_000);
        let sc = windowed(&data, 20, window);
        assert!(sc.segments.len() >= 3, "expected several windows");
        let bytes = sc.to_bytes();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.aerocorrect");
        std::fs::write(&path, &bytes).unwrap();

        let mut reader = AeroCorrectSidecarReader::open(&path).expect("open streaming reader");
        // Header + directory match the in-memory parse.
        assert_eq!(reader.content_sha256, sc.content_sha256);
        assert_eq!(reader.total_len, sc.total_len);
        assert_eq!(reader.segments().len(), sc.segments.len());
        for (i, seg) in sc.segments.iter().enumerate() {
            assert_eq!(reader.segments()[i].window_offset, seg.window_offset);
            assert_eq!(reader.segments()[i].window_len, seg.window_len);
            // On-demand parity read is byte-identical to the in-memory segment.
            assert_eq!(reader.read_segment_avec(i).unwrap(), seg.avec_bytes);
        }
        reader
            .verify_binding(&sc.content_sha256)
            .expect("binding holds");

        // Tiling validates over the directory iterator the streamed repair uses.
        validate_window_tiling_iter(
            reader
                .segments()
                .iter()
                .map(|s| (s.window_offset, s.window_len)),
            data.len() as u64,
        )
        .expect("good tiling");
    }

    /// Byte offset of v2 directory copy `i` for a sidecar with `n` segments.
    fn v2_copy_base(n: usize, i: usize) -> usize {
        let dir_len = DIR_FIXED_LEN + DIR_PER_SEG_LEN * n;
        V2_PREFIX_LEN + i * (dir_len + COPY_CKSUM_LEN)
    }

    /// First byte of the parity region for a v2 sidecar with `n` segments.
    fn v2_parity_start(n: usize) -> usize {
        let dir_len = DIR_FIXED_LEN + DIR_PER_SEG_LEN * n;
        V2_PREFIX_LEN + DIRECTORY_COPIES * (dir_len + COPY_CKSUM_LEN)
    }

    #[test]
    fn streaming_reader_tolerates_parity_rot_but_rejects_structural_damage() {
        let data = sample(80_000);
        let bytes = single_segment(&data, 20).to_bytes();
        let dir = tempfile::tempdir().unwrap();

        // A flipped byte in the parity region NO LONGER blocks open (v2 has no wholesale
        // envelope): the rotted shard is left for the per-shard RS routing at repair time.
        let mut rotted = bytes.clone();
        rotted[v2_parity_start(1) + 64] ^= 0xFF;
        let p1 = dir.path().join("parity_rot.aerocorrect");
        std::fs::write(&p1, &rotted).unwrap();
        let reader = AeroCorrectSidecarReader::open(&p1).expect("parity rot must not block open");
        assert_eq!(reader.content_sha256, sha256(&data));

        // A truncated file is still rejected (declared parity no longer fits).
        let p2 = dir.path().join("short.aerocorrect");
        std::fs::write(&p2, &bytes[..V2_PREFIX_LEN + 4]).unwrap();
        assert!(AeroCorrectSidecarReader::open(&p2).is_err());

        // The locator damaged past self-heal (the SAME binding byte clobbered in all three
        // directory copies) is rejected: no copy validates and the majority vote is no
        // longer internally consistent.
        let mut wrecked = bytes.clone();
        for i in 0..DIRECTORY_COPIES {
            wrecked[v2_copy_base(1, i) + 10] = 0xFF; // byte inside binding_id
        }
        let p3 = dir.path().join("wrecked.aerocorrect");
        std::fs::write(&p3, &wrecked).unwrap();
        assert!(AeroCorrectSidecarReader::open(&p3).is_err());
    }

    /// A single corrupted directory copy heals from a good sibling: a lightly-damaged
    /// locator still opens and serves byte-identical parity (Ehud #276 point 2).
    #[test]
    fn one_damaged_directory_copy_heals_from_a_good_one() {
        let data = sample(80_000);
        let sc = single_segment(&data, 20);
        let mut bytes = sc.to_bytes();
        // Clobber the FIRST directory copy (header + a per-segment field); copies 2/3 heal it.
        bytes[v2_copy_base(1, 0) + 4] ^= 0xFF; // total_len byte
        bytes[v2_copy_base(1, 0) + DIR_FIXED_LEN + 18] ^= 0xFF; // an avec_len byte

        let parsed = AeroCorrectSidecar::from_bytes(&bytes).expect("heals from a good copy");
        assert_eq!(parsed, sc);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("healed.aerocorrect");
        std::fs::write(&path, &bytes).unwrap();
        let mut reader = AeroCorrectSidecarReader::open(&path).expect("streaming heal");
        assert_eq!(reader.total_len, sc.total_len);
        assert_eq!(
            reader.read_segment_avec(0).unwrap(),
            sc.segments[0].avec_bytes
        );
    }

    /// When no single directory copy survives its checksum but each byte is intact in a
    /// majority of copies, the locator is reconstructed by per-byte majority vote.
    #[test]
    fn byte_majority_heals_when_no_single_copy_is_intact() {
        let data = sample(80_000);
        let sc = single_segment(&data, 20);
        let mut bytes = sc.to_bytes();
        // One distinct corrupted byte per copy: no copy validates, but every byte position
        // is correct in 2 of 3 copies, so the majority vote rebuilds the original.
        bytes[v2_copy_base(1, 0) + 5] ^= 0xFF;
        bytes[v2_copy_base(1, 1) + DIR_FIXED_LEN + 3] ^= 0xFF;
        bytes[v2_copy_base(1, 2) + 20] ^= 0xFF;

        let parsed = AeroCorrectSidecar::from_bytes(&bytes).expect("majority vote heals locator");
        assert_eq!(parsed, sc);
    }

    /// A rotted in-payload geometry header is healed from the directory's protected copy,
    /// so the AVEC still parses and reconstructs.
    #[test]
    fn rotted_geometry_header_heals_from_directory_copy() {
        let data = sample(80_000);
        let sc = single_segment(&data, 20);
        let mut bytes = sc.to_bytes();
        // Corrupt the first bytes of the on-disk AVEC payload (its geometry header).
        let ps = v2_parity_start(1);
        bytes[ps] ^= 0xFF;
        bytes[ps + 5] ^= 0xFF;

        let parsed = AeroCorrectSidecar::from_bytes(&bytes).expect("geom header healed");
        assert_eq!(parsed.segments[0].avec_bytes, sc.segments[0].avec_bytes);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("geom.aerocorrect");
        std::fs::write(&path, &bytes).unwrap();
        let mut reader = AeroCorrectSidecarReader::open(&path).expect("streaming geom heal");
        assert_eq!(
            reader.read_segment_avec(0).unwrap(),
            sc.segments[0].avec_bytes
        );
    }

    /// Beyond the self-heal budget (the locator clobbered in all three copies) a repair
    /// fails closed and leaves the target file byte-for-byte untouched: open rejects the
    /// sidecar before the repair path can touch the file.
    #[test]
    fn locator_destroyed_beyond_budget_fails_repair_closed() {
        use super::super::aerosync::{
            generate_sync_sidecar_for_bytes, verify_repair_sync_file_streamed,
        };
        let data = sample(80_000);
        let generated = generate_sync_sidecar_for_bytes("rel", &data, 20);
        let mut bytes = generated.sidecar_bytes.clone();
        // Clobber the binding in all three directory copies (past majority-vote recovery).
        for i in 0..DIRECTORY_COPIES {
            bytes[v2_copy_base(1, i) + 10] = 0xFF;
        }

        let dir = tempfile::tempdir().unwrap();
        let sidecar_path = dir.path().join("wrecked.aerocorrect");
        std::fs::write(&sidecar_path, &bytes).unwrap();
        let path = dir.path().join("payload.bin");
        let mut corrupt = data.clone();
        corrupt[5_000] ^= 0xAA; // a normally-recoverable hit
        std::fs::write(&path, &corrupt).unwrap();
        let before = std::fs::read(&path).unwrap();

        assert!(
            verify_repair_sync_file_streamed("rel", &generated.file_sha256, &path, &sidecar_path)
                .is_err(),
            "a sidecar damaged past self-heal must not repair"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "target untouched when the sidecar cannot be opened"
        );
    }

    /// Encode a sidecar in the legacy v1 framing (wholesale-checksum envelope) so the
    /// dual-read path can be exercised without a real on-disk v1 fixture.
    fn v1_encode(sc: &AeroCorrectSidecar) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(AEROCORRECT_MAGIC);
        out.push(AEROCORRECT_VERSION_V1);
        out.extend_from_slice(&sc.binding_id);
        out.extend_from_slice(&sc.content_sha256);
        out.extend_from_slice(&sc.total_len.to_le_bytes());
        out.extend_from_slice(&(sc.segments.len() as u32).to_le_bytes());
        for s in &sc.segments {
            out.extend_from_slice(&s.window_offset.to_le_bytes());
            out.extend_from_slice(&s.window_len.to_le_bytes());
            out.extend_from_slice(&(s.avec_bytes.len() as u64).to_le_bytes());
            out.extend_from_slice(&s.avec_bytes);
        }
        let cks = blake3::hash(&out);
        out.extend_from_slice(cks.as_bytes());
        out
    }

    /// Read-back of a legacy v1 sidecar (dual-read), both in-memory and streaming. The v1
    /// in-payload geometry header must be left untouched (no geom splice on the v1 path).
    #[test]
    fn reads_back_legacy_v1_sidecar() {
        let data = sample(96 * 1024 + 17);
        let sc = single_segment(&data, 15);
        let v1 = v1_encode(&sc);
        assert_eq!(v1[8], AEROCORRECT_VERSION_V1);

        let parsed = AeroCorrectSidecar::from_bytes(&v1).expect("v1 in-memory read-back");
        assert_eq!(parsed, sc);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.aerocorrect");
        std::fs::write(&path, &v1).unwrap();
        let mut reader = AeroCorrectSidecarReader::open(&path).expect("v1 streaming read-back");
        assert_eq!(reader.content_sha256, sc.content_sha256);
        assert_eq!(reader.total_len, sc.total_len);
        // The v1 AVEC is served verbatim (geometry header intact), not geom-spliced.
        assert_eq!(
            reader.read_segment_avec(0).unwrap(),
            sc.segments[0].avec_bytes
        );

        // v1 keeps its wholesale-reject behavior: a flipped parity byte is rejected at open.
        let mut rotted = v1.clone();
        rotted[HEADER_LEN + SEGMENT_HEADER_LEN + 8] ^= 0xFF;
        let p2 = dir.path().join("legacy_rot.aerocorrect");
        std::fs::write(&p2, &rotted).unwrap();
        assert!(AeroCorrectSidecarReader::open(&p2).is_err());
    }

    #[test]
    fn rejects_segment_avec_len_above_window_max_before_reading_payload() {
        let max_avec = avec_len_for(1, ERROR_CORRECTION_MAX_PCT);
        // A directory claiming an AVEC longer than any legal payload for its window is
        // rejected by `validate_segment_avec_len` before the payload is materialized.
        let oversize = AeroCorrectSidecar::new(
            sha256(&[0u8]),
            1,
            vec![AeroCorrectSegment {
                window_offset: 0,
                window_len: 1,
                avec_bytes: vec![0u8; max_avec as usize + 1],
            }],
        );
        let forged = oversize.to_bytes();

        let err = AeroCorrectSidecar::from_bytes(&forged).expect_err("oversize avec rejected");
        assert!(err.contains("exceeds maximum"), "got: {err}");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oversize.aerocorrect");
        std::fs::write(&path, &forged).unwrap();
        let err = match AeroCorrectSidecarReader::open(&path) {
            Ok(_) => panic!("streaming oversize avec must be rejected"),
            Err(err) => err,
        };
        assert!(err.contains("exceeds maximum"), "got: {err}");
    }
}
