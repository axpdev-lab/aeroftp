//! AeroVault v3 draft backend.
//!
//! v3 is the wrapper-stack format: content-defined chunks, keyed BLAKE3
//! chunk identifiers, zstd-per-chunk compression, AES-256-GCM-SIV content
//! encryption, and an extension directory reserved for v4 ECC.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use crate::vault_telemetry::VaultReport;
use aes_gcm_siv::aead::{Aead, Payload};
use aes_gcm_siv::{Aes256GcmSiv, KeyInit, Nonce};
use aes_kw::Kek;
use hmac::{Hmac, Mac};
use rand::RngCore;
use secrecy::zeroize::Zeroize;
use serde::{Deserialize, Serialize};
use sha2::Sha512;
use std::collections::{BTreeMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use reed_solomon_erasure::ReedSolomon;

const MAGIC: &[u8; 10] = b"AEROVAULT3";
const VERSION: u8 = 3;
const HEADER_SIZE: usize = 1024;
const HEADER_MAC_OFFSET: usize = 960;
const MAC_SIZE: usize = 64;
const SALT_SIZE: usize = 32;
const KEY_SIZE: usize = 32;
const WRAPPED_KEY_SIZE: usize = 40;
const NONCE_SIZE: usize = 12;
const MIN_PASSWORD_LEN: usize = 8;
const MAX_MANIFEST_SIZE: u64 = 128 * 1024 * 1024;
const MAX_EXTENSION_DIR_SIZE: u64 = 16 * 1024 * 1024;
const MAX_BLOCK_SIZE: u64 = 64 * 1024 * 1024;
/// Absolute upper bound on the decompressed plaintext of a single content block,
/// matching the largest `max` a `CdcBounds` may declare (`CdcBounds::validate`).
/// The effective per-block cap is the vault's own recorded chunking `max` (or the
/// default `CDC_MAX`), clamped to this ceiling, so a decompression bomb cannot
/// expand a block past one legitimate chunk worth of RAM (CLAUDE-AV-005).
const MAX_PLAINTEXT_BLOCK_SIZE: u64 = 256 * 1024 * 1024;

const DATA_OFFSET: u64 = HEADER_SIZE as u64;
const DEFAULT_ZSTD_LEVEL: i32 = 9;
/// The only wrapper-header layout this build understands. `open_vault` rejects
/// anything else instead of silently decoding with the hardcoded cipher stack
/// (CLAUDE-AV-024 / CODEX-AV-006).
const SUPPORTED_WRAPPER_HEADER_VERSION: u16 = 1;
const CDC_MIN: usize = 256 * 1024;
const CDC_AVG: usize = 1024 * 1024;
const CDC_MAX: usize = 4 * 1024 * 1024;

/// Files strictly smaller than this are batched into shared packs before the
/// CDC chunker runs, so a tree of tiny files still yields multi-MiB chunks.
const PACK_SMALL_FILE_THRESHOLD: usize = CDC_MIN;
/// A pack is flushed once it reaches this size; the CDC chunker then runs over
/// the whole pack rather than per tiny file.
const PACK_TARGET: usize = CDC_MAX;

const HKDF_MASTER: &[u8] = b"AeroVault v3 KEK for master key";
const HKDF_MAC: &[u8] = b"AeroVault v3 KEK for MAC key";
const HKDF_CHUNK_ID: &[u8] = b"AeroVault v3 keyed BLAKE3 chunk ids";
const MANIFEST_AAD: &[u8] = b"AeroVault v3 manifest";
const BLOCK_AAD_PREFIX: &[u8] = b"AeroVault v3 block";

/// Extension ID for the v4 ECC (Reed-Solomon) layer.
/// This is emitted as a non-critical extension (critical=false) so that
/// pure v3 readers can still open and extract from v4+ECC vaults
/// (per the forward-compat contract in AEROVAULT-V3-SPEC.md and discussion #276).
const ECC_EXTENSION_ID: &str = "ecc.reed-solomon";
const ECC_ALGORITHM_ID: &str = "reed-solomon";
const ECC_ALGORITHM_VERSION: u32 = 1;

#[derive(Debug, Clone)]
struct VaultHeaderV3 {
    flags: u8,
    salt: [u8; SALT_SIZE],
    wrapped_master_key: [u8; WRAPPED_KEY_SIZE],
    wrapped_mac_key: [u8; WRAPPED_KEY_SIZE],
    data_offset: u64,
    data_len: u64,
    manifest_offset: u64,
    manifest_len: u64,
    extension_dir_offset: u64,
    extension_dir_len: u64,
    extension_payload_offset: u64,
    extension_payload_len: u64,
    wrapper_header_version: u16,
    header_mac: [u8; MAC_SIZE],
}

/// Content-defined-chunking bounds. Recorded on the `chunking` wrapper so a
/// reader uses the exact bounds the writer used. Absent in pre-GAP-5 v3 vaults
/// and in non-`chunking` wrappers: callers fall back to the const defaults,
/// which keeps every existing vault byte-identical.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct CdcBounds {
    min: usize,
    avg: usize,
    max: usize,
}

impl CdcBounds {
    fn defaults() -> Self {
        Self {
            min: CDC_MIN,
            avg: CDC_AVG,
            max: CDC_MAX,
        }
    }

