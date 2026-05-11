//! AeroVault v3 draft backend.
//!
//! v3 is the wrapper-stack format: content-defined chunks, keyed BLAKE3
//! chunk identifiers, zstd-per-chunk compression, AES-256-GCM-SIV content
//! encryption, and an extension directory reserved for v4 ECC.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use aes_gcm_siv::aead::{Aead, Payload};
use aes_gcm_siv::{Aes256GcmSiv, KeyInit, Nonce};
use aes_kw::Kek;
use hmac::{Hmac, Mac};
use rand::RngCore;
use secrecy::zeroize::Zeroize;
use serde::{Deserialize, Serialize};
use sha2::Sha512;
use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

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

const DATA_OFFSET: u64 = HEADER_SIZE as u64;
const DEFAULT_ZSTD_LEVEL: i32 = 9;
const CDC_MIN: usize = 256 * 1024;
const CDC_AVG: usize = 1024 * 1024;
const CDC_MAX: usize = 4 * 1024 * 1024;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AlgorithmSpec {
    algorithm_id: String,
    algorithm_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    level: Option<i32>,
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
    master_key: [u8; KEY_SIZE],
    mac_key: [u8; KEY_SIZE],
    manifest: VaultManifestV3,
    extensions: Vec<ExtensionEntryV3>,
    data: Vec<u8>,
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
        },
        chunking: AlgorithmSpec {
            algorithm_id: "gear-cdc".to_string(),
            algorithm_version: 1,
            level: None,
        },
        chunk_id: AlgorithmSpec {
            algorithm_id: "blake3-keyed-128".to_string(),
            algorithm_version: 1,
            level: None,
        },
        compression: AlgorithmSpec {
            algorithm_id: "zstd".to_string(),
            algorithm_version: 1,
            level: Some(level),
        },
        crypt: AlgorithmSpec {
            algorithm_id: "aes-256-gcm-siv".to_string(),
            algorithm_version: 1,
            level: None,
        },
        cipher_hash: AlgorithmSpec {
            algorithm_id: "blake3-256".to_string(),
            algorithm_version: 1,
            level: None,
        },
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

