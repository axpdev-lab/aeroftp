use reed_solomon_erasure::ReedSolomon;

pub(crate) mod aerosync;
pub(crate) mod sidecar;

/// P2-09: On-disk payload format (v2) for Reed-Solomon Error Correction.
///
/// v1 mapped one ciphertext block to one Reed-Solomon shard and sized every shard
/// to the largest block. With content-defined chunking (min 256 KiB, avg 1 MiB) real
/// vaults have few, large chunks, so under-filled stripes still stored two full-size
/// parity shards: a 300 KB single-chunk vault produced ~600 KB of parity (approx 200%).
///
/// v2 protects the *concatenated* live-block stream with a fixed shard grid:
///   - Concatenate the protected byte blocks into one logical stream D of length L.
///   - Cut D into a regular grid of `shard_size` (S) data shards; S is chosen so a
///     small payload is exactly one full RS group (overhead == P/K) and a large payload
///     is many full groups (capped granularity). Overhead is approx P/K regardless of
///     how many / how large the blocks are.
///   - Each group of K data shards gets P parity shards via RS(K, P).
///
/// Damage localization stores a truncated-BLAKE3 checksum per shard (data and parity).
/// On repair, any shard whose checksum mismatches is treated as an RS erasure, so
/// localized rot erases only the affected shard(s), and rotted parity is detected and
/// routed around. Callers still perform their own end-to-end verification after repair.
///
/// Layout (all multi-byte fields little-endian):
///   [ErrorCorrectionPayloadHeader: 32 bytes]
///   [data-shard checksums:   num_data_shards * ERROR_CORRECTION_SHARD_CKSUM_LEN]
///   [parity-shard checksums: num_groups * P  * ERROR_CORRECTION_SHARD_CKSUM_LEN]
///   [parity data:            num_groups * P  * S]
/// where num_data_shards = ceil(L/S) and num_groups = ceil(num_data_shards/K).
///
/// The format is pre-release; bumping ERROR_CORRECTION_PAYLOAD_VERSION needs no migration.
pub(crate) const ERROR_CORRECTION_PAYLOAD_MAGIC: &[u8; 4] = b"AVEC";
pub(crate) const ERROR_CORRECTION_PAYLOAD_VERSION: u16 = 2;

/// Reed-Solomon group geometry. K data + P parity per group => P/K == 20% overhead,
/// tolerating up to P erased shards (data or parity) per group.
pub(crate) const ERROR_CORRECTION_DATA_SHARDS: usize = 10;
pub(crate) const ERROR_CORRECTION_PARITY_SHARDS: usize = 2;
/// Shard-size grid bounds. For small payloads S = ceil(L/K) yields a single full group
/// (exactly P/K overhead); ERROR_CORRECTION_MIN_SHARD keeps micro-payload shards sane and
/// ERROR_CORRECTION_MAX_SHARD bounds shard granularity and per-shard recovery cost.
pub(crate) const ERROR_CORRECTION_MIN_SHARD: usize = 4096;
pub(crate) const ERROR_CORRECTION_MAX_SHARD: usize = 1 << 20; // 1 MiB
/// Truncated BLAKE3 length stored per shard for erasure localization. 128 bits makes
/// an accidental-rot collision (~2^-128) irrelevant; this is a rot detector, not a
/// security primitive.
pub(crate) const ERROR_CORRECTION_SHARD_CKSUM_LEN: usize = 16;

/// QR-style overhead levels (#276). The user picks a target storage-overhead
/// percentage; the grid below maps it to a Reed-Solomon (K, P). The default 20%
/// reproduces the original fixed K=10/P=2 grid, so payloads created before this knob
/// (no recorded percentage) keep their exact geometry.
pub(crate) const ERROR_CORRECTION_DEFAULT_PCT: u32 = 20;
pub(crate) const ERROR_CORRECTION_MIN_PCT: u32 = 5;
pub(crate) const ERROR_CORRECTION_MAX_PCT: u32 = 50;

/// Map a target storage-overhead percentage to a Reed-Solomon (K data, P parity)
/// group. Overhead is P/K; we fix P (one parity shard for the lowest band, two above
/// it for two-shard erasure tolerance) then choose the K closest to the target ratio.
/// `pct` is clamped to [MIN, MAX]. 20% -> (10, 2); approx 7% -> (14, 1);
/// approx 30% -> (7, 2).
pub(crate) fn error_correction_grid(pct: u32) -> (usize, usize) {
    let pct = pct.clamp(ERROR_CORRECTION_MIN_PCT, ERROR_CORRECTION_MAX_PCT);
    let p: u32 = if pct < 10 { 1 } else { 2 };
    // K = round(P*100 / pct), at least 2 so every group keeps real data slots.
    let k = (((p * 100) + pct / 2) / pct).max(2);
    (k as usize, p as usize)
}