    /// Profile-driven defaults. `archive` widens the per-chunk zstd window
    /// (bigger avg/max) for ratio at the cost of finer-grained dedup; the
    /// other profiles keep the original bounds.
    fn for_level(level: i32) -> Self {
        if level >= 19 {
            Self {
                min: 1024 * 1024,
                avg: 4 * 1024 * 1024,
                max: 16 * 1024 * 1024,
            }
        } else {
            Self::defaults()
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.min < 4096
            || self.min > self.avg
            || self.avg > self.max
            || self.max > 256 * 1024 * 1024
            || !self.avg.is_power_of_two()
        {
            return Err(format!(
                "Invalid AeroVault v3 CDC bounds: min={} avg={} max={}",
                self.min, self.avg, self.max
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AlgorithmSpec {
    algorithm_id: String,
    algorithm_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    level: Option<i32>,
    /// Only set on the `chunking` wrapper (GAP-5). Additive, serde-default so
    /// older v3 vaults deserialize to `None` and use the const bounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bounds: Option<CdcBounds>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WrapperManifest {
    packing: AlgorithmSpec,
    chunking: AlgorithmSpec,
    chunk_id: AlgorithmSpec,
    compression: AlgorithmSpec,
    crypt: AlgorithmSpec,
    cipher_hash: AlgorithmSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManifestEntryV3 {
    path: String,
    size: u64,
    modified: String,
    is_dir: bool,
    chunks: Vec<String>,
    /// Byte offset of this file inside the concatenation of its listed chunks.
    /// `None` (or absent in older v3 vaults) means the file owns its chunks
    /// whole, starting at offset 0: identical to pre-packing behaviour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pack_offset: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChunkRecordV3 {
    id: String,
    block_index: u64,
    data_offset: u64,
    block_len: u64,
    plaintext_len: u64,
    compressed_len: u64,
    cipher_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VaultManifestV3 {
    format: u8,
    created: String,
    modified: String,
    wrappers: WrapperManifest,
    entries: Vec<ManifestEntryV3>,
    chunks: BTreeMap<String, ChunkRecordV3>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExtensionEntryV3 {
    extension_id: String,
    algorithm_id: String,
    algorithm_version: u32,
    critical: bool,
    offset: u64,
    length: u64,
}

/// P2-09: On-disk payload format (v2) for the "ecc.reed-solomon" extension.
///
/// v1 mapped one ciphertext block to one Reed-Solomon shard and sized every shard
/// to the largest block. With content-defined chunking (min 256 KiB, avg 1 MiB) real
/// vaults have few, large chunks, so under-filled stripes still stored two full-size
/// parity shards: a 300 KB single-chunk vault produced ~600 KB of parity (≈200%).
///
/// v2 protects the *concatenated* live-block stream with a fixed shard grid:
///   - Concatenate the on-disk blocks ([u64 len][ciphertext]) in data-section order
///     into one logical stream D of length L.
///   - Cut D into a regular grid of `shard_size` (S) data shards; S is chosen so a
///     small vault is exactly one full RS group (overhead == P/K) and a large vault
///     is many full groups (capped granularity). Overhead is ~P/K regardless of how
///     many / how large the chunks are.
///   - Each group of K data shards gets P parity shards via RS(K, P).
///
/// Damage localization no longer relies on the per-block cipher_hash (too coarse: a
/// few rotted bytes in a large chunk would mark the whole chunk, erasing every shard
/// it spans). Instead the payload stores a truncated-BLAKE3 checksum per shard (data
/// and parity). On repair, any shard whose checksum mismatches is treated as an RS
/// erasure, so localized rot inside a large chunk erases only the affected shard(s),
/// and a rotted parity shard is detected and routed around. Correctness is still
/// guaranteed end-to-end by re-verifying every repaired block against its
/// authenticated manifest cipher_hash (the all-or-nothing safety gate in repair_vault).
///
/// Layout (all multi-byte fields little-endian):
///   [EccPayloadHeader: 32 bytes]
///   [data-shard checksums:   num_data_shards * ECC_SHARD_CKSUM_LEN]
///   [parity-shard checksums: num_groups * P  * ECC_SHARD_CKSUM_LEN]
///   [parity data:            num_groups * P  * S]
/// where num_data_shards = ceil(L/S) and num_groups = ceil(num_data_shards/K).
///
/// The format is pre-release; bumping ECC_PAYLOAD_VERSION needs no migration.
const ECC_PAYLOAD_MAGIC: &[u8; 4] = b"AVEC";
const ECC_PAYLOAD_VERSION: u16 = 2;

/// Reed-Solomon group geometry. K data + P parity per group => P/K == 20% overhead,
/// tolerating up to P erased shards (data or parity) per group.
const ECC_DATA_SHARDS: usize = 10;
const ECC_PARITY_SHARDS: usize = 2;
/// Shard-size grid bounds. For small vaults S = ceil(L/K) yields a single full group
/// (exactly P/K overhead); ECC_MIN_SHARD keeps micro-vault shards sane and
/// ECC_MAX_SHARD bounds shard granularity (and per-shard recovery cost) for large
/// vaults, which then span multiple full groups.
const ECC_MIN_SHARD: usize = 4096;
const ECC_MAX_SHARD: usize = 1 << 20; // 1 MiB
/// Truncated BLAKE3 length stored per shard for erasure localization. 128 bits makes
/// an accidental-rot collision (~2^-128) irrelevant; this is a rot detector, not a
/// security primitive (block integrity remains the manifest cipher_hash).
const ECC_SHARD_CKSUM_LEN: usize = 16;

/// 16-byte rot-detection checksum for one shard.
fn ecc_shard_checksum(shard: &[u8]) -> [u8; ECC_SHARD_CKSUM_LEN] {
    let h = blake3::hash(shard);
    let mut out = [0u8; ECC_SHARD_CKSUM_LEN];
    out.copy_from_slice(&h.as_bytes()[..ECC_SHARD_CKSUM_LEN]);
    out
}

#[derive(Debug, Clone, Copy)]
struct EccPayloadHeader {
    data_shards: u16,    // K per group
    parity_shards: u16,  // P per group
    shard_size: u32,     // S (bytes per shard; data is zero-padded to this)
    total_data_len: u64, // L (length of the concatenated live-block stream)
}

impl EccPayloadHeader {
    fn to_bytes(&self) -> [u8; 32] {
        let mut buf = [0u8; 32];
        buf[0..4].copy_from_slice(ECC_PAYLOAD_MAGIC);
        buf[4..6].copy_from_slice(&ECC_PAYLOAD_VERSION.to_le_bytes());
        buf[6..8].copy_from_slice(&self.data_shards.to_le_bytes());
        buf[8..10].copy_from_slice(&self.parity_shards.to_le_bytes());
        buf[10..14].copy_from_slice(&self.shard_size.to_le_bytes());
        buf[14..22].copy_from_slice(&self.total_data_len.to_le_bytes());
        // bytes 22..32 reserved (zero)
        buf
    }

    fn from_bytes(data: &[u8]) -> Result<Self, String> {
        if data.len() < 32 {
            return Err("EccPayloadHeader too short".to_string());
        }
        if &data[0..4] != ECC_PAYLOAD_MAGIC {
            return Err("bad ECC payload magic".to_string());
        }
        let version = u16::from_le_bytes(data[4..6].try_into().unwrap());
        if version != ECC_PAYLOAD_VERSION {
            return Err(format!("unsupported ECC payload version {}", version));
        }
        let h = EccPayloadHeader {
            data_shards: u16::from_le_bytes(data[6..8].try_into().unwrap()),
            parity_shards: u16::from_le_bytes(data[8..10].try_into().unwrap()),
            shard_size: u32::from_le_bytes(data[10..14].try_into().unwrap()),
            total_data_len: u64::from_le_bytes(data[14..22].try_into().unwrap()),
        };
        if h.data_shards == 0 || h.shard_size == 0 {
            return Err("invalid ECC payload header (zero shard geometry)".to_string());
        }
        Ok(h)
    }
}

/// (num_data_shards, num_groups) derived from a header.
fn ecc_geometry(h: &EccPayloadHeader) -> (usize, usize) {
    let k = h.data_shards as usize;
    let s = h.shard_size as usize;
    let l = h.total_data_len as usize;
    let num_data_shards = (l + s - 1) / s;
    let num_groups = (num_data_shards + k - 1) / k;
    (num_data_shards, num_groups)
}

/// Full in-memory representation of one ECC extension payload (v2). This is what
/// gets written into the extension payload area when ECC is enabled.
#[derive(Debug, Clone)]
struct EccPayload {
    header: EccPayloadHeader,
    /// One checksum per data shard, indexed 0..num_data_shards (grid order).
    data_checksums: Vec<[u8; ECC_SHARD_CKSUM_LEN]>,
    /// One checksum per parity shard, indexed group-major: group g, parity p lives
    /// at g*P + p. Length == num_groups * P.
    parity_checksums: Vec<[u8; ECC_SHARD_CKSUM_LEN]>,
    /// Concatenated parity data, group-major. Length == num_groups * P * S.
    parity_data: Vec<u8>,
}

impl EccPayload {
    fn to_bytes(&self) -> Vec<u8> {
        let cksum_bytes =
            (self.data_checksums.len() + self.parity_checksums.len()) * ECC_SHARD_CKSUM_LEN;
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

    fn from_bytes(data: &[u8]) -> Result<Self, String> {
        let header = EccPayloadHeader::from_bytes(data)?;
        let (num_data_shards, num_groups) = ecc_geometry(&header);
        let p = header.parity_shards as usize;
        let s = header.shard_size as usize;

        let num_parity = num_groups * p;
        let cksum_table = (num_data_shards + num_parity) * ECC_SHARD_CKSUM_LEN;
        let parity_len = num_parity * s;
        let expected = 32 + cksum_table + parity_len;
        if data.len() != expected {
            return Err(format!(
                "EccPayload length mismatch: got {}, expected {}",
                data.len(),
                expected
            ));
        }

        let mut off = 32;
        let read_cksums = |count: usize, off: &mut usize| {
            let mut v = Vec::with_capacity(count);
            for _ in 0..count {
                let mut c = [0u8; ECC_SHARD_CKSUM_LEN];
                c.copy_from_slice(&data[*off..*off + ECC_SHARD_CKSUM_LEN]);
                v.push(c);
                *off += ECC_SHARD_CKSUM_LEN;
            }
            v
        };
        let data_checksums = read_cksums(num_data_shards, &mut off);
        let parity_checksums = read_cksums(num_parity, &mut off);
        let parity_data = data[off..].to_vec();

        Ok(EccPayload { header, data_checksums, parity_checksums, parity_data })
    }
}

/// P2-09: Compute the ECC payload (v2 fixed-grid format) for the concatenated
/// live-block stream.
///
/// `data_blocks`: the on-disk stored form of each live chunk in data-section order
///                (i.e. [u64 little-endian length][ciphertext bytes...]). These are
///                exactly the bytes that have a corresponding `cipher_hash` in the
///                manifest.
///
/// Returns the serialized payload bytes to store in the extension payload area for
/// the "ecc.reed-solomon" entry (empty vec when there is no data). See the format
/// doc above EccPayloadHeader for the on-disk layout and the overhead rationale.
fn compute_ecc_shards(data_blocks: &[&[u8]]) -> Vec<u8> {
    // Concatenate the live blocks into the logical stream D of length L.
    let l: usize = data_blocks.iter().map(|b| b.len()).sum();
    if l == 0 {
        return vec![];
    }
    let mut d = Vec::with_capacity(l);
    for b in data_blocks {
        d.extend_from_slice(b);
    }

    let k = ECC_DATA_SHARDS;
    let p = ECC_PARITY_SHARDS;
    // S = ceil(L/K) clamped: a small vault becomes one full group (overhead == P/K);
    // a large vault becomes many full groups at capped shard granularity.
    let s = ((l + k - 1) / k).clamp(ECC_MIN_SHARD, ECC_MAX_SHARD);

    let num_data_shards = (l + s - 1) / s;
    let num_groups = (num_data_shards + k - 1) / k;

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
        data_checksums.push(ecc_shard_checksum(&shard_at(i)));
    }

    let rs = ReedSolomon::<reed_solomon_erasure::galois_8::Field>::new(k, p)
        .expect("invalid ReedSolomon parameters");

    let mut parity_data = Vec::with_capacity(num_groups * p * s);
    let mut parity_checksums = Vec::with_capacity(num_groups * p);

    for g in 0..num_groups {
        // K data slots + P parity slots. Slots past num_data_shards stay zero
        // (virtual padding); parity slots are filled by encode.
        let mut shards: Vec<Vec<u8>> = vec![vec![0u8; s]; k + p];
        for local in 0..k {
            let gi = g * k + local;
            if gi < num_data_shards {
                shards[local] = shard_at(gi);
            }
        }
        rs.encode(&mut shards).expect("RS encode failed");
        for pp in 0..p {
            let par = &shards[k + pp];
            parity_checksums.push(ecc_shard_checksum(par));
            parity_data.extend_from_slice(par);
        }
    }

    let header = EccPayloadHeader {
        data_shards: k as u16,
        parity_shards: p as u16,
        shard_size: s as u32,
        total_data_len: l as u64,
    };

    EccPayload { header, data_checksums, parity_checksums, parity_data }.to_bytes()
}

/// P2-09: Reconstruct damaged bytes in the live-block stream using the v2 ECC payload.
///
/// `blocks`: the on-disk blocks ([u64 len][ciphertext]) in data-section order, each
///           EXACTLY `8 + block_len` bytes (the caller zero-pads truncated blocks) so
///           the concatenation length matches the payload's recorded stream length.
/// `ecc_payload_bytes`: the bytes stored in the ECC extension payload.
///
/// Damaged shards are located by per-shard checksum mismatch (data and parity), then
/// RS-reconstructed per group; recovered bytes are written back into `blocks` in place.
/// Returns the number of data shards successfully reconstructed.
///
/// A successful return does NOT imply correctness: the caller (repair_vault) must
/// re-verify every repaired block against its authenticated cipher_hash before
/// persisting (all-or-nothing safety gate). A grid misalignment (stream length
/// mismatch) is rejected up front so good data can never be silently overwritten.
fn reconstruct_from_ecc(
    blocks: &mut [Vec<u8>],
    ecc_payload_bytes: &[u8],
) -> Result<usize, String> {
    if ecc_payload_bytes.is_empty() {
        return Ok(0);
    }
    let payload = EccPayload::from_bytes(ecc_payload_bytes)?;
    let k = payload.header.data_shards as usize;
    let p = payload.header.parity_shards as usize;
    let s = payload.header.shard_size as usize;
    let l = payload.header.total_data_len as usize;

    // The block stream must match the stream the parity was computed over, otherwise
    // the shard grid would be misaligned and reconstruction could corrupt good data.
    let total: usize = blocks.iter().map(|b| b.len()).sum();
    if total != l {
        return Err(format!(
            "ECC reconstruct: block stream length {} != payload stream length {}",
            total, l
        ));
    }

    let mut d = Vec::with_capacity(l);
    for b in blocks.iter() {
        d.extend_from_slice(b);
    }

    let (num_data_shards, num_groups) = ecc_geometry(&payload.header);

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

        for local in 0..k {
            let gi = g * k + local;
            if gi < num_data_shards {
                let sh = shard_at(&d, gi);
                if ecc_shard_checksum(&sh) == payload.data_checksums[gi] {
                    opt[local] = Some(sh); // shard intact
                } else {
                    erased_data += 1; // damaged -> RS erasure
                }
            } else {
                opt[local] = Some(vec![0u8; s]); // virtual zero-pad slot
            }
        }

        for pp in 0..p {
            let pidx = g * p + pp;
            let start = pidx * s;
            if start + s <= payload.parity_data.len() {
                let par = payload.parity_data[start..start + s].to_vec();
                if ecc_shard_checksum(&par) == payload.parity_checksums[pidx] {
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

        for local in 0..k {
            let gi = g * k + local;
            if gi >= num_data_shards {
                continue;
            }
            if let Some(sh) = &opt[local] {
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

/// P2-06: Scrub primitive.
/// Walks the chunks in data section order, verifies each cipher_hash against the
/// stored ciphertext block. Returns list of damaged chunks with their full on-disk
/// byte range (starting at the u64 length prefix).
// Module-private: only scrub_vault/repair_vault and the vault_v3_scrub command (all
// in this module) consume it, so it need not be `pub` over the private ChunkRecordV3.
#[derive(Debug, Clone)]
struct DamagedChunk {
    record: ChunkRecordV3,
    /// Start offset in the vault file's data section (includes the u64 prefix).
    on_disk_start: u64,
    /// Full length of the stored unit (8 + cipher len).
    on_disk_len: u64,
}

fn scrub_vault(vault: &OpenVaultV3) -> Vec<DamagedChunk> {
    let mut damaged = vec![];

    // Collect and sort chunks by their physical order in the data section.
    let mut chunks: Vec<_> = vault.manifest.chunks.values().cloned().collect();
    chunks.sort_by_key(|c| c.data_offset);

    for rec in chunks {
        let start = rec.data_offset as usize;
        if start + 8 > vault.data.len() {
            // Truncated block - definitely damaged
            damaged.push(DamagedChunk {
                record: rec.clone(),
                on_disk_start: rec.data_offset,
                on_disk_len: 8,
            });
            continue;
        }

        let stored_len = u64::from_le_bytes(
            vault.data[start..start + 8].try_into().expect("slice"),
        ) as usize;

        let block_start = start + 8;
        let block_end = block_start + stored_len;

        if block_end > vault.data.len() || stored_len != rec.block_len as usize {
            damaged.push(DamagedChunk {
                record: rec.clone(),
                on_disk_start: rec.data_offset,
                on_disk_len: (8 + stored_len) as u64,
            });
            continue;
        }

        let cipher_block = &vault.data[block_start..block_end];
        let actual_hash = blake3::hash(cipher_block).to_hex().to_string();

        if actual_hash != rec.cipher_hash {
            damaged.push(DamagedChunk {
                record: rec.clone(),
                on_disk_start: rec.data_offset,
                on_disk_len: (8 + stored_len) as u64,
            });
        }
    }

    damaged
}

fn repair_vault(vault: &mut OpenVaultV3, dry_run: bool) -> Result<usize, String> {
    let damaged = scrub_vault(vault);
    if damaged.is_empty() {
        return Ok(0);
    }

    let ecc_entry = vault.extensions.iter().find(|e| e.extension_id == ECC_EXTENSION_ID).cloned();
    let ecc_bytes = if let Some(entry) = &ecc_entry {
        if entry.length > 0 {
            let mut f = File::open(&vault.path).map_err(|e| format!("open for repair: {e}"))?;
            let abs = vault.header.extension_payload_offset + entry.offset;
            f.seek(SeekFrom::Start(abs)).map_err(|e| format!("seek for repair: {e}"))?;
            let mut b = vec![0u8; entry.length as usize];
            f.read_exact(&mut b).map_err(|e| format!("read ecc payload: {e}"))?;
            Some(b)
        } else {
            None
        }
    } else {
        None
    };

    let mut repaired_count = 0;

    if let Some(ecc_b) = ecc_bytes {
        let mut ordered: Vec<(String, ChunkRecordV3)> = vault.manifest.chunks
            .iter()
            .map(|(id, r)| (id.clone(), r.clone()))
            .collect();
        ordered.sort_by_key(|(_, r)| r.data_offset);

        let mut blocks: Vec<Vec<u8>> = ordered.iter().map(|(_, rec)| {
            let start = rec.data_offset as usize;
            let full = 8 + rec.block_len as usize;
            // Always a fixed-length (8 + block_len) buffer so the concatenated stream
            // length matches what the ECC parity was computed over, even when the block
            // is truncated on disk: the missing tail is zero-padded here, flagged as
            // damaged by its shard checksum, then reconstructed.
            let mut buf = vec![0u8; full];
            if start < vault.data.len() {
                let avail = (vault.data.len() - start).min(full);
                buf[..avail].copy_from_slice(&vault.data[start..start + avail]);
            }
            buf
        }).collect();

        let bad_indices: Vec<usize> = damaged.iter().filter_map(|d| {
            ordered.iter().position(|(id, _)| id == &d.record.id)
        }).collect();

        let _ = reconstruct_from_ecc(&mut blocks, &ecc_b)?;

        // Safety gate (CLAUDE-AV-ECC-01): RS reconstruction is only correct when
        // the surviving data shards AND the parity shards were themselves intact.
        // The parity lives in the extension payload, which scrub does not cover, so
        // a rotted parity shard (or more erasures than parity in a stripe) silently
        // yields wrong bytes. Verify every reconstructed block against its
        // authenticated manifest cipher_hash before trusting it, and only persist
        // when ALL damaged blocks verify: persisting a wrong reconstruction would
        // recompute parity over the garbage on the next seal, destroying the very
        // redundancy needed to recover. Conservative all-or-nothing matches the
        // repair safety contract ("never overwrite without hash verification").
        let all_verified = bad_indices.iter().all(|&i| {
            let blk = &blocks[i];
            if blk.len() < 8 {
                return false;
            }
            let body = u64::from_le_bytes(blk[0..8].try_into().unwrap()) as usize;
            blk.len() == 8 + body
                && body as u64 == ordered[i].1.block_len
                && blake3::hash(&blk[8..8 + body]).to_hex().to_string() == ordered[i].1.cipher_hash
        });

        if all_verified {
            repaired_count = bad_indices.len();
            if !dry_run {
                let mut new_data = vec![];
                let mut new_chunks = BTreeMap::new();
                for (i, (id, mut rec)) in ordered.into_iter().enumerate() {
                    rec.data_offset = new_data.len() as u64;
                    if blocks[i].len() >= 8 {
                        rec.block_len = u64::from_le_bytes(blocks[i][0..8].try_into().unwrap());
                    }
                    new_data.extend_from_slice(&blocks[i]);
                    new_chunks.insert(id, rec);
                }
                vault.data = new_data;
                vault.manifest.chunks = new_chunks;
                save_open_vault(vault)?;
            }
        }
        // else: reconstruction could not be verified -> leave the vault
        // byte-for-byte untouched (repaired_count stays 0) so no redundancy is lost.
    }

    Ok(repaired_count)
}

#[derive(Debug, Serialize)]
pub struct VaultV3Info {
    pub version: u8,
    pub file_count: usize,
    pub chunk_count: usize,
    pub dedup_chunks: usize,
    pub compression_level: i32,
    pub files: Vec<VaultV3FileInfo>,
    /// Behind-the-scenes technical receipt for the operation that produced
    /// this info (additive: `None` for plain open/listing). Serde-skipped
    /// when absent so the frontend TS interface only gains an optional field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<VaultReport>,
}

#[derive(Debug, Serialize)]
pub struct VaultV3FileInfo {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub modified: String,
    pub chunk_count: usize,
}

#[derive(Debug)]
struct OpenVaultV3 {
    path: PathBuf,
    header: VaultHeaderV3,
    opened_file_len: u64,
    opened_header_mac: [u8; MAC_SIZE],
    master_key: [u8; KEY_SIZE],
    mac_key: [u8; KEY_SIZE],
    manifest: VaultManifestV3,
    extensions: Vec<ExtensionEntryV3>,
    data: Vec<u8>,
    /// Behind-the-scenes technical telemetry accumulated by the current
    /// operation (compression / encryption / chunking / dedup). Not persisted.
    report: VaultReport,
}

impl Drop for OpenVaultV3 {
    /// Wipe the long-lived key material when the open vault is dropped.
    /// Ephemeral KEKs and plaintext/pack buffers are already zeroized at
    /// every use site; the master/MAC keys live for the whole command, so
    /// without this they would linger in freed memory (swap / core dump)
    /// after every add/extract/delete/move/rename/copy/change-password.
    fn drop(&mut self) {
        self.master_key.zeroize();
        self.mac_key.zeroize();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKindV3 {
    File,
    Directory,
}

fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn read_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().expect("slice length"))
}

fn write_u64(data: &mut [u8], offset: usize, value: u64) {
    data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

impl VaultHeaderV3 {
    fn to_bytes(&self) -> [u8; HEADER_SIZE] {
        let mut buf = [0u8; HEADER_SIZE];
        buf[0..10].copy_from_slice(MAGIC);
        buf[10] = VERSION;
        buf[11] = self.flags;
        buf[12..44].copy_from_slice(&self.salt);
        buf[44..84].copy_from_slice(&self.wrapped_master_key);
        buf[84..124].copy_from_slice(&self.wrapped_mac_key);
        buf[124..128].copy_from_slice(&(HEADER_SIZE as u32).to_le_bytes());
        write_u64(&mut buf, 128, self.data_offset);
        write_u64(&mut buf, 136, self.data_len);
        write_u64(&mut buf, 144, self.manifest_offset);
        write_u64(&mut buf, 152, self.manifest_len);
        write_u64(&mut buf, 160, self.extension_dir_offset);
        write_u64(&mut buf, 168, self.extension_dir_len);
        write_u64(&mut buf, 176, self.extension_payload_offset);
        write_u64(&mut buf, 184, self.extension_payload_len);
        buf[192..194].copy_from_slice(&self.wrapper_header_version.to_le_bytes());
        buf[HEADER_MAC_OFFSET..HEADER_MAC_OFFSET + MAC_SIZE].copy_from_slice(&self.header_mac);
        buf
    }

    fn from_bytes(data: &[u8]) -> Result<Self, String> {
        if data.len() < HEADER_SIZE {
            return Err("AeroVault v3 header is truncated".to_string());
        }
        if &data[0..10] != MAGIC {
            return Err("Not an AeroVault v3 file".to_string());
        }
        if data[10] != VERSION {
            return Err(format!("Unsupported AeroVault v3 version: {}", data[10]));
        }
        let header_len = u32::from_le_bytes(data[124..128].try_into().expect("slice length"));
        if header_len != HEADER_SIZE as u32 {
            return Err(format!("Invalid AeroVault v3 header length: {header_len}"));
        }
        if data[194..HEADER_MAC_OFFSET].iter().any(|b| *b != 0) {
            return Err("AeroVault v3 reserved header bytes are not zero".to_string());
        }

        let mut salt = [0u8; SALT_SIZE];
        salt.copy_from_slice(&data[12..44]);
        let mut wrapped_master_key = [0u8; WRAPPED_KEY_SIZE];
        wrapped_master_key.copy_from_slice(&data[44..84]);
        let mut wrapped_mac_key = [0u8; WRAPPED_KEY_SIZE];
        wrapped_mac_key.copy_from_slice(&data[84..124]);
        let mut header_mac = [0u8; MAC_SIZE];
        header_mac.copy_from_slice(&data[HEADER_MAC_OFFSET..HEADER_MAC_OFFSET + MAC_SIZE]);

        Ok(Self {
            flags: data[11],
            salt,
            wrapped_master_key,
            wrapped_mac_key,
            data_offset: read_u64(data, 128),
            data_len: read_u64(data, 136),
            manifest_offset: read_u64(data, 144),
            manifest_len: read_u64(data, 152),
            extension_dir_offset: read_u64(data, 160),
            extension_dir_len: read_u64(data, 168),
            extension_payload_offset: read_u64(data, 176),
            extension_payload_len: read_u64(data, 184),
            wrapper_header_version: u16::from_le_bytes(
                data[192..194].try_into().expect("slice length"),
            ),
            header_mac,
        })
    }

    fn compute_mac(&self, mac_key: &[u8; KEY_SIZE]) -> Result<[u8; MAC_SIZE], String> {
        let mut bytes = self.to_bytes();
        bytes[HEADER_MAC_OFFSET..HEADER_MAC_OFFSET + MAC_SIZE].fill(0);
        let mut hmac = <Hmac<Sha512> as Mac>::new_from_slice(mac_key)
            .map_err(|e| format!("HMAC init failed: {e}"))?;
        hmac.update(&bytes);
        let mut out = [0u8; MAC_SIZE];
        out.copy_from_slice(&hmac.finalize().into_bytes());
        Ok(out)
    }

    fn verify_mac(&self, mac_key: &[u8; KEY_SIZE]) -> Result<(), String> {
        let mut bytes = self.to_bytes();
        bytes[HEADER_MAC_OFFSET..HEADER_MAC_OFFSET + MAC_SIZE].fill(0);
        let mut hmac = <Hmac<Sha512> as Mac>::new_from_slice(mac_key)
            .map_err(|e| format!("HMAC init failed: {e}"))?;
        hmac.update(&bytes);
        hmac.verify_slice(&self.header_mac)
            .map_err(|_| "AeroVault v3 header MAC mismatch".to_string())
    }
}

fn random_array<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    rand::rngs::OsRng.fill_bytes(&mut out);
    out
}

fn derive_base_kek(password: &str, salt: &[u8; SALT_SIZE]) -> Result<[u8; KEY_SIZE], String> {
    let params = argon2::Params::new(128 * 1024, 4, 4, Some(KEY_SIZE))
        .map_err(|e| format!("Argon2 params: {e}"))?;
    let argon2 = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut key = [0u8; KEY_SIZE];
    argon2
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| format!("Argon2 derive: {e}"))?;
    Ok(key)
}

fn hkdf_expand<const N: usize>(ikm: &[u8], label: &[u8]) -> Result<[u8; N], String> {
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(None, ikm);
    let mut out = [0u8; N];
    hk.expand(label, &mut out)
        .map_err(|_| "HKDF expand failed".to_string())?;
    Ok(out)
}

fn derive_keks(base_kek: &[u8; KEY_SIZE]) -> Result<([u8; KEY_SIZE], [u8; KEY_SIZE]), String> {
    Ok((
        hkdf_expand::<KEY_SIZE>(base_kek, HKDF_MASTER)?,
        hkdf_expand::<KEY_SIZE>(base_kek, HKDF_MAC)?,
    ))
}

fn wrap_key(kek: &[u8; KEY_SIZE], key: &[u8; KEY_SIZE]) -> Result<[u8; WRAPPED_KEY_SIZE], String> {
    let kek = Kek::from(*kek);
    let mut out = [0u8; WRAPPED_KEY_SIZE];
    kek.wrap(key, &mut out)
        .map_err(|_| "AES-KW wrap failed".to_string())?;
    Ok(out)
}

fn unwrap_key(
    kek: &[u8; KEY_SIZE],
    wrapped: &[u8; WRAPPED_KEY_SIZE],
) -> Result<[u8; KEY_SIZE], String> {
    let kek = Kek::from(*kek);
    let mut out = [0u8; KEY_SIZE];
    kek.unwrap(wrapped, &mut out)
        .map_err(|_| "AES-KW unwrap failed".to_string())?;
    Ok(out)
}

fn default_wrappers(level: i32) -> WrapperManifest {
    WrapperManifest {
        packing: AlgorithmSpec {
            algorithm_id: "small-file-batching".to_string(),
            algorithm_version: 1,
            level: None,
            bounds: None,
        },
        chunking: AlgorithmSpec {
            algorithm_id: "gear-cdc".to_string(),
            algorithm_version: 1,
            level: None,
            bounds: Some(CdcBounds::for_level(level)),
        },
        chunk_id: AlgorithmSpec {
            algorithm_id: "blake3-keyed-128".to_string(),
            algorithm_version: 1,
            level: None,
            bounds: None,
        },
        compression: AlgorithmSpec {
            algorithm_id: "zstd".to_string(),
            algorithm_version: 1,
            level: Some(level),
            bounds: None,
        },
        crypt: AlgorithmSpec {
            algorithm_id: "aes-256-gcm-siv".to_string(),
            algorithm_version: 1,
            level: None,
            bounds: None,
        },
        cipher_hash: AlgorithmSpec {
            algorithm_id: "blake3-256".to_string(),
            algorithm_version: 1,
            level: None,
            bounds: None,
        },
    }
}

/// Effective CDC bounds for a manifest: the recorded `chunking.bounds` if
/// present and valid, otherwise the const defaults (pre-GAP-5 vaults).
fn manifest_cdc_bounds(manifest: &VaultManifestV3) -> Result<CdcBounds, String> {
    match manifest.wrappers.chunking.bounds {
        Some(b) => {
            b.validate()?;
            Ok(b)
        }
        None => Ok(CdcBounds::defaults()),
    }
}

fn empty_manifest(level: i32) -> VaultManifestV3 {
    let now = now_iso();
    VaultManifestV3 {
        format: VERSION,
        created: now.clone(),
        modified: now,
        wrappers: default_wrappers(level),
        entries: Vec::new(),
        chunks: BTreeMap::new(),
    }
}

/// Returns the stub ExtensionEntry for the ECC layer (length 0 payload for Phase 1 stub).
/// The actual payload (Reed-Solomon shards) will be written in Phase 2.
/// Marked non-critical so v3 readers can still extract.
fn ecc_stub_extension() -> ExtensionEntryV3 {
    ExtensionEntryV3 {
        extension_id: ECC_EXTENSION_ID.to_string(),
        algorithm_id: ECC_ALGORITHM_ID.to_string(),
        algorithm_version: ECC_ALGORITHM_VERSION,
        critical: false,
        offset: 0, // will be overwritten by build_file_bytes when placed after manifest
        length: 0,
    }
}

fn manifest_zstd_level(manifest: &VaultManifestV3) -> i32 {
    manifest
        .wrappers
        .compression
        .level
        .unwrap_or(DEFAULT_ZSTD_LEVEL)
}

fn encrypt_with_aad(key: &[u8; KEY_SIZE], plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, String> {
    let cipher = Aes256GcmSiv::new_from_slice(key).map_err(|e| format!("AES-GCM-SIV init: {e}"))?;
    let nonce_bytes = random_array::<NONCE_SIZE>();
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| "AES-GCM-SIV encrypt failed".to_string())?;
    let mut out = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

fn decrypt_with_aad(key: &[u8; KEY_SIZE], encrypted: &[u8], aad: &[u8]) -> Result<Vec<u8>, String> {
    if encrypted.len() < NONCE_SIZE + 16 {
        return Err("AES-GCM-SIV payload is too short".to_string());
    }
    let cipher = Aes256GcmSiv::new_from_slice(key).map_err(|e| format!("AES-GCM-SIV init: {e}"))?;
    let nonce = Nonce::from_slice(&encrypted[..NONCE_SIZE]);
    cipher
        .decrypt(
            nonce,
            Payload {
                msg: &encrypted[NONCE_SIZE..],
                aad,
            },
        )
        .map_err(|_| "AES-GCM-SIV decrypt failed".to_string())
}

fn block_aad(block_index: u64, chunk_id: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(BLOCK_AAD_PREFIX.len() + 8 + chunk_id.len());
    aad.extend_from_slice(BLOCK_AAD_PREFIX);
    aad.extend_from_slice(&block_index.to_le_bytes());
    aad.extend_from_slice(chunk_id.as_bytes());
    aad
}

fn encrypt_manifest(key: &[u8; KEY_SIZE], manifest: &VaultManifestV3) -> Result<Vec<u8>, String> {
    let json = serde_json::to_vec(manifest).map_err(|e| format!("Manifest serialize: {e}"))?;
    encrypt_with_aad(key, &json, MANIFEST_AAD)
}

fn decrypt_manifest(key: &[u8; KEY_SIZE], encrypted: &[u8]) -> Result<VaultManifestV3, String> {
    let json = decrypt_with_aad(key, encrypted, MANIFEST_AAD)?;
    serde_json::from_slice(&json).map_err(|e| format!("Manifest parse: {e}"))
}

fn keyed_chunk_id(key: &[u8; KEY_SIZE], plaintext: &[u8]) -> String {
    let hash = blake3::keyed_hash(key, plaintext);
    hex::encode(&hash.as_bytes()[..16])
}

fn gear_table() -> [u64; 256] {
    let mut table = [0u64; 256];
    for (idx, slot) in table.iter_mut().enumerate() {
        let mut input = b"AeroVault v3 gear-cdc table".to_vec();
        input.push(idx as u8);
        let hash = blake3::hash(&input);
        *slot = u64::from_le_bytes(hash.as_bytes()[0..8].try_into().expect("slice length"));
    }
    table
}

fn chunk_ranges_with(data: &[u8], bounds: &CdcBounds) -> Vec<(usize, usize)> {
    if data.is_empty() {
        return Vec::new();
    }
    if data.len() <= bounds.min {
        return vec![(0, data.len())];
    }

    let table = gear_table();
    let mask = (bounds.avg as u64) - 1;
    let mut ranges = Vec::new();
    let mut start = 0usize;
    let mut rolling = 0u64;

    for (idx, byte) in data.iter().enumerate() {
        rolling = rolling.rotate_left(1).wrapping_add(table[*byte as usize]);
        let len = idx + 1 - start;
        if len >= bounds.min && ((rolling & mask) == 0 || len >= bounds.max) {
            ranges.push((start, idx + 1));
            start = idx + 1;
            rolling = 0;
        }
    }
    if start < data.len() {
        ranges.push((start, data.len()));
    }
    ranges
}

fn validate_vault_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.contains('\0')
        || path.contains('\\')
        || path.split('/').any(|part| part == "..")
        || path.as_bytes().get(1) == Some(&b':')
    {
        return Err(format!("Invalid AeroVault path: {path}"));
    }
    Ok(())
}

fn safe_entry_name(path: &Path) -> Result<String, String> {
    let name = path
        .file_name()
        .ok_or_else(|| format!("Invalid file name: {}", path.display()))?
        .to_string_lossy()
        .to_string();
    validate_vault_path(&name)?;
    Ok(name)
}

fn normalize_vault_relative_path(path: &str) -> Result<String, String> {
    let trimmed = path.trim().trim_matches('/');
    if trimmed.is_empty() {
        return Err("Invalid AeroVault path: empty".to_string());
    }
    validate_vault_path(trimmed)?;
    if trimmed
        .split('/')
        .any(|part| part.is_empty() || part == ".")
    {
        return Err(format!("Invalid AeroVault path: {trimmed}"));
    }
    Ok(trimmed.to_string())
}

fn normalize_leaf_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains("..")
        || trimmed.contains('\0')
    {
        return Err("Invalid AeroVault name".to_string());
    }
    Ok(trimmed.to_string())
}

fn validate_manifest_paths(manifest: &VaultManifestV3) -> Result<(), String> {
    let mut seen = HashSet::new();
    for entry in &manifest.entries {
        let normalized = normalize_vault_relative_path(&entry.path)?;
        if normalized != entry.path {
            return Err(format!(
                "Invalid non-canonical AeroVault path: {}",
                entry.path
            ));
        }
        if !seen.insert(entry.path.as_str()) {
            return Err(format!(
                "Duplicate AeroVault path in manifest: {}",
                entry.path
            ));
        }
    }
    Ok(())
}

fn join_vault_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

fn path_parent(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(parent, _)| parent)
}

fn path_basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn is_descendant_of(path: &str, parent: &str) -> bool {
    path.len() > parent.len()
        && path.starts_with(parent)
        && path.as_bytes().get(parent.len()) == Some(&b'/')
}

fn entry_kind(manifest: &VaultManifestV3, path: &str) -> Option<EntryKindV3> {
    if let Some(entry) = manifest.entries.iter().find(|entry| entry.path == path) {
        return Some(if entry.is_dir {
            EntryKindV3::Directory
        } else {
            EntryKindV3::File
        });
    }
    if manifest
        .entries
        .iter()
        .any(|entry| is_descendant_of(&entry.path, path))
    {
        return Some(EntryKindV3::Directory);
    }
    None
}

fn ensure_no_file_ancestor(manifest: &VaultManifestV3, path: &str) -> Result<(), String> {
    let mut current = path;
    while let Some(parent) = path_parent(current) {
        if manifest
            .entries
            .iter()
            .any(|entry| entry.path == parent && !entry.is_dir)
        {
            return Err(format!("Parent path is a file: {parent}"));
        }
        current = parent;
    }
    Ok(())
}

fn sort_entries(manifest: &mut VaultManifestV3) {
    manifest.entries.sort_by(|a, b| a.path.cmp(&b.path));
}

fn create_directory_in_manifest(
    manifest: &mut VaultManifestV3,
    dir_path: &str,
) -> Result<bool, String> {
    let dir_path = normalize_vault_relative_path(dir_path)?;
    ensure_no_file_ancestor(manifest, &dir_path)?;

    if let Some(existing) = manifest.entries.iter().find(|entry| entry.path == dir_path) {
        return if existing.is_dir {
            Ok(false)
        } else {
            Err(format!("A file already exists at: {dir_path}"))
        };
    }

    if let Some(parent) = path_parent(&dir_path) {
        create_directory_in_manifest(manifest, parent)?;
    }

    manifest.entries.push(ManifestEntryV3 {
        path: dir_path,
        size: 0,
        modified: now_iso(),
        is_dir: true,
        chunks: Vec::new(),
        pack_offset: None,
    });
    sort_entries(manifest);
    manifest.modified = now_iso();
    Ok(true)
}

fn ensure_parent_directories(manifest: &mut VaultManifestV3, path: &str) -> Result<(), String> {
    if let Some(parent) = path_parent(path) {
        create_directory_in_manifest(manifest, parent)?;
    }
    Ok(())
}

fn next_block_index(manifest: &VaultManifestV3) -> u64 {
    manifest
        .chunks
        .values()
        .map(|record| record.block_index)
        .max()
        .map(|max| max + 1)
        .unwrap_or(0)
}

/// Compress + encrypt + dedup of one already-delimited plaintext chunk.
/// Returns the chunk id. Shared by the per-file path and the pack path so the
/// wrapper chain stays single-sourced.
fn ingest_chunk(
    vault: &mut OpenVaultV3,
    chunk: &[u8],
    chunk_key: &[u8; KEY_SIZE],
    level: i32,
) -> Result<String, String> {
    let chunk_id = keyed_chunk_id(chunk_key, chunk);
    if !vault.manifest.chunks.contains_key(&chunk_id) {
        let compressed = zstd::stream::encode_all(chunk, level)
            .map_err(|e| format!("zstd compress failed: {e}"))?;
        let block_index = next_block_index(&vault.manifest);
        let aad = block_aad(block_index, &chunk_id);
        let encrypted = encrypt_with_aad(&vault.master_key, &compressed, &aad)?;
        let cipher_hash = blake3::hash(&encrypted).to_hex().to_string();
        let data_offset = vault.data.len() as u64;
        vault
            .data
            .extend_from_slice(&(encrypted.len() as u64).to_le_bytes());
        vault.data.extend_from_slice(&encrypted);
        let (pt, cz, enc) = (
            chunk.len() as u64,
            compressed.len() as u64,
            encrypted.len() as u64,
        );
        vault.manifest.chunks.insert(
            chunk_id.clone(),
            ChunkRecordV3 {
                id: chunk_id.clone(),
                block_index,
                data_offset,
                block_len: enc,
                plaintext_len: pt,
                compressed_len: cz,
                cipher_hash,
            },
        );
        vault.report.on_chunk(true, pt, cz, enc);
    } else {
        vault.report.on_chunk(false, chunk.len() as u64, 0, 0);
    }
    Ok(chunk_id)
}

fn append_file_at(vault: &mut OpenVaultV3, source: &Path, entry_path: &str) -> Result<(), String> {
    let entry_path = normalize_vault_relative_path(entry_path)?;
    if !source.is_file() {
        return Err(format!("Not a regular file: {}", source.display()));
    }
    ensure_parent_directories(&mut vault.manifest, &entry_path)?;

    if let Some(kind) = entry_kind(&vault.manifest, &entry_path) {
        match kind {
            EntryKindV3::Directory => {
                return Err(format!(
                    "Destination already exists as directory: {entry_path}"
                ));
            }
            EntryKindV3::File => {
                vault
                    .manifest
                    .entries
                    .retain(|entry| entry.path != entry_path);
            }
        }
    }

    let mut plaintext =
        std::fs::read(source).map_err(|e| format!("Read {}: {e}", source.display()))?;
    let size = plaintext.len() as u64;
    let chunk_key = hkdf_expand::<KEY_SIZE>(&vault.master_key, HKDF_CHUNK_ID)?;
    let level = manifest_zstd_level(&vault.manifest);
    let bounds = manifest_cdc_bounds(&vault.manifest)?;
    let mut entry_chunks = Vec::new();

    let ranges = chunk_ranges_with(&plaintext, &bounds);
    for (start, end) in ranges {
        let chunk_id = ingest_chunk(vault, &plaintext[start..end], &chunk_key, level)?;
        entry_chunks.push(chunk_id);
    }
    plaintext.zeroize();

    vault.manifest.entries.push(ManifestEntryV3 {
        path: entry_path,
        size,
        modified: now_iso(),
        is_dir: false,
        chunks: entry_chunks,
        pack_offset: None,
    });
    vault.report.on_file(false);
    sort_entries(&mut vault.manifest);
    vault.manifest.modified = now_iso();
    Ok(())
}

/// Chunk one assembled pack, ingest its chunks, then map every member file to
/// the chunks that cover its byte span plus the offset of its first byte inside
/// the first covering chunk. The manifest is the index: the pack itself carries
/// no per-file framing.
fn flush_pack(
    vault: &mut OpenVaultV3,
    pack: &[u8],
    members: &[(String, u64, u64)],
    chunk_key: &[u8; KEY_SIZE],
    level: i32,
    bounds: &CdcBounds,
) -> Result<(), String> {
    if members.is_empty() {
        return Ok(());
    }
    vault.report.on_pack();

    let ranges = chunk_ranges_with(pack, bounds);
    let mut chunks: Vec<(String, u64, u64)> = Vec::with_capacity(ranges.len());
    for (start, end) in &ranges {
        let id = ingest_chunk(vault, &pack[*start..*end], chunk_key, level)?;
        chunks.push((id, *start as u64, *end as u64));
    }
    vault.report.step(format!(
        "pack: {} file(s), {} B -> chunk+compress+encrypt {} chunk(s)",
        members.len(),
        pack.len(),
        chunks.len()
    ));

    for (entry_path, fstart, flen) in members {
        let fstart_v = *fstart;
        let flen_v = *flen;
        let fend = fstart_v + flen_v;

        ensure_parent_directories(&mut vault.manifest, entry_path)?;
        if let Some(kind) = entry_kind(&vault.manifest, entry_path) {
            match kind {
                EntryKindV3::Directory => {
                    return Err(format!(
                        "Destination already exists as directory: {entry_path}"
                    ));
                }
                EntryKindV3::File => {
                    vault.manifest.entries.retain(|e| &e.path != entry_path);
                }
            }
        }

        let (covering, pack_offset) = if flen_v == 0 {
            (Vec::new(), Some(0u64))
        } else {
            let mut cov = Vec::new();
            let mut first: Option<u64> = None;
            for (id, cstart, cend) in &chunks {
                if *cstart < fend && fstart_v < *cend {
                    if first.is_none() {
                        first = Some(*cstart);
                    }
                    cov.push(id.clone());
                }
            }
            let fc = first.ok_or_else(|| format!("Packing failed to cover file: {entry_path}"))?;
            (cov, Some(fstart_v - fc))
        };

        vault.manifest.entries.push(ManifestEntryV3 {
            path: entry_path.clone(),
            size: flen_v,
            modified: now_iso(),
            is_dir: false,
            chunks: covering,
            pack_offset,
        });
        vault.report.on_file(true);
    }
    Ok(())
}

/// Add a set of sources, batching sub-threshold files into shared packs before
/// chunking and routing large files through the per-file path. The deterministic
/// path ordering keeps packs (and therefore dedup) stable across identical adds.
fn append_sources_batched(
    vault: &mut OpenVaultV3,
    sources: &[(PathBuf, String)],
) -> Result<(), String> {
    let chunk_key = hkdf_expand::<KEY_SIZE>(&vault.master_key, HKDF_CHUNK_ID)?;
    let level = manifest_zstd_level(&vault.manifest);
    let bounds = manifest_cdc_bounds(&vault.manifest)?;
    vault.report.set_cdc(bounds.min, bounds.avg, bounds.max);
    vault
        .report
        .step(format!("scan: {} source(s) to add", sources.len()));

    let mut small_meta: Vec<(PathBuf, String)> = Vec::new();
    let mut large_count = 0usize;
    for (source, entry_path) in sources {
        let entry_path = normalize_vault_relative_path(entry_path)?;
        if !source.is_file() {
            return Err(format!("Not a regular file: {}", source.display()));
        }
        let len = std::fs::metadata(source)
            .map_err(|e| format!("Stat {}: {e}", source.display()))?
            .len();
        if (len as usize) < PACK_SMALL_FILE_THRESHOLD {
            small_meta.push((source.clone(), entry_path));
        } else {
            large_count += 1;
            append_file_at(vault, source, &entry_path)?;
        }
    }
    vault.report.step(format!(
        "partition: {} small (< {} B, batched) / {} large (per-file)",
        small_meta.len(),
        PACK_SMALL_FILE_THRESHOLD,
        large_count
    ));

    if !small_meta.is_empty() {
        small_meta.sort_by(|a, b| a.1.cmp(&b.1));

        let mut pack: Vec<u8> = Vec::new();
        let mut members: Vec<(String, u64, u64)> = Vec::new();
        for (source, entry_path) in &small_meta {
            let mut data =
                std::fs::read(source).map_err(|e| format!("Read {}: {e}", source.display()))?;
            let start = pack.len() as u64;
            pack.extend_from_slice(&data);
            let len = data.len() as u64;
            data.zeroize();
            members.push((entry_path.clone(), start, len));
            if pack.len() >= PACK_TARGET {
                flush_pack(vault, &pack, &members, &chunk_key, level, &bounds)?;
                pack.zeroize();
                pack.clear();
                members.clear();
            }
        }
        if !members.is_empty() {
            flush_pack(vault, &pack, &members, &chunk_key, level, &bounds)?;
            pack.zeroize();
        }
    }

    sort_entries(&mut vault.manifest);
    vault.manifest.modified = now_iso();
    Ok(())
}

fn compact_live_chunks(vault: &mut OpenVaultV3) -> Result<(), String> {
    let live_chunk_ids: HashSet<String> = vault
        .manifest
        .entries
        .iter()
        .flat_map(|entry| entry.chunks.iter().cloned())
        .collect();

    if live_chunk_ids.is_empty() {
        vault.manifest.chunks.clear();
        vault.data.clear();
        return Ok(());
    }

    let mut ordered_ids: Vec<(u64, String)> = vault
        .manifest
        .chunks
        .iter()
        .filter(|(id, _)| live_chunk_ids.contains(*id))
        .map(|(id, record)| (record.block_index, id.clone()))
        .collect();
    ordered_ids.sort_by_key(|(index, _)| *index);

    let mut new_data = Vec::new();
    let mut new_chunks = BTreeMap::new();

    for (_, chunk_id) in ordered_ids {
        let mut record = vault
            .manifest
            .chunks
            .get(&chunk_id)
            .cloned()
            .ok_or_else(|| format!("Missing chunk record: {chunk_id}"))?;
        let len_start = record.data_offset as usize;
        let len_end = len_start
            .checked_add(8)
            .ok_or_else(|| "Chunk length offset overflow".to_string())?;
        if len_end > vault.data.len() {
            return Err("Chunk length is outside data section".to_string());
        }
        let block_len = u64::from_le_bytes(
            vault.data[len_start..len_end]
                .try_into()
                .expect("slice length"),
        );
        if block_len != record.block_len || block_len > MAX_BLOCK_SIZE {
            return Err("Chunk length metadata mismatch".to_string());
        }
        let block_start = len_end;
        let block_end = block_start
            .checked_add(block_len as usize)
            .ok_or_else(|| "Chunk block offset overflow".to_string())?;
        if block_end > vault.data.len() {
            return Err("Chunk block is outside data section".to_string());
        }

        record.data_offset = new_data.len() as u64;
        new_data.extend_from_slice(&block_len.to_le_bytes());
        new_data.extend_from_slice(&vault.data[block_start..block_end]);
        new_chunks.insert(chunk_id, record);
    }

    vault.data = new_data;
    vault.manifest.chunks = new_chunks;
    Ok(())
}

fn delete_entries_from_manifest(
    vault: &mut OpenVaultV3,
    entry_names: &[String],
    recursive: bool,
) -> Result<usize, String> {
    let mut removed = 0usize;

    for entry_name in entry_names {
        let entry_name = normalize_vault_relative_path(entry_name)?;
        let kind = entry_kind(&vault.manifest, &entry_name)
            .ok_or_else(|| format!("Entry not found: {entry_name}"))?;

        match kind {
            EntryKindV3::File => {
                let before = vault.manifest.entries.len();
                vault
                    .manifest
                    .entries
                    .retain(|entry| entry.path != entry_name);
                removed += before.saturating_sub(vault.manifest.entries.len());
            }
            EntryKindV3::Directory => {
                let has_children = vault
                    .manifest
                    .entries
                    .iter()
                    .any(|entry| is_descendant_of(&entry.path, &entry_name));
                if has_children && !recursive {
                    return Err(format!("Directory is not empty: {entry_name}"));
                }
                let before = vault.manifest.entries.len();
                vault.manifest.entries.retain(|entry| {
                    entry.path != entry_name && !is_descendant_of(&entry.path, &entry_name)
                });
                removed += before.saturating_sub(vault.manifest.entries.len());
            }
        }
    }

    if removed > 0 {
        compact_live_chunks(vault)?;
        sort_entries(&mut vault.manifest);
        vault.manifest.modified = now_iso();
    }

    Ok(removed)
}

fn remap_entry_path(path: &str, from: &str, to: &str) -> String {
    if path == from {
        to.to_string()
    } else {
        format!("{}/{}", to, &path[from.len() + 1..])
    }
}

fn prepare_relocation(
    manifest: &VaultManifestV3,
    from: &str,
    to: &str,
) -> Result<EntryKindV3, String> {
    let from = normalize_vault_relative_path(from)?;
    let to = normalize_vault_relative_path(to)?;
    let kind = entry_kind(manifest, &from).ok_or_else(|| format!("Entry not found: {from}"))?;

    if from == to {
        return Ok(kind);
    }
    if kind == EntryKindV3::Directory && is_descendant_of(&to, &from) {
        return Err("Cannot move a directory inside itself".to_string());
    }
    if entry_kind(manifest, &to).is_some() {
        return Err(format!("Destination already exists: {to}"));
    }
    ensure_no_file_ancestor(manifest, &to)?;
    Ok(kind)
}

fn move_entry_in_manifest(vault: &mut OpenVaultV3, from: &str, to: &str) -> Result<(), String> {
    let from = normalize_vault_relative_path(from)?;
    let to = normalize_vault_relative_path(to)?;
    let _ = prepare_relocation(&vault.manifest, &from, &to)?;
    if from == to {
        return Ok(());
    }
    ensure_parent_directories(&mut vault.manifest, &to)?;
    for entry in &mut vault.manifest.entries {
        if entry.path == from || is_descendant_of(&entry.path, &from) {
            entry.path = remap_entry_path(&entry.path, &from, &to);
            entry.modified = now_iso();
        }
    }
    sort_entries(&mut vault.manifest);
    vault.manifest.modified = now_iso();
    Ok(())
}

fn copy_entry_in_manifest(vault: &mut OpenVaultV3, from: &str, to: &str) -> Result<(), String> {
    let from = normalize_vault_relative_path(from)?;
    let to = normalize_vault_relative_path(to)?;
    let _ = prepare_relocation(&vault.manifest, &from, &to)?;
    if from == to {
        return Ok(());
    }
    ensure_parent_directories(&mut vault.manifest, &to)?;
    let clones: Vec<ManifestEntryV3> = vault
        .manifest
        .entries
        .iter()
        .filter(|entry| entry.path == from || is_descendant_of(&entry.path, &from))
        .cloned()
        .map(|mut entry| {
            entry.path = remap_entry_path(&entry.path, &from, &to);
            entry.modified = now_iso();
            entry
        })
        .collect();
    if clones.is_empty() {
        return Err(format!("Entry not found: {from}"));
    }
    vault.manifest.entries.extend(clones);
    sort_entries(&mut vault.manifest);
    vault.manifest.modified = now_iso();
    Ok(())
}

fn change_password_in_place(vault: &mut OpenVaultV3, new_password: &str) -> Result<(), String> {
    if new_password.len() < MIN_PASSWORD_LEN {
        return Err("Password must be at least 8 characters".to_string());
    }
    let salt = random_array::<SALT_SIZE>();
    let mut base_kek = derive_base_kek(new_password, &salt)?;
    let (kek_master, kek_mac) = derive_keks(&base_kek)?;
    base_kek.zeroize();
    vault.header.salt = salt;
    vault.header.wrapped_master_key = wrap_key(&kek_master, &vault.master_key)?;
    vault.header.wrapped_mac_key = wrap_key(&kek_mac, &vault.mac_key)?;
    vault.manifest.modified = now_iso();
    Ok(())
}

fn extract_file_entry(
    vault: &OpenVaultV3,
    entry: &ManifestEntryV3,
    output_path: &Path,
) -> Result<PathBuf, String> {
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Create output dir: {e}"))?;
    }

    // We only ever slice `out[offset..offset+size]`, so decoding never needs to
    // grow `out` past that bound. Tracking it lets us stop early and refuse a
    // manifest that repeats the same chunk id to amplify memory use far beyond
    // the entry's real extent (CLAUDE-AV-005).
    let offset = entry.pack_offset.unwrap_or(0) as usize;
    let size = entry.size as usize;
    let end = offset
        .checked_add(size)
        .ok_or_else(|| "Entry slice range overflow".to_string())?;

    // The largest plaintext a single block may legitimately hold is this vault's
    // recorded chunking `max` (or the default), clamped to the format ceiling.
    let max_block_plaintext = vault
        .manifest
        .wrappers
        .chunking
        .bounds
        .map(|b| b.max as u64)
        .unwrap_or(CDC_MAX as u64)
        .min(MAX_PLAINTEXT_BLOCK_SIZE);

    let mut out = Vec::with_capacity(end.min(32 * 1024 * 1024));
    for chunk_id in &entry.chunks {
        if out.len() >= end {
            // Everything this entry slices is already decoded; ignore the rest
            // (a hostile pack may list extra/duplicate chunks past this point).
            break;
        }
        let record = vault
            .manifest
            .chunks
            .get(chunk_id)
            .ok_or_else(|| format!("Missing chunk record: {chunk_id}"))?;
        let len_start = record.data_offset as usize;
        let len_end = len_start
            .checked_add(8)
            .ok_or_else(|| "Chunk length offset overflow".to_string())?;
        if len_end > vault.data.len() {
            return Err("Chunk length is outside data section".to_string());
        }
        let block_len = u64::from_le_bytes(
            vault.data[len_start..len_end]
                .try_into()
                .expect("slice length"),
        );
        if block_len != record.block_len || block_len > MAX_BLOCK_SIZE {
            return Err("Chunk length metadata mismatch".to_string());
        }
        // Reject an over-declared plaintext length before decompressing so a
        // single block cannot expand to gigabytes (CLAUDE-AV-005).
        if record.plaintext_len > max_block_plaintext {
            return Err(format!(
                "Plaintext block too large for chunk {chunk_id}: {} bytes (max {max_block_plaintext})",
                record.plaintext_len
            ));
        }
        let block_start = len_end;
        let block_end = block_start
            .checked_add(block_len as usize)
            .ok_or_else(|| "Chunk block offset overflow".to_string())?;
        if block_end > vault.data.len() {
            return Err("Chunk block is outside data section".to_string());
        }
        let encrypted = &vault.data[block_start..block_end];
        let actual_hash = blake3::hash(encrypted).to_hex().to_string();
        if actual_hash != record.cipher_hash {
            return Err(format!("Cipher block hash mismatch for chunk {chunk_id}"));
        }
        let aad = block_aad(record.block_index, chunk_id);
        let mut compressed = decrypt_with_aad(&vault.master_key, encrypted, &aad)?;
        // Bound the decompressor output to `plaintext_len + 1`: with the cap
        // above this is at most one chunk (4 MiB), so a zstd bomb cannot
        // materialise more than that before the length mismatch is detected.
        let mut decoder = zstd::stream::read::Decoder::new(&compressed[..])
            .map_err(|e| format!("zstd decompress init failed: {e}"))?;
        let mut plaintext = Vec::with_capacity(record.plaintext_len as usize);
        decoder
            .by_ref()
            .take(record.plaintext_len + 1)
            .read_to_end(&mut plaintext)
            .map_err(|e| format!("zstd decompress failed: {e}"))?;
        compressed.zeroize();
        if plaintext.len() as u64 != record.plaintext_len {
            plaintext.zeroize();
            return Err(format!("Plaintext length mismatch for chunk {chunk_id}"));
        }
        out.extend_from_slice(&plaintext);
        plaintext.zeroize();
    }
    if end > out.len() {
        return Err(format!(
            "Entry slice [{offset}..{end}] exceeds decoded data ({})",
            out.len()
        ));
    }
    let mut sliced = out[offset..end].to_vec();
    out.zeroize();
    atomic_write(output_path, &sliced)?;
    sliced.zeroize();
    Ok(output_path.to_path_buf())
}

fn read_capped(
    file: &mut std::fs::File,
    offset: u64,
    len: u64,
    cap: u64,
    label: &str,
) -> Result<Vec<u8>, String> {
    if len > cap {
        return Err(format!("{label} too large: {len} bytes"));
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| format!("Seek {label}: {e}"))?;
    let mut buf = vec![0u8; len as usize];
    file.read_exact(&mut buf)
        .map_err(|e| format!("Read {label}: {e}"))?;
    Ok(buf)
}

fn atomic_write(target: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| format!("Create parent dir: {e}"))?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".aerovault-v3-")
        .tempfile_in(parent)
        .map_err(|e| format!("Create temp file: {e}"))?;
    tmp.write_all(bytes)
        .map_err(|e| format!("Write temp file: {e}"))?;
    tmp.as_file_mut()
        .sync_all()
        .map_err(|e| format!("Sync temp file: {e}"))?;
    tmp.persist(target)
        .map_err(|e| format!("Persist vault: {}", e.error))?;
    #[cfg(unix)]
    {
        if let Some(parent) = target.parent() {
            if let Ok(dir) = std::fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }
    }
    Ok(())
}

fn build_file_bytes(
    mut header: VaultHeaderV3,
    mac_key: &[u8; KEY_SIZE],
    master_key: &[u8; KEY_SIZE],
    manifest: &VaultManifestV3,
    extensions: &[ExtensionEntryV3],
    extension_payloads: &[u8],  // content of the payload area; entries' offset/len are relative to this
    data: &[u8],
) -> Result<Vec<u8>, String> {
    let encrypted_manifest = encrypt_manifest(master_key, manifest)?;
    let extension_dir =
        serde_json::to_vec(extensions).map_err(|e| format!("Extension serialize: {e}"))?;

    header.data_offset = DATA_OFFSET;
    header.data_len = data.len() as u64;
    header.manifest_offset = DATA_OFFSET + header.data_len;
    header.manifest_len = encrypted_manifest.len() as u64;
    header.extension_dir_offset = header.manifest_offset + header.manifest_len;
    header.extension_dir_len = extension_dir.len() as u64;
    header.extension_payload_offset = header.extension_dir_offset + header.extension_dir_len;
    header.extension_payload_len = extension_payloads.len() as u64;
    header.header_mac = [0u8; MAC_SIZE];
    header.header_mac = header.compute_mac(mac_key)?;

    let mut out = Vec::with_capacity(
        HEADER_SIZE + data.len() + encrypted_manifest.len() + extension_dir.len() + extension_payloads.len(),
    );
    out.extend_from_slice(&header.to_bytes());
    out.extend_from_slice(data);
    out.extend_from_slice(&encrypted_manifest);
    out.extend_from_slice(&extension_dir);
    out.extend_from_slice(extension_payloads);
    Ok(out)
}

fn create_empty_vault(path: &Path, password: &str, level: i32, with_ecc: bool) -> Result<(), String> {
    if password.len() < MIN_PASSWORD_LEN {
        return Err("Password must be at least 8 characters".to_string());
    }

    let salt = random_array::<SALT_SIZE>();
    let mut base_kek = derive_base_kek(password, &salt)?;
    let (kek_master, kek_mac) = derive_keks(&base_kek)?;
    base_kek.zeroize();

    let mut master_key = random_array::<KEY_SIZE>();
    let mut mac_key = random_array::<KEY_SIZE>();
    let wrapped_master_key = wrap_key(&kek_master, &master_key)?;
    let wrapped_mac_key = wrap_key(&kek_mac, &mac_key)?;

    let header = VaultHeaderV3 {
        flags: 0,
        salt,
        wrapped_master_key,
        wrapped_mac_key,
        data_offset: DATA_OFFSET,
        data_len: 0,
        manifest_offset: DATA_OFFSET,
        manifest_len: 0,
        extension_dir_offset: DATA_OFFSET,
        extension_dir_len: 0,
        extension_payload_offset: DATA_OFFSET,
        extension_payload_len: 0,
        wrapper_header_version: 1,
        header_mac: [0u8; MAC_SIZE],
    };

    let manifest = empty_manifest(level);
    let mut extensions = if with_ecc {
        vec![ecc_stub_extension()]
    } else {
        vec![]
    };
    let ext_payloads = if with_ecc {
        let p = compute_ecc_shards(&[]);
        if let Some(e) = extensions.first_mut() {
            e.offset = 0;
            e.length = p.len() as u64;
        }
        p
    } else {
        vec![]
    };
    let bytes = build_file_bytes(header, &mac_key, &master_key, &manifest, &extensions, &ext_payloads, &[])?;
    master_key.zeroize();
    mac_key.zeroize();
    atomic_write(path, &bytes)
}

/// Reject a manifest whose wrapper algorithms differ from the ones this build
/// hardcodes (AES-256-GCM-SIV / zstd / keyed-blake3 / gear-CDC). The fields are
/// authenticated, so this is not a downgrade today, but asserting them turns a
/// future version-confusion bug into a clean, fail-closed error rather than a
/// silent wrong-algorithm decode (CLAUDE-AV-024 / CODEX-AV-006).
fn check_wrapper(slot: &str, spec: &AlgorithmSpec, id: &str, ver: u32) -> Result<(), String> {
    if spec.algorithm_id != id || spec.algorithm_version != ver {
        return Err(format!(
            "Unsupported AeroVault v3 {slot} algorithm: {} v{} (expected {id} v{ver})",
            spec.algorithm_id, spec.algorithm_version
        ));
    }
    Ok(())
}

fn validate_supported_wrappers(w: &WrapperManifest) -> Result<(), String> {
    check_wrapper("packing", &w.packing, "small-file-batching", 1)?;
    check_wrapper("chunking", &w.chunking, "gear-cdc", 1)?;
    check_wrapper("chunk_id", &w.chunk_id, "blake3-keyed-128", 1)?;
    check_wrapper("compression", &w.compression, "zstd", 1)?;
    check_wrapper("crypt", &w.crypt, "aes-256-gcm-siv", 1)?;
    check_wrapper("cipher_hash", &w.cipher_hash, "blake3-256", 1)?;
    Ok(())
}

fn open_vault(path: impl Into<PathBuf>, password: &str) -> Result<OpenVaultV3, String> {
    let path = path.into();
    let mut file = std::fs::File::open(&path).map_err(|e| format!("Open vault: {e}"))?;
    let file_len = file
        .metadata()
        .map_err(|e| format!("Vault metadata: {e}"))?
        .len();
    let mut header_bytes = [0u8; HEADER_SIZE];
    file.read_exact(&mut header_bytes)
        .map_err(|e| format!("Read header: {e}"))?;
    let header = VaultHeaderV3::from_bytes(&header_bytes)?;

    let mut base_kek = derive_base_kek(password, &header.salt)?;
    let (kek_master, kek_mac) = derive_keks(&base_kek)?;
    base_kek.zeroize();
    let mac_key = unwrap_key(&kek_mac, &header.wrapped_mac_key)?;
    header.verify_mac(&mac_key)?;
    // Reject an unknown (authenticated) wrapper-header version instead of
    // decoding it with the hardcoded cipher stack (CLAUDE-AV-024 / CODEX-AV-006).
    if header.wrapper_header_version != SUPPORTED_WRAPPER_HEADER_VERSION {
        return Err(format!(
            "Unsupported AeroVault v3 wrapper-header version: {} (expected {})",
            header.wrapper_header_version, SUPPORTED_WRAPPER_HEADER_VERSION
        ));
    }
    let master_key = unwrap_key(&kek_master, &header.wrapped_master_key)?;

    validate_ranges(&header, file_len)?;

    let data = read_capped(
        &mut file,
        header.data_offset,
        header.data_len,
        // The data section is authenticated and validate_ranges has already
        // bounded it within the file; cap explicitly at file length instead of
        // u64::MAX so the eager read can never exceed the on-disk size
        // (CODEX-AV-006).
        file_len,
        "data section",
    )?;
    let encrypted_manifest = read_capped(
        &mut file,
        header.manifest_offset,
        header.manifest_len,
        MAX_MANIFEST_SIZE,
        "manifest",
    )?;
    let manifest = decrypt_manifest(&master_key, &encrypted_manifest)?;
    if manifest.format != VERSION {
        return Err(format!(
            "Unsupported AeroVault manifest version: {}",
            manifest.format
        ));
    }
    validate_supported_wrappers(&manifest.wrappers)?;
    validate_manifest_paths(&manifest)?;

    let extension_json = read_capped(
        &mut file,
        header.extension_dir_offset,
        header.extension_dir_len,
        MAX_EXTENSION_DIR_SIZE,
        "extension directory",
    )?;
    let extensions: Vec<ExtensionEntryV3> = serde_json::from_slice(&extension_json)
        .map_err(|e| format!("Extension directory parse: {e}"))?;

    let mut _has_ecc = false;
    for ext in &extensions {
        if ext.extension_id == ECC_EXTENSION_ID {
            _has_ecc = true;
            // For Phase 1 stub the payload length is 0.
            // In Phase 2+ we will validate/load the RS shards here when present.
        }
        if ext.critical {
            return Err(format!(
                "Unsupported critical AeroVault v3 extension: {}",
                ext.extension_id
            ));
        }
    }
    // `has_ecc` is recorded for future use (scrub/repair paths, info surfaces).
    // The extensions vec is already stored in OpenVaultV3 for round-tripping.

    Ok(OpenVaultV3 {
        path,
        opened_file_len: file_len,
        opened_header_mac: header.header_mac,
        header,
        master_key,
        mac_key,
        manifest,
        extensions,
        data,
        report: VaultReport::new("open", VERSION),
    })
}

fn validate_ranges(header: &VaultHeaderV3, file_len: u64) -> Result<(), String> {
    if header.data_offset != DATA_OFFSET {
        return Err("Invalid AeroVault v3 data offset".to_string());
    }
    let ranges = [
        (header.data_offset, header.data_len, "data"),
        (header.manifest_offset, header.manifest_len, "manifest"),
        (
            header.extension_dir_offset,
            header.extension_dir_len,
            "extension directory",
        ),
        (
            header.extension_payload_offset,
            header.extension_payload_len,
            "extension payload",
        ),
    ];
    for (offset, len, label) in ranges {
        let end = offset
            .checked_add(len)
            .ok_or_else(|| format!("{label} range overflows"))?;
        if end > file_len {
            return Err(format!("{label} range exceeds file size"));
        }
    }
    Ok(())
}

fn save_open_vault(vault: &OpenVaultV3) -> Result<(), String> {
    assert_vault_generation_current(vault)?;

    let mut extensions = vault.extensions.clone();
    let mut ext_payloads = vec![];

    // P2-05: if ECC extension is present, recompute the shards on the current data
    // and update the entry + payload. Recompute on every seal (cost is acceptable
    // for the ECC use case; most vaults won't have it enabled).
    if let Some(ecc_idx) = extensions.iter().position(|e| e.extension_id == ECC_EXTENSION_ID) {
        // Collect on-disk blocks in the order they appear in the data section
        // (sorted by data_offset). Each full block is [u64 len][ciphertext of that len].
        let mut chunk_records: Vec<_> = vault.manifest.chunks.values().cloned().collect();
        chunk_records.sort_by_key(|r| r.data_offset);

        let blocks: Vec<&[u8]> = chunk_records.iter().map(|rec| {
            let start = rec.data_offset as usize;
            let full_len = 8 + rec.block_len as usize;
            if start + full_len <= vault.data.len() {
                &vault.data[start..start + full_len]
            } else {
                &[] as &[u8]
            }
        }).collect();

        let payload = compute_ecc_shards(&blocks);

        let entry = &mut extensions[ecc_idx];
        entry.offset = 0;
        entry.length = payload.len() as u64;

        ext_payloads = payload;
    }

    let bytes = build_file_bytes(
        vault.header.clone(),
        &vault.mac_key,
        &vault.master_key,
        &vault.manifest,
        &extensions,
        &ext_payloads,
        &vault.data,
    )?;
    atomic_write(&vault.path, &bytes)
}

struct VaultWriteLock {
    path: PathBuf,
    _file: File,
}

impl Drop for VaultWriteLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn lock_path_for(vault_path: &Path) -> Result<PathBuf, String> {
    let parent = vault_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = vault_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("Invalid vault path: {}", vault_path.display()))?;
    Ok(parent.join(format!(".{name}.lock")))
}

fn acquire_vault_write_lock(vault_path: &Path) -> Result<VaultWriteLock, String> {
    let lock_path = lock_path_for(vault_path)?;
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Create lock dir: {e}"))?;
    }

    let started = Instant::now();
    loop {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(mut file) => {
                let _ = writeln!(
                    file,
                    "pid={} created_at={}",
                    std::process::id(),
                    chrono::Utc::now().to_rfc3339()
                );
                let _ = file.sync_all();
                return Ok(VaultWriteLock {
                    path: lock_path,
                    _file: file,
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if started.elapsed() > Duration::from_secs(30) {
                    return Err(format!(
                        "AeroVault v3 write lock is busy: {}",
                        lock_path.display()
                    ));
                }
                sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("Create vault write lock: {e}")),
        }
    }
}

fn assert_vault_generation_current(vault: &OpenVaultV3) -> Result<(), String> {
    let mut file = std::fs::File::open(&vault.path).map_err(|e| format!("Open vault: {e}"))?;
    let file_len = file
        .metadata()
        .map_err(|e| format!("Vault metadata: {e}"))?
        .len();
    let mut header_bytes = [0u8; HEADER_SIZE];
    file.read_exact(&mut header_bytes)
        .map_err(|e| format!("Read header: {e}"))?;
    let header = VaultHeaderV3::from_bytes(&header_bytes)?;
    if file_len != vault.opened_file_len || header.header_mac != vault.opened_header_mac {
        return Err("Vault changed while this write was in progress; retry operation".to_string());
    }
    Ok(())
}

fn extract_entry(
    vault: &OpenVaultV3,
    entry_name: &str,
    dest_path: &Path,
) -> Result<PathBuf, String> {
    let entry_name = normalize_vault_relative_path(entry_name)?;
    match entry_kind(&vault.manifest, &entry_name) {
        Some(EntryKindV3::File) => {
            let entry = vault
                .manifest
                .entries
                .iter()
                .find(|entry| entry.path == entry_name)
                .ok_or_else(|| format!("Entry not found: {entry_name}"))?;
            let output_path = if dest_path.is_dir() {
                dest_path.join(&entry.path)
            } else {
                dest_path.to_path_buf()
            };
            extract_file_entry(vault, entry, &output_path)
        }
        Some(EntryKindV3::Directory) => {
            let output_root = if dest_path.exists() {
                if !dest_path.is_dir() {
                    return Err(
                        "Destination for directory extraction must be a directory".to_string()
                    );
                }
                dest_path.join(path_basename(&entry_name))
            } else {
                dest_path.to_path_buf()
            };
            std::fs::create_dir_all(&output_root).map_err(|e| format!("Create output dir: {e}"))?;

            let prefix = format!("{entry_name}/");
            let mut descendants: Vec<&ManifestEntryV3> = vault
                .manifest
                .entries
                .iter()
                .filter(|entry| entry.path == entry_name || entry.path.starts_with(&prefix))
                .collect();
            descendants.sort_by(|a, b| a.path.cmp(&b.path));

            for entry in descendants {
                normalize_vault_relative_path(&entry.path)?;
                let rel = if entry.path == entry_name {
                    String::new()
                } else {
                    entry.path[entry_name.len() + 1..].to_string()
                };
                if !rel.is_empty() {
                    normalize_vault_relative_path(&rel)?;
                }
                let child_output = if rel.is_empty() {
                    output_root.clone()
                } else {
                    output_root.join(&rel)
                };
                if entry.is_dir {
                    std::fs::create_dir_all(&child_output)
                        .map_err(|e| format!("Create output dir: {e}"))?;
                } else {
                    extract_file_entry(vault, entry, &child_output)?;
                }
            }

            Ok(output_root)
        }
        None => Err(format!("Entry not found: {entry_name}")),
    }
}

fn info_from_manifest(manifest: &VaultManifestV3) -> VaultV3Info {
    let file_count = manifest
        .entries
        .iter()
        .filter(|entry| !entry.is_dir)
        .count();
    let logical_chunks: usize = manifest
        .entries
        .iter()
        .map(|entry| entry.chunks.len())
        .sum();
    VaultV3Info {
        version: VERSION,
        file_count,
        chunk_count: manifest.chunks.len(),
        dedup_chunks: logical_chunks.saturating_sub(manifest.chunks.len()),
        compression_level: manifest_zstd_level(manifest),
        files: manifest
            .entries
            .iter()
            .map(|entry| VaultV3FileInfo {
                name: entry.path.clone(),
                size: entry.size,
                is_dir: entry.is_dir,
                modified: entry.modified.clone(),
                chunk_count: entry.chunks.len(),
            })
            .collect(),
        report: None,
    }
}

/// Algorithm chain for the receipt, derived from the manifest wrappers.
fn algorithm_chain(m: &VaultManifestV3) -> Vec<String> {
    let w = &m.wrappers;
    let line = |name: &str, s: &AlgorithmSpec| {
        format!("{name}:{} v{}", s.algorithm_id, s.algorithm_version)
    };
    vec![
        line("packing", &w.packing),
        line("chunking", &w.chunking),
        line("chunk_id", &w.chunk_id),
        line("compression", &w.compression),
        line("crypt", &w.crypt),
        line("cipher_hash", &w.cipher_hash),
    ]
}

#[tauri::command]
pub async fn vault_v3_create(
    vault_path: String,
    password: String,
    compression_profile: Option<String>,
) -> Result<String, String> {
    let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
    let level = match compression_profile.as_deref() {
        Some("fast") => 3,
        Some("archive") => 19,
        Some("balanced") | None | Some("") => DEFAULT_ZSTD_LEVEL,
        Some(other) => return Err(format!("Unknown AeroVault v3 compression profile: {other}")),
    };
    create_empty_vault(Path::new(&vault_path), &password, level, false)?;
    Ok(vault_path)
}

/// Create a new AeroVault v3 container **with the ECC (error-correction) extension stub**.
/// This is the Phase 1 entry point for v4+ECC work (stub only: the extension directory
/// entry is present with length=0; real Reed-Solomon shards are added in Phase 2).
///
/// The extension is emitted as non-critical so that existing v3 readers can still
/// open the vault and extract data (per AEROVAULT-V3-SPEC.md + discussion #276).
#[tauri::command]
pub async fn vault_v3_create_with_ecc(
    vault_path: String,
    password: String,
    profile: Option<String>,
) -> Result<String, String> {
    let level = match profile.as_deref() {
        Some("fast") => 3,
        Some("archive") => 19,
        Some("balanced") | None | Some("") => DEFAULT_ZSTD_LEVEL,
        Some(other) => return Err(format!("Unknown AeroVault v3 compression profile: {other}")),
    };
    create_empty_vault(Path::new(&vault_path), &password, level, true)?;
    Ok(vault_path)
}

#[tauri::command]
pub async fn vault_v3_open(vault_path: String, password: String) -> Result<VaultV3Info, String> {
    let vault = open_vault(vault_path, &password)?;
    Ok(info_from_manifest(&vault.manifest))
}

#[tauri::command]
pub async fn is_vault_v3(path: String) -> Result<bool, String> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return Ok(false),
    };
    let mut buf = [0u8; 11];
    if file.read_exact(&mut buf).is_err() {
        return Ok(false);
    }
    Ok(&buf[..10] == MAGIC && buf[10] == VERSION)
}

/// Lightweight check for the presence of the ECC (error-correction) extension.
/// Does **not** require the vault password: it only reads the header and the
/// plaintext extension directory. This is safe for `vault info` / pre-flight
/// use cases and matches the "has_ecc_extension" need from the plan (P1-05).
///
/// Returns true if a non-critical (or any) "ecc.reed-solomon" entry is present
/// in the extension directory.
#[tauri::command]
pub async fn vault_v3_has_ecc(path: String) -> Result<bool, String> {
    let mut file = std::fs::File::open(&path)
        .map_err(|e| format!("Open vault for ECC check: {e}"))?;

    let mut header_bytes = [0u8; HEADER_SIZE];
    file.read_exact(&mut header_bytes)
        .map_err(|e| format!("Read header for ECC check: {e}"))?;

    let header = VaultHeaderV3::from_bytes(&header_bytes)?;

    if header.extension_dir_len == 0 {
        return Ok(false);
    }

    let extension_json = read_capped(
        &mut file,
        header.extension_dir_offset,
        header.extension_dir_len,
        MAX_EXTENSION_DIR_SIZE,
        "extension directory (has_ecc)",
    )?;

    let extensions: Vec<ExtensionEntryV3> = serde_json::from_slice(&extension_json)
        .map_err(|e| format!("Extension directory parse (has_ecc): {e}"))?;

    Ok(extensions
        .iter()
        .any(|e| e.extension_id == ECC_EXTENSION_ID))
}

#[tauri::command]
pub async fn vault_v3_scrub(vault_path: String, password: String) -> Result<serde_json::Value, String> {
    let vault = open_vault(vault_path, &password)?;
    let checked = vault.manifest.chunks.len();
    let damaged = scrub_vault(&vault);
    let list: Vec<_> = damaged.into_iter().map(|d| serde_json::json!({
        "id": d.record.id,
        "on_disk_start": d.on_disk_start,
        "on_disk_len": d.on_disk_len,
        "cipher_hash": d.record.cipher_hash,
    })).collect();
    Ok(serde_json::json!({
        "damaged": list,
        "count": list.len(),
        "checked": checked
    }))
}

#[tauri::command]
pub async fn vault_v3_repair(vault_path: String, password: String, dry_run: bool) -> Result<serde_json::Value, String> {
    // A real repair mutates and atomically re-seals the vault, so take the same
    // write lock the other mutating ops use to keep a concurrent add/delete from
    // racing the rewrite. Dry-run is read-only and needs no lock.
    let _lock = if dry_run {
        None
    } else {
        Some(acquire_vault_write_lock(Path::new(&vault_path))?)
    };
    let mut vault = open_vault(&vault_path, &password)?;
    let damaged = scrub_vault(&vault).len();
    let repaired = repair_vault(&mut vault, dry_run)?;
    Ok(serde_json::json!({
        "repaired": repaired,
        "damaged": damaged,
        "dry_run": dry_run
    }))
}

#[tauri::command]
pub async fn vault_v3_add_files(
    vault_path: String,
    password: String,
    file_paths: Vec<String>,
) -> Result<VaultV3Info, String> {
    let started = Instant::now();
    let mut sources: Vec<(PathBuf, String)> = Vec::with_capacity(file_paths.len());
    for file_path in &file_paths {
        let path = PathBuf::from(file_path);
        if !path.is_file() {
            return Err(format!("Not a regular file: {file_path}"));
        }
        let name = safe_entry_name(&path)?;
        sources.push((path, name));
    }
    let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
    let mut vault = open_vault(&vault_path, &password)?;
    vault.report = VaultReport::new("add_files", VERSION);
    vault
        .report
        .set_profile(match manifest_zstd_level(&vault.manifest) {
            3 => "fast",
            19 => "archive",
            _ => "balanced",
        });
    vault
        .report
        .set_algorithms(algorithm_chain(&vault.manifest));
    append_sources_batched(&mut vault, &sources)?;
    vault.report.step("seal: rebuild manifest + atomic write");
    save_open_vault(&vault)?;
    vault.report.finish(started.elapsed().as_millis() as u64);
    let (np, dh, ratio) = (
        vault.report.new_physical_chunks,
        vault.report.dedup_hits,
        vault.report.compression_ratio_pct,
    );
    vault.report.step(format!(
        "done: {np} new physical chunk(s), {dh} dedup hit(s), {ratio:.1}% compressed"
    ));

    let mut info = info_from_manifest(&vault.manifest);
    info.report = Some(vault.report.clone());
    Ok(info)
}

#[tauri::command]
pub async fn vault_v3_add_files_to_dir(
    vault_path: String,
    password: String,
    file_paths: Vec<String>,
    target_dir: String,
) -> Result<serde_json::Value, String> {
    let target_dir = normalize_vault_relative_path(&target_dir)?;
    let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
    let mut vault = open_vault(&vault_path, &password)?;
    create_directory_in_manifest(&mut vault.manifest, &target_dir)?;
    let mut sources: Vec<(PathBuf, String)> = Vec::with_capacity(file_paths.len());
    for file_path in &file_paths {
        let path = PathBuf::from(file_path);
        let name = safe_entry_name(&path)?;
        sources.push((path, join_vault_path(&target_dir, &name)));
    }
    let added = sources.len();
    append_sources_batched(&mut vault, &sources)?;
    save_open_vault(&vault)?;
    Ok(serde_json::json!({
        "added": added,
        "total": vault.manifest.entries.len()
    }))
}

#[tauri::command]
pub async fn vault_v3_create_directory(
    vault_path: String,
    password: String,
    dir_name: String,
) -> Result<serde_json::Value, String> {
    let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
    let mut vault = open_vault(&vault_path, &password)?;
    let created = create_directory_in_manifest(&mut vault.manifest, &dir_name)?;
    save_open_vault(&vault)?;
    Ok(serde_json::json!({
        "created": created,
        "dir": normalize_vault_relative_path(&dir_name)?
    }))
}

#[tauri::command]
pub async fn vault_v3_delete_entry(
    vault_path: String,
    password: String,
    entry_name: String,
) -> Result<serde_json::Value, String> {
    let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
    let mut vault = open_vault(&vault_path, &password)?;
    delete_entries_from_manifest(&mut vault, std::slice::from_ref(&entry_name), false)?;
    save_open_vault(&vault)?;
    Ok(serde_json::json!({
        "deleted": normalize_vault_relative_path(&entry_name)?,
        "remaining": vault.manifest.entries.len()
    }))
}

#[tauri::command]
pub async fn vault_v3_delete_entries(
    vault_path: String,
    password: String,
    entry_names: Vec<String>,
    recursive: bool,
) -> Result<serde_json::Value, String> {
    let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
    let mut vault = open_vault(&vault_path, &password)?;
    let removed = delete_entries_from_manifest(&mut vault, &entry_names, recursive)?;
    save_open_vault(&vault)?;
    Ok(serde_json::json!({
        "removed": removed,
        "remaining": vault.manifest.entries.len()
    }))
}

#[tauri::command]
pub async fn vault_v3_move_entry(
    vault_path: String,
    password: String,
    from: String,
    to: String,
) -> Result<serde_json::Value, String> {
    let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
    let mut vault = open_vault(&vault_path, &password)?;
    move_entry_in_manifest(&mut vault, &from, &to)?;
    save_open_vault(&vault)?;
    Ok(serde_json::json!({
        "moved": true,
        "from": normalize_vault_relative_path(&from)?,
        "to": normalize_vault_relative_path(&to)?
    }))
}

#[tauri::command]
pub async fn vault_v3_rename_entry(
    vault_path: String,
    password: String,
    current_name: String,
    new_name: String,
) -> Result<serde_json::Value, String> {
    let current_name = normalize_vault_relative_path(&current_name)?;
    let new_name = normalize_leaf_name(&new_name)?;
    let destination = if let Some(parent) = path_parent(&current_name) {
        join_vault_path(parent, &new_name)
    } else {
        new_name.clone()
    };
    let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
    let mut vault = open_vault(&vault_path, &password)?;
    move_entry_in_manifest(&mut vault, &current_name, &destination)?;
    save_open_vault(&vault)?;
    Ok(serde_json::json!({
        "renamed": true,
        "from": current_name,
        "to": destination
    }))
}

#[tauri::command]
pub async fn vault_v3_copy_entry(
    vault_path: String,
    password: String,
    from: String,
    to: String,
) -> Result<serde_json::Value, String> {
    let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
    let mut vault = open_vault(&vault_path, &password)?;
    copy_entry_in_manifest(&mut vault, &from, &to)?;
    save_open_vault(&vault)?;
    Ok(serde_json::json!({
        "copied": true,
        "from": normalize_vault_relative_path(&from)?,
        "to": normalize_vault_relative_path(&to)?
    }))
}

#[tauri::command]
pub async fn vault_v3_change_password(
    vault_path: String,
    old_password: String,
    new_password: String,
) -> Result<String, String> {
    let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
    let mut vault = open_vault(&vault_path, &old_password)?;
    change_password_in_place(&mut vault, &new_password)?;
    save_open_vault(&vault)?;
    Ok("Password changed successfully".to_string())
}

#[tauri::command]
pub async fn vault_v3_add_directory(
    app: tauri::AppHandle,
    vault_path: String,
    password: String,
    source_dir: String,
    target_prefix: Option<String>,
) -> Result<serde_json::Value, String> {
    use tauri::Emitter;

    let source = Path::new(&source_dir)
        .canonicalize()
        .map_err(|e| format!("Failed to resolve directory: {e}"))?;
    if !source.is_dir() {
        return Err(format!("Not a directory: {source_dir}"));
    }

    struct DirEntry {
        rel_path: String,
        is_dir: bool,
        abs_path: PathBuf,
        depth: usize,
    }

    let normalized_prefix = target_prefix
        .as_deref()
        .map(|prefix| prefix.trim_matches('/'))
        .filter(|prefix| !prefix.is_empty())
        .map(normalize_vault_relative_path)
        .transpose()?;

    let mut all_entries: Vec<DirEntry> = Vec::new();
    for entry in walkdir::WalkDir::new(&source)
        .follow_links(false)
        .max_depth(100)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.path() == source {
            continue;
        }
        if all_entries.len() >= 500_000 {
            return Err("Directory exceeds maximum entry limit (500000)".to_string());
        }

        let rel_path = entry
            .path()
            .strip_prefix(&source)
            .map_err(|_| "Failed to compute relative path".to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        let full_rel = if let Some(prefix) = &normalized_prefix {
            join_vault_path(prefix, &rel_path)
        } else {
            rel_path
        };
        let full_rel = normalize_vault_relative_path(&full_rel)?;

        all_entries.push(DirEntry {
            rel_path: full_rel,
            is_dir: entry.file_type().is_dir(),
            abs_path: entry.path().to_path_buf(),
            depth: entry.depth(),
        });
    }

    let mut dirs: Vec<&DirEntry> = all_entries.iter().filter(|entry| entry.is_dir).collect();
    let files: Vec<&DirEntry> = all_entries.iter().filter(|entry| !entry.is_dir).collect();
    dirs.sort_by_key(|entry| entry.depth);

    let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
    let mut vault = open_vault(&vault_path, &password)?;
    let mut added_dirs = 0usize;
    for dir_entry in dirs {
        if create_directory_in_manifest(&mut vault.manifest, &dir_entry.rel_path)? {
            added_dirs += 1;
        }
    }

    let total_files = files.len();
    let sources: Vec<(PathBuf, String)> = files
        .iter()
        .map(|f| (f.abs_path.clone(), f.rel_path.clone()))
        .collect();
    append_sources_batched(&mut vault, &sources)?;
    let added_files = total_files;
    let _ = app.emit(
        "vault-add-progress",
        serde_json::json!({
            "current": added_files,
            "total": total_files,
            "current_file": ""
        }),
    );

    save_open_vault(&vault)?;
    Ok(serde_json::json!({
        "added_files": added_files,
        "added_dirs": added_dirs,
        "total_entries": added_files + added_dirs
    }))
}

#[tauri::command]
pub async fn vault_v3_extract_entry(
    vault_path: String,
    password: String,
    entry_name: String,
    dest_path: String,
) -> Result<String, String> {
    let vault = open_vault(vault_path, &password)?;
    let extracted = extract_entry(&vault, &entry_name, Path::new(&dest_path))?;
    Ok(extracted.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn vault_v3_security_info(path: Option<String>) -> serde_json::Value {
    let mut info = serde_json::json!({
        "version": "3.0-draft",
        "pipeline": [
            "small-file-batching",
            "gear-cdc",
            "blake3-keyed-128 chunk ids",
            "zstd per chunk",
            "AES-256-GCM-SIV",
            "BLAKE3-256 cipher block hashes",
            "extension directory for ECC (reed-solomon)"
        ],
        "compression_profiles": {
            "fast": 3,
            "balanced": 9,
            "archive": 19
        },
        "compatibility": "v4 is expected to read v3 directly; v3 skips unknown non-critical extensions",
        "ecc_support": "stub (Phase 1): vault_v3_create_with_ecc emits non-critical 'ecc.reed-solomon' entry; real RS shards in Phase 2. See T-AEROVAULT-ECC (#272)"
    });

    if let Some(p) = path {
        if let Ok(has_ecc) = vault_v3_has_ecc(p).await {
            if let Some(obj) = info.as_object_mut() {
                obj.insert(
                    "ecc".to_string(),
                    serde_json::json!({
                        "enabled": has_ecc,
                        "algorithm": "reed-solomon",
                        "version": 1,
                        "critical": false
                    }),
                );
            }
        }
    }

    info
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdc_ranges_cover_input() {
        let data = vec![7u8; CDC_MAX + 1234];
        let ranges = chunk_ranges_with(&data, &CdcBounds::defaults());
        assert!(ranges.len() >= 2);
        let mut cursor = 0usize;
        for (start, end) in ranges {
            assert_eq!(start, cursor);
            assert!(end > start);
            cursor = end;
        }
        assert_eq!(cursor, data.len());
    }

    #[test]
    fn v3_round_trip_and_dedup() {
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("test.aerovault");
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, b"hello AeroVault v3\n".repeat(4096)).unwrap();
        std::fs::write(&b, b"hello AeroVault v3\n".repeat(4096)).unwrap();

        create_empty_vault(
            &vault_path,
            "correct horse battery staple",
            DEFAULT_ZSTD_LEVEL,
            false,
        )
        .unwrap();
        let mut vault = open_vault(&vault_path, "correct horse battery staple").unwrap();
        append_file_at(&mut vault, &a, "a.txt").unwrap();
        append_file_at(&mut vault, &b, "b.txt").unwrap();
        save_open_vault(&vault).unwrap();

        let reopened = open_vault(&vault_path, "correct horse battery staple").unwrap();
        let info = info_from_manifest(&reopened.manifest);
        assert_eq!(info.file_count, 2);
        assert!(info.dedup_chunks >= 1);

        let out = dir.path().join("out.txt");
        extract_entry(&reopened, "a.txt", &out).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), std::fs::read(&a).unwrap());
    }

    /// P1-01: ECC stub creation + roundtrip + v3 compatibility.
    /// Creates a vault with the non-critical "ecc.reed-solomon" extension entry
    /// (payload length 0 for stub phase), performs add + extract, re-opens,
    /// and verifies the extension is present and non-critical.
    /// Also asserts that is_vault_v3 (magic-based) still returns true.
    #[test]
    fn v3_ecc_stub_roundtrip_and_v3_compatibility() {
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("ecc-stub.aerovault");
        let payload = dir.path().join("payload.bin");
        std::fs::write(&payload, b"hello from ECC stub phase").unwrap();

        // Create with ECC stub (Phase 1)
        create_empty_vault(&vault_path, "ecc-test-pw", DEFAULT_ZSTD_LEVEL, true).unwrap();

        // Add a file (exercises seal path that must preserve the extension dir)
        let mut vault = open_vault(&vault_path, "ecc-test-pw").unwrap();
        append_file_at(&mut vault, &payload, "data/payload.bin").unwrap();
        save_open_vault(&vault).unwrap();

        // Re-open and inspect extensions (proves roundtrip of the dir)
        let reopened = open_vault(&vault_path, "ecc-test-pw").unwrap();
        let ecc_ext = reopened
            .extensions
            .iter()
            .find(|e| e.extension_id == ECC_EXTENSION_ID);
        assert!(ecc_ext.is_some(), "ecc extension should be present after roundtrip");
        let ecc_ext = ecc_ext.unwrap();
        assert_eq!(ecc_ext.critical, false, "ECC extension must be non-critical for v3 compat");
        assert_eq!(ecc_ext.algorithm_id, ECC_ALGORITHM_ID);
        assert_eq!(ecc_ext.algorithm_version, ECC_ALGORITHM_VERSION);

        // Extract must succeed (pure v3 reader path compatibility)
        let out = dir.path().join("restored.bin");
        extract_entry(&reopened, "data/payload.bin", &out).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"hello from ECC stub phase");

        // is_vault_v3 (the fast magic path used by dispatch / old v3 binaries) must still say yes.
        // We check the magic directly here (same logic as is_vault_v3) to avoid needing an async runtime in the test.
        let mut f = std::fs::File::open(&vault_path).unwrap();
        let mut magic = [0u8; 11];
        f.read_exact(&mut magic).unwrap();
        let is_v3_magic = &magic[..10] == MAGIC && magic[10] == VERSION;
        assert!(is_v3_magic, "vault with ECC stub extension must still be recognized as v3 by magic (for pure v3 reader compat)");

        // The extension dir itself survived (we can check via header on re-open)
        // Re-open header has non-zero extension_dir_len
        assert!(reopened.header.extension_dir_len > 0, "extension directory must be present on disk");
    }

    #[test]
    fn v3_security_info_advertises_ecc_when_present() {
        // P1-04 test: security_info with path should report the ecc object when the stub extension is present.
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("sec-info-test.aerovault");

        create_empty_vault(&vault_path, "sec-pw-1234", DEFAULT_ZSTD_LEVEL, true).unwrap();

        let info = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(vault_v3_security_info(Some(
                vault_path.to_string_lossy().to_string(),
            )));

        let ecc = info.get("ecc").expect("ecc field should be present when path given");
        assert_eq!(ecc["enabled"], true);
        assert_eq!(ecc["algorithm"], "reed-solomon");
        assert_eq!(ecc["version"], 1);
        assert_eq!(ecc["critical"], false);

        // Also check general fields still there
        assert!(info.get("pipeline").is_some());
        assert!(info.get("ecc_support").is_some());
    }

    #[test]
    fn p2_02_ecc_payload_format_roundtrip() {
        // P2-09 (v2): the fixed-grid payload (header + per-shard checksum table +
        // parity) must serialize and deserialize losslessly.
        let s = 4096usize;
        let l = s * 25; // 25 data shards => 3 groups of 10 (last group partial)
        let header = EccPayloadHeader {
            data_shards: 10,
            parity_shards: 2,
            shard_size: s as u32,
            total_data_len: l as u64,
        };
        let (num_data_shards, num_groups) = ecc_geometry(&header);
        assert_eq!(num_data_shards, 25);
        assert_eq!(num_groups, 3);
        let num_parity = num_groups * header.parity_shards as usize;

        let data_checksums: Vec<[u8; ECC_SHARD_CKSUM_LEN]> =
            (0..num_data_shards).map(|i| [i as u8; ECC_SHARD_CKSUM_LEN]).collect();
        let parity_checksums: Vec<[u8; ECC_SHARD_CKSUM_LEN]> =
            (0..num_parity).map(|i| [(200 + i) as u8; ECC_SHARD_CKSUM_LEN]).collect();
        let parity_data = vec![0xABu8; num_parity * s];

        let payload = EccPayload {
            header,
            data_checksums: data_checksums.clone(),
            parity_checksums: parity_checksums.clone(),
            parity_data: parity_data.clone(),
        };

        let bytes = payload.to_bytes();
        let decoded = EccPayload::from_bytes(&bytes).expect("roundtrip failed");

        assert_eq!(decoded.header.data_shards, 10);
        assert_eq!(decoded.header.parity_shards, 2);
        assert_eq!(decoded.header.shard_size, s as u32);
        assert_eq!(decoded.header.total_data_len, l as u64);
        assert_eq!(decoded.data_checksums, data_checksums);
        assert_eq!(decoded.parity_checksums, parity_checksums);
        assert_eq!(decoded.parity_data, parity_data);

        // A truncated / length-mismatched payload must be rejected, not misparsed.
        assert!(EccPayload::from_bytes(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn p2_03_compute_ecc_shards_basic() {
        // P2-09: compute_ecc_shards produces a well-formed v2 payload over the
        // concatenated block stream.
        let block1: Vec<u8> = vec![0u8; 100];
        let block2: Vec<u8> = vec![1u8; 200];
        let data_blocks: Vec<&[u8]> = vec![&block1, &block2];

        let payload_bytes = compute_ecc_shards(&data_blocks);
        assert!(!payload_bytes.is_empty());

        let parsed = EccPayload::from_bytes(&payload_bytes).expect("payload should parse");
        assert_eq!(parsed.header.data_shards, ECC_DATA_SHARDS as u16);
        assert_eq!(parsed.header.parity_shards, ECC_PARITY_SHARDS as u16);
        assert_eq!(parsed.header.total_data_len, 300);
        // L=300 < ECC_MIN_SHARD => S clamps to the floor => one data shard, one group.
        assert_eq!(parsed.header.shard_size as usize, ECC_MIN_SHARD);
        let (num_data_shards, num_groups) = ecc_geometry(&parsed.header);
        assert_eq!(num_data_shards, 1);
        assert_eq!(num_groups, 1);
        assert_eq!(parsed.data_checksums.len(), 1);
        assert_eq!(parsed.parity_checksums.len(), ECC_PARITY_SHARDS);
        assert_eq!(parsed.parity_data.len(), ECC_PARITY_SHARDS * ECC_MIN_SHARD);
    }

    #[test]
    fn p2_04_reconstruct_from_ecc_basic() {
        // P2-09: damage one block, reconstruct it via the v2 ECC payload (the shard
        // checksum localizes the erasure; no externally supplied bad-index list).
        let orig1: Vec<u8> = (0u8..100).collect();
        let orig2: Vec<u8> = (0u8..150).map(|x| 100 + x).collect();

        let make_on_disk = |data: &[u8]| -> Vec<u8> {
            let mut b = (data.len() as u64).to_le_bytes().to_vec();
            b.extend_from_slice(data);
            b
        };

        let mut blocks = vec![make_on_disk(&orig1), make_on_disk(&orig2)];
        let payload =
            compute_ecc_shards(&blocks.iter().map(|v| v.as_slice()).collect::<Vec<_>>());

        // Corrupt the second block; its shard checksum will mismatch -> RS erasure.
        blocks[1][10] ^= 0xFF;

        let recovered = reconstruct_from_ecc(&mut blocks, &payload).expect("reconstruct");
        assert!(recovered >= 1);

        // The second block is restored to its original on-disk form.
        let restored = &blocks[1];
        let len = u64::from_le_bytes(restored[0..8].try_into().unwrap()) as usize;
        assert_eq!(&restored[8..8 + len], &orig2[..]);
    }

    #[test]
    fn p2_06_scrub_detects_tampered_block() {
        // P2-06: create a small vault, tamper one cipher block in the data section,
        // run scrub, should report exactly one damaged chunk with correct range.
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("scrub-test.aerovault");

        create_empty_vault(&vault_path, "scrub-pw-1234", DEFAULT_ZSTD_LEVEL, false).unwrap();
        let mut vault = open_vault(&vault_path, "scrub-pw-1234").unwrap();

        // Directly inject a fake chunk block for testing scrub
        let encrypted = vec![0u8; 32];
        let cipher_hash = blake3::hash(&encrypted).to_hex().to_string();
        let block: Vec<u8> = {
            let mut b = (encrypted.len() as u64).to_le_bytes().to_vec();
            b.extend_from_slice(&encrypted);
            b
        };
        let data_offset = vault.data.len() as u64;
        vault.data.extend_from_slice(&block);
        vault.manifest.chunks.insert(
            "fakeid".to_string(),
            ChunkRecordV3 {
                id: "fakeid".to_string(),
                block_index: 0,
                data_offset,
                block_len: encrypted.len() as u64,
                plaintext_len: 16,
                compressed_len: 16,
                cipher_hash: cipher_hash.clone(),
            },
        );

        // Tamper inside the cipher part (after the u64 prefix)
        let tamper_pos = data_offset as usize + 8 + 5;
        if tamper_pos < vault.data.len() {
            vault.data[tamper_pos] ^= 0xFF;
        }

        let damaged = scrub_vault(&vault);
        assert_eq!(damaged.len(), 1);
        assert_eq!(damaged[0].record.id, "fakeid");
        assert_eq!(damaged[0].on_disk_len, 8 + 32);
    }

    #[test]
    fn p2_07_repair_end_to_end() {
        // P2-07: full cycle with real ECC vault + corruption + repair via primitive.
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("repair-e2e.aerovault");
        let f1 = dir.path().join("f1.txt");
        std::fs::write(&f1, b"hello repair world one").unwrap();
        let f2 = dir.path().join("f2.txt");
        std::fs::write(&f2, b"hello repair world two with more data to have a decent block").unwrap();

        // Create with ECC and add files (triggers seal with ECC)
        create_empty_vault(&vault_path, "repair-pw-1234", DEFAULT_ZSTD_LEVEL, true).unwrap();
        let mut vault = open_vault(&vault_path, "repair-pw-1234").unwrap();
        append_file_at(&mut vault, &f1, "f1.txt").unwrap();
        append_file_at(&mut vault, &f2, "f2.txt").unwrap();
        save_open_vault(&vault).unwrap();

        // Identify the second file's data block, then exercise the real bad-hash path
        // by corrupting its cipher bytes in memory on a freshly re-opened vault.
        let mut recs: Vec<_> = vault.manifest.chunks.values().cloned().collect();
        recs.sort_by_key(|r| r.data_offset);
        assert!(recs.len() >= 2);

        let mut vault2 = open_vault(&vault_path, "repair-pw-1234").unwrap();

        // Corrupt in-memory the second block's cipher part
        if let Some(rec) = vault2.manifest.chunks.values().find(|r| r.data_offset == recs[1].data_offset) {
            let s = rec.data_offset as usize + 8 + 3;
            if s < vault2.data.len() {
                vault2.data[s] ^= 0xFF;
            }
        }

        // Now scrub sees damage
        let damaged_before = scrub_vault(&vault2);
        assert!(!damaged_before.is_empty());

        // Repair (not dry)
        let repaired = repair_vault(&mut vault2, false).expect("repair should succeed");
        assert!(repaired > 0);

        // After repair + re-seal, scrub should be clean
        let damaged_after = scrub_vault(&vault2);
        assert!(damaged_after.is_empty());

        // Verify content by extract
        let out1 = dir.path().join("out1.txt");
        extract_entry(&vault2, "f1.txt", &out1).unwrap();
        assert_eq!(std::fs::read(&out1).unwrap(), b"hello repair world one");

        let out2 = dir.path().join("out2.txt");
        extract_entry(&vault2, "f2.txt", &out2).unwrap();
        assert!(std::fs::read(&out2).unwrap().starts_with(b"hello repair world two"));
    }

    #[test]
    fn p2_08_cli_stress_multiple_damage_repair() {
        // Stress test: ECC vault with 12 files, corrupt 4 blocks (across stripes), repair, verify all.
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("stress-repair.aerovault");

        create_empty_vault(&vault_path, "stress-pw-1234", DEFAULT_ZSTD_LEVEL, true).unwrap();
        let mut vault = open_vault(&vault_path, "stress-pw-1234").unwrap();

        let mut sources = vec![];
        for i in 0..12 {
            let p = dir.path().join(format!("s{i:02}.txt"));
            let content = format!("stress file {} with some padding data to make blocks decent size", i).repeat(3);
            std::fs::write(&p, content.as_bytes()).unwrap();
            append_file_at(&mut vault, &p, &format!("s{i:02}.txt")).unwrap();
            sources.push((p, format!("s{i:02}.txt")));
        }
        save_open_vault(&vault).unwrap();

        // Re-open, corrupt a few chunk blocks. These tiny blocks share one shard, so
        // any of them mismatching erases that single shard, which RS(10,2) recovers.
        let mut vault2 = open_vault(&vault_path, "stress-pw-1234").unwrap();
        let mut recs: Vec<_> = vault2.manifest.chunks.values().cloned().collect();
        recs.sort_by_key(|r| r.data_offset);

        let to_corrupt = vec![2, 5, 11]; // three distinct blocks (all share one shard)
        for &idx in &to_corrupt {
            if idx < recs.len() {
                let rec = &recs[idx];
                let pos = rec.data_offset as usize + 8 + 2;
                if pos < vault2.data.len() {
                    vault2.data[pos] ^= 0xAA;
                }
            }
        }

        // Scrub sees multiple
        let damaged = scrub_vault(&vault2);
        assert!(damaged.len() >= 3);

        // Repair
        let fixed = repair_vault(&mut vault2, false).expect("repair");
        assert!(fixed >= 3);

        // Post-scrub clean
        let after = scrub_vault(&vault2);
        assert!(after.is_empty());

        // Verify a few extracts
        for &idx in &[0, 3, 11] {
            if idx < sources.len() {
                let (p, name) = &sources[idx];
                let out = dir.path().join(format!("verif_{}.txt", idx));
                extract_entry(&vault2, name, &out).unwrap();
                assert_eq!(std::fs::read(&out).unwrap(), std::fs::read(p).unwrap());
            }
        }
    }

    /// Helper: ~`n` bytes of high-entropy (splitmix64-derived) data so zstd cannot
    /// compress it and the vault stays a realistically-sized ciphertext stream that
    /// spans many shards (a low-entropy pattern would compress to a tiny single shard
    /// and defeat the overhead / multi-shard-damage tests).
    fn ecc_test_blob(n: u32) -> Vec<u8> {
        (0..n)
            .map(|i| {
                let mut z = (i as u64).wrapping_add(0x9E3779B97F4A7C15);
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
                (z ^ (z >> 31)) as u8
            })
            .collect()
    }

    /// P2-09 robustness: with per-shard checksums a rotted PARITY shard is detected
    /// and treated as an erasure, so RS routes around it. A single damaged data shard
    /// plus one corrupt parity shard is 2 erasures (== P), which must still recover
    /// correctly and heal the vault. This is strictly better than the v1 behaviour,
    /// where a corrupt parity silently produced wrong bytes that only the cipher_hash
    /// gate could catch (CLAUDE-AV-ECC-01).
    #[test]
    fn p2_repair_recovers_despite_corrupt_parity_shard() {
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("recover-bad-parity.aerovault");
        let f = dir.path().join("f.bin");
        let data = ecc_test_blob(300_000);
        std::fs::write(&f, &data).unwrap();

        create_empty_vault(&vault_path, "recover-pw", DEFAULT_ZSTD_LEVEL, true).unwrap();
        let mut vault = open_vault(&vault_path, "recover-pw").unwrap();
        append_file_at(&mut vault, &f, "f.bin").unwrap();
        save_open_vault(&vault).unwrap();

        // Corrupt the bytes of parity shard 0 on disk (its stored checksum will no
        // longer match -> reconstruct erases it and uses parity shard 1 instead).
        // save_open_vault takes &OpenVaultV3 and updates only a local extensions clone,
        // so the persisted payload location is read straight from the on-disk header
        // (extension_payload_offset @176, extension_payload_len @184).
        let mut raw = std::fs::read(&vault_path).unwrap();
        let payload_abs = u64::from_le_bytes(raw[176..184].try_into().unwrap()) as usize;
        let payload_len = u64::from_le_bytes(raw[184..192].try_into().unwrap()) as usize;
        let payload =
            EccPayload::from_bytes(&raw[payload_abs..payload_abs + payload_len]).unwrap();
        let (nds, ng) = ecc_geometry(&payload.header);
        let p = payload.header.parity_shards as usize;
        let parity0_abs = payload_abs + 32 + (nds + ng * p) * ECC_SHARD_CKSUM_LEN;
        for i in 0..64usize {
            raw[parity0_abs + i] ^= 0xAA;
        }
        std::fs::write(&vault_path, &raw).unwrap();

        // Open and damage exactly one data shard in memory.
        let mut vault2 = open_vault(&vault_path, "recover-pw").unwrap();
        let first_off = {
            let mut recs: Vec<_> = vault2.manifest.chunks.values().cloned().collect();
            recs.sort_by_key(|r| r.data_offset);
            recs[0].data_offset as usize
        };
        vault2.data[first_off + 8 + 5] ^= 0xFF;
        assert!(!scrub_vault(&vault2).is_empty(), "scrub must see the damage");

        let repaired = repair_vault(&mut vault2, false).expect("repair");
        assert!(repaired > 0, "must recover despite the corrupt parity shard");
        assert!(scrub_vault(&vault2).is_empty(), "vault must be clean after repair");

        // Content verifies AND the parity was re-sealed correctly (re-open + scrub).
        let out = dir.path().join("out.bin");
        extract_entry(&vault2, "f.bin", &out).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), data);
        let reopened = open_vault(&vault_path, "recover-pw").unwrap();
        assert!(scrub_vault(&reopened).is_empty());
    }

    /// P2-09 safety gate (CLAUDE-AV-ECC-01): when damage exceeds the per-group parity
    /// budget (here 3 erasures with P=2) reconstruction cannot succeed. repair must
    /// NOT claim success and must leave the vault byte-for-byte untouched (persisting
    /// unverifiable bytes would recompute parity over garbage and destroy redundancy).
    #[test]
    fn p2_repair_refuses_when_damage_exceeds_redundancy() {
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("over-budget.aerovault");
        let f = dir.path().join("f.bin");
        let data = ecc_test_blob(300_000);
        std::fs::write(&f, &data).unwrap();

        create_empty_vault(&vault_path, "over-pw-1234", DEFAULT_ZSTD_LEVEL, true).unwrap();
        let mut vault = open_vault(&vault_path, "over-pw-1234").unwrap();
        append_file_at(&mut vault, &f, "f.bin").unwrap();
        save_open_vault(&vault).unwrap();

        let before = std::fs::read(&vault_path).unwrap();
        let mut vault2 = open_vault(&vault_path, "over-pw-1234").unwrap();

        // Damage three widely-separated regions: with S ~= L/10 these fall in three
        // distinct data shards of the single RS group, i.e. 3 erasures > P=2.
        let n = vault2.data.len();
        for pos in [13usize, n / 2, n - 100] {
            if pos < n {
                vault2.data[pos] ^= 0xFF;
            }
        }
        assert!(!scrub_vault(&vault2).is_empty(), "scrub must detect the damage");

        let repaired = repair_vault(&mut vault2, false).expect("repair call should not error");
        assert_eq!(repaired, 0, "repair must not claim an unverifiable success");

        let after = std::fs::read(&vault_path).unwrap();
        assert_eq!(before, after, "repair must leave the vault untouched when it cannot fix it");
    }

    /// P2-09 regression: the v1 format produced ~200% parity for a small single-chunk
    /// vault (300 KB -> ~600 KB parity). The v2 fixed-grid format must keep the stored
    /// ECC payload near the nominal P/K (20%).
    #[test]
    fn p2_09_ecc_overhead_is_bounded_for_single_chunk_vault() {
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("overhead.aerovault");
        let f = dir.path().join("blob.bin");
        let data = ecc_test_blob(300_000);
        std::fs::write(&f, &data).unwrap();

        create_empty_vault(&vault_path, "ovh-pw-1234", DEFAULT_ZSTD_LEVEL, true).unwrap();
        let mut vault = open_vault(&vault_path, "ovh-pw-1234").unwrap();
        append_file_at(&mut vault, &f, "blob.bin").unwrap();
        save_open_vault(&vault).unwrap();

        // save_open_vault updates only a local extensions clone, so read the persisted
        // ECC payload length from the on-disk header (extension_payload_len @184) rather
        // than the stale in-memory entry.
        let raw = std::fs::read(&vault_path).unwrap();
        let ecc_payload = u64::from_le_bytes(raw[184..192].try_into().unwrap()) as f64;
        let protected: f64 = vault
            .manifest
            .chunks
            .values()
            .map(|c| 8.0 + c.block_len as f64)
            .sum();

        assert!(
            ecc_payload < protected * 0.30,
            "ECC overhead too high: {} payload bytes for {} protected bytes ({:.0}%)",
            ecc_payload,
            protected,
            ecc_payload / protected * 100.0
        );
    }

    /// P1-06: First compatibility test - a "v4-stub" vault (created with ECC extension)
    /// must still be fully readable using the pure v3 open/extract paths
    /// (simulating an older v3-only reader or binary).
    #[test]
    fn v3_stub_ecc_vault_readable_by_pure_v3_open_and_extract() {
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("compat-stub.aerovault");
        let payload = dir.path().join("payload.bin");
        std::fs::write(&payload, b"P1-06 compatibility payload for stub ECC vault").unwrap();

        // Create using the ECC stub path (what will be "v4" in future)
        create_empty_vault(&vault_path, "compat-pw-1234", DEFAULT_ZSTD_LEVEL, true).unwrap();

        // Use pure v3 open path (internal open_vault, as old reader would)
        let mut vault = open_vault(&vault_path, "compat-pw-1234").unwrap();
        append_file_at(&mut vault, &payload, "data/compat.bin").unwrap();
        save_open_vault(&vault).unwrap();

        // Re-open with pure v3 path and extract
        let reopened = open_vault(&vault_path, "compat-pw-1234").unwrap();
        let out = dir.path().join("extracted.bin");
        extract_entry(&reopened, "data/compat.bin", &out).unwrap();

        assert_eq!(
            std::fs::read(&out).unwrap(),
            b"P1-06 compatibility payload for stub ECC vault"
        );
    }

    #[test]
    fn v3_directory_ops_and_password_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::create_dir_all(source.join("docs/nested")).unwrap();
        std::fs::write(source.join("docs/guide.txt"), b"guide").unwrap();
        std::fs::write(source.join("docs/nested/readme.txt"), b"nested").unwrap();

        let vault_path = dir.path().join("dir-test.aerovault");
        create_empty_vault(&vault_path, "old-password", DEFAULT_ZSTD_LEVEL, false).unwrap();

        let mut vault = open_vault(&vault_path, "old-password").unwrap();
        create_directory_in_manifest(&mut vault.manifest, "empty").unwrap();
        append_file_at(&mut vault, &source.join("docs/guide.txt"), "docs/guide.txt").unwrap();
        append_file_at(
            &mut vault,
            &source.join("docs/nested/readme.txt"),
            "docs/nested/readme.txt",
        )
        .unwrap();
        copy_entry_in_manifest(&mut vault, "docs", "docs-copy").unwrap();
        move_entry_in_manifest(&mut vault, "docs-copy", "docs-archived").unwrap();
        delete_entries_from_manifest(&mut vault, &["docs-archived".to_string()], true).unwrap();
        change_password_in_place(&mut vault, "new-password").unwrap();
        save_open_vault(&vault).unwrap();

        assert!(open_vault(&vault_path, "old-password").is_err());
        let reopened = open_vault(&vault_path, "new-password").unwrap();
        assert!(entry_kind(&reopened.manifest, "docs").is_some());
        assert!(entry_kind(&reopened.manifest, "empty").is_some());
        assert!(entry_kind(&reopened.manifest, "docs-archived").is_none());

        let extract_root = dir.path().join("extract-docs");
        let extracted = extract_entry(&reopened, "docs", &extract_root).unwrap();
        assert!(extracted.is_dir());
        assert_eq!(
            std::fs::read(extracted.join("guide.txt")).unwrap(),
            b"guide"
        );
        assert_eq!(
            std::fs::read(extracted.join("nested/readme.txt")).unwrap(),
            b"nested"
        );
    }

    #[test]
    fn v3_header_mac_detects_tamper() {
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("tamper.aerovault");
        create_empty_vault(
            &vault_path,
            "correct horse battery staple",
            DEFAULT_ZSTD_LEVEL,
            false,
        )
        .unwrap();

        let mut bytes = std::fs::read(&vault_path).unwrap();
        bytes[136] ^= 0x01;
        std::fs::write(&vault_path, bytes).unwrap();

        let err = open_vault(&vault_path, "correct horse battery staple").unwrap_err();
        assert!(err.contains("header MAC mismatch"));
    }

    #[test]
    fn v3_packs_small_files_with_stable_dedup() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("small");
        std::fs::create_dir_all(&src).unwrap();
        let mut sources: Vec<(PathBuf, String)> = Vec::new();
        for i in 0..200 {
            let p = src.join(format!("f{i:03}.txt"));
            std::fs::write(&p, format!("small-file-{i}\n").repeat(64)).unwrap();
            sources.push((p, format!("f{i:03}.txt")));
        }

        let vault_path = dir.path().join("pack.aerovault");
        create_empty_vault(&vault_path, "pack-password", DEFAULT_ZSTD_LEVEL, false).unwrap();

        let mut vault = open_vault(&vault_path, "pack-password").unwrap();
        append_sources_batched(&mut vault, &sources).unwrap();
        save_open_vault(&vault).unwrap();

        let reopened = open_vault(&vault_path, "pack-password").unwrap();
        let info = info_from_manifest(&reopened.manifest);
        assert_eq!(info.file_count, 200);
        // 200 tiny files must collapse into far fewer physical chunks.
        assert!(
            info.chunk_count < info.file_count,
            "expected packing: {} chunks for {} files",
            info.chunk_count,
            info.file_count
        );
        assert!(info.dedup_chunks >= 1);

        for i in [0usize, 1, 99, 150, 199] {
            let out = dir.path().join(format!("out{i}.txt"));
            extract_entry(&reopened, &format!("f{i:03}.txt"), &out).unwrap();
            assert_eq!(
                std::fs::read(&out).unwrap(),
                std::fs::read(src.join(format!("f{i:03}.txt"))).unwrap(),
                "packed file {i} round-trip mismatch"
            );
        }

        // Re-adding the identical set must not grow the physical chunk store.
        let mut v2 = open_vault(&vault_path, "pack-password").unwrap();
        let before = v2.manifest.chunks.len();
        append_sources_batched(&mut v2, &sources).unwrap();
        assert_eq!(
            v2.manifest.chunks.len(),
            before,
            "dedup unstable across adds"
        );
    }

    #[test]
    fn v3_pack_multi_chunk_straddle_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("many");
        std::fs::create_dir_all(&src).unwrap();
        let mut sources: Vec<(PathBuf, String)> = Vec::new();
        // ~6 MiB of distinct small files forces a pack past CDC_MAX, so the
        // CDC chunker produces multiple chunks and some files straddle a
        // chunk boundary (covering chunks == 2, pack_offset > 0).
        for i in 0..600 {
            let p = src.join(format!("d{i:04}.bin"));
            let body = format!("DISTINCT-{i:04}-").repeat(700);
            std::fs::write(&p, &body).unwrap();
            sources.push((p, format!("d{i:04}.bin")));
        }

        let vault_path = dir.path().join("multi.aerovault");
        create_empty_vault(&vault_path, "multi-password", DEFAULT_ZSTD_LEVEL, false).unwrap();
        let mut vault = open_vault(&vault_path, "multi-password").unwrap();
        append_sources_batched(&mut vault, &sources).unwrap();
        save_open_vault(&vault).unwrap();

        let reopened = open_vault(&vault_path, "multi-password").unwrap();
        assert!(
            reopened.manifest.chunks.len() >= 2,
            "expected multi-chunk pack"
        );
        let straddlers = reopened
            .manifest
            .entries
            .iter()
            .filter(|e| !e.is_dir && e.chunks.len() >= 2)
            .count();
        assert!(
            straddlers >= 1,
            "expected at least one boundary-straddling file"
        );

        for i in [0usize, 1, 250, 599] {
            let out = dir.path().join(format!("m{i}.bin"));
            extract_entry(&reopened, &format!("d{i:04}.bin"), &out).unwrap();
            assert_eq!(
                std::fs::read(&out).unwrap(),
                std::fs::read(src.join(format!("d{i:04}.bin"))).unwrap(),
                "straddling file {i} round-trip mismatch"
            );
        }
    }

    #[test]
    fn v3_delete_inside_shared_pack_keeps_others() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("shared");
        std::fs::create_dir_all(&src).unwrap();
        let mut sources: Vec<(PathBuf, String)> = Vec::new();
        for i in 0..10 {
            let p = src.join(format!("s{i}.txt"));
            std::fs::write(&p, format!("shared-pack-{i}\n").repeat(32)).unwrap();
            sources.push((p, format!("s{i}.txt")));
        }

        let vault_path = dir.path().join("shared.aerovault");
        create_empty_vault(&vault_path, "shared-password", DEFAULT_ZSTD_LEVEL, false).unwrap();
        let mut vault = open_vault(&vault_path, "shared-password").unwrap();
        append_sources_batched(&mut vault, &sources).unwrap();
        // All ten tiny files share one physical pack chunk.
        assert!(vault.manifest.chunks.len() < 10);
        delete_entries_from_manifest(&mut vault, &["s3.txt".to_string()], false).unwrap();
        save_open_vault(&vault).unwrap();

        let reopened = open_vault(&vault_path, "shared-password").unwrap();
        assert!(entry_kind(&reopened.manifest, "s3.txt").is_none());
        assert!(
            !reopened.manifest.chunks.is_empty(),
            "shared chunk wrongly GCd"
        );
        for i in [0usize, 5, 9] {
            let out = dir.path().join(format!("k{i}.txt"));
            extract_entry(&reopened, &format!("s{i}.txt"), &out).unwrap();
            assert_eq!(
                std::fs::read(&out).unwrap(),
                std::fs::read(src.join(format!("s{i}.txt"))).unwrap()
            );
        }
    }

    // --- GAP-3: corruption-injection harness ---------------------------------
    // Reusable scaffolding for the v4 ECC scrub work: deterministic ways to
    // damage a sealed vault so integrity / recovery paths can be tested.

    /// Flip one bit at an absolute byte offset inside a sealed vault file.
    fn flip_byte_in_file(path: &Path, offset: usize) {
        let mut bytes = std::fs::read(path).unwrap();
        assert!(
            offset < bytes.len(),
            "offset {offset} past EOF {}",
            bytes.len()
        );
        bytes[offset] ^= 0x01;
        std::fs::write(path, bytes).unwrap();
    }

    /// Truncate a sealed vault file to `keep` bytes (data loss at the tail).
    fn truncate_vault(path: &Path, keep: u64) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_len(keep).unwrap();
    }

