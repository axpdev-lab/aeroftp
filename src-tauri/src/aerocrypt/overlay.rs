//! AeroCrypt transparent overlay codec (file format `AECR`).
//!
//! One local file maps to one remote object: per-file encrypted blobs with
//! obfuscated names, the shape that keeps an encrypted scope browsable and
//! syncable object by object (master plan 3.7).
//!
//! - **v3 (current)** is built on the shared [`crate::aerocrypt`] engine:
//!   AES-256-GCM-SIV content under a per-file random DEK that is wrapped with
//!   AES-256-KW under the Argon2id-128 master KEK. Every block binds the block
//!   index **and the total block count** as AAD, and the total count is also
//!   carried (authenticated) in the header, so truncation and append are
//!   detected and fail closed. The config carries a key-bound MAC so a tampered
//!   `version`/`salt` is rejected on unlock (closes the unauthenticated-config
//!   downgrade and gives a clean wrong-password signal).
//! - **v2 / v1 (legacy)** stay **read-only**: existing overlays keep decrypting
//!   transparently, but new objects are always written as v3. v2 = GCM-SIV per
//!   the shared engine without the length binding; v1 = original plain
//!   AES-256-GCM (HKDF per-file key, Argon2id-64).
//!
//! Filenames use AES-256-SIV via [`crate::aerocrypt::names`] in every version.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce as AesNonce};
use base64::Engine as _;
use hkdf::Hkdf;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use super::{
    decrypt_with_aad, derive_base_kek, encrypt_with_aad, hkdf_expand, random_array, unwrap_key,
    wrap_key, KEY_SIZE, NONCE_SIZE, WRAPPED_KEY_SIZE,
};

/// Magic bytes for an AeroCrypt-encrypted file.
pub const MAGIC: &[u8; 4] = b"AECR";
/// Legacy plain-GCM format (read-only).
pub const VERSION_V1: u8 = 1;
/// Legacy GCM-SIV format without length binding (read-only).
pub const VERSION_V2: u8 = 2;
/// Current format: GCM-SIV + AES-KW + per-block index/total binding.
pub const VERSION_V3: u8 = 3;
/// Streaming block size (64 KiB plaintext per AEAD block).
const BLOCK_SIZE: usize = 64 * 1024;
const SALT_V1_SIZE: usize = 16;
const SALT_V2_SIZE: usize = 32; // == super::SALT_SIZE
const SALT_V3_SIZE: usize = 32;
const CONFIG_MAC_SIZE: usize = 32;

/// Legacy Argon2id parameters for v1 overlays (balanced 64 MiB / t3 / p4).
const ARGON2_V1_MEM_KIB: u32 = 65536;
const ARGON2_V1_TIME: u32 = 3;
const ARGON2_V1_LANES: u32 = 4;

/// Domain-separating AAD prefix for v2 content blocks; the block index is
/// appended so blocks cannot be reordered within a file.
const V2_BLOCK_AAD_PREFIX: &[u8] = b"AeroCrypt overlay v2 block";
/// Domain-separating AAD prefix for v3 content blocks; the block index **and**
/// the total block count are appended so reorder, truncation, and append all
/// fail closed.
const V3_BLOCK_AAD_PREFIX: &[u8] = b"AeroCrypt overlay v3 block";
/// Domain-separating label for the v3 config MAC (key-bound config integrity).
const V3_CONFIG_MAC_LABEL: &[u8] = b"AeroCrypt overlay v3 config MAC";

/// GCM tag length added to every AEAD block.
const GCM_TAG: usize = 16;

/// A parsed `.aeroftp-crypt.json` overlay configuration.
#[derive(Debug, Clone)]
pub enum OverlayConfig {
    V1 {
        salt: [u8; SALT_V1_SIZE],
    },
    V2 {
        salt: [u8; SALT_V2_SIZE],
    },
    V3 {
        salt: [u8; SALT_V3_SIZE],
        mac: [u8; CONFIG_MAC_SIZE],
    },
}