/// The (K, P) grid to use from a recorded overhead percentage (or the default for
/// payloads created before the percentage knob existed).
pub(crate) fn manifest_error_correction_grid(error_correction_pct: Option<u32>) -> (usize, usize) {
    error_correction_grid(error_correction_pct.unwrap_or(ERROR_CORRECTION_DEFAULT_PCT))
}

/// 16-byte rot-detection checksum for one shard.
pub(crate) fn error_correction_shard_checksum(
    shard: &[u8],
) -> [u8; ERROR_CORRECTION_SHARD_CKSUM_LEN] {
    let h = blake3::hash(shard);
    let mut out = [0u8; ERROR_CORRECTION_SHARD_CKSUM_LEN];
    out.copy_from_slice(&h.as_bytes()[..ERROR_CORRECTION_SHARD_CKSUM_LEN]);
    out
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ErrorCorrectionPayloadHeader {
    pub(crate) data_shards: u16,    // K per group
    pub(crate) parity_shards: u16,  // P per group
    pub(crate) shard_size: u32,     // S (bytes per shard; data is zero-padded to this)
    pub(crate) total_data_len: u64, // L (length of the concatenated protected stream)
}

impl ErrorCorrectionPayloadHeader {
    pub(crate) fn to_bytes(self) -> [u8; 32] {
        let mut buf = [0u8; 32];
        buf[0..4].copy_from_slice(ERROR_CORRECTION_PAYLOAD_MAGIC);
        buf[4..6].copy_from_slice(&ERROR_CORRECTION_PAYLOAD_VERSION.to_le_bytes());
        buf[6..8].copy_from_slice(&self.data_shards.to_le_bytes());
        buf[8..10].copy_from_slice(&self.parity_shards.to_le_bytes());
        buf[10..14].copy_from_slice(&self.shard_size.to_le_bytes());
        buf[14..22].copy_from_slice(&self.total_data_len.to_le_bytes());
        // bytes 22..32 reserved (zero)
        buf
    }

    pub(crate) fn from_bytes(data: &[u8]) -> Result<Self, String> {
        if data.len() < 32 {
            return Err("ErrorCorrectionPayloadHeader too short".to_string());
        }
        if &data[0..4] != ERROR_CORRECTION_PAYLOAD_MAGIC {
            return Err("bad Error Correction payload magic".to_string());
        }
        let version = u16::from_le_bytes(data[4..6].try_into().unwrap());
        if version != ERROR_CORRECTION_PAYLOAD_VERSION {
            return Err(format!(
                "unsupported Error Correction payload version {}",
                version
            ));
        }
        let h = ErrorCorrectionPayloadHeader {
            data_shards: u16::from_le_bytes(data[6..8].try_into().unwrap()),
            parity_shards: u16::from_le_bytes(data[8..10].try_into().unwrap()),
            shard_size: u32::from_le_bytes(data[10..14].try_into().unwrap()),
            total_data_len: u64::from_le_bytes(data[14..22].try_into().unwrap()),
        };
        if h.data_shards == 0 || h.shard_size == 0 {
            return Err(
                "invalid Error Correction payload header (zero shard geometry)".to_string(),
            );
        }
        Ok(h)
    }
}

/// (num_data_shards, num_groups) derived from a header.
pub(crate) fn error_correction_geometry(h: &ErrorCorrectionPayloadHeader) -> (usize, usize) {
    let k = h.data_shards as usize;
    let s = h.shard_size as usize;
    let l = h.total_data_len as usize;
    let num_data_shards = l.div_ceil(s);
    let num_groups = num_data_shards.div_ceil(k);
    (num_data_shards, num_groups)
}

/// Full in-memory representation of one Error Correction payload (v2). This is what
/// gets serialized as the AVEC blob in vault sidecars/extensions and future sync EC.
#[derive(Debug, Clone)]
pub(crate) struct ErrorCorrectionPayload {
    pub(crate) header: ErrorCorrectionPayloadHeader,
    /// One checksum per data shard, indexed 0..num_data_shards (grid order).
    pub(crate) data_checksums: Vec<[u8; ERROR_CORRECTION_SHARD_CKSUM_LEN]>,
    /// One checksum per parity shard, indexed group-major: group g, parity p lives
    /// at g*P + p. Length == num_groups * P.
    pub(crate) parity_checksums: Vec<[u8; ERROR_CORRECTION_SHARD_CKSUM_LEN]>,
    /// Concatenated parity data, group-major. Length == num_groups * P * S.
    pub(crate) parity_data: Vec<u8>,
}

impl ErrorCorrectionPayload {
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let cksum_bytes = (self.data_checksums.len() + self.parity_checksums.len())
            * ERROR_CORRECTION_SHARD_CKSUM_LEN;
        let mut out = Vec::with_capacity(32 + cksum_bytes + self.parity_data.len());
        out.extend_from_slice(&self.header.to_bytes());
        for c in &self.data_checksums {
            out.extend_from_slice(c);
        }
        for c in &self.parity_checksums {
            out.extend_from_slice(c);
        }
        out.extend_from_slice(&self.parity_data);
        out
    }

    pub(crate) fn from_bytes(data: &[u8]) -> Result<Self, String> {
        let header = ErrorCorrectionPayloadHeader::from_bytes(data)?;
        let (num_data_shards, num_groups) = error_correction_geometry(&header);
        let p = header.parity_shards as usize;
        let s = header.shard_size as usize;

        // Geometry is derived from attacker-controllable header fields: an AVEC blob can
        // arrive inside an untrusted `.aerocorrect` sidecar read from a remote.
        // Derive the expected buffer size with checked arithmetic so a crafted header can
        // never (a) overflow into a small `expected` that happens to match `data.len()` and
        // then drive an unbounded `Vec::with_capacity` in `read_cksums`, nor (b) panic on
        // multiply-overflow in debug builds. Any overflow is rejected before allocating.
        let num_parity = num_groups
            .checked_mul(p)
            .ok_or("Error Correction payload geometry overflow (parity count)")?;
        let total_shards = num_data_shards
            .checked_add(num_parity)
            .ok_or("Error Correction payload geometry overflow (shard count)")?;
        let cksum_table = total_shards
            .checked_mul(ERROR_CORRECTION_SHARD_CKSUM_LEN)
            .ok_or("Error Correction payload geometry overflow (checksum table)")?;
        let parity_len = num_parity
            .checked_mul(s)
            .ok_or("Error Correction payload geometry overflow (parity data)")?;
        let expected = 32usize
            .checked_add(cksum_table)
            .and_then(|v| v.checked_add(parity_len))
            .ok_or("Error Correction payload geometry overflow (total length)")?;
        if data.len() != expected {
            return Err(format!(
                "ErrorCorrectionPayload length mismatch: got {}, expected {}",
                data.len(),
                expected
            ));
        }
        // Past this point `data.len() == expected`, so `num_data_shards` and `num_parity`
        // are each bounded by `data.len() / ERROR_CORRECTION_SHARD_CKSUM_LEN`: the
        // `Vec::with_capacity` calls below can no longer be driven huge by the header.

        let mut off = 32;
        let read_cksums = |count: usize, off: &mut usize| {
            let mut v = Vec::with_capacity(count);
            for _ in 0..count {
                let mut c = [0u8; ERROR_CORRECTION_SHARD_CKSUM_LEN];
                c.copy_from_slice(&data[*off..*off + ERROR_CORRECTION_SHARD_CKSUM_LEN]);
                v.push(c);
                *off += ERROR_CORRECTION_SHARD_CKSUM_LEN;
            }
            v
        };
        let data_checksums = read_cksums(num_data_shards, &mut off);
        let parity_checksums = read_cksums(num_parity, &mut off);
        let parity_data = data[off..].to_vec();

        Ok(ErrorCorrectionPayload {
            header,
            data_checksums,
            parity_checksums,
            parity_data,
        })
    }
}