    fn build_filled_vault(dir: &Path, name: &str, password: &str, payload_kib: usize) -> PathBuf {
        let src = dir.join("payload.bin");
        // Distinct, low-redundancy bytes so the cipher block is sizeable.
        let body: Vec<u8> = (0..payload_kib * 1024)
            .map(|i| (i * 31 + 7) as u8)
            .collect();
        std::fs::write(&src, &body).unwrap();
        let vault_path = dir.join(name);
        create_empty_vault(&vault_path, password, DEFAULT_ZSTD_LEVEL, false).unwrap();
        let mut v = open_vault(&vault_path, password).unwrap();
        append_file_at(&mut v, &src, "payload.bin").unwrap();
        save_open_vault(&v).unwrap();
        vault_path
    }

    #[test]
    fn v3_tampered_cipher_block_detected_pre_decrypt() {
        let dir = tempfile::tempdir().unwrap();
        let vault_path = build_filled_vault(dir.path(), "tamper-block.aerovault", "scrub-pw", 200);

        // HEADER_SIZE + 8-byte block-length prefix, then well into the
        // ciphertext body of the first stored block.
        flip_byte_in_file(&vault_path, HEADER_SIZE + 8 + 256);

        let v = open_vault(&vault_path, "scrub-pw").unwrap();
        let out = dir.path().join("out.bin");
        let err = extract_entry(&v, "payload.bin", &out).unwrap_err();
        // cipher_hash mismatch is caught BEFORE AEAD decrypt: this is the
        // exact hook the v4 ECC scrub will hang off.
        assert!(
            err.contains("Cipher block hash mismatch")
                || err.contains("Chunk length metadata mismatch"),
            "unexpected error for tampered block: {err}"
        );
    }