impl OverlayConfig {
    pub fn version(&self) -> u8 {
        match self {
            OverlayConfig::V1 { .. } => VERSION_V1,
            OverlayConfig::V2 { .. } => VERSION_V2,
            OverlayConfig::V3 { .. } => VERSION_V3,
        }
    }

    /// True for legacy formats that are kept readable but never written.
    pub fn is_read_only(&self) -> bool {
        !matches!(self, OverlayConfig::V3 { .. })
    }
}

// --- Key derivation -------------------------------------------------------

/// Legacy v1 master-key derivation (Argon2id 64 MiB / t3 / p4, 16-byte salt).
fn derive_master_key_v1(
    password: &str,
    salt: &[u8; SALT_V1_SIZE],
) -> Result<[u8; KEY_SIZE], String> {
    let params = argon2::Params::new(
        ARGON2_V1_MEM_KIB,
        ARGON2_V1_TIME,
        ARGON2_V1_LANES,
        Some(KEY_SIZE),
    )
    .map_err(|e| format!("Argon2 params: {e}"))?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut key = [0u8; KEY_SIZE];
    argon
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| format!("Argon2 derive: {e}"))?;
    Ok(key)
}

/// Derive the overlay master key for the given config version.
pub fn derive_master_key(cfg: &OverlayConfig, password: &str) -> Result<[u8; KEY_SIZE], String> {
    match cfg {
        OverlayConfig::V1 { salt } => derive_master_key_v1(password, salt),
        OverlayConfig::V2 { salt } => derive_base_kek(password, salt),
        OverlayConfig::V3 { salt, .. } => derive_base_kek(password, salt),
    }
}

/// Verify the key-bound config MAC for a v3 overlay. Returns `Err` when the
/// password is wrong or the config (`version`/`salt`) was tampered with.
/// Legacy v1/v2 carry no MAC and always pass (they fail closed later on the
/// AEAD instead).
pub fn verify_config_mac(cfg: &OverlayConfig, master_key: &[u8; KEY_SIZE]) -> Result<(), String> {
    match cfg {
        OverlayConfig::V3 { salt, mac } => {
            let expected = compute_config_mac_v3(master_key, salt)?;
            if expected.ct_eq(mac).into() {
                Ok(())
            } else {
                Err("wrong password or tampered crypt config".to_string())
            }
        }
        _ => Ok(()),
    }
}

/// Compute the v3 config MAC: a key-bound PRF over the security-relevant
/// parameters (label, version, block size, Argon2 profile, salt). Because the
/// master key already depends on the salt, an attacker who rewrites the config
/// cannot forge a matching MAC without the password.
fn compute_config_mac_v3(
    master_key: &[u8; KEY_SIZE],
    salt: &[u8; SALT_V3_SIZE],
) -> Result<[u8; CONFIG_MAC_SIZE], String> {
    let mut info = Vec::with_capacity(V3_CONFIG_MAC_LABEL.len() + 1 + 4 + 12 + SALT_V3_SIZE);
    info.extend_from_slice(V3_CONFIG_MAC_LABEL);
    info.push(VERSION_V3);
    info.extend_from_slice(&(BLOCK_SIZE as u32).to_le_bytes());
    info.extend_from_slice(&super::argon2_mem_kib().to_le_bytes());
    info.extend_from_slice(&super::argon2_time().to_le_bytes());
    info.extend_from_slice(&super::argon2_lanes().to_le_bytes());
    info.extend_from_slice(salt);
    hkdf_expand::<CONFIG_MAC_SIZE>(master_key, &info)
}

// --- v1 legacy content path (read-only) -----------------------------------

fn derive_file_key_v1(
    master_key: &[u8; KEY_SIZE],
    nonce: &[u8; NONCE_SIZE],
) -> Result<Zeroizing<[u8; KEY_SIZE]>, String> {
    let hk = Hkdf::<Sha256>::new(Some(nonce), master_key);
    let mut file_key = Zeroizing::new([0u8; KEY_SIZE]);
    hk.expand(b"aeroftp-crypt-file-key", file_key.as_mut())
        .map_err(|_| "HKDF file-key expand failed".to_string())?;
    Ok(file_key)
}