fn chunk_ranges(data: &[u8]) -> Vec<(usize, usize)> {
    if data.is_empty() {
        return Vec::new();
    }
    if data.len() <= CDC_MIN {
        return vec![(0, data.len())];
    }

    let table = gear_table();
    let mask = (CDC_AVG as u64) - 1;
    let mut ranges = Vec::new();
    let mut start = 0usize;
    let mut rolling = 0u64;

    for (idx, byte) in data.iter().enumerate() {
        rolling = rolling.rotate_left(1).wrapping_add(table[*byte as usize]);
        let len = idx + 1 - start;
        if len >= CDC_MIN && ((rolling & mask) == 0 || len >= CDC_MAX) {
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

fn create_directory_in_manifest(manifest: &mut VaultManifestV3, dir_path: &str) -> Result<bool, String> {
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

fn append_file_at(vault: &mut OpenVaultV3, source: &Path, entry_path: &str) -> Result<(), String> {
    let entry_path = normalize_vault_relative_path(entry_path)?;
    if !source.is_file() {
        return Err(format!("Not a regular file: {}", source.display()));
    }
    ensure_parent_directories(&mut vault.manifest, &entry_path)?;

    if let Some(kind) = entry_kind(&vault.manifest, &entry_path) {
        match kind {
            EntryKindV3::Directory => {
                return Err(format!("Destination already exists as directory: {entry_path}"));
            }
            EntryKindV3::File => {
                vault.manifest.entries.retain(|entry| entry.path != entry_path);
            }
        }
    }

    let mut plaintext =
        std::fs::read(source).map_err(|e| format!("Read {}: {e}", source.display()))?;
    let size = plaintext.len() as u64;
    let chunk_key = hkdf_expand::<KEY_SIZE>(&vault.master_key, HKDF_CHUNK_ID)?;
    let level = manifest_zstd_level(&vault.manifest);
    let mut entry_chunks = Vec::new();

    for (start, end) in chunk_ranges(&plaintext) {
        let chunk = &plaintext[start..end];
        let chunk_id = keyed_chunk_id(&chunk_key, chunk);
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
            vault.manifest.chunks.insert(
                chunk_id.clone(),
                ChunkRecordV3 {
                    id: chunk_id.clone(),
                    block_index,
                    data_offset,
                    block_len: encrypted.len() as u64,
                    plaintext_len: chunk.len() as u64,
                    compressed_len: compressed.len() as u64,
                    cipher_hash,
                },
            );
        }
        entry_chunks.push(chunk_id);
    }
    plaintext.zeroize();

    vault.manifest.entries.push(ManifestEntryV3 {
        path: entry_path,
        size,
        modified: now_iso(),
        is_dir: false,
        chunks: entry_chunks,
    });
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
                vault.manifest.entries.retain(|entry| entry.path != entry_name);
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

    let mut out = Vec::with_capacity(entry.size.min(32 * 1024 * 1024) as usize);
    for chunk_id in &entry.chunks {
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
        let compressed = decrypt_with_aad(&vault.master_key, encrypted, &aad)?;
        let plaintext = zstd::stream::decode_all(&compressed[..])
            .map_err(|e| format!("zstd decompress failed: {e}"))?;
        if plaintext.len() as u64 != record.plaintext_len {
            return Err(format!("Plaintext length mismatch for chunk {chunk_id}"));
        }
        out.extend_from_slice(&plaintext);
    }
    out.truncate(entry.size as usize);
    atomic_write(output_path, &out)?;
    out.zeroize();
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
    let master_key = unwrap_key(&kek_master, &header.wrapped_master_key)?;

    validate_ranges(&header, file_len)?;

    let data = read_capped(
        &mut file,
        header.data_offset,
        header.data_len,
        u64::MAX,
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
        header,
        master_key,
        mac_key,
        manifest,
        extensions,
        data,
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

fn append_file(vault: &mut OpenVaultV3, source: &Path) -> Result<(), String> {
    let name = safe_entry_name(source)?;
    append_file_at(vault, source, &name)
}

fn save_open_vault(vault: &OpenVaultV3) -> Result<(), String> {
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
                    return Err("Destination for directory extraction must be a directory".to_string());
                }
                dest_path.join(path_basename(&entry_name))
            } else {
                dest_path.to_path_buf()
            };
            std::fs::create_dir_all(&output_root)
                .map_err(|e| format!("Create output dir: {e}"))?;

            let prefix = format!("{entry_name}/");
            let mut descendants: Vec<&ManifestEntryV3> = vault
                .manifest
                .entries
                .iter()
                .filter(|entry| entry.path == entry_name || entry.path.starts_with(&prefix))
                .collect();
            descendants.sort_by(|a, b| a.path.cmp(&b.path));

            for entry in descendants {
                let rel = if entry.path == entry_name {
                    String::new()
                } else {
                    entry.path[entry_name.len() + 1..].to_string()
                };
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
    }
}

#[tauri::command]
pub async fn vault_v3_create(
    vault_path: String,
    password: String,
    compression_profile: Option<String>,
) -> Result<String, String> {
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
    let mut vault = open_vault(&vault_path, &password)?;
    for file_path in file_paths {
        let path = PathBuf::from(&file_path);
        if !path.is_file() {
            return Err(format!("Not a regular file: {file_path}"));
        }
        append_file(&mut vault, &path)?;
    }
    save_open_vault(&vault)?;
    Ok(info_from_manifest(&vault.manifest))
}

#[tauri::command]
pub async fn vault_v3_add_files_to_dir(
    vault_path: String,
    password: String,
    file_paths: Vec<String>,
    target_dir: String,
) -> Result<serde_json::Value, String> {
    let target_dir = normalize_vault_relative_path(&target_dir)?;
    let mut vault = open_vault(&vault_path, &password)?;
    create_directory_in_manifest(&mut vault.manifest, &target_dir)?;
    let mut added = 0usize;
    for file_path in file_paths {
        let path = PathBuf::from(&file_path);
        let name = safe_entry_name(&path)?;
        append_file_at(&mut vault, &path, &join_vault_path(&target_dir, &name))?;
        added += 1;
    }
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

    let mut vault = open_vault(&vault_path, &password)?;
    let mut added_dirs = 0usize;
    for dir_entry in dirs {
        if create_directory_in_manifest(&mut vault.manifest, &dir_entry.rel_path)? {
            added_dirs += 1;
        }
    }

    let total_files = files.len();
    let mut added_files = 0usize;
    let mut last_emit = std::time::Instant::now();
    let throttle = std::time::Duration::from_millis(150);

    for file_entry in files {
        append_file_at(&mut vault, &file_entry.abs_path, &file_entry.rel_path)?;
        added_files += 1;
        if last_emit.elapsed() >= throttle || added_files == total_files {
            let _ = app.emit(
                "vault-add-progress",
                serde_json::json!({
                    "current": added_files,
                    "total": total_files,
                    "current_file": file_entry.rel_path
                }),
            );
            last_emit = std::time::Instant::now();
        }
    }

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
        let ranges = chunk_ranges(&data);
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
        append_file(&mut vault, &a).unwrap();
        append_file(&mut vault, &b).unwrap();
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
        append_file_at(&mut vault, &source.join("docs/nested/readme.txt"), "docs/nested/readme.txt").unwrap();
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
}