    #[test]
    fn v3_truncated_vault_rejected_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let vault_path = build_filled_vault(dir.path(), "truncated.aerovault", "scrub-pw", 200);
        let full = std::fs::metadata(&vault_path).unwrap().len();

        // Cut the tail (manifest / extension dir / part of the data section).
        truncate_vault(&vault_path, full / 2);

        // Must be a clean Err, never a panic.
        let result = std::panic::catch_unwind(|| open_vault(&vault_path, "scrub-pw"));
        assert!(result.is_ok(), "open_vault panicked on a truncated vault");
        assert!(result.unwrap().is_err(), "truncated vault opened as valid");
    }

    #[test]
    fn v3_custom_and_archive_cdc_bounds_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("big.bin");
        let body: Vec<u8> = (0..6 * 1024 * 1024).map(|i| (i * 131 + 17) as u8).collect();
        std::fs::write(&src, &body).unwrap();

        // archive profile (level 19) widens CDC bounds.
        let archive_path = dir.path().join("archive.aerovault");
        create_empty_vault(&archive_path, "bounds-pw", 19, false).unwrap();
        let mut av = open_vault(&archive_path, "bounds-pw").unwrap();
        let b = manifest_cdc_bounds(&av.manifest).unwrap();
        assert_eq!(b.avg, 4 * 1024 * 1024, "archive must widen CDC avg");
        append_file_at(&mut av, &src, "big.bin").unwrap();
        save_open_vault(&av).unwrap();

        // balanced profile keeps the const bounds.
        let bal_path = dir.path().join("balanced.aerovault");
        create_empty_vault(&bal_path, "bounds-pw", DEFAULT_ZSTD_LEVEL, false).unwrap();
        let mut bv = open_vault(&bal_path, "bounds-pw").unwrap();
        assert_eq!(manifest_cdc_bounds(&bv.manifest).unwrap().avg, CDC_AVG);
        append_file_at(&mut bv, &src, "big.bin").unwrap();
        save_open_vault(&bv).unwrap();

        // Wider bounds => fewer, larger chunks for the same 6 MiB input.
        let archive_chunks = open_vault(&archive_path, "bounds-pw")
            .unwrap()
            .manifest
            .chunks
            .len();
        let balanced_chunks = open_vault(&bal_path, "bounds-pw")
            .unwrap()
            .manifest
            .chunks
            .len();
        assert!(
            archive_chunks <= balanced_chunks,
            "archive bounds should not produce more chunks ({archive_chunks} vs {balanced_chunks})"
        );

        // Both must round-trip byte-identically regardless of bounds.
        for (vp, tag) in [(&archive_path, "a"), (&bal_path, "b")] {
            let v = open_vault(vp, "bounds-pw").unwrap();
            let out = dir.path().join(format!("out-{tag}.bin"));
            extract_entry(&v, "big.bin", &out).unwrap();
            assert_eq!(
                std::fs::read(&out).unwrap(),
                body,
                "bounds round-trip {tag}"
            );
        }

        // Invalid bounds are rejected (defence against a hostile manifest).
        assert!(CdcBounds {
            min: 0,
            avg: 1024,
            max: 2048
        }
        .validate()
        .is_err());
        assert!(CdcBounds {
            min: 4096,
            avg: 3000,
            max: 8192
        }
        .validate()
        .is_err());
        assert!(CdcBounds::defaults().validate().is_ok());
        assert!(CdcBounds::for_level(19).validate().is_ok());
    }
}