fn decrypt_data_v1(master_key: &[u8; KEY_SIZE], ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    let header = 4 + 1 + NONCE_SIZE;
    if ciphertext.len() < header {
        return Err("AeroCrypt v1 data too short".into());
    }
    let master_nonce: [u8; NONCE_SIZE] = ciphertext[5..header]
        .try_into()
        .map_err(|_| "AeroCrypt v1 header truncated".to_string())?;
    let file_key = derive_file_key_v1(master_key, &master_nonce)?;
    let cipher = Aes256Gcm::new((&*file_key).into());

    let data = &ciphertext[header..];
    let block_cipher_size = BLOCK_SIZE + GCM_TAG;
    let mut plaintext = Vec::with_capacity(data.len());
    let mut block_idx = 0usize;
    let mut pos = 0usize;
    while pos < data.len() {
        let end = (pos + block_cipher_size).min(data.len());
        let block = &data[pos..end];
        let mut block_nonce = master_nonce;
        let idx_bytes = (block_idx as u32).to_le_bytes();
        for i in 0..4 {
            block_nonce[i] ^= idx_bytes[i];
        }
        let nonce = AesNonce::from_slice(&block_nonce);
        let decrypted = cipher
            .decrypt(nonce, block)
            .map_err(|_| format!("AeroCrypt v1 decrypt failed at block {block_idx}"))?;
        plaintext.extend_from_slice(&decrypted);
        pos = end;
        block_idx += 1;
    }
    Ok(plaintext)
}

// --- v2 legacy content path (read-only) -----------------------------------

fn v2_block_aad(block_index: u64) -> Vec<u8> {
    let mut aad = Vec::with_capacity(V2_BLOCK_AAD_PREFIX.len() + 8);
    aad.extend_from_slice(V2_BLOCK_AAD_PREFIX);
    aad.extend_from_slice(&block_index.to_le_bytes());
    aad
}

/// Ciphertext length of a full GCM-SIV block: per-block nonce + plaintext + tag.
const FULL_BLOCK_CIPHER: usize = NONCE_SIZE + BLOCK_SIZE + GCM_TAG;

/// Recover the plaintext length of a v3 AeroCrypt container from its on-wire
/// (ciphertext) length, WITHOUT reading the object.
///
/// The v3 layout is a fixed [`V3_HEADER_LEN`] header followed by GCM-SIV blocks;
/// each block adds a per-block nonce + tag ([`FULL_BLOCK_CIPHER`] for a full
/// [`BLOCK_SIZE`] block), so the mapping is deterministic and reversible from the
/// length alone (mirrors rclone's deterministic overhead map). This lets `stat`
/// and `size` report the DECRYPTED size for a v3 overlay, so the browser size
/// column and AeroSync size compares converge instead of re-flagging on every run.
///
/// Legacy v1/v2 containers use a different header and are NOT handled here (they
/// are read-only and their size stays deferred). A length that cannot correspond
/// to a well-formed v3 container (too short, or a final block below the per-block
/// overhead) is clamped rather than erroring: this feeds size-only UI/compare and
/// must never panic on a foreign or truncated object.
pub fn v3_decrypted_size(ciphertext_len: u64) -> u64 {
    let header = V3_HEADER_LEN as u64;
    let full_block = FULL_BLOCK_CIPHER as u64;
    let block = BLOCK_SIZE as u64;
    let per_block_overhead = (NONCE_SIZE + GCM_TAG) as u64;
    let Some(data) = ciphertext_len.checked_sub(header) else {
        // Too short to be a v3 container: leave the length unchanged.
        return ciphertext_len;
    };
    let full_blocks = data / full_block;
    let rem = data % full_block;
    let mut plain = full_blocks.saturating_mul(block);
    if rem > 0 {
        // Final partial block = nonce + partial_plaintext + tag; clamp a
        // malformed sub-overhead remainder to zero rather than underflowing.
        plain += rem.saturating_sub(per_block_overhead);
    }
    plain
}

