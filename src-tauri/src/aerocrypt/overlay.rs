//! AeroCrypt transparent overlay codec (file format `AECR`).
//!
//! One local file maps to one remote object: per-file encrypted blobs with
//! obfuscated names, the shape that keeps an encrypted scope browsable and
//! syncable object by object (master plan 3.7).
//!
//! - **v2 (current, P1)** is built on the shared [`crate::aerocrypt`] engine:
//!   AES-256-GCM-SIV content under a per-file random DEK that is wrapped with
//!   AES-256-KW under the Argon2id-128 master KEK, with the block index bound
//!   as AAD. This is the same audited engine AeroVault uses (#276 "one codec,
//!   one audit pass").
//! - **v1 (legacy)** is the original CLI-only format (plain AES-256-GCM, HKDF
//!   per-file key, Argon2id-64). Kept readable and writable so existing
//!   `.aeroftp-crypt` overlays keep working; new overlays are created as v2.
//!
//! Filenames use AES-256-SIV via [`crate::aerocrypt::names`] in both versions.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce as AesNonce};
use base64::Engine as _;
use hkdf::Hkdf;
use sha2::Sha256;

use super::{
    decrypt_with_aad, derive_base_kek, encrypt_with_aad, random_array, unwrap_key, wrap_key,
    KEY_SIZE, NONCE_SIZE, WRAPPED_KEY_SIZE,
};

/// Magic bytes for an AeroCrypt-encrypted file.
pub const MAGIC: &[u8; 4] = b"AECR";
/// Legacy plain-GCM format.
pub const VERSION_V1: u8 = 1;
/// Current format on the shared GCM-SIV + AES-KW engine.
pub const VERSION_V2: u8 = 2;
/// Streaming block size (64 KiB plaintext per AEAD block).
const BLOCK_SIZE: usize = 64 * 1024;
const SALT_V1_SIZE: usize = 16;
const SALT_V2_SIZE: usize = 32; // == super::SALT_SIZE

/// Legacy Argon2id parameters for v1 overlays (balanced 64 MiB / t3 / p4).
const ARGON2_V1_MEM_KIB: u32 = 65536;
const ARGON2_V1_TIME: u32 = 3;
const ARGON2_V1_LANES: u32 = 4;

/// Domain-separating AAD prefix for v2 content blocks; the block index is
/// appended so blocks cannot be reordered within a file. Cross-file splicing
/// is already impossible because every file has its own random wrapped DEK.
const V2_BLOCK_AAD_PREFIX: &[u8] = b"AeroCrypt overlay v2 block";

/// GCM tag length added to every AEAD block.
const GCM_TAG: usize = 16;

/// A parsed `.aeroftp-crypt.json` overlay configuration.
#[derive(Debug, Clone)]
pub enum OverlayConfig {
    V1 { salt: [u8; SALT_V1_SIZE] },
    V2 { salt: [u8; SALT_V2_SIZE] },
}

impl OverlayConfig {
    pub fn version(&self) -> u8 {
        match self {
            OverlayConfig::V1 { .. } => VERSION_V1,
            OverlayConfig::V2 { .. } => VERSION_V2,
        }
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
    }
}

// --- v1 legacy content path (verbatim from the original CLI module) -------

fn derive_file_key_v1(
    master_key: &[u8; KEY_SIZE],
    nonce: &[u8; NONCE_SIZE],
) -> Result<[u8; KEY_SIZE], String> {
    let hk = Hkdf::<Sha256>::new(Some(nonce), master_key);
    let mut file_key = [0u8; KEY_SIZE];
    hk.expand(b"aeroftp-crypt-file-key", &mut file_key)
        .map_err(|_| "HKDF file-key expand failed".to_string())?;
    Ok(file_key)
}

fn encrypt_data_v1(master_key: &[u8; KEY_SIZE], plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let master_nonce = random_array::<NONCE_SIZE>();
    let file_key = derive_file_key_v1(master_key, &master_nonce)?;
    let cipher = Aes256Gcm::new((&file_key).into());

    let mut output = Vec::with_capacity(
        4 + 1 + NONCE_SIZE + plaintext.len() + (plaintext.len() / BLOCK_SIZE + 1) * GCM_TAG,
    );
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
        let ciphertext = cipher
            .encrypt(nonce, chunk)
            .map_err(|_| "AES-GCM encrypt failed".to_string())?;
        output.extend_from_slice(&ciphertext);
    }
    Ok(output)
}

