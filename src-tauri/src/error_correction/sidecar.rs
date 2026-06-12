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
/// On-disk format version. Bump only on a wire-incompatible change.
pub(crate) const AEROCORRECT_VERSION: u8 = 1;
/// Conventional sidecar extension: `<file>` -> `<file>.aerocorrect`.
pub(crate) const AEROCORRECT_EXTENSION: &str = ".aerocorrect";

const BINDING_DOMAIN: &[u8] = b"aerocorrect-binding-v1";
const BINDING_LEN: usize = 32;
const CHECKSUM_LEN: usize = 32;
/// magic(8) + version(1) + binding(32) + content_sha256(32) + total_len(8)
/// + segment_count(4).
const HEADER_LEN: usize = 8 + 1 + BINDING_LEN + 32 + 8 + 4;
/// Per-segment fixed header: window_offset(8) + window_len(8) + avec_len(8).
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

    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let segments_len: usize = self
            .segments
            .iter()
            .map(|s| SEGMENT_HEADER_LEN + s.avec_bytes.len())
            .sum();
        let mut out = Vec::with_capacity(HEADER_LEN + segments_len + CHECKSUM_LEN);
        out.extend_from_slice(AEROCORRECT_MAGIC);
        out.push(AEROCORRECT_VERSION);
        out.extend_from_slice(&self.binding_id);
        out.extend_from_slice(&self.content_sha256);
        out.extend_from_slice(&self.total_len.to_le_bytes());
        out.extend_from_slice(&(self.segments.len() as u32).to_le_bytes());
        for segment in &self.segments {
            out.extend_from_slice(&segment.window_offset.to_le_bytes());
            out.extend_from_slice(&segment.window_len.to_le_bytes());
            out.extend_from_slice(&(segment.avec_bytes.len() as u64).to_le_bytes());
            out.extend_from_slice(&segment.avec_bytes);
        }
        let checksum = blake3::hash(&out);
        out.extend_from_slice(checksum.as_bytes());
        out
    }

    pub(crate) fn from_bytes(data: &[u8]) -> Result<Self, String> {
        if data.len() < HEADER_LEN + CHECKSUM_LEN {
            return Err("aerocorrect sidecar too short".to_string());
        }
        if &data[..AEROCORRECT_MAGIC.len()] != AEROCORRECT_MAGIC {
            return Err("bad aerocorrect sidecar magic".to_string());
        }
        let version = data[AEROCORRECT_MAGIC.len()];
        if version != AEROCORRECT_VERSION {
            return Err(format!("unsupported aerocorrect sidecar version {version}"));
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
        // Read from an untrusted remote. Each segment occupies at least
        // SEGMENT_HEADER_LEN bytes on the wire, so a claimed count larger than the
        // buffer can physically hold is a forgery: reject before `with_capacity` so a
        // crafted u32 (~4e9) cannot drive a multi-GB allocation/abort.
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
    (HEADER_LEN + SEGMENT_HEADER_LEN + CHECKSUM_LEN) as u64 + avec_len_for(content_len, pct)
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
    let mut total = (HEADER_LEN + CHECKSUM_LEN) as u64;
    let segment_len = |len| (SEGMENT_HEADER_LEN as u64).saturating_add(avec_len_for(len, pct));
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
/// range of its parity (AVEC) payload. The parity bytes themselves are NOT loaded.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AeroCorrectSegmentRef {
    pub(crate) window_offset: u64,
    pub(crate) window_len: u64,
    avec_file_offset: u64,
    avec_len: u64,
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
        if file_len < (HEADER_LEN + CHECKSUM_LEN) as u64 {
            return Err("aerocorrect sidecar too short".to_string());
        }
        let body_end = file_len - CHECKSUM_LEN as u64;

        // Fixed header.
        let mut header = [0u8; HEADER_LEN];
        file.read_exact(&mut header)
            .map_err(|e| format!("read aerocorrect header: {e}"))?;
        if &header[..AEROCORRECT_MAGIC.len()] != AEROCORRECT_MAGIC {
            return Err("bad aerocorrect sidecar magic".to_string());
        }
        let version = header[AEROCORRECT_MAGIC.len()];
        if version != AEROCORRECT_VERSION {
            return Err(format!("unsupported aerocorrect sidecar version {version}"));
        }
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
        // Same anti-forgery bound as `from_bytes`: a claimed count larger than the file
        // can physically hold is rejected before `with_capacity`.
        let max_segments = ((body_end - HEADER_LEN as u64) / SEGMENT_HEADER_LEN as u64) as usize;
        if segment_count > max_segments {
            return Err(format!(
                "aerocorrect sidecar segment count {segment_count} exceeds buffer capacity {max_segments}"
            ));
        }

        // Walk the segment directory, seeking past each AVEC payload (never reading it).
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
    /// resident; callers process windows one at a time for bounded memory.
    pub(crate) fn read_segment_avec(&mut self, index: usize) -> Result<Vec<u8>, String> {
        let seg = self
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
        // truncated
        assert!(AeroCorrectSidecar::from_bytes(&bytes[..bytes.len() - 1]).is_err());
        // forged huge segment count cannot drive a giant allocation
        let mut forged = bytes.clone();
        let sc_off = 8 + 1 + BINDING_LEN + 32 + 8;
        forged[sc_off..sc_off + 4].copy_from_slice(&u32::MAX.to_le_bytes());
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

    #[test]
    fn streaming_reader_rejects_corruption() {
        let data = sample(80_000);
        let bytes = single_segment(&data, 20).to_bytes();
        let dir = tempfile::tempdir().unwrap();

        // A flipped byte in the parity body fails the whole-file integrity checksum.
        let mut corrupt = bytes.clone();
        corrupt[HEADER_LEN + SEGMENT_HEADER_LEN + 4] ^= 0xFF;
        let p1 = dir.path().join("corrupt.aerocorrect");
        std::fs::write(&p1, &corrupt).unwrap();
        assert!(AeroCorrectSidecarReader::open(&p1).is_err());

        // A truncated file is rejected too.
        let p2 = dir.path().join("short.aerocorrect");
        std::fs::write(&p2, &bytes[..HEADER_LEN + 4]).unwrap();
        assert!(AeroCorrectSidecarReader::open(&p2).is_err());
    }

    #[test]
    fn rejects_segment_avec_len_above_window_max_before_reading_payload() {
        let data = sample(1);
        let bytes = single_segment(&data, 20).to_bytes();
        let max_avec = avec_len_for(data.len() as u64, ERROR_CORRECTION_MAX_PCT);
        let mut body = bytes[..bytes.len() - CHECKSUM_LEN].to_vec();
        let avec_len_off = HEADER_LEN + 8 + 8;
        body[avec_len_off..avec_len_off + 8].copy_from_slice(&(max_avec + 1).to_le_bytes());
        body.push(0);
        let checksum = blake3::hash(&body);
        let mut forged = body;
        forged.extend_from_slice(checksum.as_bytes());

        let err = AeroCorrectSidecar::from_bytes(&forged).expect_err("oversize avec rejected");
        assert!(err.contains("exceeds maximum"));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oversize.aerocorrect");
        std::fs::write(&path, &forged).unwrap();
        let err = match AeroCorrectSidecarReader::open(&path) {
            Ok(_) => panic!("streaming oversize avec rejected"),
            Err(err) => err,
        };
        assert!(err.contains("exceeds maximum"));
    }
}
