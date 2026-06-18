use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use super::sidecar::{
    aerocorrect_sidecar_path, aerocorrect_windows, estimate_windowed_sidecar_len,
    validate_window_tiling, validate_window_tiling_iter, AeroCorrectSegment, AeroCorrectSidecar,
    AeroCorrectSidecarReader, AEROCORRECT_WINDOW_SIZE,
};
use super::{
    compute_error_correction_shards_grid, error_correction_grid, reconstruct_from_error_correction,
};

/// Default per-file cap for sync error correction. Windowed streaming bounds the
/// *plaintext* memory of generation/verification/repair to a single window
/// (`AEROCORRECT_WINDOW_SIZE`), so a large file no longer has to fit in RAM. The cap
/// still prevents accidental huge explicit EC runs in the sync pipeline; callers may
/// raise `max_file_size` when they intentionally accept the transfer/storage cost.
pub(crate) const AEROSYNC_EC_MAX_FILE_SIZE: u64 = 1024 * 1024 * 1024;

/// Read buffer for the streaming hash pass (fast verify). Independent of the parity
/// window; just bounds the read syscall size.
const HASH_READ_CHUNK: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct SyncEcGeneratedSidecar {
    pub(crate) sidecar_bytes: Vec<u8>,
    pub(crate) file_size: u64,
    pub(crate) file_sha256: [u8; 32],
    pub(crate) shards: u64,
    pub(crate) bytes_protected: u64,
    pub(crate) overhead_pct: f64,
    pub(crate) avec_payload_len: u64,
    pub(crate) sidecar_len: u64,
}