fn decrypt_data_v1(master_key: &[u8; KEY_SIZE], ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    let header = 4 + 1 + NONCE_SIZE;
    if ciphertext.len() < header {
        return Err("AeroCrypt v1 data too short".into());
    }
    let master_nonce: [u8; NONCE_SIZE] = ciphertext[5..header].try_into().expect("slice length");
    let file_key = derive_file_key_v1(master_key, &master_nonce)?;
    let cipher = Aes256Gcm::new((&file_key).into());

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

// --- v2 content path (shared GCM-SIV + AES-KW engine) ---------------------

fn v2_block_aad(block_index: u64) -> Vec<u8> {
    let mut aad = Vec::with_capacity(V2_BLOCK_AAD_PREFIX.len() + 8);
    aad.extend_from_slice(V2_BLOCK_AAD_PREFIX);
    aad.extend_from_slice(&block_index.to_le_bytes());
    aad
}

/// Ciphertext length of a full v2 block: per-block nonce + plaintext + GCM tag.
const V2_FULL_BLOCK_CIPHER: usize = NONCE_SIZE + BLOCK_SIZE + GCM_TAG;

fn encrypt_data_v2(master_key: &[u8; KEY_SIZE], plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let dek = random_array::<KEY_SIZE>();
    let wrapped = wrap_key(master_key, &dek)?;

    let mut output =
        Vec::with_capacity(4 + 1 + WRAPPED_KEY_SIZE + plaintext.len() + V2_FULL_BLOCK_CIPHER);
    output.extend_from_slice(MAGIC);
    output.push(VERSION_V2);
    output.extend_from_slice(&wrapped);

    for (block_idx, chunk) in plaintext.chunks(BLOCK_SIZE).enumerate() {
        let block = encrypt_with_aad(&dek, chunk, &v2_block_aad(block_idx as u64))?;
        output.extend_from_slice(&block);
    }
    Ok(output)
}

fn decrypt_data_v2(master_key: &[u8; KEY_SIZE], ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    let header = 4 + 1 + WRAPPED_KEY_SIZE;
    if ciphertext.len() < header {
        return Err("AeroCrypt v2 data too short".into());
    }
    let wrapped: [u8; WRAPPED_KEY_SIZE] = ciphertext[5..header].try_into().expect("slice length");
    let dek = unwrap_key(master_key, &wrapped)?;

    let data = &ciphertext[header..];
    let mut plaintext = Vec::with_capacity(data.len());
    let mut block_idx = 0u64;
    let mut pos = 0usize;
    while pos < data.len() {
        let end = (pos + V2_FULL_BLOCK_CIPHER).min(data.len());
        let block = &data[pos..end];
        let pt = decrypt_with_aad(&dek, block, &v2_block_aad(block_idx))?;
        plaintext.extend_from_slice(&pt);
        pos = end;
        block_idx += 1;
    }
    Ok(plaintext)
}

// --- Public content API (version-dispatched) ------------------------------

/// Encrypt a file's bytes in the overlay's format version.
pub fn encrypt_data(
    cfg: &OverlayConfig,
    master_key: &[u8; KEY_SIZE],
    plaintext: &[u8],
) -> Result<Vec<u8>, String> {
    match cfg {
        OverlayConfig::V1 { .. } => encrypt_data_v1(master_key, plaintext),
        OverlayConfig::V2 { .. } => encrypt_data_v2(master_key, plaintext),
    }
}

/// Decrypt an AeroCrypt blob, dispatching on its embedded version byte so a
/// reader transparently handles both v1 and v2 objects.
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
        other => Err(format!("Unsupported AeroCrypt version {other}")),
    }
}

// --- Config (`.aeroftp-crypt.json`) ---------------------------------------

/// Generate a fresh 32-byte salt for a new v2 overlay.
pub fn random_salt_v2() -> [u8; SALT_V2_SIZE] {
    random_array::<SALT_V2_SIZE>()
}