/// Compute the Error Correction payload (v2 fixed-grid format) for the concatenated
/// protected stream.
///
/// Returns (serialized_payload, shards_generated, bytes_protected, overhead_pct).
/// shards_generated = total (data+parity) shards in the v2 grid.
/// bytes_protected = L (sum of protected block sizes).
/// overhead_pct uses the actual serialized Error Correction payload size
/// (header+checksums+parity data) vs protected.
/// Empty input -> (vec![], 0, 0, 0.0).
pub(crate) fn compute_error_correction_shards(data_blocks: &[&[u8]]) -> (Vec<u8>, u64, u64, f64) {
    compute_error_correction_shards_grid(
        data_blocks,
        ERROR_CORRECTION_DATA_SHARDS,
        ERROR_CORRECTION_PARITY_SHARDS,
    )
}

/// As `compute_error_correction_shards`, with an explicit (K data, P parity) group so
/// the QR-style overhead level (#276) is honored. The grid is recorded in the AVEC
/// payload header, so reconstruction reads K/P back from the payload regardless of the
/// level the protected data was created with.
pub(crate) fn compute_error_correction_shards_grid(
    data_blocks: &[&[u8]],
    k: usize,
    p: usize,
) -> (Vec<u8>, u64, u64, f64) {
    // Concatenate the live blocks into the logical stream D of length L.
    let l: usize = data_blocks.iter().map(|b| b.len()).sum();
    if l == 0 {
        return (vec![], 0, 0, 0.0);
    }
    let mut d = Vec::with_capacity(l);
    for b in data_blocks {
        d.extend_from_slice(b);
    }
    // S = ceil(L/K) clamped: a small payload becomes one full group (overhead == P/K);
    // a large payload becomes many full groups at capped shard granularity.
    let s = l
        .div_ceil(k)
        .clamp(ERROR_CORRECTION_MIN_SHARD, ERROR_CORRECTION_MAX_SHARD);

    let num_data_shards = l.div_ceil(s);
    let num_groups = num_data_shards.div_ceil(k);
    let total_shards = (num_data_shards + num_groups * p) as u64;

    // Bytes of data shard `idx` (zero-padded past the end of D).
    let shard_at = |idx: usize| -> Vec<u8> {
        let start = idx * s;
        let end = (start + s).min(l);
        let mut v = vec![0u8; s];
        if start < end {
            v[..end - start].copy_from_slice(&d[start..end]);
        }
        v
    };

    let mut data_checksums = Vec::with_capacity(num_data_shards);
    for i in 0..num_data_shards {
        data_checksums.push(error_correction_shard_checksum(&shard_at(i)));
    }

    // Invariant: (k, p) always originate from `error_correction_grid` (k in [2,20],
    // p in {1,2}, so k+p < 256 and both >= 1). RS construction therefore cannot fail,
    // and below every shard is exactly `s` bytes with `k+p` slots, so `encode` cannot
    // fail either. The expects document those invariants rather than masking real errors.
    let rs = ReedSolomon::<reed_solomon_erasure::galois_8::Field>::new(k, p)
        .expect("invalid ReedSolomon parameters (k,p must come from error_correction_grid)");

    let mut parity_data = Vec::with_capacity(num_groups * p * s);
    let mut parity_checksums = Vec::with_capacity(num_groups * p);

    for g in 0..num_groups {
        // K data slots + P parity slots. Slots past num_data_shards stay zero
        // (virtual padding); parity slots are filled by encode.
        let mut shards: Vec<Vec<u8>> = vec![vec![0u8; s]; k + p];
        for (local, shard) in shards.iter_mut().take(k).enumerate() {
            let gi = g * k + local;
            if gi < num_data_shards {
                *shard = shard_at(gi);
            }
        }
        rs.encode(&mut shards).expect("RS encode failed");
        for pp in 0..p {
            let par = &shards[k + pp];
            parity_checksums.push(error_correction_shard_checksum(par));
            parity_data.extend_from_slice(par);
        }
    }

    let header = ErrorCorrectionPayloadHeader {
        data_shards: k as u16,
        parity_shards: p as u16,
        shard_size: s as u32,
        total_data_len: l as u64,
    };

    let payload = ErrorCorrectionPayload {
        header,
        data_checksums,
        parity_checksums,
        parity_data,
    }
    .to_bytes();
    let protected = l as u64;
    let overhead = if protected > 0 {
        (payload.len() as f64 / protected as f64) * 100.0
    } else {
        0.0
    };
    (payload, total_shards, protected, overhead)
}

