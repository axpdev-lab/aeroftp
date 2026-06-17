//! AeroVault v3 draft backend.
//!
//! v3 is the wrapper-stack format: content-defined chunks, keyed BLAKE3
//! chunk identifiers, zstd-per-chunk compression, AES-256-GCM-SIV content
//! encryption, and an extension directory reserved for v4 Error Correction.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use crate::aerocrypt::{
    decrypt_with_aad, derive_base_kek, encrypt_with_aad, hkdf_expand, random_array, unwrap_key,
    wrap_key, KEY_SIZE, SALT_SIZE, WRAPPED_KEY_SIZE,
};
use crate::error_correction::sidecar::{
    aerocorrect_sidecar_path, AeroCorrectSegment, AeroCorrectSidecar,
};
use crate::error_correction::{
    compute_error_correction_shards, compute_error_correction_shards_grid, compute_metadata_parity,
    manifest_error_correction_grid, reconstruct_from_error_correction,
    ERROR_CORRECTION_DEFAULT_PCT, ERROR_CORRECTION_MAX_PCT, ERROR_CORRECTION_MIN_PCT,
};
use crate::vault_telemetry::VaultReport;
use hmac::{Hmac, Mac};
use secrecy::zeroize::Zeroize;
use serde::{Deserialize, Serialize};
use sha2::Sha512;
use std::collections::{BTreeMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

const MAGIC: &[u8; 10] = b"AEROVAULT3";
const VERSION: u8 = 3;
const HEADER_SIZE: usize = 1024;
const HEADER_MAC_OFFSET: usize = 960;
const MAC_SIZE: usize = 64;
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

/// Extension ID for the v4 Error Correction (Reed-Solomon) layer.
/// This is emitted as a non-critical extension (critical=false) so that
/// pure v3 readers can still open and extract from v4+Error Correction vaults
/// (per the forward-compat contract in AEROVAULT-V3-SPEC.md and discussion #276).
const ERROR_CORRECTION_EXTENSION_ID: &str = "error-correction.reed-solomon";
// GAP-4: parity over the encrypted manifest itself (the "locator" that scrub needs).
// Stored as a second non-critical extension so a corrupted manifest can be rebuilt
// before any per-block cipher_hash is read. v3 readers skip it like any non-critical.
const ERROR_CORRECTION_META_EXTENSION_ID: &str = "error-correction-metadata.reed-solomon";
const ERROR_CORRECTION_ALGORITHM_ID: &str = "reed-solomon";
const ERROR_CORRECTION_ALGORITHM_VERSION: u32 = 1;

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
    /// QR-style Error Correction overhead level (#276), as a target storage-overhead
    /// percentage. Drives the Reed-Solomon (K, P) grid for both embedded re-seals and
    /// detached `export-parity`. Absent in pre-knob vaults and in vaults without Error
    /// Correction; readers then fall back to `ERROR_CORRECTION_DEFAULT_PCT` (the
    /// original fixed K=10/P=2 grid), keeping every existing vault byte-compatible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error_correction_pct: Option<u32>,
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

// ---------------------------------------------------------------------------
// Detached Error Correction placement (the "sidecar" file, `.aerocorrect`).
//
// Design (par2-inspired, see APPENDIX-AEROVAULT-V4-ECC and #276): the recovery
// data lives in a SEPARATE file alongside the vault instead of embedded in the
// container. The reconstruction engine is already placement-agnostic
// (`reconstruct_from_error_correction` takes the parity bytes), so a detached
// file simply carries the same `ErrorCorrectionPayload` AVEC blobs.
//
// The on-disk format is the unified `.aerocorrect` sidecar shared with AeroSync
// (Ehud's #276 call: one detached parity format for any file). That format is
// multi-segment: a vault writes exactly three windows over its container file, in
// a fixed order, so each region's parity is found by position:
//   segment 0 = header window   [0, HEADER_SIZE)          -> header parity
//   segment 1 = manifest window [manifest_offset, +len)   -> manifest (locator) parity
//   segment 2 = data window     [data_offset, +len)       -> data-block parity
// An empty `avec_bytes` means "this region is not protected". The header and
// manifest cannot self-locate their own recovery once damaged (chicken-and-egg),
// which is exactly why their parity must travel OUTSIDE the container, here.
//
// Binding is by content (the unified format stores the container's SHA-256), but
// the vault never enforces that binding on the repair path: a vault being repaired
// is corrupt by definition, so its live content cannot match the good hash. The
// real safety gate is unchanged: every reconstructed region is re-verified against
// the vault's authenticated values (header MAC / manifest cipher_hash) before being
// persisted, so a foreign or stale sidecar can only make a repair FAIL, never
// overwrite good data.

/// Segment roles in a vault's `.aerocorrect` sidecar, by fixed index.
const VAULT_SIDECAR_SEG_HEADER: usize = 0;
const VAULT_SIDECAR_SEG_MANIFEST: usize = 1;
const VAULT_SIDECAR_SEG_DATA: usize = 2;

/// Where Error Correction parity lives relative to the vault container.
///
/// - `Embedded`: parity is a non-critical extension inside the `.aerovault` file,
///   recomputed on every seal (auto-fresh, but it grows the container).
/// - `Detached`: parity lives in a sibling `.aerocorrect` sidecar; the container
///   stays byte-identical to a non-Error-Correction vault (Ehud's "storage view stays
///   stable" point on #276). The sidecar is regenerated on demand via `export-parity`
///   (par2 semantics: edit the data, regenerate the recovery file).
/// - `Both`: embed AND write the sidecar (detached-first, the order Ehud asked for).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryPlacement {
    Embedded,
    Detached,
    Both,
}

impl RecoveryPlacement {
    fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "embedded" => Ok(Self::Embedded),
            "detached" => Ok(Self::Detached),
            "both" => Ok(Self::Both),
            other => Err(format!(
                "Unknown recovery placement: {other} (expected embedded|detached|both)"
            )),
        }
    }

    /// True when the placement keeps a copy of the parity inside the container.
    fn embeds(self) -> bool {
        matches!(self, Self::Embedded | Self::Both)
    }

    /// True when the placement writes a `.aerocorrect` sidecar.
    fn writes_sidecar(self) -> bool {
        matches!(self, Self::Detached | Self::Both)
    }
}

/// Which source `resolve_parity_source` ended up using, for honest reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParitySource {
    Explicit,
    Detached,
    Embedded,
    None,
}

impl ParitySource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Detached => "detached",
            Self::Embedded => "embedded",
            Self::None => "none",
        }
    }
}

/// Default sidecar path for a vault: any file `X` gets `X.aerocorrect`, the same
/// convention AeroSync uses, so the exporter and the resolver always agree on one
/// location and the format is uniform across the app.
fn default_sidecar_path(vault_path: &Path) -> PathBuf {
    PathBuf::from(aerocorrect_sidecar_path(&vault_path.to_string_lossy()))
}

/// SHA-256 of a byte slice (the unified `.aerocorrect` format binds by content hash).
fn container_sha256(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(data);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// Build a vault's `.aerocorrect` sidecar from the three region parities, in the
/// fixed segment order (header, manifest, data). `container_bytes` are the on-disk
/// vault file bytes the sidecar protects; their SHA-256 is the unified format's
/// content binding (informational for the vault, never enforced on the repair path,
/// where the container is corrupt by definition). An empty parity vector marks an
/// unprotected region.
fn build_vault_sidecar(
    container_bytes: &[u8],
    header: &VaultHeaderV3,
    data_parity: Vec<u8>,
    manifest_parity: Vec<u8>,
    header_parity: Vec<u8>,
) -> AeroCorrectSidecar {
    let segments = vec![
        AeroCorrectSegment {
            window_offset: 0,
            window_len: HEADER_SIZE as u64,
            avec_bytes: header_parity,
        },
        AeroCorrectSegment {
            window_offset: header.manifest_offset,
            window_len: header.manifest_len,
            avec_bytes: manifest_parity,
        },
        AeroCorrectSegment {
            window_offset: header.data_offset,
            window_len: header.data_len,
            avec_bytes: data_parity,
        },
    ];
    AeroCorrectSidecar::new(
        container_sha256(container_bytes),
        container_bytes.len() as u64,
        segments,
    )
}

/// The parity (AVEC) bytes a vault sidecar carries for one region, by fixed segment
/// index. Returns an empty slice when the region is absent or unprotected.
fn vault_sidecar_parity(sidecar: &AeroCorrectSidecar, role: usize) -> &[u8] {
    sidecar
        .segments
        .get(role)
        .map(|s| s.avec_bytes.as_slice())
        .unwrap_or(&[])
}

/// Collect the live on-disk blocks in data-section order ([u64 len][ciphertext]),
/// the exact stream the Error Correction parity is computed over. A block whose
/// recorded range falls outside the data section contributes an empty slice, matching
/// the seal path's handling of truncated blocks.
fn collect_live_block_refs(vault: &OpenVaultV3) -> Vec<&[u8]> {
    let mut ranges: Vec<(usize, usize)> = vault
        .manifest
        .chunks
        .values()
        .map(|rec| (rec.data_offset as usize, 8 + rec.block_len as usize))
        .collect();
    ranges.sort_by_key(|(off, _)| *off);
    ranges
        .into_iter()
        .map(|(start, full_len)| {
            if start + full_len <= vault.data.len() {
                &vault.data[start..start + full_len]
            } else {
                &[] as &[u8]
            }
        })
        .collect()
}

/// Read the embedded `ERROR_CORRECTION_EXTENSION_ID` payload from the vault file,
/// if present and non-empty. The extension is located via the MAC-verified header.
fn read_embedded_error_correction(vault: &OpenVaultV3) -> Result<Option<Vec<u8>>, String> {
    let entry = match vault
        .extensions
        .iter()
        .find(|e| e.extension_id == ERROR_CORRECTION_EXTENSION_ID)
    {
        Some(e) if e.length > 0 => e.clone(),
        _ => return Ok(None),
    };
    let mut f = File::open(&vault.path).map_err(|e| format!("open for parity read: {e}"))?;
    let abs = vault.header.extension_payload_offset + entry.offset;
    f.seek(SeekFrom::Start(abs))
        .map_err(|e| format!("seek for parity read: {e}"))?;
    let mut b = vec![0u8; entry.length as usize];
    f.read_exact(&mut b)
        .map_err(|e| format!("read error_correction payload: {e}"))?;
    Ok(Some(b))
}

/// Resolve the Error Correction data-block parity bytes for a vault, in priority
/// order: explicit `--parity` path -> detached `.aerocorrect` sidecar -> embedded
/// extension. A detached/explicit sidecar is parsed (its own integrity check rejects
/// truncated or corrupt files), but its content binding is NOT enforced here: a vault
/// being repaired is corrupt by definition, so its live content cannot match the
/// sidecar's good-content hash. A foreign or stale sidecar can only feed parity that
/// fails the caller's post-reconstruction re-verify (header MAC / manifest
/// cipher_hash), so it can make a repair FAIL but never overwrite good data.
/// Returns the raw AVEC payload plus which source it came from, for honest reporting.
fn resolve_parity_source(
    vault: &OpenVaultV3,
    explicit: Option<&Path>,
) -> Result<(Vec<u8>, ParitySource), String> {
    // 1. Explicit --parity wins; an unreadable or malformed file is a hard error
    //    because the user named it on purpose.
    if let Some(p) = explicit {
        let bytes =
            std::fs::read(p).map_err(|e| format!("read recovery file {}: {e}", p.display()))?;
        let sidecar = AeroCorrectSidecar::from_bytes(&bytes)?;
        return Ok((
            vault_sidecar_parity(&sidecar, VAULT_SIDECAR_SEG_DATA).to_vec(),
            ParitySource::Explicit,
        ));
    }
    // 2. Detached sidecar next to the vault.
    let sidecar_path = default_sidecar_path(&vault.path);
    if sidecar_path.exists() {
        let bytes = std::fs::read(&sidecar_path)
            .map_err(|e| format!("read recovery file {}: {e}", sidecar_path.display()))?;
        let sidecar = AeroCorrectSidecar::from_bytes(&bytes)?;
        return Ok((
            vault_sidecar_parity(&sidecar, VAULT_SIDECAR_SEG_DATA).to_vec(),
            ParitySource::Detached,
        ));
    }
    // 3. Embedded extension payload.
    if let Some(bytes) = read_embedded_error_correction(vault)? {
        return Ok((bytes, ParitySource::Embedded));
    }
    Err(
        "No Error Correction parity available (no --parity, no .aerocorrect sidecar, no embedded extension)"
            .to_string(),
    )
}

/// Read the raw on-disk encrypted-manifest bytes (the exact bytes the manifest
/// parity must protect; re-encrypting would use a fresh nonce and not match).
fn read_manifest_raw(vault_path: &Path, header: &VaultHeaderV3) -> Result<Vec<u8>, String> {
    let mut f = File::open(vault_path).map_err(|e| format!("open for manifest read: {e}"))?;
    read_capped(
        &mut f,
        header.manifest_offset,
        header.manifest_len,
        MAX_MANIFEST_SIZE,
        "manifest",
    )
}

/// Outcome of `export_parity`, surfaced verbatim by the CLI/Tauri layers.
struct ExportParityResult {
    path: PathBuf,
    shards: u64,
    bytes_protected: u64,
    overhead_pct: f64,
    payload_len: u64,
    file_len: u64,
    /// Bytes of header parity carried in the sidecar (0 when none).
    header_parity_len: u64,
    /// Bytes of manifest (locator) parity carried in the sidecar (0 when none).
    manifest_parity_len: u64,
}

/// Write a detached `.aerocorrect` recovery file for an existing vault. This is
/// the "add parity later" win over Kopia (which can only enable ECC at repo
/// creation): the encrypted container is read but never rewritten. Defaults to
/// `<vault>.aerocorrect`; pass `out_path` to override.
fn export_parity(
    vault_path: &Path,
    password: &str,
    out_path: Option<&Path>,
) -> Result<ExportParityResult, String> {
    // Take the same write lock the mutating ops use so a concurrent add cannot
    // race the read and produce a sidecar bound to a half-written data section.
    let _lock = acquire_vault_write_lock(vault_path)?;
    let vault = open_vault(vault_path, password)?;
    // QR-style overhead level (#276): the grid recorded on the vault drives every
    // parity section so a detached sidecar matches the user's chosen overhead.
    let (k, p) = manifest_error_correction_grid(vault.manifest.error_correction_pct);
    let blocks = collect_live_block_refs(&vault);
    let (payload, shards, protected, overhead) =
        compute_error_correction_shards_grid(&blocks, k, p);
    // GAP-4 metadata bundle: protect the two regions the detached container cannot
    // self-locate once damaged. The header parity is computed over the authoritative
    // in-memory header (`to_bytes()` round-trips a clean header byte-for-byte, and
    // heals one that open had to rebuild); the manifest parity is over the exact
    // on-disk encrypted bytes (a re-encrypt would use a fresh nonce and not match).
    let header_parity = compute_metadata_parity(&vault.header.to_bytes(), k, p);
    let manifest_raw = read_manifest_raw(vault_path, &vault.header)?;
    let manifest_parity = compute_metadata_parity(&manifest_raw, k, p);
    let payload_len = payload.len() as u64;
    let header_parity_len = header_parity.len() as u64;
    let manifest_parity_len = manifest_parity.len() as u64;
    // Content binding: the SHA-256 of the on-disk container we are protecting. Read
    // under the same write lock, so the file is stable.
    let container_bytes = std::fs::read(vault_path)
        .map_err(|e| format!("read vault container for sidecar binding: {e}"))?;
    let sidecar = build_vault_sidecar(
        &container_bytes,
        &vault.header,
        payload,
        manifest_parity,
        header_parity,
    );
    let bytes = sidecar.to_bytes();
    let out = out_path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| default_sidecar_path(vault_path));
    atomic_write(&out, &bytes)?;
    Ok(ExportParityResult {
        path: out,
        shards,
        bytes_protected: protected,
        overhead_pct: overhead,
        payload_len,
        file_len: bytes.len() as u64,
        header_parity_len,
        manifest_parity_len,
    })
}