/// Build the v2 config JSON written at the root of a new overlay.
pub fn init_config_v2(salt: &[u8; SALT_V2_SIZE]) -> String {
    serde_json::json!({
        "version": VERSION_V2,
        "cipher": "AES-256-GCM-SIV",
        "filename_cipher": "AES-256-SIV",
        "key_wrap": "AES-256-KW",
        "kdf": "Argon2id",
        "kdf_mem_kib": 128 * 1024,
        "kdf_time": 4,
        "kdf_lanes": 4,
        "salt": base64::engine::general_purpose::STANDARD.encode(salt),
        "block_size": BLOCK_SIZE,
    })
    .to_string()
}

/// Parse an overlay config, accepting both v1 and v2 layouts so an existing
/// v1 overlay keeps working.
pub fn parse_config(config_json: &str) -> Result<OverlayConfig, String> {
    let val: serde_json::Value =
        serde_json::from_str(config_json).map_err(|e| format!("invalid crypt config: {e}"))?;
    let version = val.get("version").and_then(|v| v.as_u64()).unwrap_or(1);
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

    #[test]
    fn v2_round_trip_all_sizes() {
        let salt = [9u8; SALT_V2_SIZE];
        let cfg = OverlayConfig::V2 { salt };
        let master = derive_master_key(&cfg, "correct horse battery staple").unwrap();
        for n in sizes_to_test() {
            let pt: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            let blob = encrypt_data(&cfg, &master, &pt).unwrap();
            assert_eq!(&blob[0..4], MAGIC);
            assert_eq!(blob[4], VERSION_V2);
            assert_eq!(
                decrypt_data(&master, &blob).unwrap(),
                pt,
                "v2 round trip {n}"
            );
        }
    }

    #[test]
    fn v1_blobs_still_read_and_write() {
        let salt = [3u8; SALT_V1_SIZE];
        let cfg = OverlayConfig::V1 { salt };
        let master = derive_master_key(&cfg, "legacy-password").unwrap();
        let pt = b"legacy AeroCrypt v1 payload".repeat(5000);
        let blob = encrypt_data(&cfg, &master, &pt).unwrap();
        assert_eq!(blob[4], VERSION_V1);
        assert_eq!(decrypt_data(&master, &blob).unwrap(), pt);
    }

    #[test]
    fn v2_wrong_password_fails_closed() {
        let salt = [9u8; SALT_V2_SIZE];
        let cfg = OverlayConfig::V2 { salt };
        let master = derive_master_key(&cfg, "right").unwrap();
        let blob = encrypt_data(&cfg, &master, b"secret").unwrap();
        let wrong = derive_master_key(&cfg, "wrong").unwrap();
        assert!(decrypt_data(&wrong, &blob).is_err());
    }

    #[test]
    fn v2_block_reorder_is_rejected() {
        let salt = [9u8; SALT_V2_SIZE];
        let cfg = OverlayConfig::V2 { salt };
        let master = derive_master_key(&cfg, "pw").unwrap();
        // Two full blocks + tail so there is more than one AEAD block to swap.
        let pt: Vec<u8> = (0..BLOCK_SIZE * 2 + 10).map(|i| i as u8).collect();
        let mut blob = encrypt_data(&cfg, &master, &pt).unwrap();
        let header = 4 + 1 + WRAPPED_KEY_SIZE;
        // Swap the first two full ciphertext blocks: the AAD block index no
        // longer matches, so decryption must fail rather than silently reorder.
        let b0 = header;
        let b1 = header + V2_FULL_BLOCK_CIPHER;
        let b2 = header + 2 * V2_FULL_BLOCK_CIPHER;
        let mut swapped = blob[..b0].to_vec();
        swapped.extend_from_slice(&blob[b1..b2]);
        swapped.extend_from_slice(&blob[b0..b1]);
        swapped.extend_from_slice(&blob[b2..]);
        blob = swapped;
        assert!(decrypt_data(&master, &blob).is_err());
    }

    #[test]
    fn config_round_trip_v2_and_legacy_v1() {
        let salt = random_salt_v2();
        let json = init_config_v2(&salt);
        match parse_config(&json).unwrap() {
            OverlayConfig::V2 { salt: parsed } => assert_eq!(parsed, salt),
            _ => panic!("expected v2 config"),
        }
        // A legacy v1 config (version 1, 16-byte salt) must still parse.
        let v1_json = serde_json::json!({
            "version": 1,
            "salt": base64::engine::general_purpose::STANDARD.encode([1u8; SALT_V1_SIZE]),
        })
        .to_string();
        assert_eq!(parse_config(&v1_json).unwrap().version(), VERSION_V1);
    }
}