/// Compute the AVEC parity for a single fixed metadata region, treating it as one
/// block. An empty region yields an empty payload.
pub(crate) fn compute_metadata_parity(region: &[u8], k: usize, p: usize) -> Vec<u8> {
    if region.is_empty() {
        return Vec::new();
    }
    let (payload, _shards, _prot, _ov) = compute_error_correction_shards_grid(&[region], k, p);
    payload
}

/// Reconstruct damaged bytes in the protected block stream using the v2 Error
/// Correction payload.
///
/// `blocks`: the protected blocks in order, each with exactly the same length used
///           when parity was computed; callers zero-pad truncated blocks if needed.
/// `error_correction_payload_bytes`: serialized AVEC payload bytes.
///
/// Damaged shards are located by per-shard checksum mismatch (data and parity), then
/// RS-reconstructed per group; recovered bytes are written back into `blocks` in place.
/// Returns the number of data shards successfully reconstructed.
///
/// A successful return does NOT imply caller-level correctness: the caller must
/// re-verify repaired bytes against its authenticated manifest/checksum before
/// persisting. A grid misalignment is rejected up front so good data can never be
/// silently overwritten.
pub(crate) fn reconstruct_from_error_correction(
    blocks: &mut [Vec<u8>],
    error_correction_payload_bytes: &[u8],
) -> Result<usize, String> {
    if error_correction_payload_bytes.is_empty() {
        return Ok(0);
    }
    let payload = ErrorCorrectionPayload::from_bytes(error_correction_payload_bytes)?;
    let k = payload.header.data_shards as usize;
    let p = payload.header.parity_shards as usize;
    let s = payload.header.shard_size as usize;
    let l = payload.header.total_data_len as usize;

    // The block stream must match the stream the parity was computed over, otherwise
    // the shard grid would be misaligned and reconstruction could corrupt good data.
    let total: usize = blocks.iter().map(|b| b.len()).sum();
    if total != l {
        return Err(format!(
            "Error Correction reconstruct: block stream length {} != payload stream length {}",
            total, l
        ));
    }

    let mut d = Vec::with_capacity(l);
    for b in blocks.iter() {
        d.extend_from_slice(b);
    }

    let (num_data_shards, num_groups) = error_correction_geometry(&payload.header);

    let shard_at = |d: &[u8], idx: usize| -> Vec<u8> {
        let start = idx * s;
        let end = (start + s).min(l);
        let mut v = vec![0u8; s];
        if start < end {
            v[..end - start].copy_from_slice(&d[start..end]);
        }
        v
    };

    let rs = ReedSolomon::<reed_solomon_erasure::galois_8::Field>::new(k, p)
        .map_err(|e| format!("RS create for reconstruct: {:?}", e))?;

    let mut recovered = 0usize;
    let mut changed = false;

    for g in 0..num_groups {
        let mut opt: Vec<Option<Vec<u8>>> = vec![None; k + p];
        let mut erased_data = 0usize;

        for (local, slot) in opt.iter_mut().take(k).enumerate() {
            let gi = g * k + local;
            if gi < num_data_shards {
                let sh = shard_at(&d, gi);
                if error_correction_shard_checksum(&sh) == payload.data_checksums[gi] {
                    *slot = Some(sh); // shard intact
                } else {
                    erased_data += 1; // damaged -> RS erasure
                }
            } else {
                *slot = Some(vec![0u8; s]); // virtual zero-pad slot
            }
        }

        for pp in 0..p {
            let pidx = g * p + pp;
            let start = pidx * s;
            if start + s <= payload.parity_data.len() {
                let par = payload.parity_data[start..start + s].to_vec();
                if error_correction_shard_checksum(&par) == payload.parity_checksums[pidx] {
                    opt[k + pp] = Some(par); // parity intact
                }
                // else: rotted parity -> leave None so RS routes around it
            }
        }

        if erased_data == 0 {
            continue; // nothing damaged in this group
        }
        if rs.reconstruct(&mut opt).is_err() {
            continue; // more erasures than parity can cover; leave group untouched
        }

        for (local, slot) in opt.iter().take(k).enumerate() {
            let gi = g * k + local;
            if gi >= num_data_shards {
                continue;
            }
            if let Some(sh) = slot {
                let start = gi * s;
                let end = (start + s).min(l);
                if d[start..end] != sh[..end - start] {
                    d[start..end].copy_from_slice(&sh[..end - start]);
                    changed = true;
                }
            }
        }
        recovered += erased_data;
    }

    if changed {
        // Re-slice the recovered stream back into the fixed-length blocks.
        let mut pos = 0usize;
        for b in blocks.iter_mut() {
            let len = b.len();
            b.copy_from_slice(&d[pos..pos + len]);
            pos += len;
        }
    }

    Ok(recovered)
}

