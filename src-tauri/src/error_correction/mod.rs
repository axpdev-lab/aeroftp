use reed_solomon_erasure::ReedSolomon;

pub(crate) mod aerosync;

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

        let num_parity = num_groups * p;
        let cksum_table = (num_data_shards + num_parity) * ERROR_CORRECTION_SHARD_CKSUM_LEN;
        let parity_len = num_parity * s;
        let expected = 32 + cksum_table + parity_len;
        if data.len() != expected {
            return Err(format!(
                "ErrorCorrectionPayload length mismatch: got {}, expected {}",
                data.len(),
                expected
            ));
        }

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

    let rs = ReedSolomon::<reed_solomon_erasure::galois_8::Field>::new(k, p)
        .expect("invalid ReedSolomon parameters");

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