fn decrypt_data_v2(master_key: &[u8; KEY_SIZE], ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    let header = 4 + 1 + WRAPPED_KEY_SIZE;
    if ciphertext.len() < header {
        return Err("AeroCrypt v2 data too short".into());
    }
    let wrapped: [u8; WRAPPED_KEY_SIZE] = ciphertext[5..header]
        .try_into()
        .map_err(|_| "AeroCrypt v2 header truncated".to_string())?;
    let dek = Zeroizing::new(unwrap_key(master_key, &wrapped)?);

    let data = &ciphertext[header..];
    let mut plaintext = Vec::with_capacity(data.len());
    let mut block_idx = 0u64;
    let mut pos = 0usize;
    while pos < data.len() {
        let end = (pos + FULL_BLOCK_CIPHER).min(data.len());
        let block = &data[pos..end];
        let pt = decrypt_with_aad(&dek, block, &v2_block_aad(block_idx))?;
        plaintext.extend_from_slice(&pt);
        pos = end;
        block_idx += 1;
    }
    Ok(plaintext)
}

// --- v3 content path (current; length-bound) -------------------------------

fn v3_block_aad(block_index: u64, total_blocks: u64) -> Vec<u8> {
    let mut aad = Vec::with_capacity(V3_BLOCK_AAD_PREFIX.len() + 16);
    aad.extend_from_slice(V3_BLOCK_AAD_PREFIX);
    aad.extend_from_slice(&block_index.to_le_bytes());
    aad.extend_from_slice(&total_blocks.to_le_bytes());
    aad
}

/// v3 header: MAGIC(4) + VERSION(1) + total_blocks(8 LE) + wrapped DEK(40).
const V3_HEADER_LEN: usize = 4 + 1 + 8 + WRAPPED_KEY_SIZE;

fn encrypt_data_v3(master_key: &[u8; KEY_SIZE], plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let dek = Zeroizing::new(random_array::<KEY_SIZE>());
    let wrapped = wrap_key(master_key, &dek)?;

    // ceil(len / BLOCK_SIZE); an empty file has zero blocks.
    let total_blocks = plaintext.len().div_ceil(BLOCK_SIZE) as u64;

    let mut output = Vec::with_capacity(V3_HEADER_LEN + plaintext.len() + FULL_BLOCK_CIPHER);
    output.extend_from_slice(MAGIC);
    output.push(VERSION_V3);
    output.extend_from_slice(&total_blocks.to_le_bytes());
    output.extend_from_slice(&wrapped);

    for (block_idx, chunk) in plaintext.chunks(BLOCK_SIZE).enumerate() {
        let block = encrypt_with_aad(&dek, chunk, &v3_block_aad(block_idx as u64, total_blocks))?;
        output.extend_from_slice(&block);
    }
    Ok(output)
}

fn decrypt_data_v3(master_key: &[u8; KEY_SIZE], ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    if ciphertext.len() < V3_HEADER_LEN {
        return Err("AeroCrypt v3 data too short".into());
    }
    let total_blocks = u64::from_le_bytes(
        ciphertext[5..13]
            .try_into()
            .map_err(|_| "AeroCrypt v3 header truncated".to_string())?,
    );
    let wrapped: [u8; WRAPPED_KEY_SIZE] = ciphertext[13..V3_HEADER_LEN]
        .try_into()
        .map_err(|_| "AeroCrypt v3 header truncated".to_string())?;
    let dek = Zeroizing::new(unwrap_key(master_key, &wrapped)?);

    let data = &ciphertext[V3_HEADER_LEN..];
    let mut plaintext = Vec::with_capacity(data.len());
    let mut block_idx = 0u64;
    let mut pos = 0usize;
    while pos < data.len() {
        let end = (pos + FULL_BLOCK_CIPHER).min(data.len());
        let block = &data[pos..end];
        let pt = decrypt_with_aad(&dek, block, &v3_block_aad(block_idx, total_blocks))?;
        plaintext.extend_from_slice(&pt);
        pos = end;
        block_idx += 1;
    }
    // Length binding: the number of blocks actually present must equal the
    // authenticated count, with no trailing bytes. This rejects silent
    // truncation (fewer blocks) and append (extra bytes / blocks).
    if block_idx != total_blocks {
        return Err(format!(
            "AeroCrypt v3 truncated: expected {total_blocks} blocks, found {block_idx}"
        ));
    }
    if pos != data.len() {
        return Err("AeroCrypt v3 trailing data after final block".into());
    }
    Ok(plaintext)
}