// ───────────────────────────────────────────────────────────────────────────
// Public standalone API: error-correct ANY file with a detached `.aerocorrect`
// sidecar (the `aeroftp correct` subcommand). The format binds by content
// SHA-256, so this is format-agnostic (works on vault containers or any file).
// This is the curated public surface; the codec/format submodules stay crate-private.
// ───────────────────────────────────────────────────────────────────────────

use serde::Serialize;
use std::path::Path;

/// Result of generating a standalone `.aerocorrect` sidecar.
#[derive(Debug, Clone, Serialize)]
pub struct CorrectGenerateReport {
    pub file: String,
    pub sidecar: String,
    pub file_size: u64,
    pub sidecar_size: u64,
    pub overhead_pct: f64,
    /// Number of windows (parity segments) in the sidecar.
    pub segments: u64,
    pub shards: u64,
    pub level_pct: u32,
}

/// Result of a read-only standalone verify.
#[derive(Debug, Clone, Serialize)]
pub struct CorrectVerifyReport {
    pub file: String,
    pub sidecar: String,
    /// `"verified"` (intact) or `"needs_repair"` (corruption detected).
    pub status: String,
    pub verified: bool,
}

/// Result of a standalone repair attempt.
#[derive(Debug, Clone, Serialize)]
pub struct CorrectRepairReport {
    pub file: String,
    pub sidecar: String,
    /// `"verified"` (was already intact) or `"repaired"`.
    pub status: String,
    pub repaired: bool,
    pub recovered_shards: u64,
}

/// Default sidecar path for `file`: `<file>.aerocorrect`.
pub fn aerocorrect_sidecar_path_for(file: &str) -> String {
    sidecar::aerocorrect_sidecar_path(file)
}

fn rel_name(file: &str) -> String {
    Path::new(file)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(file)
        .to_string()
}