/// Outcome of `strip_parity`.
struct StripParityResult {
    sidecar_present: bool,
    sidecar_path: PathBuf,
}

/// Drop the embedded Error Correction extension on the next seal. Refuses unless a
/// detached sidecar already exists or `force` is set, so a vault is never silently
/// left with zero recovery.
fn strip_parity(
    vault_path: &Path,
    password: &str,
    force: bool,
) -> Result<StripParityResult, String> {
    let _lock = acquire_vault_write_lock(vault_path)?;
    let mut vault = open_vault(vault_path, password)?;
    let had_embedded = vault
        .extensions
        .iter()
        .any(|e| e.extension_id == ERROR_CORRECTION_EXTENSION_ID);
    if !had_embedded {
        return Err("Vault has no embedded Error Correction parity to strip".to_string());
    }
    let sidecar = default_sidecar_path(vault_path);
    let has_sidecar = sidecar.exists();
    if !has_sidecar && !force {
        return Err(
            "Refusing to strip embedded parity: no detached recovery file exists. Run \"vault export-parity\" first, or pass --force to drop recovery entirely."
                .to_string(),
        );
    }
    vault
        .extensions
        .retain(|e| e.extension_id != ERROR_CORRECTION_EXTENSION_ID);
    save_open_vault(&mut vault)?;
    Ok(StripParityResult {
        sidecar_present: has_sidecar,
        sidecar_path: sidecar,
    })
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

        let stored_len =
            u64::from_le_bytes(vault.data[start..start + 8].try_into().expect("slice")) as usize;

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

fn repair_vault(
    vault: &mut OpenVaultV3,
    dry_run: bool,
    parity: Option<&Path>,
) -> Result<(usize, ParitySource), String> {
    let damaged = scrub_vault(vault);
    if damaged.is_empty() {
        // GAP-4 / HEADER: no damaged data blocks, but if open had to rebuild a
        // corrupted manifest (metadata parity) or header (sidecar header parity),
        // persist the healed region. The seal rewrites the header with a fresh MAC
        // and re-encrypts the (correct, in-memory) manifest, regenerating parities.
        if vault.manifest_repaired_on_open || vault.header_repaired_on_open {
            if !dry_run {
                save_open_vault(vault)?;
            }
            return Ok((1, ParitySource::None));
        }
        return Ok((0, ParitySource::None));
    }

    // Resolve parity in priority order (explicit -> detached sidecar -> embedded).
    // An explicitly named source that fails is a hard error; a missing default
    // source just means "nothing to repair from", leaving the vault untouched.
    let (error_correction_bytes, source) = match resolve_parity_source(vault, parity) {
        Ok((b, s)) => (Some(b), s),
        Err(e) => {
            if parity.is_some() {
                return Err(e);
            }
            (None, ParitySource::None)
        }
    };

    let mut repaired_count = 0;

    if let Some(error_correction_b) = error_correction_bytes {
        let mut ordered: Vec<(String, ChunkRecordV3)> = vault
            .manifest
            .chunks
            .iter()
            .map(|(id, r)| (id.clone(), r.clone()))
            .collect();
        ordered.sort_by_key(|(_, r)| r.data_offset);

        let mut blocks: Vec<Vec<u8>> = ordered
            .iter()
            .map(|(_, rec)| {
                let start = rec.data_offset as usize;
                let full = 8 + rec.block_len as usize;
                // Always a fixed-length (8 + block_len) buffer so the concatenated stream
                // length matches what the Error Correction parity was computed over, even when the block
                // is truncated on disk: the missing tail is zero-padded here, flagged as
                // damaged by its shard checksum, then reconstructed.
                let mut buf = vec![0u8; full];
                if start < vault.data.len() {
                    let avail = (vault.data.len() - start).min(full);
                    buf[..avail].copy_from_slice(&vault.data[start..start + avail]);
                }
                buf
            })
            .collect();

        let bad_indices: Vec<usize> = damaged
            .iter()
            .filter_map(|d| ordered.iter().position(|(id, _)| id == &d.record.id))
            .collect();

        let _ = reconstruct_from_error_correction(&mut blocks, &error_correction_b)?;

        // Safety gate (CLAUDE-AV-ECC-01): RS reconstruction is only correct when
        // the surviving data shards AND the parity shards were themselves intact.
        // The parity lives in the extension payload, which scrub does not cover, so
        // a rotted parity shard (or more erasures than parity in a stripe) silently
        // yields wrong bytes. Verify every reconstructed block against its
        // authenticated manifest cipher_hash before trusting it, including blocks
        // that scrub considered healthy: a stale/foreign sidecar can mark those
        // shards as erasures and rewrite them too. Persist only when the entire
        // reconstructed stream matches the manifest, preserving the all-or-nothing
        // repair safety contract ("never overwrite without hash verification").
        let block_verified = |i: usize| {
            let blk = &blocks[i];
            if blk.len() < 8 {
                return false;
            }
            let body_u64 = u64::from_le_bytes(blk[0..8].try_into().unwrap());
            let Ok(body) = usize::try_from(body_u64) else {
                return false;
            };
            let Some(full_len) = 8usize.checked_add(body) else {
                return false;
            };
            blk.len() == full_len
                && body_u64 == ordered[i].1.block_len
                && blake3::hash(&blk[8..8 + body]).to_hex().to_string() == ordered[i].1.cipher_hash
        };
        let all_verified = blocks.iter().enumerate().all(|(i, _)| block_verified(i));

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

    Ok((repaired_count, source))
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
    /// GAP-4: set when open_vault had to rebuild a corrupted encrypted manifest
    /// from the metadata parity. Repair persists the healed manifest even when no
    /// data block is damaged. Not persisted (recomputed per open).
    manifest_repaired_on_open: bool,
    /// HEADER parity: set when open_vault had to rebuild a corrupted 1024-byte
    /// header from the detached sidecar's header parity. Repair persists the healed
    /// header even when no data block is damaged. Not persisted (recomputed per open).
    header_repaired_on_open: bool,
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

#[cfg(not(feature = "test-vectors"))]
fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Fixed timestamp under the `test-vectors` feature so the AEROVAULT3 cross-impl
/// golden is reproducible. MUST match the `aerovault` crate's test-vector value.
#[cfg(feature = "test-vectors")]
fn now_iso() -> String {
    "2026-06-17T00:00:00Z".to_string()
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

fn derive_keks(base_kek: &[u8; KEY_SIZE]) -> Result<([u8; KEY_SIZE], [u8; KEY_SIZE]), String> {
    Ok((
        hkdf_expand::<KEY_SIZE>(base_kek, HKDF_MASTER)?,
        hkdf_expand::<KEY_SIZE>(base_kek, HKDF_MAC)?,
    ))
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
        error_correction_pct: None,
    }
}

/// Returns the stub ExtensionEntry for the Error Correction layer (length 0 payload for Phase 1 stub).
/// The actual payload (Reed-Solomon shards) will be written in Phase 2.
/// Marked non-critical so v3 readers can still extract.
fn error_correction_stub_extension() -> ExtensionEntryV3 {
    ExtensionEntryV3 {
        extension_id: ERROR_CORRECTION_EXTENSION_ID.to_string(),
        algorithm_id: ERROR_CORRECTION_ALGORITHM_ID.to_string(),
        algorithm_version: ERROR_CORRECTION_ALGORITHM_VERSION,
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
    extension_payloads: &[u8], // content of the payload area; entries' offset/len are relative to this
    data: &[u8],
) -> Result<Vec<u8>, String> {
    let encrypted_manifest = encrypt_manifest(master_key, manifest)?;

    // GAP-4: when Error Correction is enabled (the data-block parity extension is
    // present), also protect the locator. The per-block cipher_hash that scrub reads
    // lives inside the encrypted manifest, so a corrupted manifest would leave scrub
    // with no map to repair from. Compute a fixed-rate Reed-Solomon parity over the
    // encrypted manifest bytes (reusing the v2 grid, manifest treated as one block)
    // and store it as a second non-critical extension, rebuilt on every seal. It is
    // located via the MAC-verified header (whose offsets survive a manifest hit), so
    // repair can rebuild the manifest before reading any cipher_hash.
    let mut extensions: Vec<ExtensionEntryV3> = extensions
        .iter()
        .filter(|e| e.extension_id != ERROR_CORRECTION_META_EXTENSION_ID)
        .cloned()
        .collect();
    let mut extension_payloads = extension_payloads.to_vec();
    if extensions
        .iter()
        .any(|e| e.extension_id == ERROR_CORRECTION_EXTENSION_ID)
    {
        let (k, p) = manifest_error_correction_grid(manifest.error_correction_pct);
        let (meta_payload, _shards, _prot, _ov) =
            compute_error_correction_shards_grid(&[&encrypted_manifest], k, p);
        extensions.push(ExtensionEntryV3 {
            extension_id: ERROR_CORRECTION_META_EXTENSION_ID.to_string(),
            algorithm_id: ERROR_CORRECTION_ALGORITHM_ID.to_string(),
            algorithm_version: ERROR_CORRECTION_ALGORITHM_VERSION,
            critical: false,
            offset: extension_payloads.len() as u64,
            length: meta_payload.len() as u64,
        });
        extension_payloads.extend_from_slice(&meta_payload);
    }
    let extension_dir =
        serde_json::to_vec(&extensions).map_err(|e| format!("Extension serialize: {e}"))?;

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
        HEADER_SIZE
            + data.len()
            + encrypted_manifest.len()
            + extension_dir.len()
            + extension_payloads.len(),
    );
    out.extend_from_slice(&header.to_bytes());
    out.extend_from_slice(data);
    out.extend_from_slice(&encrypted_manifest);
    out.extend_from_slice(&extension_dir);
    out.extend_from_slice(&extension_payloads);
    Ok(out)
}

fn create_empty_vault(
    path: &Path,
    password: &str,
    level: i32,
    error_correction: Option<RecoveryPlacement>,
    error_correction_pct: u32,
) -> Result<(), String> {
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

    let mut manifest = empty_manifest(level);
    // Record the QR-style overhead level so every later seal / export uses the same
    // grid (#276). Only meaningful when Error Correction is enabled.
    if error_correction.is_some() {
        manifest.error_correction_pct =
            Some(error_correction_pct.clamp(ERROR_CORRECTION_MIN_PCT, ERROR_CORRECTION_MAX_PCT));
    }
    // Embed the extension only when the placement keeps an in-container copy.
    let embed = error_correction.is_some_and(|p| p.embeds());
    let mut extensions = if embed {
        vec![error_correction_stub_extension()]
    } else {
        vec![]
    };
    let ext_payloads = if embed {
        let (p, _shards, _prot, _ov) = compute_error_correction_shards(&[]);
        if let Some(e) = extensions.first_mut() {
            e.offset = 0;
            e.length = p.len() as u64;
        }
        p
    } else {
        vec![]
    };
    let bytes = build_file_bytes(
        header,
        &mac_key,
        &master_key,
        &manifest,
        &extensions,
        &ext_payloads,
        &[],
    )?;
    master_key.zeroize();
    mac_key.zeroize();
    atomic_write(path, &bytes)?;

    // Detached/both placements seed the sidecar so the file exists from creation.
    // An empty vault has an empty parity payload; re-run `export-parity` after
    // adding files (par2 semantics: the recovery file tracks a fixed data set).
    if error_correction.is_some_and(|p| p.writes_sidecar()) {
        // An empty vault has no data, manifest, or stable header parity yet; seed the
        // three regions with empty parity so the file exists from creation.
        // `export-parity` fills them once files are added (par2 semantics: protect a
        // fixed set). Offsets/lengths match the empty-vault header above.
        let segments = vec![
            AeroCorrectSegment {
                window_offset: 0,
                window_len: HEADER_SIZE as u64,
                avec_bytes: Vec::new(),
            },
            AeroCorrectSegment {
                window_offset: DATA_OFFSET,
                window_len: 0,
                avec_bytes: Vec::new(),
            },
            AeroCorrectSegment {
                window_offset: DATA_OFFSET,
                window_len: 0,
                avec_bytes: Vec::new(),
            },
        ];
        let sidecar =
            AeroCorrectSidecar::new(container_sha256(&bytes), bytes.len() as u64, segments);
        atomic_write(&default_sidecar_path(path), &sidecar.to_bytes())?;
    }
    Ok(())
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

/// A parsed, MAC-verified header together with its unwrapped MAC and master keys
/// (the result of unlocking a header with the vault password).
type UnlockedHeader = (VaultHeaderV3, [u8; KEY_SIZE], [u8; KEY_SIZE]);

/// Load a header from raw bytes and unlock it with `password`, returning the parsed
/// header plus the unwrapped MAC and master keys. This is the on-disk happy path and
/// also the verification step a sidecar-reconstructed header must pass: a successful
/// MAC verify on the (possibly rebuilt) bytes is the proof the header is correct.
fn open_header_bytes(header_bytes: &[u8], password: &str) -> Result<UnlockedHeader, String> {
    let header = VaultHeaderV3::from_bytes(header_bytes)?;
    let mut base_kek = derive_base_kek(password, &header.salt)?;
    let (kek_master, kek_mac) = derive_keks(&base_kek)?;
    base_kek.zeroize();
    let mac_key = unwrap_key(&kek_mac, &header.wrapped_mac_key)?;
    header.verify_mac(&mac_key)?;
    // Reject an unknown (authenticated) wrapper-header version instead of decoding it
    // with the hardcoded cipher stack (CLAUDE-AV-024 / CODEX-AV-006).
    if header.wrapper_header_version != SUPPORTED_WRAPPER_HEADER_VERSION {
        return Err(format!(
            "Unsupported AeroVault v3 wrapper-header version: {} (expected {})",
            header.wrapper_header_version, SUPPORTED_WRAPPER_HEADER_VERSION
        ));
    }
    let master_key = unwrap_key(&kek_master, &header.wrapped_master_key)?;
    Ok((header, mac_key, master_key))
}

/// HEADER parity: rebuild a corrupted 1024-byte header from the detached sidecar's
/// header parity. The header cannot point at embedded recovery once it is itself
/// damaged, so its parity lives in the sidecar (the only out-of-container store).
/// Returns the verified header + keys when a sidecar carries header parity and the
/// rebuild unlocks with `password`; `Ok(None)` when there is no sidecar / no header
/// parity / the rebuild does not verify (the caller then keeps the original open
/// error). The MAC verify inside `open_header_bytes` is the correctness proof.
fn recover_header_from_sidecar(
    vault_path: &Path,
    on_disk_header: &[u8],
    password: &str,
) -> Result<Option<UnlockedHeader>, String> {
    let sidecar_path = default_sidecar_path(vault_path);
    if !sidecar_path.exists() {
        return Ok(None);
    }
    let bytes = match std::fs::read(&sidecar_path) {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };
    let sidecar = match AeroCorrectSidecar::from_bytes(&bytes) {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };
    let header_parity = vault_sidecar_parity(&sidecar, VAULT_SIDECAR_SEG_HEADER);
    if header_parity.is_empty() {
        return Ok(None);
    }
    // The header is a single fixed-length block; reconstruct requires the on-disk
    // bytes at exactly HEADER_SIZE so the stream lengths line up.
    if on_disk_header.len() != HEADER_SIZE {
        return Ok(None);
    }
    let mut blocks = vec![on_disk_header.to_vec()];
    if reconstruct_from_error_correction(&mut blocks, header_parity).is_err() {
        return Ok(None);
    }
    let rebuilt = match blocks.into_iter().next() {
        Some(b) if b.len() == HEADER_SIZE => b,
        _ => return Ok(None),
    };
    match open_header_bytes(&rebuilt, password) {
        Ok(triple) => Ok(Some(triple)),
        Err(_) => Ok(None),
    }
}

/// GAP-4 (detached): rebuild a corrupted encrypted manifest from the sidecar's
/// manifest parity. Used as a fallback after the embedded metadata extension on the
/// detached path, where the container keeps no embedded copy. Returns the rebuilt
/// encrypted-manifest bytes; the caller proves correctness by a successful AEAD
/// decrypt. `Ok(None)` when there is no sidecar / no manifest parity / no change.
fn reconstruct_manifest_from_sidecar(
    vault_path: &Path,
    on_disk_manifest: &[u8],
) -> Result<Option<Vec<u8>>, String> {
    let sidecar_path = default_sidecar_path(vault_path);
    if !sidecar_path.exists() {
        return Ok(None);
    }
    let bytes = match std::fs::read(&sidecar_path) {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };
    let sidecar = match AeroCorrectSidecar::from_bytes(&bytes) {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };
    let manifest_parity = vault_sidecar_parity(&sidecar, VAULT_SIDECAR_SEG_MANIFEST);
    if manifest_parity.is_empty() {
        return Ok(None);
    }
    let mut blocks = vec![on_disk_manifest.to_vec()];
    if reconstruct_from_error_correction(&mut blocks, manifest_parity).is_err() {
        return Ok(None);
    }
    Ok(blocks.into_iter().next())
}

/// Which GAP-4 metadata regions a vault's detached sidecar protects, read without the
/// password (the sidecar is a separate, self-integrity-checked file). Returns
/// `(manifest_parity_present, header_parity_present)`; `(false, false)` when there is
/// no sidecar or it cannot be parsed.
fn sidecar_parity_flags(vault_path: &Path) -> (bool, bool) {
    let sidecar_path = default_sidecar_path(vault_path);
    match std::fs::read(&sidecar_path)
        .ok()
        .and_then(|b| AeroCorrectSidecar::from_bytes(&b).ok())
    {
        Some(sidecar) => (
            !vault_sidecar_parity(&sidecar, VAULT_SIDECAR_SEG_MANIFEST).is_empty(),
            !vault_sidecar_parity(&sidecar, VAULT_SIDECAR_SEG_HEADER).is_empty(),
        ),
        None => (false, false),
    }
}

/// GAP-4: try to rebuild a corrupted encrypted manifest from the metadata-parity
/// extension. The extension directory and payload are located via the MAC-verified
/// header, whose offsets survive a manifest-region hit. Returns the reconstructed
/// encrypted-manifest bytes when a metadata extension is present, or `None` when
/// there is none (the caller then keeps the original decrypt error). Correctness is
/// not asserted here: the caller re-runs `decrypt_manifest`, whose AEAD authentication
/// is the proof that the reconstruction is right.
fn reconstruct_encrypted_manifest(
    file: &mut std::fs::File,
    header: &VaultHeaderV3,
    file_len: u64,
) -> Result<Option<Vec<u8>>, String> {
    let ext_json = read_capped(
        file,
        header.extension_dir_offset,
        header.extension_dir_len,
        MAX_EXTENSION_DIR_SIZE,
        "extension directory",
    )?;
    let extensions: Vec<ExtensionEntryV3> = match serde_json::from_slice(&ext_json) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let meta = match extensions
        .iter()
        .find(|e| e.extension_id == ERROR_CORRECTION_META_EXTENSION_ID)
    {
        Some(m) if m.length > 0 => m,
        _ => return Ok(None),
    };
    let payload_abs = header
        .extension_payload_offset
        .checked_add(meta.offset)
        .ok_or("metadata parity offset overflows")?;
    let end = payload_abs
        .checked_add(meta.length)
        .ok_or("metadata parity range overflows")?;
    if end > file_len {
        return Err("metadata parity range exceeds file size".to_string());
    }
    // Manifest parity is at most ~20% over the manifest plus a per-shard checksum
    // table; bound it generously by the manifest cap.
    let meta_payload = read_capped(
        file,
        payload_abs,
        meta.length,
        2 * MAX_MANIFEST_SIZE,
        "metadata parity",
    )?;
    let corrupt = read_capped(
        file,
        header.manifest_offset,
        header.manifest_len,
        MAX_MANIFEST_SIZE,
        "manifest",
    )?;
    let mut blocks = vec![corrupt];
    reconstruct_from_error_correction(&mut blocks, &meta_payload)?;
    Ok(blocks.into_iter().next())
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

    // HEADER parity: the on-disk header is the happy path. If it fails to parse or
    // its MAC does not verify (bit-rot / bad sector), fall back to rebuilding it from
    // the detached sidecar's header parity. A missing sidecar / no header parity /
    // a rebuild that still does not unlock keeps the original error.
    let (header, mac_key, master_key, header_repaired_on_open) =
        match open_header_bytes(&header_bytes, password) {
            Ok((h, mac, master)) => (h, mac, master, false),
            Err(orig) => match recover_header_from_sidecar(&path, &header_bytes, password)? {
                Some((h, mac, master)) => (h, mac, master, true),
                None => return Err(orig),
            },
        };

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
    let (manifest, manifest_repaired_on_open) =
        match decrypt_manifest(&master_key, &encrypted_manifest) {
            Ok(m) => (m, false),
            Err(orig) => {
                // GAP-4: the manifest region may be corrupted (bit-rot, bad sector).
                // Rebuild the encrypted manifest from parity and retry. Try the
                // embedded metadata extension first (auto-fresh on the embedded path),
                // then the detached sidecar's manifest parity (the only copy a
                // pure-detached vault keeps). A successful AEAD decrypt on the rebuilt
                // bytes is the correctness proof; otherwise keep the original error.
                let embedded = reconstruct_encrypted_manifest(&mut file, &header, file_len)?;
                let rebuilt = match embedded {
                    Some(r) if r != encrypted_manifest => Some(r),
                    _ => reconstruct_manifest_from_sidecar(&path, &encrypted_manifest)?,
                };
                match rebuilt {
                    Some(r) if r != encrypted_manifest => {
                        (decrypt_manifest(&master_key, &r)?, true)
                    }
                    _ => return Err(orig),
                }
            }
        };
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

    let mut _has_error_correction = false;
    for ext in &extensions {
        if ext.extension_id == ERROR_CORRECTION_EXTENSION_ID {
            _has_error_correction = true;
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
    // `has_error_correction` is recorded for future use (scrub/repair paths, info surfaces).
    // The extensions vec is already stored in OpenVaultV3 for round-tripping.

    // Do not round-trip the GAP-4 metadata-parity extension; build_file_bytes is its
    // sole author and recomputes it on every seal from the freshly encrypted manifest.
    let extensions: Vec<ExtensionEntryV3> = extensions
        .into_iter()
        .filter(|e| e.extension_id != ERROR_CORRECTION_META_EXTENSION_ID)
        .collect();

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
        manifest_repaired_on_open,
        header_repaired_on_open,
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

fn save_open_vault(vault: &mut OpenVaultV3) -> Result<(), String> {
    assert_vault_generation_current(vault)?;

    let mut extensions = vault.extensions.clone();
    let mut ext_payloads = vec![];

    // P2-05: if Error Correction extension is present, recompute the shards on the current data
    // and update the entry + payload. Recompute on every seal (cost is acceptable
    // for the Error Correction use case; most vaults won't have it enabled).
    if let Some(error_correction_idx) = extensions
        .iter()
        .position(|e| e.extension_id == ERROR_CORRECTION_EXTENSION_ID)
    {
        // Collect on-disk blocks in the order they appear in the data section
        // (sorted by data_offset). Each full block is [u64 len][ciphertext of that len].
        let mut chunk_records: Vec<_> = vault.manifest.chunks.values().cloned().collect();
        chunk_records.sort_by_key(|r| r.data_offset);

        let blocks: Vec<&[u8]> = chunk_records
            .iter()
            .map(|rec| {
                let start = rec.data_offset as usize;
                let full_len = 8 + rec.block_len as usize;
                if start + full_len <= vault.data.len() {
                    &vault.data[start..start + full_len]
                } else {
                    &[] as &[u8]
                }
            })
            .collect();

        let (k, p) = manifest_error_correction_grid(vault.manifest.error_correction_pct);
        let (payload, shards, protected, overhead) =
            compute_error_correction_shards_grid(&blocks, k, p);

        let entry = &mut extensions[error_correction_idx];
        entry.offset = 0;
        entry.length = payload.len() as u64;

        ext_payloads = payload;

        // P3-03: surface Error Correction telemetry in the op report (when the caller set one, e.g. add_files).
        // This captures shards/bytes/overhead for receipts even if report op is "add_files" etc.
        if shards > 0 || protected > 0 {
            vault
                .report
                .set_error_correction_protection(shards, protected, overhead);
        }
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

/// Extract the whole vault tree into `dest_root`, recreating every entry's path
/// under it (Ehud #2: a one-click "extract all" instead of per-entry extraction).
/// Returns the number of files written. Every entry path is normalized first, so a
/// crafted manifest cannot escape `dest_root`.
fn extract_all_entries(vault: &OpenVaultV3, dest_root: &Path) -> Result<u64, String> {
    std::fs::create_dir_all(dest_root).map_err(|e| format!("Create output dir: {e}"))?;
    let mut entries: Vec<&ManifestEntryV3> = vault.manifest.entries.iter().collect();
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let mut files_written = 0u64;
    for entry in entries {
        let rel = normalize_vault_relative_path(&entry.path)?;
        let output = dest_root.join(&rel);
        if entry.is_dir {
            std::fs::create_dir_all(&output).map_err(|e| format!("Create output dir: {e}"))?;
        } else {
            extract_file_entry(vault, entry, &output)?;
            files_written += 1;
        }
    }
    Ok(files_written)
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
    create_empty_vault(
        Path::new(&vault_path),
        &password,
        level,
        None,
        ERROR_CORRECTION_DEFAULT_PCT,
    )?;
    Ok(vault_path)
}

/// Create a new AeroVault v3 container **with Reed-Solomon Error Correction**.
///
/// `placement` selects where the parity lives: `embedded` (default; non-critical
/// in-container extension, recomputed on every seal), `detached` (a sibling
/// `.aerocorrect` sidecar, container stays byte-identical to a plain vault), or
/// `both`. The embedded extension is non-critical so existing v3 readers can still
/// open the vault and extract data (per AEROVAULT-V3-SPEC.md + discussion #276).
#[tauri::command]
pub async fn vault_v3_create_with_error_correction(
    vault_path: String,
    password: String,
    profile: Option<String>,
    placement: Option<String>,
    error_correction_pct: Option<u32>,
) -> Result<String, String> {
    let level = match profile.as_deref() {
        Some("fast") => 3,
        Some("archive") => 19,
        Some("balanced") | None | Some("") => DEFAULT_ZSTD_LEVEL,
        Some(other) => return Err(format!("Unknown AeroVault v3 compression profile: {other}")),
    };
    let placement = match placement.as_deref() {
        None | Some("") => RecoveryPlacement::Embedded,
        Some(p) => RecoveryPlacement::parse(p)?,
    };
    // QR-style overhead level (#276): default 20% reproduces the original K=10/P=2 grid.
    let pct = error_correction_pct.unwrap_or(ERROR_CORRECTION_DEFAULT_PCT);
    create_empty_vault(
        Path::new(&vault_path),
        &password,
        level,
        Some(placement),
        pct,
    )?;
    Ok(vault_path)
}

/// Export a detached `.aerocorrect` recovery file for an existing vault. This is
/// the "add Error Correction later" path: the encrypted container is read but never
/// rewritten. Pass `out_path` to override the default `<vault>.aerocorrect`.
#[tauri::command]
pub async fn vault_v3_export_parity(
    vault_path: String,
    password: String,
    out_path: Option<String>,
) -> Result<serde_json::Value, String> {
    let out = out_path.as_deref().map(Path::new);
    let res = export_parity(Path::new(&vault_path), &password, out)?;
    Ok(serde_json::json!({
        "path": res.path.to_string_lossy(),
        "shards": res.shards,
        "bytes_protected": res.bytes_protected,
        "overhead_pct": res.overhead_pct,
        "payload_len": res.payload_len,
        "file_len": res.file_len,
        "header_parity_len": res.header_parity_len,
        "manifest_parity_len": res.manifest_parity_len,
    }))
}

/// Drop the embedded Error Correction extension from a vault on the next seal.
/// Refuses unless a detached sidecar exists or `force` is set, so a vault is never
/// silently left with zero recovery.
#[tauri::command]
pub async fn vault_v3_strip_parity(
    vault_path: String,
    password: String,
    force: bool,
) -> Result<serde_json::Value, String> {
    let res = strip_parity(Path::new(&vault_path), &password, force)?;
    Ok(serde_json::json!({
        "stripped": true,
        "sidecar_present": res.sidecar_present,
        "sidecar_path": res.sidecar_path.to_string_lossy(),
    }))
}

/// Report the Error Correction recovery surfaces available for a vault without the
/// password: `embedded` (in-container extension present) and `detached` (a sibling
/// `.aerocorrect` sidecar exists). Used by the GUI to show the badge and enable
/// scrub/repair when either source is present.
#[tauri::command]
pub async fn vault_v3_recovery_status(path: String) -> Result<serde_json::Value, String> {
    let embedded = vault_v3_has_error_correction(path.clone())
        .await
        .unwrap_or(false);
    let detached = default_sidecar_path(Path::new(&path)).exists();
    // The detached sidecar additionally reports which GAP-4 metadata regions it
    // protects (locator + 1024-byte header), so the GUI can show that detached
    // recovery covers the header, not just the data blocks.
    let (manifest_parity, header_parity) = sidecar_parity_flags(Path::new(&path));
    Ok(serde_json::json!({
        "embedded": embedded,
        "detached": detached,
        "any": embedded || detached,
        "manifest_parity": manifest_parity,
        "header_parity": header_parity,
    }))
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

/// Lightweight check for the presence of the Error Correction (error-correction) extension.
/// Does **not** require the vault password: it only reads the header and the
/// plaintext extension directory. This is safe for `vault info` / pre-flight
/// use cases and matches the "has_error_correction_extension" need from the plan (P1-05).
///
/// Returns true if a non-critical (or any) "error-correction.reed-solomon" entry is present
/// in the extension directory.
#[tauri::command]
pub async fn vault_v3_has_error_correction(path: String) -> Result<bool, String> {
    let mut file = std::fs::File::open(&path)
        .map_err(|e| format!("Open vault for Error Correction check: {e}"))?;

    let mut header_bytes = [0u8; HEADER_SIZE];
    file.read_exact(&mut header_bytes)
        .map_err(|e| format!("Read header for Error Correction check: {e}"))?;

    let header = VaultHeaderV3::from_bytes(&header_bytes)?;

    if header.extension_dir_len == 0 {
        return Ok(false);
    }

    let extension_json = read_capped(
        &mut file,
        header.extension_dir_offset,
        header.extension_dir_len,
        MAX_EXTENSION_DIR_SIZE,
        "extension directory (has_error_correction)",
    )?;

    let extensions: Vec<ExtensionEntryV3> = serde_json::from_slice(&extension_json)
        .map_err(|e| format!("Extension directory parse (has_error_correction): {e}"))?;

    Ok(extensions
        .iter()
        .any(|e| e.extension_id == ERROR_CORRECTION_EXTENSION_ID))
}

#[tauri::command]
pub async fn vault_v3_scrub(
    vault_path: String,
    password: String,
    parity_path: Option<String>,
) -> Result<serde_json::Value, String> {
    let vault = open_vault(&vault_path, &password)?;
    let checked = vault.manifest.chunks.len();
    let damaged = scrub_vault(&vault);
    // Pre-flight: report which parity source a repair would draw from. An
    // explicitly named source that fails is a hard error; an absent default
    // source reports "none".
    let explicit = parity_path.as_deref().map(Path::new);
    let parity_source = match resolve_parity_source(&vault, explicit) {
        Ok((_, s)) => s,
        Err(e) => {
            if explicit.is_some() {
                return Err(e);
            }
            ParitySource::None
        }
    };
    let list: Vec<_> = damaged
        .into_iter()
        .map(|d| {
            serde_json::json!({
                "id": d.record.id,
                "on_disk_start": d.on_disk_start,
                "on_disk_len": d.on_disk_len,
                "cipher_hash": d.record.cipher_hash,
            })
        })
        .collect();
    Ok(serde_json::json!({
        "damaged": list,
        "count": list.len(),
        "checked": checked,
        "parity_source": parity_source.as_str(),
    }))
}

#[tauri::command]
pub async fn vault_v3_repair(
    vault_path: String,
    password: String,
    dry_run: bool,
    parity_path: Option<String>,
) -> Result<serde_json::Value, String> {
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
    let explicit = parity_path.as_deref().map(Path::new);
    let (repaired, source) = repair_vault(&mut vault, dry_run, explicit)?;
    Ok(serde_json::json!({
        "repaired": repaired,
        "damaged": damaged,
        "dry_run": dry_run,
        "parity_source": source.as_str(),
    }))
}

/// Peak in-memory multiplier over the projected container size. While sealing,
/// AeroVault v3 holds the data plus working copies (plaintext, compressed,
/// encrypted), so the real peak is a few times the stored bytes.
const VAULT_MEMORY_PEAK_MULTIPLIER: u64 = 3;
/// Memory budget used when available RAM cannot be read (a conservative floor).
const VAULT_MEMORY_FIXED_BUDGET: u64 = 2 * 1024 * 1024 * 1024;

/// Human-readable byte size for user-facing guard messages.
fn human_size(n: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

/// Best-effort available physical memory in bytes. Linux reads
/// `/proc/meminfo MemAvailable`; other platforms return `None` and the caller
/// falls back to a fixed budget.
fn available_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
        for line in meminfo.lines() {
            if let Some(rest) = line.strip_prefix("MemAvailable:") {
                let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
                return Some(kb.saturating_mul(1024));
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Ehud #2: AeroVault v3 buffers the whole container in memory during create/add,
/// so a multi-GB input OOMs (real streaming I/O is the proper fix, tracked as
/// debt). Reject up front, with a clear message instead of a crash, when the
/// projected peak in-memory size would blow the budget. `existing` is taken from
/// the vault file on disk; `sources` are the files about to be added.
fn preflight_vault_memory_guard(
    vault_path: &Path,
    sources: &[(PathBuf, String)],
) -> Result<(), String> {
    let existing = std::fs::metadata(vault_path).map(|m| m.len()).unwrap_or(0);
    let mut added: u64 = 0;
    for (path, _) in sources {
        if let Ok(meta) = std::fs::metadata(path) {
            added = added.saturating_add(meta.len());
        }
    }
    let projected = existing.saturating_add(added);
    let peak = projected.saturating_mul(VAULT_MEMORY_PEAK_MULTIPLIER);
    // 60% of available memory, or the fixed floor when RAM cannot be read.
    let budget = available_memory_bytes()
        .map(|avail| avail / 5 * 3)
        .unwrap_or(VAULT_MEMORY_FIXED_BUDGET);
    if peak > budget {
        return Err(format!(
            "Adding these files needs about {} of memory (vault {} + {} new, peaking near {}x while sealing), above the safe limit of {}. AeroVault v3 buffers the whole container in memory, so add fewer or smaller files, or split them across vaults. Streaming large vaults is planned.",
            human_size(peak),
            human_size(existing),
            human_size(added),
            VAULT_MEMORY_PEAK_MULTIPLIER,
            human_size(budget),
        ));
    }
    Ok(())
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
    preflight_vault_memory_guard(Path::new(&vault_path), &sources)?;
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
    save_open_vault(&mut vault)?;
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
    let mut sources: Vec<(PathBuf, String)> = Vec::with_capacity(file_paths.len());
    for file_path in &file_paths {
        let path = PathBuf::from(file_path);
        let name = safe_entry_name(&path)?;
        sources.push((path, join_vault_path(&target_dir, &name)));
    }
    preflight_vault_memory_guard(Path::new(&vault_path), &sources)?;
    let _lock = acquire_vault_write_lock(Path::new(&vault_path))?;
    let mut vault = open_vault(&vault_path, &password)?;
    create_directory_in_manifest(&mut vault.manifest, &target_dir)?;
    let added = sources.len();
    append_sources_batched(&mut vault, &sources)?;
    save_open_vault(&mut vault)?;
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
    save_open_vault(&mut vault)?;
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
    save_open_vault(&mut vault)?;
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
    save_open_vault(&mut vault)?;
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
    save_open_vault(&mut vault)?;
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
    save_open_vault(&mut vault)?;
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
    save_open_vault(&mut vault)?;
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
    save_open_vault(&mut vault)?;
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

    save_open_vault(&mut vault)?;
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

/// Extract every entry in the vault into `dest_path`, preserving the tree
/// (Ehud #2). Returns the number of files written.
#[tauri::command]
pub async fn vault_v3_extract_all(
    vault_path: String,
    password: String,
    dest_path: String,
) -> Result<u64, String> {
    let vault = open_vault(vault_path, &password)?;
    extract_all_entries(&vault, Path::new(&dest_path))
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
            "extension directory for Error Correction (reed-solomon)"
        ],
        "compression_profiles": {
            "fast": 3,
            "balanced": 9,
            "archive": 19
        },
        "compatibility": "v4 is expected to read v3 directly; v3 skips unknown non-critical extensions",
        "error_correction_support": "stub (Phase 1): vault_v3_create_with_error_correction emits non-critical 'error-correction.reed-solomon' entry; real RS shards in Phase 2. See T-AEROVAULT-ECC (#272)"
    });

    if let Some(p) = path {
        if let Ok(has_error_correction) = vault_v3_has_error_correction(p).await {
            if let Some(obj) = info.as_object_mut() {
                obj.insert(
                    "error_correction".to_string(),
                    serde_json::json!({
                        "enabled": has_error_correction,
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
    use crate::error_correction::{
        error_correction_geometry, error_correction_grid, ErrorCorrectionPayload,
        ErrorCorrectionPayloadHeader, ERROR_CORRECTION_DATA_SHARDS, ERROR_CORRECTION_MIN_SHARD,
        ERROR_CORRECTION_PARITY_SHARDS, ERROR_CORRECTION_SHARD_CKSUM_LEN,
    };

    use super::*;

    /// T5 cross-impl byte-compat (app side). Under the `test-vectors` seam, build
    /// the SAME fixed tree as the `aerovault` crate's
    /// `tests/aerovault3_cross_impl_v3.rs` and assert the container's BLAKE3
    /// equals the shared golden. Identical hash here and in the crate means the
    /// app and the crate write AEROVAULT3 byte-for-byte identically, so a
    /// container made by either opens in the other. Keep this hash in lockstep
    /// with the crate golden (regenerate both together on a deliberate change).
    #[cfg(feature = "test-vectors")]
    #[test]
    fn aerovault3_cross_impl_golden_matches_crate() {
        const FIXTURE_PW: &str = "aerovault-fixture-pw";
        const GOLDEN_BLAKE3: &str =
            "756dc2112d2492fa6ac4fe3a943058bd4ed7ccf5492b23a830ddabb059c6d570";

        let mut big = Vec::with_capacity(300_000);
        while big.len() < 300_000 {
            big.extend_from_slice(b"AeroVault v3 cross-impl big-file pattern block. ");
        }
        big.truncate(300_000);
        let tree: Vec<(&str, Vec<u8>)> = vec![
            ("readme.txt", b"AeroVault v3 cross-impl fixture\n".to_vec()),
            ("data/small.bin", vec![0xA5u8; 1000]),
            ("data/big.bin", big),
        ];

        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let mut sources: Vec<(PathBuf, String)> = Vec::new();
        for (rel, bytes) in &tree {
            let abs = src.join(rel);
            std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
            std::fs::write(&abs, bytes).unwrap();
            sources.push((abs, rel.to_string()));
        }
        let vp = tmp.path().join("golden.aerovault");

        // Re-open before each mutating op, exactly as the Tauri command layer
        // does (open -> mutate -> save per command). open_vault draws no
        // randomness, so the deterministic stream is unchanged; this just keeps
        // the per-command staleness guard happy, matching production flow.
        crate::aerocrypt::reset_test_vectors();
        create_empty_vault(&vp, FIXTURE_PW, DEFAULT_ZSTD_LEVEL, None, 0).unwrap();
        {
            let mut vault = open_vault(&vp, FIXTURE_PW).unwrap();
            create_directory_in_manifest(&mut vault.manifest, "emptydir").unwrap();
            save_open_vault(&mut vault).unwrap();
        }
        {
            let mut vault = open_vault(&vp, FIXTURE_PW).unwrap();
            append_sources_batched(&mut vault, &sources).unwrap();
            save_open_vault(&mut vault).unwrap();
        }

        let bytes = std::fs::read(&vp).unwrap();
        let got = blake3::hash(&bytes).to_hex().to_string();
        assert_eq!(
            got,
            GOLDEN_BLAKE3,
            "app AEROVAULT3 bytes diverge from the aerovault crate golden ({} bytes)",
            bytes.len()
        );
    }

    // --- Vault `.aerocorrect` sidecar helpers ---
    // The on-disk format round-trip / corruption / binding is covered in
    // `error_correction::sidecar::tests`; here we test the vault's three-segment
    // layout: each region's parity is found by fixed index.

    #[test]
    fn vault_sidecar_parity_reads_roles_by_index() {
        let header_p = b"header-parity".to_vec();
        let manifest_p = b"manifest-parity".to_vec();
        let data_p = b"data-parity".to_vec();
        let segments = vec![
            AeroCorrectSegment {
                window_offset: 0,
                window_len: HEADER_SIZE as u64,
                avec_bytes: header_p.clone(),
            },
            AeroCorrectSegment {
                window_offset: 1024,
                window_len: 200,
                avec_bytes: manifest_p.clone(),
            },
            AeroCorrectSegment {
                window_offset: 2048,
                window_len: 4096,
                avec_bytes: data_p.clone(),
            },
        ];
        let sidecar = AeroCorrectSidecar::new(container_sha256(b"container"), 6144, segments);
        // Round-trip through the unified format, then read each role back by index.
        let parsed = AeroCorrectSidecar::from_bytes(&sidecar.to_bytes()).expect("parse");
        assert_eq!(parsed.segments.len(), 3);
        assert_eq!(
            vault_sidecar_parity(&parsed, VAULT_SIDECAR_SEG_HEADER),
            header_p
        );
        assert_eq!(
            vault_sidecar_parity(&parsed, VAULT_SIDECAR_SEG_MANIFEST),
            manifest_p
        );
        assert_eq!(
            vault_sidecar_parity(&parsed, VAULT_SIDECAR_SEG_DATA),
            data_p
        );
        // An out-of-range role yields an empty slice, not a panic.
        assert!(vault_sidecar_parity(&parsed, 9).is_empty());
    }

    #[test]
    fn vault_sidecar_data_segment_reconstructs_like_embedded() {
        // The data segment carries the exact same AVEC payload as the embedded
        // placement, so the placement-agnostic engine reconstructs identically.
        let blocks: Vec<Vec<u8>> = (0..5u8).map(|i| vec![i; 4096]).collect();
        let refs: Vec<&[u8]> = blocks.iter().map(|b| b.as_slice()).collect();
        let (payload, _shards, _protected, _ovh) = compute_error_correction_shards(&refs);
        assert!(!payload.is_empty());

        let segments = vec![
            AeroCorrectSegment {
                window_offset: 0,
                window_len: HEADER_SIZE as u64,
                avec_bytes: Vec::new(),
            },
            AeroCorrectSegment {
                window_offset: 1024,
                window_len: 0,
                avec_bytes: Vec::new(),
            },
            AeroCorrectSegment {
                window_offset: 2048,
                window_len: (5 * 4096) as u64,
                avec_bytes: payload,
            },
        ];
        let sidecar = AeroCorrectSidecar::new(container_sha256(b"x"), 100, segments);
        let parsed = AeroCorrectSidecar::from_bytes(&sidecar.to_bytes()).unwrap();
        let data_parity = vault_sidecar_parity(&parsed, VAULT_SIDECAR_SEG_DATA);

        // Damage one block, then reconstruct from the detached data parity.
        let mut damaged: Vec<Vec<u8>> = blocks.clone();
        damaged[2] = vec![0u8; damaged[2].len()];
        reconstruct_from_error_correction(&mut damaged, data_parity).expect("reconstruct");
        assert_eq!(damaged, blocks);
    }

    #[test]
    fn recovery_placement_parses_and_reports() {
        assert_eq!(
            RecoveryPlacement::parse("embedded").unwrap(),
            RecoveryPlacement::Embedded
        );
        assert_eq!(
            RecoveryPlacement::parse(" Detached ").unwrap(),
            RecoveryPlacement::Detached
        );
        assert_eq!(
            RecoveryPlacement::parse("BOTH").unwrap(),
            RecoveryPlacement::Both
        );
        assert!(RecoveryPlacement::parse("nope").is_err());
        assert!(
            RecoveryPlacement::Embedded.embeds() && !RecoveryPlacement::Embedded.writes_sidecar()
        );
        assert!(
            !RecoveryPlacement::Detached.embeds() && RecoveryPlacement::Detached.writes_sidecar()
        );
        assert!(RecoveryPlacement::Both.embeds() && RecoveryPlacement::Both.writes_sidecar());
    }

    #[test]
    fn default_sidecar_path_appends_aerocorrect() {
        // Unified convention: any file `X` gets `X.aerocorrect`, like AeroSync.
        assert_eq!(
            default_sidecar_path(Path::new("/tmp/secret.aerovault")),
            PathBuf::from("/tmp/secret.aerovault.aerocorrect")
        );
        assert_eq!(
            default_sidecar_path(Path::new("/tmp/secret.vault")),
            PathBuf::from("/tmp/secret.vault.aerocorrect")
        );
    }

    #[test]
    fn detached_export_and_repair_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("detached.aerovault");
        let f1 = dir.path().join("d1.txt");
        std::fs::write(
            &f1,
            b"detached recovery payload, with enough bytes to form a real block",
        )
        .unwrap();

        // Detached create: no embedded extension, but a sidecar is seeded.
        create_empty_vault(
            &vault_path,
            "detached-pw-1234",
            DEFAULT_ZSTD_LEVEL,
            Some(RecoveryPlacement::Detached),
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();
        let sidecar = default_sidecar_path(&vault_path);
        assert!(sidecar.exists(), "create should seed the sidecar");
        {
            let v = open_vault(&vault_path, "detached-pw-1234").unwrap();
            assert!(
                !v.extensions
                    .iter()
                    .any(|e| e.extension_id == ERROR_CORRECTION_EXTENSION_ID),
                "detached placement must not embed the extension"
            );
        }

        // Add a file, then (re-)export parity over the now-populated data.
        let mut vault = open_vault(&vault_path, "detached-pw-1234").unwrap();
        append_file_at(&mut vault, &f1, "d1.txt").unwrap();
        save_open_vault(&mut vault).unwrap();
        let res = export_parity(&vault_path, "detached-pw-1234", None).unwrap();
        assert!(res.shards > 0 && res.bytes_protected > 0);

        // Damage a block on a fresh open; repair pulls from the detached sidecar.
        let mut vault2 = open_vault(&vault_path, "detached-pw-1234").unwrap();
        let off = vault2
            .manifest
            .chunks
            .values()
            .map(|r| r.data_offset)
            .min()
            .unwrap() as usize;
        vault2.data[off + 8] ^= 0xFF;
        assert!(!scrub_vault(&vault2).is_empty());
        let (repaired, src) =
            repair_vault(&mut vault2, false, None).expect("repair from detached sidecar");
        assert!(repaired > 0);
        assert_eq!(src, ParitySource::Detached);
        assert!(scrub_vault(&vault2).is_empty());
    }

    #[test]
    fn detached_sidecar_rebuilds_corrupted_header() {
        // HEADER parity: corrupt the on-disk 1024-byte header so its MAC fails, then
        // confirm open rebuilds it from the detached sidecar and repair persists it.
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("hdr.aerovault");
        let f1 = dir.path().join("h1.txt");
        std::fs::write(
            &f1,
            b"header recovery test payload, enough bytes for a block",
        )
        .unwrap();

        create_empty_vault(
            &vault_path,
            "header-pw-1234",
            DEFAULT_ZSTD_LEVEL,
            Some(RecoveryPlacement::Detached),
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();
        let mut vault = open_vault(&vault_path, "header-pw-1234").unwrap();
        append_file_at(&mut vault, &f1, "h1.txt").unwrap();
        save_open_vault(&mut vault).unwrap();
        // Export AFTER populating so the sidecar carries the final header parity.
        let res = export_parity(&vault_path, "header-pw-1234", None).unwrap();
        assert!(
            res.header_parity_len > 0,
            "sidecar must carry header parity"
        );
        assert!(
            res.manifest_parity_len > 0,
            "sidecar must carry manifest parity"
        );

        // Flip a byte inside the header (past the magic/version) so its MAC fails.
        flip_byte_in_file(&vault_path, 64);
        // Without recovery the header would be unreadable; the sidecar rebuilds it.
        let mut healed = open_vault(&vault_path, "header-pw-1234").unwrap();
        assert!(
            healed.header_repaired_on_open,
            "header rebuilt from sidecar"
        );
        let (repaired, _src) = repair_vault(&mut healed, false, None).unwrap();
        assert!(repaired >= 1, "repair persists the healed header");

        // Re-open the now-healed vault straight from disk: no recovery needed.
        let reopened = open_vault(&vault_path, "header-pw-1234").unwrap();
        assert!(
            !reopened.header_repaired_on_open,
            "on-disk header is healed; no sidecar recovery the second time"
        );
    }

    #[test]
    fn detached_sidecar_rebuilds_corrupted_manifest() {
        // GAP-4 (detached): the pure-detached path keeps no embedded metadata
        // extension, so a corrupted manifest must be rebuilt from the sidecar.
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("man.aerovault");
        let f1 = dir.path().join("m1.txt");
        std::fs::write(
            &f1,
            b"manifest recovery payload, enough bytes for a real block",
        )
        .unwrap();

        create_empty_vault(
            &vault_path,
            "manifest-pw-1234",
            DEFAULT_ZSTD_LEVEL,
            Some(RecoveryPlacement::Detached),
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();
        let mut vault = open_vault(&vault_path, "manifest-pw-1234").unwrap();
        append_file_at(&mut vault, &f1, "m1.txt").unwrap();
        save_open_vault(&mut vault).unwrap();
        export_parity(&vault_path, "manifest-pw-1234", None).unwrap();

        // Corrupt a byte inside the encrypted manifest region (AEAD decrypt fails).
        let man_off = {
            let v = open_vault(&vault_path, "manifest-pw-1234").unwrap();
            v.header.manifest_offset as usize
        };
        flip_byte_in_file(&vault_path, man_off + 4);
        // Open must rebuild the manifest from the detached sidecar's manifest parity.
        let healed = open_vault(&vault_path, "manifest-pw-1234").unwrap();
        assert!(
            healed.manifest_repaired_on_open,
            "manifest rebuilt from sidecar"
        );
        assert!(healed.manifest.entries.iter().any(|e| e.path == "m1.txt"));
    }

    #[test]
    fn error_correction_grid_maps_percentages() {
        // Default 20% reproduces the original fixed K=10/P=2 grid.
        assert_eq!(error_correction_grid(20), (10, 2));
        // QR-style named levels.
        assert_eq!(error_correction_grid(7), (14, 1)); // round(100/7)
        assert_eq!(error_correction_grid(15), (13, 2)); // round(200/15)
        assert_eq!(error_correction_grid(25), (8, 2));
        assert_eq!(error_correction_grid(30), (7, 2)); // round(200/30)
                                                       // Out-of-range values clamp to [MIN, MAX].
        assert_eq!(
            error_correction_grid(1),
            error_correction_grid(ERROR_CORRECTION_MIN_PCT)
        );
        assert_eq!(
            error_correction_grid(99),
            error_correction_grid(ERROR_CORRECTION_MAX_PCT)
        );
    }

    #[test]
    fn error_correction_level_persists_and_lowers_overhead() {
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("lvl.aerovault");
        let f1 = dir.path().join("l1.bin");
        // High-entropy (incompressible) body so the on-disk block stream is large
        // enough that the chosen grid, not the min-shard floor, drives the overhead.
        let mut body = Vec::with_capacity(300_000);
        let mut block = *blake3::hash(b"ec-level-test-seed").as_bytes();
        while body.len() < 300_000 {
            body.extend_from_slice(&block);
            block = *blake3::hash(&block).as_bytes();
        }
        body.truncate(300_000);
        std::fs::write(&f1, &body).unwrap();

        // Low overhead level (~7%).
        create_empty_vault(
            &vault_path,
            "level-pw-1234",
            DEFAULT_ZSTD_LEVEL,
            Some(RecoveryPlacement::Detached),
            7,
        )
        .unwrap();
        let mut vault = open_vault(&vault_path, "level-pw-1234").unwrap();
        assert_eq!(vault.manifest.error_correction_pct, Some(7));
        append_file_at(&mut vault, &f1, "l1.bin").unwrap();
        save_open_vault(&mut vault).unwrap();
        // The chosen level survives a re-seal (it lives in the manifest).
        let reopened = open_vault(&vault_path, "level-pw-1234").unwrap();
        assert_eq!(reopened.manifest.error_correction_pct, Some(7));

        let res = export_parity(&vault_path, "level-pw-1234", None).unwrap();
        // ~7% is well under the default 20% grid.
        assert!(
            res.overhead_pct < 12.0,
            "low level overhead {} should be < 12%",
            res.overhead_pct
        );
    }

    #[test]
    fn strip_parity_refuses_without_sidecar_then_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("strip.aerovault");
        let f1 = dir.path().join("s1.txt");
        std::fs::write(&f1, b"strip test data block contents here, padded a bit").unwrap();

        create_empty_vault(
            &vault_path,
            "strip-pw-1234",
            DEFAULT_ZSTD_LEVEL,
            Some(RecoveryPlacement::Embedded),
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();
        let mut vault = open_vault(&vault_path, "strip-pw-1234").unwrap();
        append_file_at(&mut vault, &f1, "s1.txt").unwrap();
        save_open_vault(&mut vault).unwrap();

        // No sidecar yet: stripping must refuse (would leave zero recovery).
        assert!(strip_parity(&vault_path, "strip-pw-1234", false).is_err());

        // Export a sidecar; resolve now prefers detached over the embedded copy.
        export_parity(&vault_path, "strip-pw-1234", None).unwrap();
        {
            let v = open_vault(&vault_path, "strip-pw-1234").unwrap();
            let (_b, src) = resolve_parity_source(&v, None).unwrap();
            assert_eq!(src, ParitySource::Detached);
        }

        // With a sidecar present, strip succeeds and drops the embedded extension.
        let r = strip_parity(&vault_path, "strip-pw-1234", false).unwrap();
        assert!(r.sidecar_present);
        let v = open_vault(&vault_path, "strip-pw-1234").unwrap();
        assert!(
            !v.extensions
                .iter()
                .any(|e| e.extension_id == ERROR_CORRECTION_EXTENSION_ID),
            "strip must remove the embedded extension"
        );
        let (_b, src) = resolve_parity_source(&v, None).unwrap();
        assert_eq!(src, ParitySource::Detached);
    }

    #[test]
    fn resolve_parity_explicit_file_integrity_and_foreign_contract() {
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("host.aerovault");
        create_empty_vault(
            &vault_path,
            "host-pw-12345",
            DEFAULT_ZSTD_LEVEL,
            Some(RecoveryPlacement::Embedded),
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();
        let payload = dir.path().join("payload.bin");
        std::fs::write(
            &payload,
            b"host payload so vault sidecar windows are non-empty",
        )
        .unwrap();
        let mut writable = open_vault(&vault_path, "host-pw-12345").unwrap();
        append_file_at(&mut writable, &payload, "payload.bin").unwrap();
        save_open_vault(&mut writable).unwrap();
        let v = open_vault(&vault_path, "host-pw-12345").unwrap();

        // A malformed explicit file (not a valid `.aerocorrect`) is a hard error: the
        // unified format's own magic/version/integrity check rejects it.
        let bad_path = dir.path().join("garbage.aerocorrect");
        std::fs::write(&bad_path, b"not an aerocorrect sidecar at all").unwrap();
        assert!(resolve_parity_source(&v, Some(&bad_path)).is_err());

        // A WELL-FORMED foreign sidecar (bound to different content) is NOT refused at
        // resolve time: the unified format binds by content, which a vault under repair
        // cannot match. Safety comes from the caller's post-reconstruction re-verify
        // (header MAC / manifest cipher_hash), so a foreign sidecar can only make a
        // repair FAIL, never overwrite good data. Resolve therefore succeeds here.
        let foreign = build_vault_sidecar(
            b"some-other-container",
            &v.header,
            b"foreign-data-parity".to_vec(),
            b"foreign-manifest-parity".to_vec(),
            b"foreign-header-parity".to_vec(),
        );
        let foreign_path = dir.path().join("foreign.aerocorrect");
        std::fs::write(&foreign_path, foreign.to_bytes()).unwrap();
        let (parity, src) = resolve_parity_source(&v, Some(&foreign_path))
            .expect("well-formed foreign sidecar resolves (binding not enforced at resolve)");
        assert_eq!(src, ParitySource::Explicit);
        assert_eq!(
            parity, b"foreign-data-parity",
            "resolve returns the DATA-segment parity verbatim; the post-verify gate, not resolve, rejects it"
        );
    }

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
    fn human_size_formats_units() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(1024 * 1024), "1.0 MiB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }

    #[test]
    fn extract_all_round_trip_preserves_tree() {
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("test.aerovault");
        let a = dir.path().join("a.txt");
        let c = dir.path().join("c.txt");
        std::fs::write(&a, b"root file\n".repeat(2048)).unwrap();
        std::fs::write(&c, b"nested file\n".repeat(2048)).unwrap();

        create_empty_vault(
            &vault_path,
            "correct horse battery staple",
            DEFAULT_ZSTD_LEVEL,
            None,
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();
        let mut vault = open_vault(&vault_path, "correct horse battery staple").unwrap();
        append_file_at(&mut vault, &a, "a.txt").unwrap();
        append_file_at(&mut vault, &c, "sub/c.txt").unwrap();
        save_open_vault(&mut vault).unwrap();

        let reopened = open_vault(&vault_path, "correct horse battery staple").unwrap();
        let out = dir.path().join("out");
        let written = extract_all_entries(&reopened, &out).unwrap();
        assert_eq!(written, 2);
        assert_eq!(
            std::fs::read(out.join("a.txt")).unwrap(),
            std::fs::read(&a).unwrap()
        );
        assert_eq!(
            std::fs::read(out.join("sub/c.txt")).unwrap(),
            std::fs::read(&c).unwrap()
        );
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
            None,
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();
        let mut vault = open_vault(&vault_path, "correct horse battery staple").unwrap();
        append_file_at(&mut vault, &a, "a.txt").unwrap();
        append_file_at(&mut vault, &b, "b.txt").unwrap();
        save_open_vault(&mut vault).unwrap();

        let reopened = open_vault(&vault_path, "correct horse battery staple").unwrap();
        let info = info_from_manifest(&reopened.manifest);
        assert_eq!(info.file_count, 2);
        assert!(info.dedup_chunks >= 1);

        let out = dir.path().join("out.txt");
        extract_entry(&reopened, "a.txt", &out).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), std::fs::read(&a).unwrap());
    }

    /// P1-01: Error Correction stub creation + roundtrip + v3 compatibility.
    /// Creates a vault with the non-critical "error-correction.reed-solomon" extension entry
    /// (payload length 0 for stub phase), performs add + extract, re-opens,
    /// and verifies the extension is present and non-critical.
    /// Also asserts that is_vault_v3 (magic-based) still returns true.
    #[test]
    fn v3_error_correction_stub_roundtrip_and_v3_compatibility() {
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("error_correction-stub.aerovault");
        let payload = dir.path().join("payload.bin");
        std::fs::write(&payload, b"hello from Error Correction stub phase").unwrap();

        // Create with Error Correction stub (Phase 1)
        create_empty_vault(
            &vault_path,
            "error_correction-test-pw",
            DEFAULT_ZSTD_LEVEL,
            Some(RecoveryPlacement::Embedded),
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();

        // Add a file (exercises seal path that must preserve the extension dir)
        let mut vault = open_vault(&vault_path, "error_correction-test-pw").unwrap();
        append_file_at(&mut vault, &payload, "data/payload.bin").unwrap();
        save_open_vault(&mut vault).unwrap();

        // Re-open and inspect extensions (proves roundtrip of the dir)
        let reopened = open_vault(&vault_path, "error_correction-test-pw").unwrap();
        let error_correction_ext = reopened
            .extensions
            .iter()
            .find(|e| e.extension_id == ERROR_CORRECTION_EXTENSION_ID);
        assert!(
            error_correction_ext.is_some(),
            "error_correction extension should be present after roundtrip"
        );
        let error_correction_ext = error_correction_ext.unwrap();
        assert!(
            !error_correction_ext.critical,
            "Error Correction extension must be non-critical for v3 compat"
        );
        assert_eq!(
            error_correction_ext.algorithm_id,
            ERROR_CORRECTION_ALGORITHM_ID
        );
        assert_eq!(
            error_correction_ext.algorithm_version,
            ERROR_CORRECTION_ALGORITHM_VERSION
        );

        // Extract must succeed (pure v3 reader path compatibility)
        let out = dir.path().join("restored.bin");
        extract_entry(&reopened, "data/payload.bin", &out).unwrap();
        assert_eq!(
            std::fs::read(&out).unwrap(),
            b"hello from Error Correction stub phase"
        );

        // is_vault_v3 (the fast magic path used by dispatch / old v3 binaries) must still say yes.
        // We check the magic directly here (same logic as is_vault_v3) to avoid needing an async runtime in the test.
        let mut f = std::fs::File::open(&vault_path).unwrap();
        let mut magic = [0u8; 11];
        f.read_exact(&mut magic).unwrap();
        let is_v3_magic = &magic[..10] == MAGIC && magic[10] == VERSION;
        assert!(is_v3_magic, "vault with Error Correction stub extension must still be recognized as v3 by magic (for pure v3 reader compat)");

        // The extension dir itself survived (we can check via header on re-open)
        // Re-open header has non-zero extension_dir_len
        assert!(
            reopened.header.extension_dir_len > 0,
            "extension directory must be present on disk"
        );
    }

    #[test]
    fn v3_security_info_advertises_error_correction_when_present() {
        // P1-04 test: security_info with path should report the error_correction object when the stub extension is present.
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("sec-info-test.aerovault");

        create_empty_vault(
            &vault_path,
            "sec-pw-1234",
            DEFAULT_ZSTD_LEVEL,
            Some(RecoveryPlacement::Embedded),
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();

        let info = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(vault_v3_security_info(Some(
                vault_path.to_string_lossy().to_string(),
            )));

        let error_correction = info
            .get("error_correction")
            .expect("error_correction field should be present when path given");
        assert_eq!(error_correction["enabled"], true);
        assert_eq!(error_correction["algorithm"], "reed-solomon");
        assert_eq!(error_correction["version"], 1);
        assert_eq!(error_correction["critical"], false);

        // Also check general fields still there
        assert!(info.get("pipeline").is_some());
        assert!(info.get("error_correction_support").is_some());
    }

    #[test]
    fn p2_02_error_correction_payload_format_roundtrip() {
        // P2-09 (v2): the fixed-grid payload (header + per-shard checksum table +
        // parity) must serialize and deserialize losslessly.
        let s = 4096usize;
        let l = s * 25; // 25 data shards => 3 groups of 10 (last group partial)
        let header = ErrorCorrectionPayloadHeader {
            data_shards: 10,
            parity_shards: 2,
            shard_size: s as u32,
            total_data_len: l as u64,
        };
        let (num_data_shards, num_groups) = error_correction_geometry(&header);
        assert_eq!(num_data_shards, 25);
        assert_eq!(num_groups, 3);
        let num_parity = num_groups * header.parity_shards as usize;

        let data_checksums: Vec<[u8; ERROR_CORRECTION_SHARD_CKSUM_LEN]> = (0..num_data_shards)
            .map(|i| [i as u8; ERROR_CORRECTION_SHARD_CKSUM_LEN])
            .collect();
        let parity_checksums: Vec<[u8; ERROR_CORRECTION_SHARD_CKSUM_LEN]> = (0..num_parity)
            .map(|i| [(200 + i) as u8; ERROR_CORRECTION_SHARD_CKSUM_LEN])
            .collect();
        let parity_data = vec![0xABu8; num_parity * s];

        let payload = ErrorCorrectionPayload {
            header,
            data_checksums: data_checksums.clone(),
            parity_checksums: parity_checksums.clone(),
            parity_data: parity_data.clone(),
        };

        let bytes = payload.to_bytes();
        let decoded = ErrorCorrectionPayload::from_bytes(&bytes).expect("roundtrip failed");

        assert_eq!(decoded.header.data_shards, 10);
        assert_eq!(decoded.header.parity_shards, 2);
        assert_eq!(decoded.header.shard_size, s as u32);
        assert_eq!(decoded.header.total_data_len, l as u64);
        assert_eq!(decoded.data_checksums, data_checksums);
        assert_eq!(decoded.parity_checksums, parity_checksums);
        assert_eq!(decoded.parity_data, parity_data);

        // A truncated / length-mismatched payload must be rejected, not misparsed.
        assert!(ErrorCorrectionPayload::from_bytes(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn p2_03_compute_error_correction_shards_basic() {
        // P2-09: compute_error_correction_shards produces a well-formed v2 payload over the
        // concatenated block stream.
        let block1: Vec<u8> = vec![0u8; 100];
        let block2: Vec<u8> = vec![1u8; 200];
        let data_blocks: Vec<&[u8]> = vec![&block1, &block2];

        let (payload_bytes, _sh, _pr, _ov) = compute_error_correction_shards(&data_blocks);
        assert!(!payload_bytes.is_empty());

        let parsed =
            ErrorCorrectionPayload::from_bytes(&payload_bytes).expect("payload should parse");
        assert_eq!(
            parsed.header.data_shards,
            ERROR_CORRECTION_DATA_SHARDS as u16
        );
        assert_eq!(
            parsed.header.parity_shards,
            ERROR_CORRECTION_PARITY_SHARDS as u16
        );
        assert_eq!(parsed.header.total_data_len, 300);
        // L=300 < ERROR_CORRECTION_MIN_SHARD => S clamps to the floor => one data shard, one group.
        assert_eq!(
            parsed.header.shard_size as usize,
            ERROR_CORRECTION_MIN_SHARD
        );
        let (num_data_shards, num_groups) = error_correction_geometry(&parsed.header);
        assert_eq!(num_data_shards, 1);
        assert_eq!(num_groups, 1);
        assert_eq!(parsed.data_checksums.len(), 1);
        assert_eq!(
            parsed.parity_checksums.len(),
            ERROR_CORRECTION_PARITY_SHARDS
        );
        assert_eq!(
            parsed.parity_data.len(),
            ERROR_CORRECTION_PARITY_SHARDS * ERROR_CORRECTION_MIN_SHARD
        );
    }

    #[test]
    fn p2_04_reconstruct_from_error_correction_basic() {
        // P2-09: damage one block, reconstruct it via the v2 Error Correction payload (the shard
        // checksum localizes the erasure; no externally supplied bad-index list).
        let orig1: Vec<u8> = (0u8..100).collect();
        let orig2: Vec<u8> = (0u8..150).map(|x| 100 + x).collect();

        let make_on_disk = |data: &[u8]| -> Vec<u8> {
            let mut b = (data.len() as u64).to_le_bytes().to_vec();
            b.extend_from_slice(data);
            b
        };

        let mut blocks = vec![make_on_disk(&orig1), make_on_disk(&orig2)];
        let payload = compute_error_correction_shards(
            &blocks.iter().map(|v| v.as_slice()).collect::<Vec<_>>(),
        )
        .0;

        // Corrupt the second block; its shard checksum will mismatch -> RS erasure.
        blocks[1][10] ^= 0xFF;

        let recovered =
            reconstruct_from_error_correction(&mut blocks, &payload).expect("reconstruct");
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

        create_empty_vault(
            &vault_path,
            "scrub-pw-1234",
            DEFAULT_ZSTD_LEVEL,
            None,
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();
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
        // P2-07: full cycle with real Error Correction vault + corruption + repair via primitive.
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("repair-e2e.aerovault");
        let f1 = dir.path().join("f1.txt");
        std::fs::write(&f1, b"hello repair world one").unwrap();
        let f2 = dir.path().join("f2.txt");
        std::fs::write(
            &f2,
            b"hello repair world two with more data to have a decent block",
        )
        .unwrap();

        // Create with Error Correction and add files (triggers seal with Error Correction)
        create_empty_vault(
            &vault_path,
            "repair-pw-1234",
            DEFAULT_ZSTD_LEVEL,
            Some(RecoveryPlacement::Embedded),
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();
        let mut vault = open_vault(&vault_path, "repair-pw-1234").unwrap();
        append_file_at(&mut vault, &f1, "f1.txt").unwrap();
        append_file_at(&mut vault, &f2, "f2.txt").unwrap();
        save_open_vault(&mut vault).unwrap();

        // Identify the second file's data block, then exercise the real bad-hash path
        // by corrupting its cipher bytes in memory on a freshly re-opened vault.
        let mut recs: Vec<_> = vault.manifest.chunks.values().cloned().collect();
        recs.sort_by_key(|r| r.data_offset);
        assert!(recs.len() >= 2);

        let mut vault2 = open_vault(&vault_path, "repair-pw-1234").unwrap();

        // Corrupt in-memory the second block's cipher part
        if let Some(rec) = vault2
            .manifest
            .chunks
            .values()
            .find(|r| r.data_offset == recs[1].data_offset)
        {
            let s = rec.data_offset as usize + 8 + 3;
            if s < vault2.data.len() {
                vault2.data[s] ^= 0xFF;
            }
        }

        // Now scrub sees damage
        let damaged_before = scrub_vault(&vault2);
        assert!(!damaged_before.is_empty());

        // Repair (not dry)
        let (repaired, _src) =
            repair_vault(&mut vault2, false, None).expect("repair should succeed");
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
        assert!(std::fs::read(&out2)
            .unwrap()
            .starts_with(b"hello repair world two"));
    }

    #[test]
    fn gap4_corrupted_manifest_rebuilt_from_metadata_parity() {
        // GAP-4: the per-block cipher_hash scrub needs lives inside the encrypted
        // manifest. A corrupted manifest must be rebuildable from the metadata parity
        // (located via the MAC-verified header) so the vault still opens and repairs.
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("gap4.aerovault");
        let f1 = dir.path().join("f1.txt");
        let payload = b"gap4 manifest locator protection: corrupt the manifest, rebuild it";
        std::fs::write(&f1, payload).unwrap();

        create_empty_vault(
            &vault_path,
            "gap4-pw-12345",
            DEFAULT_ZSTD_LEVEL,
            Some(RecoveryPlacement::Embedded),
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();
        {
            let mut vault = open_vault(&vault_path, "gap4-pw-12345").unwrap();
            append_file_at(&mut vault, &f1, "f1.txt").unwrap();
            save_open_vault(&mut vault).unwrap();
        }

        // Offsets of the manifest region (read from a clean open), then flip one byte
        // inside it on disk: a single bit change breaks AES-256-GCM-SIV authentication.
        let (m_off, m_len) = {
            let vault = open_vault(&vault_path, "gap4-pw-12345").unwrap();
            (
                vault.header.manifest_offset as usize,
                vault.header.manifest_len as usize,
            )
        };
        assert!(m_len > 0);
        let mut bytes = std::fs::read(&vault_path).unwrap();
        bytes[m_off + m_len / 2] ^= 0xFF;
        std::fs::write(&vault_path, &bytes).unwrap();

        // Open must now succeed by rebuilding the manifest from the metadata parity.
        let mut vault = open_vault(&vault_path, "gap4-pw-12345")
            .expect("open should rebuild the corrupted manifest from the metadata parity");
        assert!(vault.manifest_repaired_on_open);

        // The data block was untouched, so the content extracts intact.
        let out = dir.path().join("out.txt");
        extract_entry(&vault, "f1.txt", &out).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), payload);

        // Repair persists the healed manifest even with no damaged data block.
        let (repaired, _src) =
            repair_vault(&mut vault, false, None).expect("repair persists healed manifest");
        assert_eq!(repaired, 1);

        // Re-open: the on-disk manifest is healed, no reconstruction needed this time.
        let vault2 = open_vault(&vault_path, "gap4-pw-12345").unwrap();
        assert!(!vault2.manifest_repaired_on_open);
    }

    #[test]
    fn gap4_no_metadata_parity_means_corrupt_manifest_is_fatal() {
        // Control: a vault WITHOUT Error Correction has no metadata parity, so the
        // same manifest corruption is fatal. This proves the rebuild above is what
        // saves the Error Correction vault, not some other tolerance in the open path.
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("gap4-neg.aerovault");
        let f1 = dir.path().join("f1.txt");
        std::fs::write(&f1, b"no parity here, corruption is fatal").unwrap();

        create_empty_vault(
            &vault_path,
            "gap4-pw-12345",
            DEFAULT_ZSTD_LEVEL,
            None,
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();
        {
            let mut vault = open_vault(&vault_path, "gap4-pw-12345").unwrap();
            append_file_at(&mut vault, &f1, "f1.txt").unwrap();
            save_open_vault(&mut vault).unwrap();
        }
        let (m_off, m_len) = {
            let vault = open_vault(&vault_path, "gap4-pw-12345").unwrap();
            (
                vault.header.manifest_offset as usize,
                vault.header.manifest_len as usize,
            )
        };
        let mut bytes = std::fs::read(&vault_path).unwrap();
        bytes[m_off + m_len / 2] ^= 0xFF;
        std::fs::write(&vault_path, &bytes).unwrap();

        assert!(open_vault(&vault_path, "gap4-pw-12345").is_err());
    }

    #[test]
    fn p2_08_cli_stress_multiple_damage_repair() {
        // Stress test: Error Correction vault with 12 files, corrupt 4 blocks (across stripes), repair, verify all.
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("stress-repair.aerovault");

        create_empty_vault(
            &vault_path,
            "stress-pw-1234",
            DEFAULT_ZSTD_LEVEL,
            Some(RecoveryPlacement::Embedded),
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();
        let mut vault = open_vault(&vault_path, "stress-pw-1234").unwrap();

        let mut sources = vec![];
        for i in 0..12 {
            let p = dir.path().join(format!("s{i:02}.txt"));
            let content = format!(
                "stress file {} with some padding data to make blocks decent size",
                i
            )
            .repeat(3);
            std::fs::write(&p, content.as_bytes()).unwrap();
            append_file_at(&mut vault, &p, &format!("s{i:02}.txt")).unwrap();
            sources.push((p, format!("s{i:02}.txt")));
        }
        save_open_vault(&mut vault).unwrap();

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
        let (fixed, _src) = repair_vault(&mut vault2, false, None).expect("repair");
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
    fn error_correction_test_blob(n: u32) -> Vec<u8> {
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
        let data = error_correction_test_blob(300_000);
        std::fs::write(&f, &data).unwrap();

        create_empty_vault(
            &vault_path,
            "recover-pw",
            DEFAULT_ZSTD_LEVEL,
            Some(RecoveryPlacement::Embedded),
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();
        let mut vault = open_vault(&vault_path, "recover-pw").unwrap();
        append_file_at(&mut vault, &f, "f.bin").unwrap();
        save_open_vault(&mut vault).unwrap();

        // Corrupt the bytes of parity shard 0 on disk (its stored checksum will no
        // longer match -> reconstruct erases it and uses parity shard 1 instead).
        // save_open_vault takes &OpenVaultV3 and updates only a local extensions clone,
        // so the persisted payload location is read straight from the on-disk header
        // (extension_payload_offset @176, extension_payload_len @184).
        let mut raw = std::fs::read(&vault_path).unwrap();
        let payload_abs = u64::from_le_bytes(raw[176..184].try_into().unwrap()) as usize;
        // The extension payload region now holds both the data-block parity (entry at
        // offset 0) and the GAP-4 manifest parity; read the data-block parity's exact
        // slice via its extension-directory entry, not the whole region.
        let ext_dir_abs = u64::from_le_bytes(raw[160..168].try_into().unwrap()) as usize;
        let ext_dir_len = u64::from_le_bytes(raw[168..176].try_into().unwrap()) as usize;
        let exts: Vec<ExtensionEntryV3> =
            serde_json::from_slice(&raw[ext_dir_abs..ext_dir_abs + ext_dir_len]).unwrap();
        let data_parity_len = exts
            .iter()
            .find(|e| e.extension_id == ERROR_CORRECTION_EXTENSION_ID)
            .expect("data Error Correction extension present")
            .length as usize;
        let payload =
            ErrorCorrectionPayload::from_bytes(&raw[payload_abs..payload_abs + data_parity_len])
                .unwrap();
        let (nds, ng) = error_correction_geometry(&payload.header);
        let p = payload.header.parity_shards as usize;
        let parity0_abs = payload_abs + 32 + (nds + ng * p) * ERROR_CORRECTION_SHARD_CKSUM_LEN;
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
        assert!(
            !scrub_vault(&vault2).is_empty(),
            "scrub must see the damage"
        );

        let (repaired, _src) = repair_vault(&mut vault2, false, None).expect("repair");
        assert!(
            repaired > 0,
            "must recover despite the corrupt parity shard"
        );
        assert!(
            scrub_vault(&vault2).is_empty(),
            "vault must be clean after repair"
        );

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
        let data = error_correction_test_blob(300_000);
        std::fs::write(&f, &data).unwrap();

        create_empty_vault(
            &vault_path,
            "over-pw-1234",
            DEFAULT_ZSTD_LEVEL,
            Some(RecoveryPlacement::Embedded),
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();
        let mut vault = open_vault(&vault_path, "over-pw-1234").unwrap();
        append_file_at(&mut vault, &f, "f.bin").unwrap();
        save_open_vault(&mut vault).unwrap();

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
        assert!(
            !scrub_vault(&vault2).is_empty(),
            "scrub must detect the damage"
        );

        let (repaired, _src) =
            repair_vault(&mut vault2, false, None).expect("repair call should not error");
        assert_eq!(repaired, 0, "repair must not claim an unverifiable success");

        let after = std::fs::read(&vault_path).unwrap();
        assert_eq!(
            before, after,
            "repair must leave the vault untouched when it cannot fix it"
        );
    }

    /// P2-09 safety regression: an explicitly supplied stale sidecar can consider a
    /// currently healthy block "wrong" relative to its old shard checksums. Repair
    /// must verify every reconstructed block before persisting, not only the block
    /// that scrub marked damaged.
    #[test]
    fn p2_repair_refuses_stale_sidecar_that_rewrites_healthy_block() {
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("stale-sidecar.aerovault");
        let f1 = dir.path().join("a.txt");
        let f2 = dir.path().join("b.txt");
        std::fs::write(&f1, b"first file initial payload").unwrap();
        std::fs::write(&f2, b"second file stable payload").unwrap();

        create_empty_vault(
            &vault_path,
            "stale-pw-1234",
            DEFAULT_ZSTD_LEVEL,
            Some(RecoveryPlacement::Embedded),
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();
        let mut vault = open_vault(&vault_path, "stale-pw-1234").unwrap();
        append_file_at(&mut vault, &f1, "a.txt").unwrap();
        append_file_at(&mut vault, &f2, "b.txt").unwrap();
        save_open_vault(&mut vault).unwrap();

        let stale_sidecar = dir.path().join("old.aerocorrect");
        export_parity(&vault_path, "stale-pw-1234", Some(&stale_sidecar)).unwrap();

        // Simulate a later legitimate change to the first chunk: the manifest hash
        // matches the new bytes, so scrub considers it healthy, but the stale sidecar
        // would reconstruct it back to the old bytes.
        let mut current = open_vault(&vault_path, "stale-pw-1234").unwrap();
        let mut recs: Vec<_> = current.manifest.chunks.values().cloned().collect();
        recs.sort_by_key(|r| r.data_offset);
        assert!(recs.len() >= 2);
        let healthy_id = recs[0].id.clone();
        let damaged_id = recs[1].id.clone();
        let healthy = current.manifest.chunks.get(&healthy_id).unwrap().clone();
        let body_start = healthy.data_offset as usize + 8;
        let body_end = body_start + healthy.block_len as usize;
        current.data[body_start] ^= 0x5A;
        let new_hash = blake3::hash(&current.data[body_start..body_end])
            .to_hex()
            .to_string();
        current
            .manifest
            .chunks
            .get_mut(&healthy_id)
            .unwrap()
            .cipher_hash = new_hash;
        assert!(
            scrub_vault(&current).is_empty(),
            "the changed block is healthy according to the manifest"
        );
        save_open_vault(&mut current).unwrap();

        let before = std::fs::read(&vault_path).unwrap();
        let mut under_repair = open_vault(&vault_path, "stale-pw-1234").unwrap();
        let damaged = under_repair
            .manifest
            .chunks
            .get(&damaged_id)
            .unwrap()
            .clone();
        under_repair.data[damaged.data_offset as usize + 8] ^= 0x33;
        assert_eq!(
            scrub_vault(&under_repair).len(),
            1,
            "only the second block should be marked damaged"
        );

        let (repaired, source) =
            repair_vault(&mut under_repair, false, Some(&stale_sidecar)).unwrap();
        assert_eq!(source, ParitySource::Explicit);
        assert_eq!(
            repaired, 0,
            "stale sidecar must not be allowed to rewrite a healthy block"
        );
        assert_eq!(
            before,
            std::fs::read(&vault_path).unwrap(),
            "failed repair must leave the vault byte-for-byte untouched"
        );
    }

    /// P2-09 regression: the v1 format produced ~200% parity for a small single-chunk
    /// vault (300 KB -> ~600 KB parity). The v2 fixed-grid format must keep the stored
    /// Error Correction payload near the nominal P/K (20%).
    #[test]
    fn p2_09_error_correction_overhead_is_bounded_for_single_chunk_vault() {
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("overhead.aerovault");
        let f = dir.path().join("blob.bin");
        let data = error_correction_test_blob(300_000);
        std::fs::write(&f, &data).unwrap();

        create_empty_vault(
            &vault_path,
            "ovh-pw-1234",
            DEFAULT_ZSTD_LEVEL,
            Some(RecoveryPlacement::Embedded),
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();
        let mut vault = open_vault(&vault_path, "ovh-pw-1234").unwrap();
        append_file_at(&mut vault, &f, "blob.bin").unwrap();
        save_open_vault(&mut vault).unwrap();

        // save_open_vault updates only a local extensions clone, so read the persisted
        // Error Correction payload length from the on-disk header (extension_payload_len @184) rather
        // than the stale in-memory entry.
        let raw = std::fs::read(&vault_path).unwrap();
        let error_correction_payload = u64::from_le_bytes(raw[184..192].try_into().unwrap()) as f64;
        let protected: f64 = vault
            .manifest
            .chunks
            .values()
            .map(|c| 8.0 + c.block_len as f64)
            .sum();

        assert!(
            error_correction_payload < protected * 0.30,
            "Error Correction overhead too high: {} payload bytes for {} protected bytes ({:.0}%)",
            error_correction_payload,
            protected,
            error_correction_payload / protected * 100.0
        );
    }

    /// P1-06: First compatibility test - a "v4-stub" vault (created with Error Correction extension)
    /// must still be fully readable using the pure v3 open/extract paths
    /// (simulating an older v3-only reader or binary).
    #[test]
    fn v3_stub_error_correction_vault_readable_by_pure_v3_open_and_extract() {
        let dir = tempfile::tempdir().unwrap();
        let vault_path = dir.path().join("compat-stub.aerovault");
        let payload = dir.path().join("payload.bin");
        std::fs::write(
            &payload,
            b"P1-06 compatibility payload for stub Error Correction vault",
        )
        .unwrap();

        // Create using the Error Correction stub path (what will be "v4" in future)
        create_empty_vault(
            &vault_path,
            "compat-pw-1234",
            DEFAULT_ZSTD_LEVEL,
            Some(RecoveryPlacement::Embedded),
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();

        // Use pure v3 open path (internal open_vault, as old reader would)
        let mut vault = open_vault(&vault_path, "compat-pw-1234").unwrap();
        append_file_at(&mut vault, &payload, "data/compat.bin").unwrap();
        save_open_vault(&mut vault).unwrap();

        // Re-open with pure v3 path and extract
        let reopened = open_vault(&vault_path, "compat-pw-1234").unwrap();
        let out = dir.path().join("extracted.bin");
        extract_entry(&reopened, "data/compat.bin", &out).unwrap();

        assert_eq!(
            std::fs::read(&out).unwrap(),
            b"P1-06 compatibility payload for stub Error Correction vault"
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
        create_empty_vault(
            &vault_path,
            "old-password",
            DEFAULT_ZSTD_LEVEL,
            None,
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();

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
        save_open_vault(&mut vault).unwrap();

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
            None,
            ERROR_CORRECTION_DEFAULT_PCT,
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
        create_empty_vault(
            &vault_path,
            "pack-password",
            DEFAULT_ZSTD_LEVEL,
            None,
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();

        let mut vault = open_vault(&vault_path, "pack-password").unwrap();
        append_sources_batched(&mut vault, &sources).unwrap();
        save_open_vault(&mut vault).unwrap();

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
        create_empty_vault(
            &vault_path,
            "multi-password",
            DEFAULT_ZSTD_LEVEL,
            None,
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();
        let mut vault = open_vault(&vault_path, "multi-password").unwrap();
        append_sources_batched(&mut vault, &sources).unwrap();
        save_open_vault(&mut vault).unwrap();

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
        create_empty_vault(
            &vault_path,
            "shared-password",
            DEFAULT_ZSTD_LEVEL,
            None,
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();
        let mut vault = open_vault(&vault_path, "shared-password").unwrap();
        append_sources_batched(&mut vault, &sources).unwrap();
        // All ten tiny files share one physical pack chunk.
        assert!(vault.manifest.chunks.len() < 10);
        delete_entries_from_manifest(&mut vault, &["s3.txt".to_string()], false).unwrap();
        save_open_vault(&mut vault).unwrap();

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
    // Reusable scaffolding for the v4 Error Correction scrub work: deterministic ways to
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
        create_empty_vault(
            &vault_path,
            password,
            DEFAULT_ZSTD_LEVEL,
            None,
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();
        let mut v = open_vault(&vault_path, password).unwrap();
        append_file_at(&mut v, &src, "payload.bin").unwrap();
        save_open_vault(&mut v).unwrap();
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
        // exact hook the v4 Error Correction scrub will hang off.
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
        create_empty_vault(
            &archive_path,
            "bounds-pw",
            19,
            None,
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();
        let mut av = open_vault(&archive_path, "bounds-pw").unwrap();
        let b = manifest_cdc_bounds(&av.manifest).unwrap();
        assert_eq!(b.avg, 4 * 1024 * 1024, "archive must widen CDC avg");
        append_file_at(&mut av, &src, "big.bin").unwrap();
        save_open_vault(&mut av).unwrap();

        // balanced profile keeps the const bounds.
        let bal_path = dir.path().join("balanced.aerovault");
        create_empty_vault(
            &bal_path,
            "bounds-pw",
            DEFAULT_ZSTD_LEVEL,
            None,
            ERROR_CORRECTION_DEFAULT_PCT,
        )
        .unwrap();
        let mut bv = open_vault(&bal_path, "bounds-pw").unwrap();
        assert_eq!(manifest_cdc_bounds(&bv.manifest).unwrap().avg, CDC_AVG);
        append_file_at(&mut bv, &src, "big.bin").unwrap();
        save_open_vault(&mut bv).unwrap();

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