// --- Public content API (version-dispatched) ------------------------------

/// Encrypt a file's bytes. New objects are always written as v3; legacy v1/v2
/// overlays are read-only and return an error so a downgraded or stale config
/// can never produce weaker ciphertext.
pub fn encrypt_data(
    cfg: &OverlayConfig,
    master_key: &[u8; KEY_SIZE],
    plaintext: &[u8],
) -> Result<Vec<u8>, String> {
    match cfg {
        OverlayConfig::V3 { .. } => encrypt_data_v3(master_key, plaintext),
        OverlayConfig::V2 { .. } | OverlayConfig::V1 { .. } => Err(format!(
            "legacy AeroCrypt v{} overlay is read-only; create a new overlay to add files",
            cfg.version()
        )),
    }
}

/// Decrypt an AeroCrypt blob, dispatching on its embedded version byte so a
/// reader transparently handles v1, v2, and v3 objects.
pub fn decrypt_data(master_key: &[u8; KEY_SIZE], ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    if ciphertext.len() < 5 {
        return Err("AeroCrypt data too short".into());
    }
    if &ciphertext[0..4] != MAGIC {
        return Err("Not an AeroCrypt encrypted file".into());
    }
    match ciphertext[4] {
        VERSION_V1 => decrypt_data_v1(master_key, ciphertext),
        VERSION_V2 => decrypt_data_v2(master_key, ciphertext),
        VERSION_V3 => decrypt_data_v3(master_key, ciphertext),
        other => Err(format!("Unsupported AeroCrypt version {other}")),
    }
}

// --- Config (`.aeroftp-crypt.json`) ---------------------------------------

/// Generate a fresh 32-byte salt for a new v3 overlay.
pub fn random_salt_v3() -> [u8; SALT_V3_SIZE] {
    random_array::<SALT_V3_SIZE>()
}

/// Build the v3 config JSON written at the root of a new overlay. The MAC binds
/// the config to the master key so later tampering of `version`/`salt` (and the
/// wrong password) is detected on unlock.
pub fn init_config_v3(
    salt: &[u8; SALT_V3_SIZE],
    master_key: &[u8; KEY_SIZE],
) -> Result<String, String> {
    let mac = compute_config_mac_v3(master_key, salt)?;
    Ok(serde_json::json!({
        "version": VERSION_V3,
        "cipher": "AES-256-GCM-SIV",
        "filename_cipher": "AES-256-SIV",
        "key_wrap": "AES-256-KW",
        "kdf": "Argon2id",
        "kdf_mem_kib": super::argon2_mem_kib(),
        "kdf_time": super::argon2_time(),
        "kdf_lanes": super::argon2_lanes(),
        "salt": base64::engine::general_purpose::STANDARD.encode(salt),
        "block_size": BLOCK_SIZE,
        "mac": base64::engine::general_purpose::STANDARD.encode(mac),
    })
    .to_string())
}