/// Generate a detached `.aerocorrect` sidecar for `file` at overhead level `pct`. Streams
/// the file window by window (bounded memory). Writes the sidecar to `out` or
/// `<file>.aerocorrect`. Returns a report. Errors if the file exceeds the size cap.
pub fn correct_generate(
    file: &str,
    pct: u32,
    out: Option<&str>,
) -> Result<CorrectGenerateReport, String> {
    let path = Path::new(file);
    let rel = rel_name(file);
    let result = aerosync::generate_sync_sidecar_for_file_capped(
        &rel,
        path,
        pct,
        aerosync::AEROSYNC_EC_MAX_FILE_SIZE,
    )?;
    let generated = match result {
        aerosync::SyncEcGenerateResult::Generated(g) => g,
        aerosync::SyncEcGenerateResult::SkippedTooLarge {
            file_size,
            max_file_size,
        } => {
            return Err(format!(
                "{file} is {file_size} bytes, above the {max_file_size}-byte error-correction cap"
            ));
        }
    };
    let sidecar_path = out
        .map(|s| s.to_string())
        .unwrap_or_else(|| sidecar::aerocorrect_sidecar_path(file));
    std::fs::write(&sidecar_path, &generated.sidecar_bytes)
        .map_err(|e| format!("write sidecar {sidecar_path}: {e}"))?;
    let segments =
        sidecar::aerocorrect_windows(generated.file_size, sidecar::AEROCORRECT_WINDOW_SIZE).len()
            as u64;
    Ok(CorrectGenerateReport {
        file: file.to_string(),
        sidecar: sidecar_path,
        file_size: generated.file_size,
        sidecar_size: generated.sidecar_len,
        overhead_pct: generated.overhead_pct,
        segments,
        shards: generated.shards,
        level_pct: pct,
    })
}

/// Verify `file` against its `.aerocorrect` sidecar (read-only, never mutates the file).
/// `parity` overrides the default `<file>.aerocorrect` path.
pub fn correct_verify(file: &str, parity: Option<&str>) -> Result<CorrectVerifyReport, String> {
    let path = Path::new(file);
    let rel = rel_name(file);
    let sidecar_path = parity
        .map(|s| s.to_string())
        .unwrap_or_else(|| sidecar::aerocorrect_sidecar_path(file));
    let sidecar_bytes =
        std::fs::read(&sidecar_path).map_err(|e| format!("read sidecar {sidecar_path}: {e}"))?;
    let verified = matches!(
        aerosync::verify_standalone_file(&rel, path, &sidecar_bytes)?,
        aerosync::StandaloneVerifyResult::Verified
    );
    Ok(CorrectVerifyReport {
        file: file.to_string(),
        sidecar: sidecar_path,
        status: if verified { "verified" } else { "needs_repair" }.to_string(),
        verified,
    })
}

