// Some status/regeneration helpers are intentionally kept for the next AeroSync
// wiring slices; P2 consumes the transfer hooks but not every codec surface yet.
#![allow(dead_code)]

use sha2::{Digest, Sha256};
use std::path::Path;

use super::{
    compute_error_correction_shards_grid, error_correction_grid, reconstruct_from_error_correction,
};

const AEROSYNC_RECOVERY_FILE_MAGIC: &[u8; 8] = b"AERC1\0\0\0";
const AEROSYNC_RECOVERY_BINDING_DOMAIN: &[u8] = b"aerosync-recovery-binding-v1";
const AEROSYNC_RECOVERY_BINDING_LEN: usize = 32;
const AEROSYNC_RECOVERY_CHECKSUM_LEN: usize = 32;

pub(crate) const AEROSYNC_EC_SIDECAR_EXTENSION: &str = ".aerorec";
pub(crate) const AEROSYNC_EC_PHASE1_MAX_FILE_SIZE: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AeroSyncEcSegment {
    pub(crate) window_offset: u64,
    pub(crate) window_len: u64,
    pub(crate) avec_bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AeroSyncEcSidecar {
    pub(crate) binding_id: [u8; AEROSYNC_RECOVERY_BINDING_LEN],
    pub(crate) segments: Vec<AeroSyncEcSegment>,
}

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
    SkippedTooLarge { file_size: u64, max_file_size: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncEcRegenerateReason {
    Missing,
    CorruptSidecar,
    BindingMismatch,
    UnsupportedSegmentLayout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncEcSidecarStatus {
    Current,
    Regenerate { reason: SyncEcRegenerateReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncEcRepairResult {
    Verified,
    Repaired { recovered_shards: usize },
}

fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    let digest = Sha256::digest(data);
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
    format!("{remote_path}{AEROSYNC_EC_SIDECAR_EXTENSION}")
}

pub(crate) fn sync_error_correction_binding_id(
    rel_path: &str,
    file_size: u64,
    content_sha256: &[u8; 32],
) -> [u8; AEROSYNC_RECOVERY_BINDING_LEN] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(AEROSYNC_RECOVERY_BINDING_DOMAIN);
    hasher.update(rel_path.as_bytes());
    hasher.update(&file_size.to_le_bytes());
    hasher.update(content_sha256);
    let digest = hasher.finalize();
    let mut out = [0u8; AEROSYNC_RECOVERY_BINDING_LEN];
    out.copy_from_slice(digest.as_bytes());
    out
}

impl AeroSyncEcSidecar {
    pub(crate) fn new(
        rel_path: &str,
        file_size: u64,
        content_sha256: &[u8; 32],
        segments: Vec<AeroSyncEcSegment>,
    ) -> Self {
        Self {
            binding_id: sync_error_correction_binding_id(rel_path, file_size, content_sha256),
            segments,
        }
    }

    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let segments_len: usize = self
            .segments
            .iter()
            .map(|s| 8 + 8 + 8 + s.avec_bytes.len())
            .sum();
        let mut out = Vec::with_capacity(
            AEROSYNC_RECOVERY_FILE_MAGIC.len()
                + AEROSYNC_RECOVERY_BINDING_LEN
                + 4
                + segments_len
                + AEROSYNC_RECOVERY_CHECKSUM_LEN,
        );
        out.extend_from_slice(AEROSYNC_RECOVERY_FILE_MAGIC);
        out.extend_from_slice(&self.binding_id);
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
        let min_len = AEROSYNC_RECOVERY_FILE_MAGIC.len()
            + AEROSYNC_RECOVERY_BINDING_LEN
            + 4
            + AEROSYNC_RECOVERY_CHECKSUM_LEN;
        if data.len() < min_len {
            return Err("AeroSync EC sidecar too short".to_string());
        }
        if &data[..AEROSYNC_RECOVERY_FILE_MAGIC.len()] != AEROSYNC_RECOVERY_FILE_MAGIC {
            return Err("bad AeroSync EC sidecar magic".to_string());
        }
        let body_end = data.len() - AEROSYNC_RECOVERY_CHECKSUM_LEN;
        let actual = blake3::hash(&data[..body_end]);
        if actual.as_bytes() != &data[body_end..] {
            return Err("AeroSync EC sidecar integrity check failed".to_string());
        }

        let mut off = AEROSYNC_RECOVERY_FILE_MAGIC.len();
        let mut binding_id = [0u8; AEROSYNC_RECOVERY_BINDING_LEN];
        binding_id.copy_from_slice(&data[off..off + AEROSYNC_RECOVERY_BINDING_LEN]);
        off += AEROSYNC_RECOVERY_BINDING_LEN;

        if off + 4 > body_end {
            return Err("AeroSync EC sidecar truncated reading segment count".to_string());
        }
        let segment_count = u32::from_le_bytes(data[off..off + 4].try_into().unwrap()) as usize;
        off += 4;
        if segment_count == 0 {
            return Err("AeroSync EC sidecar has no segments".to_string());
        }

        let mut segments = Vec::with_capacity(segment_count);
        for _ in 0..segment_count {
            if off + 24 > body_end {
                return Err("AeroSync EC sidecar truncated reading segment header".to_string());
            }
            let window_offset = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
            off += 8;
            let window_len = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
            off += 8;
            let avec_len = u64::from_le_bytes(data[off..off + 8].try_into().unwrap()) as usize;
            off += 8;
            if off.checked_add(avec_len).is_none_or(|end| end > body_end) {
                return Err("AeroSync EC sidecar segment length mismatch".to_string());
            }
            let avec_bytes = data[off..off + avec_len].to_vec();
            off += avec_len;
            segments.push(AeroSyncEcSegment {
                window_offset,
                window_len,
                avec_bytes,
            });
        }

        if off != body_end {
            return Err("AeroSync EC sidecar has unexpected trailing bytes".to_string());
        }

        Ok(Self {
            binding_id,
            segments,
        })
    }

    pub(crate) fn verify_binding(
        &self,
        rel_path: &str,
        file_size: u64,
        content_sha256: &[u8; 32],
    ) -> Result<(), String> {
        let expected = sync_error_correction_binding_id(rel_path, file_size, content_sha256);
        if self.binding_id == expected {
            Ok(())
        } else {
            Err(
                "AeroSync EC sidecar does not belong to this file (binding id mismatch)"
                    .to_string(),
            )
        }
    }
}

pub(crate) fn generate_sync_sidecar_for_bytes(
    rel_path: &str,
    data: &[u8],
    pct: u32,
) -> SyncEcGeneratedSidecar {
    let file_size = data.len() as u64;
    let file_sha256 = sha256_bytes(data);
    let (k, p) = error_correction_grid(pct);
    let (avec_bytes, shards, bytes_protected, overhead_pct) =
        compute_error_correction_shards_grid(&[data], k, p);
    let avec_payload_len = avec_bytes.len() as u64;
    let sidecar = AeroSyncEcSidecar::new(
        rel_path,
        file_size,
        &file_sha256,
        vec![AeroSyncEcSegment {
            window_offset: 0,
            window_len: file_size,
            avec_bytes,
        }],
    );
    let sidecar_bytes = sidecar.to_bytes();
    SyncEcGeneratedSidecar {
        sidecar_len: sidecar_bytes.len() as u64,
        sidecar_bytes,
        file_size,
        file_sha256,
        shards,
        bytes_protected,
        overhead_pct,
        avec_payload_len,
    }
}

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

pub(crate) fn generate_sync_sidecar_for_file_capped(
    rel_path: &str,
    path: &Path,
    pct: u32,
    max_file_size: u64,
) -> Result<SyncEcGenerateResult, String> {
    let metadata = std::fs::metadata(path).map_err(|e| {
        format!(
            "read metadata for AeroSync EC source {}: {e}",
            path.display()
        )
    })?;
    if metadata.len() > max_file_size {
        return Ok(SyncEcGenerateResult::SkippedTooLarge {
            file_size: metadata.len(),
            max_file_size,
        });
    }
    let data = std::fs::read(path)
        .map_err(|e| format!("read AeroSync EC source {}: {e}", path.display()))?;
    Ok(generate_sync_sidecar_for_bytes_capped(
        rel_path,
        &data,
        pct,
        max_file_size,
    ))
}

pub(crate) fn sync_sidecar_status_for_bytes(
    rel_path: &str,
    data: &[u8],
    sidecar_bytes: Option<&[u8]>,
) -> SyncEcSidecarStatus {
    let Some(sidecar_bytes) = sidecar_bytes else {
        return SyncEcSidecarStatus::Regenerate {
            reason: SyncEcRegenerateReason::Missing,
        };
    };
    let sidecar = match AeroSyncEcSidecar::from_bytes(sidecar_bytes) {
        Ok(sidecar) => sidecar,
        Err(_) => {
            return SyncEcSidecarStatus::Regenerate {
                reason: SyncEcRegenerateReason::CorruptSidecar,
            };
        }
    };
    if sidecar.segments.len() != 1
        || sidecar.segments[0].window_offset != 0
        || sidecar.segments[0].window_len != data.len() as u64
    {
        return SyncEcSidecarStatus::Regenerate {
            reason: SyncEcRegenerateReason::UnsupportedSegmentLayout,
        };
    }
    let file_sha256 = sha256_bytes(data);
    match sidecar.verify_binding(rel_path, data.len() as u64, &file_sha256) {
        Ok(()) => SyncEcSidecarStatus::Current,
        Err(_) => SyncEcSidecarStatus::Regenerate {
            reason: SyncEcRegenerateReason::BindingMismatch,
        },
    }
}

pub(crate) fn verify_repair_sync_bytes(
    rel_path: &str,
    expected_sha256: &[u8; 32],
    data: &mut Vec<u8>,
    sidecar_bytes: &[u8],
) -> Result<SyncEcRepairResult, String> {
    let sidecar = AeroSyncEcSidecar::from_bytes(sidecar_bytes)?;
    sidecar.verify_binding(rel_path, data.len() as u64, expected_sha256)?;
    if sidecar.segments.len() != 1
        || sidecar.segments[0].window_offset != 0
        || sidecar.segments[0].window_len != data.len() as u64
    {
        return Err("AeroSync EC phase 1 only supports a single full-file segment".to_string());
    }

    if sha256_bytes(data) == *expected_sha256 {
        return Ok(SyncEcRepairResult::Verified);
    }

    let segment = &sidecar.segments[0];
    let mut blocks = vec![data.clone()];
    let recovered_shards = reconstruct_from_error_correction(&mut blocks, &segment.avec_bytes)?;
    let repaired = blocks
        .into_iter()
        .next()
        .ok_or_else(|| "AeroSync EC repair produced no output".to_string())?;
    if sha256_bytes(&repaired) != *expected_sha256 {
        return Err("AeroSync EC repair failed post-repair SHA-256 verification".to_string());
    }
    *data = repaired;
    Ok(SyncEcRepairResult::Repaired { recovered_shards })
}

pub(crate) fn verify_repair_sync_file(
    rel_path: &str,
    expected_sha256: &[u8; 32],
    path: &Path,
    sidecar_bytes: &[u8],
) -> Result<SyncEcRepairResult, String> {
    let mut data = std::fs::read(path)
        .map_err(|e| format!("read AeroSync EC target {}: {e}", path.display()))?;
    let result = verify_repair_sync_bytes(rel_path, expected_sha256, &mut data, sidecar_bytes)?;
    if matches!(result, SyncEcRepairResult::Repaired { .. }) {
        std::fs::write(path, data)
            .map_err(|e| format!("write repaired AeroSync EC target {}: {e}", path.display()))?;
    }
    Ok(result)
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
            "backup/photos.raw.aerorec"
        );

        let sidecar = AeroSyncEcSidecar::from_bytes(&generated.sidecar_bytes).expect("parse AERC1");
        assert_eq!(sidecar.to_bytes(), generated.sidecar_bytes);
        sidecar
            .verify_binding(
                "backup/photos.raw",
                generated.file_size,
                &generated.file_sha256,
            )
            .expect("binding should match");
        assert_eq!(sidecar.segments.len(), 1);
        assert_eq!(sidecar.segments[0].window_offset, 0);
        assert_eq!(sidecar.segments[0].window_len, data.len() as u64);
    }

    #[test]
    fn aerosync_sidecar_rejects_wrong_file_binding() {
        let data = sample_data(64 * 1024);
        let generated = generate_sync_sidecar_for_bytes("backup/config.tar", &data, 20);
        let mut downloaded = data.clone();
        let err = verify_repair_sync_bytes(
            "backup/other-config.tar",
            &generated.file_sha256,
            &mut downloaded,
            &generated.sidecar_bytes,
        )
        .expect_err("wrong file path must reject");
        assert!(err.contains("binding id mismatch"));
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
    fn aerosync_stale_binding_requests_regeneration() {
        let old_data = sample_data(48 * 1024);
        let mut new_data = old_data.clone();
        new_data[2048] ^= 0x5A;
        let generated = generate_sync_sidecar_for_bytes("backup/db.sqlite", &old_data, 20);

        assert_eq!(
            sync_sidecar_status_for_bytes(
                "backup/db.sqlite",
                &new_data,
                Some(&generated.sidecar_bytes)
            ),
            SyncEcSidecarStatus::Regenerate {
                reason: SyncEcRegenerateReason::BindingMismatch
            }
        );
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
            SyncEcGenerateResult::Generated(_) => panic!("oversize file should be skipped"),
        }

        match generate_sync_sidecar_for_bytes_capped(
            "backup/large.bin",
            &data,
            20,
            AEROSYNC_EC_PHASE1_MAX_FILE_SIZE,
        ) {
            SyncEcGenerateResult::Generated(generated) => {
                assert_eq!(generated.file_size, data.len() as u64);
                assert!(generated.sidecar_len > generated.avec_payload_len);
            }
            SyncEcGenerateResult::SkippedTooLarge { .. } => {
                panic!("phase 1 default cap should allow this test file")
            }
        }
    }
}