/// Parse an overlay config. A missing or unknown `version` is a hard error
/// (no silent fallback to the legacy weaker format).
pub fn parse_config(config_json: &str) -> Result<OverlayConfig, String> {
    let val: serde_json::Value =
        serde_json::from_str(config_json).map_err(|e| format!("invalid crypt config: {e}"))?;
    let version = val
        .get("version")
        .and_then(|v| v.as_u64())
        .ok_or("missing version in crypt config")?;
    let salt_b64 = val
        .get("salt")
        .and_then(|v| v.as_str())
        .ok_or("missing salt in crypt config")?;
    let salt_bytes = base64::engine::general_purpose::STANDARD
        .decode(salt_b64)
        .map_err(|e| format!("invalid salt: {e}"))?;
    match version {
        1 => {
            if salt_bytes.len() != SALT_V1_SIZE {
                return Err(format!("v1 salt must be {SALT_V1_SIZE} bytes"));
            }
            let mut salt = [0u8; SALT_V1_SIZE];
            salt.copy_from_slice(&salt_bytes);
            Ok(OverlayConfig::V1 { salt })
        }
        2 => {
            if salt_bytes.len() != SALT_V2_SIZE {
                return Err(format!("v2 salt must be {SALT_V2_SIZE} bytes"));
            }
            let mut salt = [0u8; SALT_V2_SIZE];
            salt.copy_from_slice(&salt_bytes);
            Ok(OverlayConfig::V2 { salt })
        }
        3 => {
            if salt_bytes.len() != SALT_V3_SIZE {
                return Err(format!("v3 salt must be {SALT_V3_SIZE} bytes"));
            }
            let mac_b64 = val
                .get("mac")
                .and_then(|v| v.as_str())
                .ok_or("missing mac in v3 crypt config")?;
            let mac_bytes = base64::engine::general_purpose::STANDARD
                .decode(mac_b64)
                .map_err(|e| format!("invalid mac: {e}"))?;
            if mac_bytes.len() != CONFIG_MAC_SIZE {
                return Err(format!("v3 mac must be {CONFIG_MAC_SIZE} bytes"));
            }
            let mut salt = [0u8; SALT_V3_SIZE];
            salt.copy_from_slice(&salt_bytes);
            let mut mac = [0u8; CONFIG_MAC_SIZE];
            mac.copy_from_slice(&mac_bytes);
            Ok(OverlayConfig::V3 { salt, mac })
        }
        other => Err(format!("unsupported crypt config version {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sizes_to_test() -> Vec<usize> {
        vec![
            0,
            1,
            100,
            BLOCK_SIZE - 1,
            BLOCK_SIZE,
            BLOCK_SIZE + 1,
            BLOCK_SIZE * 3 + 77,
        ]
    }

    // Legacy-format writers, kept only for read-compatibility tests.
    fn encrypt_data_v1(master_key: &[u8; KEY_SIZE], plaintext: &[u8]) -> Vec<u8> {
        let master_nonce = random_array::<NONCE_SIZE>();
        let file_key = derive_file_key_v1(master_key, &master_nonce).unwrap();
        let cipher = Aes256Gcm::new((&*file_key).into());
        let mut output = Vec::new();
        output.extend_from_slice(MAGIC);
        output.push(VERSION_V1);
        output.extend_from_slice(&master_nonce);
        for (block_idx, chunk) in plaintext.chunks(BLOCK_SIZE).enumerate() {
            let mut block_nonce = master_nonce;
            let idx_bytes = (block_idx as u32).to_le_bytes();
            for i in 0..4 {
                block_nonce[i] ^= idx_bytes[i];
            }
            let nonce = AesNonce::from_slice(&block_nonce);
            output.extend_from_slice(&cipher.encrypt(nonce, chunk).unwrap());
        }
        output
    }

    fn encrypt_data_v2(master_key: &[u8; KEY_SIZE], plaintext: &[u8]) -> Vec<u8> {
        let dek = random_array::<KEY_SIZE>();
        let wrapped = wrap_key(master_key, &dek).unwrap();
        let mut output = Vec::new();
        output.extend_from_slice(MAGIC);
        output.push(VERSION_V2);
        output.extend_from_slice(&wrapped);
        for (block_idx, chunk) in plaintext.chunks(BLOCK_SIZE).enumerate() {
            output.extend_from_slice(
                &encrypt_with_aad(&dek, chunk, &v2_block_aad(block_idx as u64)).unwrap(),
            );
        }
        output
    }

    fn v3_cfg() -> (OverlayConfig, [u8; KEY_SIZE]) {
        let salt = [9u8; SALT_V3_SIZE];
        let master = derive_base_kek("correct horse battery staple", &salt).unwrap();
        let mac = compute_config_mac_v3(&master, &salt).unwrap();
        (OverlayConfig::V3 { salt, mac }, master)
    }

    #[test]
    fn v3_round_trip_all_sizes() {
        let (cfg, master) = v3_cfg();
        for n in sizes_to_test() {
            let pt: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            let blob = encrypt_data(&cfg, &master, &pt).unwrap();
            assert_eq!(&blob[0..4], MAGIC);
            assert_eq!(blob[4], VERSION_V3);
            assert_eq!(
                decrypt_data(&master, &blob).unwrap(),
                pt,
                "v3 round trip {n}"
            );
        }
    }

    #[test]
    fn v3_decrypted_size_inverts_ciphertext_length() {
        let (cfg, master) = v3_cfg();
        for n in sizes_to_test() {
            let pt: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            let blob = encrypt_data(&cfg, &master, &pt).unwrap();
            assert_eq!(
                v3_decrypted_size(blob.len() as u64),
                n as u64,
                "v3 size map for {n} plaintext bytes (ciphertext {})",
                blob.len()
            );
        }
        // Header-only (empty file, zero blocks) maps to 0.
        assert_eq!(v3_decrypted_size(V3_HEADER_LEN as u64), 0);
        // A length shorter than the header is left unchanged (foreign object).
        assert_eq!(v3_decrypted_size(10), 10);
    }

    #[test]
    fn v3_truncation_is_rejected() {
        let (cfg, master) = v3_cfg();
        let pt: Vec<u8> = (0..BLOCK_SIZE * 3 + 10).map(|i| i as u8).collect();
        let blob = encrypt_data(&cfg, &master, &pt).unwrap();
        // Drop the final (partial) block: this is the silent-truncation attack.
        let truncated = &blob[..blob.len() - (NONCE_SIZE + 10 + GCM_TAG)];
        assert!(
            decrypt_data(&master, truncated).is_err(),
            "tail truncation must fail closed"
        );
        // Drop a whole trailing full block too.
        let truncated2 = &blob[..V3_HEADER_LEN + FULL_BLOCK_CIPHER];
        assert!(decrypt_data(&master, truncated2).is_err());
    }

    #[test]
    fn v3_append_is_rejected() {
        let (cfg, master) = v3_cfg();
        let pt: Vec<u8> = (0..BLOCK_SIZE + 5).map(|i| i as u8).collect();
        let mut blob = encrypt_data(&cfg, &master, &pt).unwrap();
        blob.extend_from_slice(&[0u8; 8]); // trailing garbage
        assert!(decrypt_data(&master, &blob).is_err());
    }

    #[test]
    fn v3_block_reorder_is_rejected() {
        let (cfg, master) = v3_cfg();
        let pt: Vec<u8> = (0..BLOCK_SIZE * 2 + 10).map(|i| i as u8).collect();
        let blob = encrypt_data(&cfg, &master, &pt).unwrap();
        let h = V3_HEADER_LEN;
        let b1 = h + FULL_BLOCK_CIPHER;
        let b2 = h + 2 * FULL_BLOCK_CIPHER;
        let mut swapped = blob[..h].to_vec();
        swapped.extend_from_slice(&blob[b1..b2]);
        swapped.extend_from_slice(&blob[h..b1]);
        swapped.extend_from_slice(&blob[b2..]);
        assert!(decrypt_data(&master, &swapped).is_err());
    }

    #[test]
    fn v3_wrong_password_fails_closed() {
        let (cfg, master) = v3_cfg();
        let blob = encrypt_data(&cfg, &master, b"secret").unwrap();
        let salt = [9u8; SALT_V3_SIZE];
        let wrong = derive_base_kek("wrong", &salt).unwrap();
        assert!(decrypt_data(&wrong, &blob).is_err());
    }

    #[test]
    fn v3_single_bit_flip_is_rejected() {
        let (cfg, master) = v3_cfg();
        let pt: Vec<u8> = (0..1000).map(|i| i as u8).collect();
        let mut blob = encrypt_data(&cfg, &master, &pt).unwrap();
        let last = blob.len() - 1;
        blob[last] ^= 0x01;
        assert!(decrypt_data(&master, &blob).is_err());
    }

    #[test]
    fn legacy_v1_and_v2_still_read() {
        let salt1 = [3u8; SALT_V1_SIZE];
        let m1 = derive_master_key(&OverlayConfig::V1 { salt: salt1 }, "legacy").unwrap();
        let pt = b"legacy AeroCrypt payload".repeat(5000);
        let blob1 = encrypt_data_v1(&m1, &pt);
        assert_eq!(blob1[4], VERSION_V1);
        assert_eq!(decrypt_data(&m1, &blob1).unwrap(), pt);

        let salt2 = [4u8; SALT_V2_SIZE];
        let m2 = derive_master_key(&OverlayConfig::V2 { salt: salt2 }, "legacy").unwrap();
        let blob2 = encrypt_data_v2(&m2, &pt);
        assert_eq!(blob2[4], VERSION_V2);
        assert_eq!(decrypt_data(&m2, &blob2).unwrap(), pt);
    }

    #[test]
    fn legacy_overlays_are_write_frozen() {
        let m = [7u8; KEY_SIZE];
        assert!(encrypt_data(
            &OverlayConfig::V1 {
                salt: [0u8; SALT_V1_SIZE]
            },
            &m,
            b"x"
        )
        .is_err());
        assert!(encrypt_data(
            &OverlayConfig::V2 {
                salt: [0u8; SALT_V2_SIZE]
            },
            &m,
            b"x"
        )
        .is_err());
    }

    #[test]
    fn cross_version_key_mismatch_fails_closed() {
        // A v3 blob must not decrypt under a key derived for a different salt.
        let (cfg, master) = v3_cfg();
        let blob = encrypt_data(&cfg, &master, b"hello world").unwrap();
        let other = derive_base_kek("correct horse battery staple", &[1u8; SALT_V3_SIZE]).unwrap();
        assert!(decrypt_data(&other, &blob).is_err());
    }

    #[test]
    fn v3_config_mac_detects_tamper_and_wrong_password() {
        let salt = random_salt_v3();
        let master = derive_base_kek("right-password", &salt).unwrap();
        let json = init_config_v3(&salt, &master).unwrap();
        let cfg = parse_config(&json).unwrap();
        assert_eq!(cfg.version(), VERSION_V3);
        // Correct password verifies.
        assert!(verify_config_mac(&cfg, &master).is_ok());
        // Wrong password fails the MAC.
        let wrong = derive_base_kek("wrong-password", &salt).unwrap();
        assert!(verify_config_mac(&cfg, &wrong).is_err());
        // Tampered salt (attacker swaps salt) fails: the derived key no longer
        // matches the stored MAC.
        if let OverlayConfig::V3 { mac, .. } = cfg {
            let evil_salt = [2u8; SALT_V3_SIZE];
            let evil_cfg = OverlayConfig::V3 {
                salt: evil_salt,
                mac,
            };
            let evil_key = derive_base_kek("right-password", &evil_salt).unwrap();
            assert!(verify_config_mac(&evil_cfg, &evil_key).is_err());
        }
    }

    #[test]
    fn parse_config_rejects_missing_and_unknown_version() {
        // Missing version is a hard error (no silent v1 fallback).
        let no_ver = serde_json::json!({
            "salt": base64::engine::general_purpose::STANDARD.encode([1u8; SALT_V1_SIZE]),
        })
        .to_string();
        assert!(parse_config(&no_ver).is_err());
        // Unknown version is rejected.
        let bad_ver = serde_json::json!({
            "version": 99,
            "salt": base64::engine::general_purpose::STANDARD.encode([1u8; SALT_V3_SIZE]),
        })
        .to_string();
        assert!(parse_config(&bad_ver).is_err());
        // v3 without a mac is rejected.
        let no_mac = serde_json::json!({
            "version": 3,
            "salt": base64::engine::general_purpose::STANDARD.encode([1u8; SALT_V3_SIZE]),
        })
        .to_string();
        assert!(parse_config(&no_mac).is_err());
    }
}