/// Repair `file` in place from its `.aerocorrect` sidecar (atomic, all-or-nothing). A file
/// already intact is reported as `verified` (no write). `parity` overrides the default path.
pub fn correct_repair(file: &str, parity: Option<&str>) -> Result<CorrectRepairReport, String> {
    let path = Path::new(file);
    let rel = rel_name(file);
    let sidecar_path = parity
        .map(|s| s.to_string())
        .unwrap_or_else(|| sidecar::aerocorrect_sidecar_path(file));
    let sidecar_bytes =
        std::fs::read(&sidecar_path).map_err(|e| format!("read sidecar {sidecar_path}: {e}"))?;
    let (status, repaired, recovered_shards) =
        match aerosync::verify_repair_standalone_file(&rel, path, &sidecar_bytes)? {
            aerosync::SyncEcRepairResult::Verified => ("verified".to_string(), false, 0u64),
            aerosync::SyncEcRepairResult::Repaired { recovered_shards } => {
                ("repaired".to_string(), true, recovered_shards as u64)
            }
        };
    Ok(CorrectRepairReport {
        file: file.to_string(),
        sidecar: sidecar_path,
        status,
        repaired,
        recovered_shards,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random bytes (BLAKE3 keystream).
    fn sample(len: usize) -> Vec<u8> {
        let mut seed = *blake3::hash(b"ec-mod-test-seed").as_bytes();
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            seed = *blake3::hash(&seed).as_bytes();
            out.extend_from_slice(&seed);
        }
        out.truncate(len);
        out
    }

    #[test]
    fn grid_table_and_monotonic() {
        assert_eq!(error_correction_grid(5), (20, 1));
        assert_eq!(error_correction_grid(7), (14, 1));
        assert_eq!(error_correction_grid(10), (20, 2)); // 10% overhead == K20/P2
        assert_eq!(error_correction_grid(15), (13, 2));
        assert_eq!(error_correction_grid(20), (10, 2));
        assert_eq!(error_correction_grid(25), (8, 2));
        assert_eq!(error_correction_grid(30), (7, 2));
        assert_eq!(error_correction_grid(50), (4, 2));
        // Out-of-range percentages clamp to the [MIN, MAX] band.
        assert_eq!(error_correction_grid(1), error_correction_grid(5));
        assert_eq!(error_correction_grid(99), error_correction_grid(50));
        // Realized overhead p/k is monotonic non-decreasing and k stays >= 2.
        let mut prev = 0.0f64;
        for pct in 5..=50 {
            let (k, p) = error_correction_grid(pct);
            assert!(k >= 2, "k must stay >= 2 at pct={pct}");
            let ratio = p as f64 / k as f64;
            assert!(
                ratio + 1e-9 >= prev,
                "overhead must be non-decreasing at pct={pct}"
            );
            prev = ratio;
        }
    }

    #[test]
    fn roundtrip_recovers_single_erasure() {
        let data = sample(50_000);
        let (payload, _shards, prot, _ov) = compute_error_correction_shards_grid(&[&data], 10, 2);
        assert_eq!(prot, data.len() as u64);
        let mut damaged = data.clone();
        damaged[12_345] ^= 0xFF;
        let mut blocks = vec![damaged];
        let recovered = reconstruct_from_error_correction(&mut blocks, &payload).unwrap();
        assert!(recovered >= 1);
        assert_eq!(blocks[0], data);
    }

    #[test]
    fn erasure_budget_boundary_recovers_p_and_leaves_p_plus_1() {
        let data = sample(60_000);
        let (payload, _s, _p, _o) = compute_error_correction_shards_grid(&[&data], 10, 2);
        let s = ErrorCorrectionPayload::from_bytes(&payload)
            .unwrap()
            .header
            .shard_size as usize;
        // Exactly P=2 erased shards: recoverable.
        let mut d2 = data.clone();
        d2[0] ^= 0xFF;
        d2[s] ^= 0xFF;
        let mut b2 = vec![d2];
        assert!(reconstruct_from_error_correction(&mut b2, &payload).unwrap() >= 2);
        assert_eq!(b2[0], data);
        // P+1=3 erased shards: beyond budget -> group untouched, no partial corruption.
        let mut d3 = data.clone();
        d3[0] ^= 0xFF;
        d3[s] ^= 0xFF;
        d3[2 * s] ^= 0xFF;
        let before = d3.clone();
        let mut b3 = vec![d3];
        assert_eq!(
            reconstruct_from_error_correction(&mut b3, &payload).unwrap(),
            0
        );
        assert_eq!(b3[0], before);
    }

    #[test]
    fn multigroup_recovers_one_erasure_per_group() {
        // 10 MiB at (4,2) forces shard size to clamp at MAX_SHARD and >1 group.
        let data = sample(10 * 1024 * 1024);
        let (payload, _s, _p, _o) = compute_error_correction_shards_grid(&[&data], 4, 2);
        let header = ErrorCorrectionPayload::from_bytes(&payload).unwrap().header;
        let (num_data, num_groups) = error_correction_geometry(&header);
        assert!(num_groups > 1, "expected multiple groups, got {num_groups}");
        let s = header.shard_size as usize;
        // Corrupt the first data shard of every group.
        let mut damaged = data.clone();
        for g in 0..num_groups {
            let gi = g * header.data_shards as usize;
            if gi < num_data {
                damaged[gi * s] ^= 0xFF;
            }
        }
        let mut blocks = vec![damaged];
        assert!(reconstruct_from_error_correction(&mut blocks, &payload).unwrap() >= num_groups);
        assert_eq!(blocks[0], data);
    }

    #[test]
    fn reconstruct_rejects_wrong_total_len() {
        let data = sample(20_000);
        let (payload, _s, _p, _o) = compute_error_correction_shards_grid(&[&data], 10, 2);
        let mut wrong = vec![sample(19_999)];
        assert!(reconstruct_from_error_correction(&mut wrong, &payload).is_err());
    }

    #[test]
    fn metadata_parity_empty_and_single_region() {
        assert!(compute_metadata_parity(&[], 10, 2).is_empty());
        let region = sample(8_000);
        let parity = compute_metadata_parity(&region, 10, 2);
        assert!(!parity.is_empty());
        let mut damaged = region.clone();
        damaged[1234] ^= 0xFF;
        let mut blocks = vec![damaged];
        assert!(reconstruct_from_error_correction(&mut blocks, &parity).unwrap() >= 1);
        assert_eq!(blocks[0], region);
    }

    #[test]
    fn payload_header_rejects_malformed() {
        let mut bad = vec![0u8; 32];
        assert!(ErrorCorrectionPayloadHeader::from_bytes(&bad).is_err()); // bad magic
        bad[0..4].copy_from_slice(ERROR_CORRECTION_PAYLOAD_MAGIC);
        bad[4..6].copy_from_slice(&999u16.to_le_bytes());
        assert!(ErrorCorrectionPayloadHeader::from_bytes(&bad).is_err()); // bad version
        let mut z = vec![0u8; 32];
        z[0..4].copy_from_slice(ERROR_CORRECTION_PAYLOAD_MAGIC);
        z[4..6].copy_from_slice(&ERROR_CORRECTION_PAYLOAD_VERSION.to_le_bytes());
        assert!(ErrorCorrectionPayloadHeader::from_bytes(&z).is_err()); // zero geometry
        assert!(ErrorCorrectionPayloadHeader::from_bytes(&[0u8; 10]).is_err()); // too short
    }

    #[test]
    fn from_bytes_rejects_overflow_geometry_without_panic_or_oom() {
        // SECURITY REGRESSION (CRITICAL-1): an AVEC blob arrives inside an untrusted
        // sidecar. A header with astronomically large geometry must be rejected by the
        // checked-arithmetic guard, never overflow (debug panic) nor drive a giant
        // Vec::with_capacity (release OOM).
        let h1 = ErrorCorrectionPayloadHeader {
            data_shards: 1,
            parity_shards: 1,
            shard_size: 1,
            total_data_len: u64::MAX,
        }
        .to_bytes();
        assert!(ErrorCorrectionPayload::from_bytes(&h1).is_err());

        let h2 = ErrorCorrectionPayloadHeader {
            data_shards: 2,
            parity_shards: u16::MAX,
            shard_size: u32::MAX,
            total_data_len: u64::MAX,
        }
        .to_bytes();
        assert!(ErrorCorrectionPayload::from_bytes(&h2).is_err());
    }

    #[test]
    fn empty_input_yields_empty_payload() {
        let (payload, shards, prot, ov) = compute_error_correction_shards_grid(&[], 10, 2);
        assert!(payload.is_empty());
        assert_eq!((shards, prot, ov), (0, 0, 0.0));
        // Reconstruct with an empty payload is a no-op.
        let mut blocks: Vec<Vec<u8>> = vec![];
        assert_eq!(
            reconstruct_from_error_correction(&mut blocks, &payload).unwrap(),
            0
        );
    }

    fn write_file(dir: &std::path::Path, name: &str, bytes: &[u8]) -> String {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn correct_generate_default_sidecar_path_and_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_file(dir.path(), "data.bin", &sample(40_000));

        let gen = correct_generate(&file, 15, None).unwrap();
        assert_eq!(gen.sidecar, format!("{file}.aerocorrect"));
        assert_eq!(gen.sidecar, aerocorrect_sidecar_path_for(&file));
        assert!(std::path::Path::new(&gen.sidecar).exists());
        assert_eq!(gen.segments, 1, "small file is a single window");
        assert!(gen.shards > 0);

        // Intact file verifies and needs no repair.
        let v = correct_verify(&file, None).unwrap();
        assert!(v.verified && v.status == "verified");
        let r = correct_repair(&file, None).unwrap();
        assert!(!r.repaired && r.status == "verified");
    }

    #[test]
    fn correct_verify_detects_and_repair_fixes_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let original = sample(50_000);
        let file = write_file(dir.path(), "doc.bin", &original);
        correct_generate(&file, 25, None).unwrap();

        // Corrupt a span within the file (well under the parity budget).
        let mut bytes = original.clone();
        for b in bytes.iter_mut().take(1_000).skip(100) {
            *b ^= 0xFF;
        }
        std::fs::write(&file, &bytes).unwrap();

        let v = correct_verify(&file, None).unwrap();
        assert!(!v.verified && v.status == "needs_repair");

        let r = correct_repair(&file, None).unwrap();
        assert!(r.repaired && r.status == "repaired" && r.recovered_shards > 0);
        assert_eq!(
            std::fs::read(&file).unwrap(),
            original,
            "byte-identical repair"
        );

        // Read-only verify after repair confirms intact.
        assert!(correct_verify(&file, None).unwrap().verified);
    }

    #[test]
    fn correct_custom_out_and_parity_paths() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_file(dir.path(), "payload.bin", &sample(30_000));
        let side = dir.path().join("custom.aerocorrect");
        let side = side.to_string_lossy().into_owned();

        let gen = correct_generate(&file, 15, Some(&side)).unwrap();
        assert_eq!(gen.sidecar, side);
        assert!(!std::path::Path::new(&format!("{file}.aerocorrect")).exists());
        assert!(correct_verify(&file, Some(&side)).unwrap().verified);
    }

    #[test]
    fn correct_repair_against_foreign_sidecar_leaves_file_intact() {
        let dir = tempfile::tempdir().unwrap();
        // Two same-size files with different content; sidecar belongs to A.
        let a = write_file(dir.path(), "a.bin", &sample(20_000));
        let b_bytes = {
            let mut v = sample(20_000);
            v.reverse();
            v
        };
        let b = write_file(dir.path(), "b.bin", &b_bytes);
        let side = correct_generate(&a, 25, None).unwrap().sidecar;

        // B does not match A's sidecar: verify says needs_repair, repair fails closed.
        assert!(!correct_verify(&b, Some(&side)).unwrap().verified);
        assert!(correct_repair(&b, Some(&side)).is_err());
        assert_eq!(
            std::fs::read(&b).unwrap(),
            b_bytes,
            "B untouched after failed repair"
        );
    }
}
