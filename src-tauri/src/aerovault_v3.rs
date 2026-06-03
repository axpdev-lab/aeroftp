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
    header.extension_payload_len = 0;
    header.header_mac = [0u8; MAC_SIZE];
    header.header_mac = header.compute_mac(mac_key)?;

    let mut out = Vec::with_capacity(
        HEADER_SIZE + data.len() + encrypted_manifest.len() + extension_dir.len(),
    );
    out.extend_from_slice(&header.to_bytes());
    out.extend_from_slice(data);
    out.extend_from_slice(&encrypted_manifest);
    out.extend_from_slice(&extension_dir);
    Ok(out)
}

fn create_empty_vault(path: &Path, password: &str, level: i32) -> Result<(), String> {
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
    let bytes = build_file_bytes(header, &mac_key, &master_key, &manifest, &[], &[])?;
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
    for ext in &extensions {
        if ext.critical {
            return Err(format!(
                "Unsupported critical AeroVault v3 extension: {}",
                ext.extension_id
            ));
        }
    }

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
    let bytes = build_file_bytes(
        vault.header.clone(),
        &vault.mac_key,
        &vault.master_key,
        &vault.manifest,
        &vault.extensions,
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
    create_empty_vault(Path::new(&vault_path), &password, level)?;
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
pub async fn vault_v3_security_info() -> serde_json::Value {
    serde_json::json!({
        "version": "3.0-draft",
        "pipeline": [
            "small-file-batching",
            "gear-cdc",
            "blake3-keyed-128 chunk ids",
            "zstd per chunk",
            "AES-256-GCM-SIV",
            "BLAKE3-256 cipher block hashes",
            "extension directory for ECC"
        ],
        "compression_profiles": {
            "fast": 3,
            "balanced": 9,
            "archive": 19
        },
        "compatibility": "v4 is expected to read v3 directly; v3 skips unknown non-critical extensions"
    })
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

    #[test]
    fn v3_directory_ops_and_password_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::create_dir_all(source.join("docs/nested")).unwrap();
        std::fs::write(source.join("docs/guide.txt"), b"guide").unwrap();
        std::fs::write(source.join("docs/nested/readme.txt"), b"nested").unwrap();

        let vault_path = dir.path().join("dir-test.aerovault");
        create_empty_vault(&vault_path, "old-password", DEFAULT_ZSTD_LEVEL).unwrap();

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
        create_empty_vault(&vault_path, "pack-password", DEFAULT_ZSTD_LEVEL).unwrap();

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
        create_empty_vault(&vault_path, "multi-password", DEFAULT_ZSTD_LEVEL).unwrap();
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
        create_empty_vault(&vault_path, "shared-password", DEFAULT_ZSTD_LEVEL).unwrap();
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
        create_empty_vault(&vault_path, password, DEFAULT_ZSTD_LEVEL).unwrap();
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
        create_empty_vault(&archive_path, "bounds-pw", 19).unwrap();
        let mut av = open_vault(&archive_path, "bounds-pw").unwrap();
        let b = manifest_cdc_bounds(&av.manifest).unwrap();
        assert_eq!(b.avg, 4 * 1024 * 1024, "archive must widen CDC avg");
        append_file_at(&mut av, &src, "big.bin").unwrap();
        save_open_vault(&av).unwrap();

        // balanced profile keeps the const bounds.
        let bal_path = dir.path().join("balanced.aerovault");
        create_empty_vault(&bal_path, "bounds-pw", DEFAULT_ZSTD_LEVEL).unwrap();
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