#[derive(Debug, Clone)]
pub(crate) enum SyncEcGenerateResult {
    Generated(SyncEcGeneratedSidecar),
    SkippedTooLarge {
        file_size: u64,
        max_file_size: u64,
    },
    /// The sidecar's storage cost would exceed the caller's minimum-benefit threshold
    /// (its size as a percentage of the file is above `max_overhead_pct`). Tiny files hit
    /// this because the fixed parity-shard floor makes even a 100-byte file produce a
    /// multi-KiB sidecar. `overhead_pct` is the rejected ratio (sidecar bytes * 100 / file
    /// bytes). Only produced when the caller opts in (`max_overhead_pct > 0`).
    SkippedLowBenefit {
        file_size: u64,
        overhead_pct: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncEcRepairResult {
    Verified,
    Repaired { recovered_shards: usize },
}

#[cfg(test)]
fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    let digest = Sha256::digest(data);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

fn finalize_sha256(hasher: Sha256) -> [u8; 32] {
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

pub(crate) fn parse_sha256_hex(input: &str) -> Result<[u8; 32], String> {
    let bytes = hex::decode(input.trim()).map_err(|e| format!("invalid SHA-256 hex: {e}"))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "invalid SHA-256 hex length".to_string())?;
    Ok(arr)
}

pub(crate) fn sync_error_correction_sidecar_path(remote_path: &str) -> String {
    aerocorrect_sidecar_path(remote_path)
}

/// Exact serialized size (in bytes) of the `.aerocorrect` sidecar that
/// `generate_sync_sidecar_for_bytes` produces for a `file_size`-byte file at overhead level
/// `pct`, derived from the same v2 fixed-grid geometry WITHOUT allocating or hashing.
///
/// This is the source of truth for the sync-doctor cost preview. A flat `size * pct / 100`
/// underestimates the real sidecar by orders of magnitude for small files (the
/// `ERROR_CORRECTION_MIN_SHARD` floor forces ≥4 KiB parity shards) and under-reports at the
/// `ERROR_CORRECTION_MAX_SHARD` cliff. It now also accounts for the per-window segment headers
/// of a windowed (large-file) sidecar.
///
/// Invariant (checked by `aerosync_estimate_matches_real_sidecar_len`):
/// `estimate_aerorec_sidecar_len(n, pct) == generate_sync_sidecar_for_bytes(_, &[_; n], pct).sidecar_len`.
pub fn estimate_aerorec_sidecar_len(file_size: u64, pct: u32) -> u64 {
    estimate_windowed_sidecar_len(file_size, pct, AEROCORRECT_WINDOW_SIZE)
}

/// Build a windowed sidecar from an in-memory buffer: one segment per window, each
/// carrying that window's own RS parity. Returns the sidecar plus aggregate telemetry.
#[cfg(test)]
fn build_windowed_sidecar(data: &[u8], pct: u32, window: u64) -> (AeroCorrectSidecar, u64, u64) {
    let file_size = data.len() as u64;
    let (k, p) = error_correction_grid(pct);
    let mut segments = Vec::new();
    let mut total_shards = 0u64;
    let mut total_avec = 0u64;
    for (off, len) in aerocorrect_windows(file_size, window) {
        let w = &data[off as usize..(off + len) as usize];
        let (avec_bytes, shards, _protected, _overhead) =
            compute_error_correction_shards_grid(&[w], k, p);
        total_shards += shards;
        total_avec += avec_bytes.len() as u64;
        segments.push(AeroCorrectSegment {
            window_offset: off,
            window_len: len,
            avec_bytes,
        });
    }
    let sidecar = AeroCorrectSidecar::new(sha256_bytes(data), file_size, segments);
    (sidecar, total_shards, total_avec)
}

fn generated_from(
    sidecar: AeroCorrectSidecar,
    file_size: u64,
    file_sha256: [u8; 32],
    shards: u64,
    avec_payload_len: u64,
) -> SyncEcGeneratedSidecar {
    let sidecar_bytes = sidecar.to_bytes();
    let overhead_pct = if file_size > 0 {
        (avec_payload_len as f64 / file_size as f64) * 100.0
    } else {
        0.0
    };
    SyncEcGeneratedSidecar {
        sidecar_len: sidecar_bytes.len() as u64,
        sidecar_bytes,
        file_size,
        file_sha256,
        shards,
        bytes_protected: file_size,
        overhead_pct,
        avec_payload_len,
    }
}

#[cfg(test)]
pub(crate) fn generate_sync_sidecar_for_bytes(
    _rel_path: &str,
    data: &[u8],
    pct: u32,
) -> SyncEcGeneratedSidecar {
    generate_sync_sidecar_for_bytes_windowed(_rel_path, data, pct, AEROCORRECT_WINDOW_SIZE)
}

/// `generate_sync_sidecar_for_bytes` with an explicit window size (tests use a small
/// window to exercise the multi-window path cheaply).
#[cfg(test)]
fn generate_sync_sidecar_for_bytes_windowed(
    _rel_path: &str,
    data: &[u8],
    pct: u32,
    window: u64,
) -> SyncEcGeneratedSidecar {
    let file_sha256 = sha256_bytes(data);
    let (sidecar, shards, avec) = build_windowed_sidecar(data, pct, window);
    generated_from(sidecar, data.len() as u64, file_sha256, shards, avec)
}

#[cfg(test)]
pub(crate) fn generate_sync_sidecar_for_bytes_capped(
    rel_path: &str,
    data: &[u8],
    pct: u32,
    max_file_size: u64,
) -> SyncEcGenerateResult {
    let file_size = data.len() as u64;
    if file_size > max_file_size {
        return SyncEcGenerateResult::SkippedTooLarge {
            file_size,
            max_file_size,
        };
    }
    SyncEcGenerateResult::Generated(generate_sync_sidecar_for_bytes(rel_path, data, pct))
}

/// `max_overhead_pct`: minimum-benefit gate. When > 0, skip generation (returning
/// `SkippedLowBenefit`) if the sidecar's serialized size would exceed this percentage of
/// the file size, computed WITHOUT generating it. 0 disables the gate (default).
pub(crate) fn generate_sync_sidecar_for_file_capped(
    rel_path: &str,
    path: &Path,
    pct: u32,
    max_file_size: u64,
    max_overhead_pct: u32,
) -> Result<SyncEcGenerateResult, String> {
    generate_sync_sidecar_for_file_capped_windowed(
        rel_path,
        path,
        pct,
        max_file_size,
        max_overhead_pct,
        AEROCORRECT_WINDOW_SIZE,
    )
}

/// Stream a file into a windowed sidecar with bounded memory: read at most one
/// `window`-sized buffer at a time, compute that window's parity, and accumulate a
/// rolling SHA-256 of the whole file. Never loads the whole file into RAM.
fn generate_sync_sidecar_for_file_capped_windowed(
    _rel_path: &str,
    path: &Path,
    pct: u32,
    max_file_size: u64,
    max_overhead_pct: u32,
    window: u64,
) -> Result<SyncEcGenerateResult, String> {
    let metadata = std::fs::metadata(path).map_err(|e| {
        format!(
            "read metadata for AeroSync EC source {}: {e}",
            path.display()
        )
    })?;
    let file_size = metadata.len();
    if file_size > max_file_size {
        return Ok(SyncEcGenerateResult::SkippedTooLarge {
            file_size,
            max_file_size,
        });
    }
    // Minimum-benefit gate (opt-in): reject absurd overhead BEFORE generating, using the
    // exact serialized cost. A tiny file at any level produces a multi-KiB sidecar (the
    // parity-shard floor), so its overhead can be many hundreds of percent.
    if max_overhead_pct > 0 && file_size > 0 {
        let estimate = estimate_windowed_sidecar_len(file_size, pct, window);
        let overhead_pct = ((estimate as u128 * 100) / file_size as u128) as u64;
        if overhead_pct > max_overhead_pct as u64 {
            return Ok(SyncEcGenerateResult::SkippedLowBenefit {
                file_size,
                overhead_pct,
            });
        }
    }
    let mut file =
        File::open(path).map_err(|e| format!("open AeroSync EC source {}: {e}", path.display()))?;
    let (k, p) = error_correction_grid(pct);
    let mut hasher = Sha256::new();
    let mut segments = Vec::new();
    let mut total_shards = 0u64;
    let mut total_avec = 0u64;
    for (off, len) in aerocorrect_windows(file_size, window) {
        let mut buf = vec![0u8; len as usize];
        file.read_exact(&mut buf).map_err(|e| {
            format!(
                "read AeroSync EC source window at {off} (+{len}) of {}: {e}",
                path.display()
            )
        })?;
        hasher.update(&buf);
        let (avec_bytes, shards, _protected, _overhead) =
            compute_error_correction_shards_grid(&[&buf], k, p);
        total_shards += shards;
        total_avec += avec_bytes.len() as u64;
        segments.push(AeroCorrectSegment {
            window_offset: off,
            window_len: len,
            avec_bytes,
        });
    }
    let file_sha256 = finalize_sha256(hasher);
    let sidecar = AeroCorrectSidecar::new(file_sha256, file_size, segments);
    Ok(SyncEcGenerateResult::Generated(generated_from(
        sidecar,
        file_size,
        file_sha256,
        total_shards,
        total_avec,
    )))
}

/// Shared sidecar validation for the verify/repair paths: parse, content-bind against
/// the expected (good) hash, confirm the declared length, and confirm the windows tile
/// the stream cleanly. Returns the parsed sidecar.
fn validated_sidecar_for(
    rel_path: &str,
    expected_sha256: &[u8; 32],
    stream_len: u64,
    sidecar_bytes: &[u8],
) -> Result<AeroCorrectSidecar, String> {
    let sidecar = AeroCorrectSidecar::from_bytes(sidecar_bytes)?;
    // Binding is by content: the sidecar must belong to the expected (good) file hash
    // the sync index recorded for this path, not to the bytes we just downloaded (those
    // may be corrupt). The rel_path/file_size identity is the sync layer's concern.
    sidecar.verify_binding(expected_sha256).map_err(|e| {
        format!("AeroSync EC sidecar for {rel_path} does not match the expected file: {e}")
    })?;
    if sidecar.total_len != stream_len {
        return Err(format!(
            "AeroSync EC sidecar total length {} != file length {stream_len} for {rel_path}",
            sidecar.total_len
        ));
    }
    validate_window_tiling(&sidecar.segments, stream_len)
        .map_err(|e| format!("AeroSync EC sidecar for {rel_path}: {e}"))?;
    Ok(sidecar)
}

#[cfg(test)]
pub(crate) fn verify_repair_sync_bytes(
    rel_path: &str,
    expected_sha256: &[u8; 32],
    data: &mut Vec<u8>,
    sidecar_bytes: &[u8],
) -> Result<SyncEcRepairResult, String> {
    let sidecar =
        validated_sidecar_for(rel_path, expected_sha256, data.len() as u64, sidecar_bytes)?;

    if sha256_bytes(data) == *expected_sha256 {
        return Ok(SyncEcRepairResult::Verified);
    }

    // Repair on a clone so the buffer is all-or-nothing: only adopt the result once the
    // whole-file SHA matches. Each window is repaired independently from its own parity.
    let mut work = data.clone();
    let mut recovered_shards = 0usize;
    for seg in &sidecar.segments {
        let start = seg.window_offset as usize;
        let end = start + seg.window_len as usize;
        let mut blocks = vec![work[start..end].to_vec()];
        recovered_shards += reconstruct_from_error_correction(&mut blocks, &seg.avec_bytes)?;
        work[start..end].copy_from_slice(&blocks[0]);
    }
    if sha256_bytes(&work) != *expected_sha256 {
        return Err("AeroSync EC repair failed post-repair SHA-256 verification".to_string());
    }
    *data = work;
    Ok(SyncEcRepairResult::Repaired { recovered_shards })
}

/// Stream a file and return its SHA-256 with bounded memory (one `HASH_READ_CHUNK`
/// buffer at a time). Shared by the verify fast path and the standalone verify.
fn hash_file_streaming(path: &Path) -> Result<[u8; 32], String> {
    let mut file =
        File::open(path).map_err(|e| format!("open {} for hashing: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; HASH_READ_CHUNK];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("read {} for hashing: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(finalize_sha256(hasher))
}

pub(crate) fn verify_repair_sync_file(
    rel_path: &str,
    expected_sha256: &[u8; 32],
    path: &Path,
    sidecar_bytes: &[u8],
) -> Result<SyncEcRepairResult, String> {
    let file_size = std::fs::metadata(path)
        .map_err(|e| format!("stat AeroSync EC target {}: {e}", path.display()))?
        .len();
    let sidecar = validated_sidecar_for(rel_path, expected_sha256, file_size, sidecar_bytes)?;

    // Fast path: stream the file once and hash it. Bounded memory (one read chunk).
    if hash_file_streaming(path)? == *expected_sha256 {
        return Ok(SyncEcRepairResult::Verified);
    }

    // Repair path: stream window by window into a temp file in the same directory,
    // repairing each window from its own parity, then atomically replace the original
    // ONLY if the whole repaired stream hashes to the expected value. Bounded memory
    // (one window at a time); all-or-nothing (a failed verify leaves the original intact).
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = tempfile::NamedTempFile::new_in(parent).map_err(|e| {
        format!(
            "create AeroSync EC repair temp in {}: {e}",
            parent.display()
        )
    })?;
    let mut src =
        File::open(path).map_err(|e| format!("open AeroSync EC target {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut recovered_shards = 0usize;
    {
        let mut out = std::io::BufWriter::new(tmp.as_file());
        for seg in &sidecar.segments {
            let mut buf = vec![0u8; seg.window_len as usize];
            src.read_exact(&mut buf)
                .map_err(|e| format!("read AeroSync EC target window {}: {e}", path.display()))?;
            let mut blocks = vec![buf];
            recovered_shards += reconstruct_from_error_correction(&mut blocks, &seg.avec_bytes)?;
            hasher.update(&blocks[0]);
            out.write_all(&blocks[0])
                .map_err(|e| format!("write AeroSync EC repair temp {}: {e}", path.display()))?;
        }
        out.flush()
            .map_err(|e| format!("flush AeroSync EC repair temp {}: {e}", path.display()))?;
    }
    if finalize_sha256(hasher) != *expected_sha256 {
        // tmp is dropped (removed) here; the original file is byte-for-byte untouched.
        return Err("AeroSync EC repair failed post-repair SHA-256 verification".to_string());
    }
    // Release the read handle on the target before persisting. On Windows, renaming the
    // repaired temp onto a file that still has a live read handle fails with
    // ERROR_ACCESS_DENIED (os error 5) (audit M1). The repair loop above is the only
    // reader of `src`, so the handle is safe to drop here.
    drop(src);
    // Preserve the original file's permissions across the atomic replace.
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(tmp.path(), meta.permissions());
    }
    // On a persist failure, recover the NamedTempFile from the error and delete it so a
    // decrypted-plaintext `.tmp*` is never left beside the target (audit M1 plaintext-at-rest).
    tmp.persist(path).map_err(|e| {
        let msg = format!(
            "persist repaired AeroSync EC target {}: {}",
            path.display(),
            e.error
        );
        let _ = e.file.close();
        msg
    })?;
    Ok(SyncEcRepairResult::Repaired { recovered_shards })
}

/// Streaming counterpart of `verify_repair_sync_file`: instead of taking the whole
/// sidecar in memory, read it from `sidecar_path` window by window via
/// `AeroCorrectSidecarReader`. Memory is bounded to one window of plaintext plus that
/// window's parity, regardless of file OR sidecar size. Used by the sync download path,
/// where the sidecar is kept on disk rather than read into RAM (the §1 "streaming
/// sidecar" follow-up: lifts the last O(sidecar size) RAM constraint).
pub(crate) fn verify_repair_sync_file_streamed(
    rel_path: &str,
    expected_sha256: &[u8; 32],
    path: &Path,
    sidecar_path: &Path,
) -> Result<SyncEcRepairResult, String> {
    let file_size = std::fs::metadata(path)
        .map_err(|e| format!("stat AeroSync EC target {}: {e}", path.display()))?
        .len();
    let mut reader = AeroCorrectSidecarReader::open(sidecar_path)?;
    // Same validation as the in-memory path: content binding, declared length, tiling.
    reader.verify_binding(expected_sha256).map_err(|e| {
        format!("AeroSync EC sidecar for {rel_path} does not match the expected file: {e}")
    })?;
    if reader.total_len != file_size {
        return Err(format!(
            "AeroSync EC sidecar total length {} != file length {file_size} for {rel_path}",
            reader.total_len
        ));
    }
    validate_window_tiling_iter(
        reader
            .segments()
            .iter()
            .map(|s| (s.window_offset, s.window_len)),
        file_size,
    )
    .map_err(|e| format!("AeroSync EC sidecar for {rel_path}: {e}"))?;

    // Fast path: stream the file once and hash it. Bounded memory.
    if hash_file_streaming(path)? == *expected_sha256 {
        return Ok(SyncEcRepairResult::Verified);
    }

    // Repair path: stream window by window into a temp file in the same directory,
    // reading each window's parity from the sidecar on demand, then atomically replace
    // the original ONLY if the whole repaired stream hashes to the expected value.
    // Bounded memory (one window + its parity); all-or-nothing.
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = tempfile::NamedTempFile::new_in(parent).map_err(|e| {
        format!(
            "create AeroSync EC repair temp in {}: {e}",
            parent.display()
        )
    })?;
    let mut src =
        File::open(path).map_err(|e| format!("open AeroSync EC target {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut recovered_shards = 0usize;
    {
        let mut out = std::io::BufWriter::new(tmp.as_file());
        for idx in 0..reader.segments().len() {
            let window_len = usize::try_from(reader.segments()[idx].window_len)
                .map_err(|_| format!("AeroSync EC sidecar window {idx} length exceeds usize"))?;
            let avec = reader.read_segment_avec(idx)?;
            let mut buf = vec![0u8; window_len];
            src.read_exact(&mut buf)
                .map_err(|e| format!("read AeroSync EC target window {}: {e}", path.display()))?;
            let mut blocks = vec![buf];
            recovered_shards += reconstruct_from_error_correction(&mut blocks, &avec)?;
            hasher.update(&blocks[0]);
            out.write_all(&blocks[0])
                .map_err(|e| format!("write AeroSync EC repair temp {}: {e}", path.display()))?;
        }
        out.flush()
            .map_err(|e| format!("flush AeroSync EC repair temp {}: {e}", path.display()))?;
    }
    if finalize_sha256(hasher) != *expected_sha256 {
        // tmp is dropped (removed) here; the original file is byte-for-byte untouched.
        return Err("AeroSync EC repair failed post-repair SHA-256 verification".to_string());
    }
    // Release the read handle on the target before persisting. On Windows, renaming the
    // repaired temp onto a file that still has a live read handle fails with
    // ERROR_ACCESS_DENIED (os error 5) (audit M1). The repair loop above is the only
    // reader of `src`, so the handle is safe to drop here.
    drop(src);
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(tmp.path(), meta.permissions());
    }
    // On a persist failure, recover the NamedTempFile from the error and delete it so a
    // decrypted-plaintext `.tmp*` is never left beside the target (audit M1 plaintext-at-rest).
    tmp.persist(path).map_err(|e| {
        let msg = format!(
            "persist repaired AeroSync EC target {}: {}",
            path.display(),
            e.error
        );
        let _ = e.file.close();
        msg
    })?;
    Ok(SyncEcRepairResult::Repaired { recovered_shards })
}

/// Outcome of a read-only standalone verify (the file is never mutated).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StandaloneVerifyResult {
    Verified,
    NeedsRepair,
}

/// Read-only verify of a standalone file against its on-disk `.aerocorrect`
/// sidecar. It keeps only the segment directory and checksum buffer in memory,
/// never the whole sidecar.
pub(crate) fn verify_standalone_file_streamed(
    rel_path: &str,
    path: &Path,
    sidecar_path: &Path,
) -> Result<StandaloneVerifyResult, String> {
    let reader = AeroCorrectSidecarReader::open(sidecar_path)?;
    let expected = reader.content_sha256;
    reader.verify_binding(&expected).map_err(|e| {
        format!("Standalone EC sidecar for {rel_path} is internally inconsistent: {e}")
    })?;
    let file_size = std::fs::metadata(path)
        .map_err(|e| format!("stat {}: {e}", path.display()))?
        .len();
    if reader.total_len != file_size {
        return Err(format!(
            "Standalone EC sidecar total length {} != file length {file_size} for {rel_path}",
            reader.total_len
        ));
    }
    validate_window_tiling_iter(
        reader
            .segments()
            .iter()
            .map(|s| (s.window_offset, s.window_len)),
        file_size,
    )
    .map_err(|e| format!("Standalone EC sidecar for {rel_path}: {e}"))?;
    if hash_file_streaming(path)? == expected {
        Ok(StandaloneVerifyResult::Verified)
    } else {
        Ok(StandaloneVerifyResult::NeedsRepair)
    }
}

/// Repair a standalone file from its own `.aerocorrect` sidecar (atomic, all-or-nothing).
/// The sidecar's stored `content_sha256` is the expected good hash; parity is
/// read window-by-window by `verify_repair_sync_file_streamed`.
pub(crate) fn verify_repair_standalone_file_streamed(
    rel_path: &str,
    path: &Path,
    sidecar_path: &Path,
) -> Result<SyncEcRepairResult, String> {
    let reader = AeroCorrectSidecarReader::open(sidecar_path)?;
    let expected = reader.content_sha256;
    drop(reader);
    verify_repair_sync_file_streamed(rel_path, &expected, path, sidecar_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_data(len: usize) -> Vec<u8> {
        let mut seed = *blake3::hash(b"aerosync-ec-p1-seed").as_bytes();
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            seed = *blake3::hash(&seed).as_bytes();
            out.extend_from_slice(&seed);
        }
        out.truncate(len);
        out
    }

    #[test]
    fn aerosync_estimate_matches_real_sidecar_len() {
        // The sync-doctor cost preview MUST equal the byte size the generator actually writes.
        for &len in &[0usize, 1, 100, 4096, 4097, 50_000, 1_000_000] {
            for &pct in &[7u32, 15, 20, 25, 30, 50] {
                let data = sample_data(len);
                let real = generate_sync_sidecar_for_bytes("backup/x.bin", &data, pct).sidecar_len;
                let est = estimate_aerorec_sidecar_len(len as u64, pct);
                assert_eq!(est, real, "estimate != real for len={len} pct={pct}");
            }
        }
        // MAX_SHARD cliff: a 10 MiB file at 50% spills into multiple full-parity groups.
        let big = sample_data(10 * 1024 * 1024);
        let real = generate_sync_sidecar_for_bytes("backup/big.bin", &big, 50).sidecar_len;
        assert_eq!(estimate_aerorec_sidecar_len(big.len() as u64, 50), real);
    }

    #[test]
    fn aerosync_sidecar_round_trips_single_segment() {
        let data = sample_data(96 * 1024 + 17);
        let generated = generate_sync_sidecar_for_bytes("backup/photos.raw", &data, 15);
        assert_eq!(generated.file_size, data.len() as u64);
        assert_eq!(generated.bytes_protected, data.len() as u64);
        assert_eq!(generated.shards, 15);
        assert!(generated.overhead_pct > 0.0);
        assert!(generated.avec_payload_len > 0);
        assert_eq!(generated.sidecar_len, generated.sidecar_bytes.len() as u64);
        assert_eq!(
            sync_error_correction_sidecar_path("backup/photos.raw"),
            "backup/photos.raw.aerocorrect"
        );

        let sidecar =
            AeroCorrectSidecar::from_bytes(&generated.sidecar_bytes).expect("parse aerocorrect");
        assert_eq!(sidecar.to_bytes(), generated.sidecar_bytes);
        sidecar
            .verify_binding(&generated.file_sha256)
            .expect("binding should match");
        // A small file is a single full-file window.
        assert_eq!(sidecar.segments.len(), 1);
        assert_eq!(sidecar.segments[0].window_offset, 0);
        assert_eq!(sidecar.segments[0].window_len, data.len() as u64);
    }

    #[test]
    fn aerosync_sidecar_rejects_wrong_content_binding() {
        let data = sample_data(64 * 1024);
        let generated = generate_sync_sidecar_for_bytes("backup/config.tar", &data, 20);
        // A different expected hash (different content) must be rejected by the binding,
        // regardless of which path the sync layer paired the sidecar to.
        let other = sample_data(64 * 1024 + 1);
        let mut downloaded = data.clone();
        let err = verify_repair_sync_bytes(
            "backup/config.tar",
            &sha256_bytes(&other),
            &mut downloaded,
            &generated.sidecar_bytes,
        )
        .expect_err("wrong expected content must reject");
        assert!(err.contains("binding mismatch"));
    }

    #[test]
    fn aerosync_sidecar_repairs_corrupt_file_bytes() {
        let data = sample_data(128 * 1024 + 9);
        let generated = generate_sync_sidecar_for_bytes("backup/archive.bin", &data, 20);
        let mut damaged = data.clone();
        damaged[17_123] ^= 0xA5;

        let result = verify_repair_sync_bytes(
            "backup/archive.bin",
            &generated.file_sha256,
            &mut damaged,
            &generated.sidecar_bytes,
        )
        .expect("repair should succeed");

        match result {
            SyncEcRepairResult::Repaired { recovered_shards } => assert!(recovered_shards >= 1),
            SyncEcRepairResult::Verified => panic!("damaged data should need repair"),
        }
        assert_eq!(damaged, data);
    }

    #[test]
    fn aerosync_generation_skips_too_large_files() {
        let data = sample_data(4097);
        match generate_sync_sidecar_for_bytes_capped("backup/large.bin", &data, 20, 4096) {
            SyncEcGenerateResult::SkippedTooLarge {
                file_size,
                max_file_size,
            } => {
                assert_eq!(file_size, 4097);
                assert_eq!(max_file_size, 4096);
            }
            other => panic!("oversize file should be skipped, got {other:?}"),
        }

        match generate_sync_sidecar_for_bytes_capped(
            "backup/large.bin",
            &data,
            20,
            AEROSYNC_EC_MAX_FILE_SIZE,
        ) {
            SyncEcGenerateResult::Generated(generated) => {
                assert_eq!(generated.file_size, data.len() as u64);
                assert!(generated.sidecar_len > generated.avec_payload_len);
            }
            other => panic!("default cap should allow this test file, got {other:?}"),
        }
    }

    // --- Windowed (large-file) streaming ---

    #[test]
    fn windowed_sidecar_has_multiple_segments_and_round_trips() {
        let window = 40_000u64;
        let data = sample_data(135_000); // 4 windows (40k,40k,40k,15k)
        let generated =
            generate_sync_sidecar_for_bytes_windowed("backup/big.bin", &data, 20, window);
        let sidecar =
            AeroCorrectSidecar::from_bytes(&generated.sidecar_bytes).expect("parse windowed");
        assert_eq!(sidecar.segments.len(), 4);
        assert_eq!(sidecar.segments[0].window_len, 40_000);
        assert_eq!(sidecar.segments[3].window_len, 15_000);
        assert_eq!(generated.bytes_protected, data.len() as u64);
        assert!(generated.avec_payload_len > 0);
    }

    #[test]
    fn windowed_repair_fixes_damage_in_each_window() {
        let window = 40_000u64;
        let data = sample_data(135_000);
        let generated =
            generate_sync_sidecar_for_bytes_windowed("backup/big.bin", &data, 20, window);
        let mut damaged = data.clone();
        // Corrupt one byte in each of the four windows.
        for off in [10_000usize, 50_000, 90_000, 130_000] {
            damaged[off] ^= 0x5A;
        }
        let result = verify_repair_sync_bytes(
            "backup/big.bin",
            &generated.file_sha256,
            &mut damaged,
            &generated.sidecar_bytes,
        )
        .expect("multi-window repair should succeed");
        assert!(matches!(result, SyncEcRepairResult::Repaired { .. }));
        assert_eq!(damaged, data);
    }

    #[test]
    fn windowed_repair_fails_when_a_window_is_beyond_recovery() {
        let window = 40_000u64;
        let data = sample_data(135_000);
        let generated =
            generate_sync_sidecar_for_bytes_windowed("backup/big.bin", &data, 20, window);
        let mut damaged = data.clone();
        // Obliterate a whole window: beyond the per-window parity budget.
        for b in damaged[0..40_000].iter_mut() {
            *b ^= 0xFF;
        }
        let err = verify_repair_sync_bytes(
            "backup/big.bin",
            &generated.file_sha256,
            &mut damaged,
            &generated.sidecar_bytes,
        )
        .expect_err("unrecoverable window must fail post-verify");
        assert!(err.contains("post-repair SHA-256"));
    }

    #[test]
    fn streaming_file_generation_matches_in_memory_and_repairs() {
        let window = 40_000u64;
        let data = sample_data(135_000);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("payload.bin");
        std::fs::write(&path, &data).unwrap();

        // Streaming file generation must be byte-identical to in-memory generation.
        let in_mem = generate_sync_sidecar_for_bytes_windowed("rel", &data, 20, window);
        let streamed = match generate_sync_sidecar_for_file_capped_windowed(
            "rel",
            &path,
            20,
            AEROSYNC_EC_MAX_FILE_SIZE,
            0,
            window,
        )
        .unwrap()
        {
            SyncEcGenerateResult::Generated(g) => g,
            other => panic!("should generate, got {other:?}"),
        };
        assert_eq!(streamed.sidecar_bytes, in_mem.sidecar_bytes);
        assert_eq!(streamed.file_sha256, in_mem.file_sha256);

        // Corrupt the file across two windows, then stream-repair it in place.
        let mut corrupt = data.clone();
        corrupt[5_000] ^= 0xAA;
        corrupt[100_000] ^= 0xBB;
        std::fs::write(&path, &corrupt).unwrap();
        let result =
            verify_repair_sync_file("rel", &streamed.file_sha256, &path, &streamed.sidecar_bytes)
                .expect("streaming repair should succeed");
        assert!(matches!(result, SyncEcRepairResult::Repaired { .. }));
        assert_eq!(std::fs::read(&path).unwrap(), data);

        // A clean file verifies without rewriting.
        let verified =
            verify_repair_sync_file("rel", &streamed.file_sha256, &path, &streamed.sidecar_bytes)
                .expect("verify clean file");
        assert_eq!(verified, SyncEcRepairResult::Verified);
    }

    #[test]
    fn streamed_repair_from_on_disk_sidecar_is_byte_identical() {
        let window = 40_000u64;
        let data = sample_data(135_000);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("payload.bin");
        std::fs::write(&path, &data).unwrap();

        // Generate a multi-window sidecar and persist it next to the file.
        let generated = match generate_sync_sidecar_for_file_capped_windowed(
            "rel",
            &path,
            20,
            AEROSYNC_EC_MAX_FILE_SIZE,
            0,
            window,
        )
        .unwrap()
        {
            SyncEcGenerateResult::Generated(g) => g,
            other => panic!("should generate, got {other:?}"),
        };
        let sidecar_path = dir.path().join("payload.bin.aerocorrect");
        std::fs::write(&sidecar_path, &generated.sidecar_bytes).unwrap();

        // Corrupt two distinct windows, then repair streaming from the on-disk sidecar.
        let mut corrupt = data.clone();
        corrupt[5_000] ^= 0xAA;
        corrupt[100_000] ^= 0xBB;
        std::fs::write(&path, &corrupt).unwrap();
        let result =
            verify_repair_sync_file_streamed("rel", &generated.file_sha256, &path, &sidecar_path)
                .expect("streamed repair should succeed");
        assert!(matches!(result, SyncEcRepairResult::Repaired { .. }));
        assert_eq!(std::fs::read(&path).unwrap(), data, "byte-identical repair");

        // A clean file verifies without rewriting.
        assert_eq!(
            verify_repair_sync_file_streamed("rel", &generated.file_sha256, &path, &sidecar_path)
                .unwrap(),
            SyncEcRepairResult::Verified
        );

        // A wrong expected hash (foreign/stale sidecar) fails closed, leaving the file intact.
        let before = std::fs::read(&path).unwrap();
        let wrong = [0u8; 32];
        assert!(
            verify_repair_sync_file_streamed("rel", &wrong, &path, &sidecar_path).is_err(),
            "binding mismatch must fail"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "file untouched on failure"
        );
    }

    /// Self-resilient sidecar (#276 Ehud point 2): a `.aerocorrect` with a stray flip
    /// in its PARITY region must still recover the target. The flipped parity byte is a
    /// rotted shard, which `reconstruct_from_error_correction` already routes around via
    /// the per-shard checksum, so the only thing that could break recovery is a wholesale
    /// envelope reject at open time. This test pins the SHIPPED behavior: the sidecar
    /// opens despite the parity rot and repairs the target byte-identically.
    #[test]
    fn lightly_corrupt_sidecar_parity_still_recovers() {
        let window = 40_000u64;
        let data = sample_data(135_000);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("payload.bin");
        std::fs::write(&path, &data).unwrap();

        let generated = match generate_sync_sidecar_for_file_capped_windowed(
            "rel",
            &path,
            20,
            AEROSYNC_EC_MAX_FILE_SIZE,
            0,
            window,
        )
        .unwrap()
        {
            SyncEcGenerateResult::Generated(g) => g,
            other => panic!("should generate, got {other:?}"),
        };
        let sidecar_path = dir.path().join("payload.bin.aerocorrect");
        let mut sidecar_bytes = generated.sidecar_bytes.clone();
        // Flip one byte deep in the sidecar's parity region (the tail is parity data:
        // a rotted parity shard, not part of the locator). Stay clear of any trailing
        // metadata by landing in the middle of the payload.
        let flip = sidecar_bytes.len() / 2;
        sidecar_bytes[flip] ^= 0xFF;
        std::fs::write(&sidecar_path, &sidecar_bytes).unwrap();

        // Corrupt the target file in a recoverable way (one byte in one window).
        let mut corrupt = data.clone();
        corrupt[5_000] ^= 0xAA;
        std::fs::write(&path, &corrupt).unwrap();

        let result =
            verify_repair_sync_file_streamed("rel", &generated.file_sha256, &path, &sidecar_path)
                .expect("a lightly-corrupted sidecar must still recover");
        assert!(matches!(result, SyncEcRepairResult::Repaired { .. }));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            data,
            "byte-identical recovery from a parity-rotted sidecar"
        );
    }

    /// Audit M1 regression (app copy): the target read handle used to be held across
    /// `persist`, which on Windows failed the rename (os error 5) and left a
    /// decrypted-plaintext `.tmp*` beside the target. After the fix, both the in-memory
    /// and on-disk-sidecar repair paths must restore the bytes and leave no temp artifact.
    #[test]
    fn repair_restores_and_leaves_no_temp_artifact() {
        let window = 40_000u64;
        let data = sample_data(135_000);
        for streamed in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("payload.bin");
            std::fs::write(&path, &data).unwrap();

            let generated = match generate_sync_sidecar_for_file_capped_windowed(
                "rel",
                &path,
                20,
                AEROSYNC_EC_MAX_FILE_SIZE,
                0,
                window,
            )
            .unwrap()
            {
                SyncEcGenerateResult::Generated(g) => g,
                other => panic!("should generate, got {other:?}"),
            };
            let sidecar_path = dir.path().join("payload.bin.aerocorrect");
            std::fs::write(&sidecar_path, &generated.sidecar_bytes).unwrap();

            let mut corrupt = data.clone();
            corrupt[5_000] ^= 0xAA;
            corrupt[100_000] ^= 0xBB;
            std::fs::write(&path, &corrupt).unwrap();

            let result = if streamed {
                verify_repair_sync_file_streamed("rel", &generated.file_sha256, &path, &sidecar_path)
            } else {
                verify_repair_sync_file("rel", &generated.file_sha256, &path, &generated.sidecar_bytes)
            }
            .expect("repair should succeed on every OS");
            assert!(matches!(result, SyncEcRepairResult::Repaired { .. }));
            assert_eq!(std::fs::read(&path).unwrap(), data, "repair must restore bytes");

            let leftovers: Vec<_> = std::fs::read_dir(dir.path())
                .unwrap()
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n != "payload.bin" && n != "payload.bin.aerocorrect")
                .collect();
            assert!(
                leftovers.is_empty(),
                "repair (streamed={streamed}) must not leave a temp artifact, found: {leftovers:?}"
            );
        }
    }

    #[test]
    fn minimum_benefit_gate_skips_tiny_high_overhead_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tiny.bin");
        // 100 bytes at 15% still produces a multi-KiB sidecar (parity-shard floor), so the
        // real overhead is many hundreds of percent.
        std::fs::write(&path, sample_data(100)).unwrap();

        // Confirm the estimate really is far above the file size (precondition for the gate).
        let est = estimate_windowed_sidecar_len(100, 15, AEROCORRECT_WINDOW_SIZE);
        assert!(
            est > 100 * 3,
            "tiny-file sidecar should be >300% of the file"
        );

        // Gate off (0): generates as before.
        match generate_sync_sidecar_for_file_capped(
            "tiny.bin",
            &path,
            15,
            AEROSYNC_EC_MAX_FILE_SIZE,
            0,
        )
        .unwrap()
        {
            SyncEcGenerateResult::Generated(_) => {}
            other => panic!("gate disabled should generate, got {other:?}"),
        }

        // Gate at 300%: the tiny file is skipped with its real overhead reported.
        match generate_sync_sidecar_for_file_capped(
            "tiny.bin",
            &path,
            15,
            AEROSYNC_EC_MAX_FILE_SIZE,
            300,
        )
        .unwrap()
        {
            SyncEcGenerateResult::SkippedLowBenefit {
                file_size,
                overhead_pct,
            } => {
                assert_eq!(file_size, 100);
                assert!(
                    overhead_pct > 300,
                    "reported overhead must exceed the threshold"
                );
            }
            other => panic!("tiny file should be skipped as low benefit, got {other:?}"),
        }
    }

    #[test]
    fn minimum_benefit_gate_keeps_large_low_overhead_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.bin");
        // A 1 MiB file at 15% has a sidecar far below 50% of the file, so a 300% gate keeps it.
        std::fs::write(&path, sample_data(1024 * 1024)).unwrap();
        match generate_sync_sidecar_for_file_capped(
            "big.bin",
            &path,
            15,
            AEROSYNC_EC_MAX_FILE_SIZE,
            300,
        )
        .unwrap()
        {
            SyncEcGenerateResult::Generated(_) => {}
            other => panic!("large low-overhead file should generate, got {other:?}"),
        }
    }

    #[test]
    fn streaming_repair_leaves_original_untouched_when_unrecoverable() {
        let window = 40_000u64;
        let data = sample_data(90_000);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("payload.bin");
        let generated = generate_sync_sidecar_for_bytes_windowed("rel", &data, 20, window);

        // Destroy a whole window beyond recovery and write it to disk.
        let mut corrupt = data.clone();
        for b in corrupt[0..40_000].iter_mut() {
            *b ^= 0xFF;
        }
        std::fs::write(&path, &corrupt).unwrap();
        let before = std::fs::read(&path).unwrap();
        let err = verify_repair_sync_file(
            "rel",
            &generated.file_sha256,
            &path,
            &generated.sidecar_bytes,
        )
        .expect_err("unrecoverable window must fail");
        assert!(err.contains("post-repair SHA-256"));
        // Original on disk is byte-for-byte untouched (temp discarded).
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}
