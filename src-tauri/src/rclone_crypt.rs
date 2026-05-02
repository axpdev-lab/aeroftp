// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

//! Rclone crypt compatibility layer.
//!
//! Decrypts files and filenames produced by `rclone crypt` (XSalsa20-Poly1305
//! content encryption, EME/AES-256 filename encryption in `standard` mode).

use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit as AesKeyInit};
use aes::Aes256;
use crypto_secretbox::aead::Aead;
use crypto_secretbox::XSalsa20Poly1305;
use rand::RngCore;
use scrypt::{scrypt, Params as ScryptParams};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use zeroize::Zeroize;

// ── Constants ──────────────────────────────────────────────────────────────

/// Magic header for rclone crypt files.
const RCLONE_MAGIC: &[u8; 8] = b"RCLONE\x00\x00";

/// File nonce size (24 bytes for XSalsa20).
const FILE_NONCE_SIZE: usize = 24;

/// Header size = magic (8) + nonce (24) = 32 bytes.
const HEADER_SIZE: usize = 8 + FILE_NONCE_SIZE;

/// Plaintext chunk size: 64 KB.
const CHUNK_DATA_SIZE: usize = 65536;

/// Poly1305 auth tag size.
const CHUNK_TAG_SIZE: usize = 16;

/// Ciphertext chunk size = plaintext + tag.
const CHUNK_CIPHER_SIZE: usize = CHUNK_DATA_SIZE + CHUNK_TAG_SIZE;

/// scrypt parameters matching rclone: N=16384 (2^14), r=8, p=1, output=64.
const SCRYPT_LOG_N: u8 = 14;
const SCRYPT_R: u32 = 8;
const SCRYPT_P: u32 = 1;
const SCRYPT_KEY_LEN: usize = 64;

/// AES block size.
const AES_BLOCK: usize = 16;
const MAX_DECRYPT_INPUT_BYTES: usize = 512 * 1024 * 1024;

// ── Types ──────────────────────────────────────────────────────────────────

/// Filename encryption mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilenameEncryption {
    Standard,
    Obfuscate,
    Off,
}

/// Derived keys for an unlocked rclone crypt remote.
pub struct RcloneCryptKeys {
    pub name_key: [u8; 32],
    pub data_key: [u8; 32],
    pub filename_encryption: FilenameEncryption,
    #[allow(dead_code)] // Used in Phase 4 directory traversal
    pub directory_name_encryption: bool,
}

struct OutputPathGuard {
    final_path: PathBuf,
    temp_file: tempfile::NamedTempFile,
}

impl OutputPathGuard {
    fn new(output_path: &str) -> Result<Self, String> {
        crate::filesystem::validate_path(output_path)?;

        let final_path = PathBuf::from(output_path);
        let parent = final_path
            .parent()
            .ok_or_else(|| "Output path must have a parent directory".to_string())?;

        let canonical_parent = std::fs::canonicalize(parent)
            .map_err(|e| format!("failed to resolve output parent directory: {}", e))?;
        if canonical_parent
            .symlink_metadata()
            .map_err(|e| format!("failed to inspect output parent directory: {}", e))?
            .file_type()
            .is_symlink()
        {
            return Err("Output parent directory cannot be a symlink".to_string());
        }

        if let Ok(meta) = std::fs::symlink_metadata(&final_path) {
            if meta.file_type().is_symlink() {
                return Err("Output path cannot be a symlink".to_string());
            }
            if meta.is_dir() {
                return Err("Output path cannot be a directory".to_string());
            }
        }

        let temp_file = tempfile::NamedTempFile::new_in(&canonical_parent)
            .map_err(|e| format!("failed to create temporary output file: {}", e))?;

        Ok(Self {
            final_path,
            temp_file,
        })
    }

    fn write_all(mut self, plaintext: &[u8]) -> Result<String, String> {
        use std::io::Write;

        self.temp_file
            .write_all(plaintext)
            .map_err(|e| format!("failed to write temporary output: {}", e))?;
        self.temp_file
            .as_file_mut()
            .sync_all()
            .map_err(|e| format!("failed to flush temporary output: {}", e))?;

        self.temp_file
            .persist(&self.final_path)
            .map_err(|e| format!("failed to persist output file: {}", e.error))?;

        Ok(self.final_path.to_string_lossy().to_string())
    }
}

impl Drop for RcloneCryptKeys {
    fn drop(&mut self) {
        self.name_key.zeroize();
        self.data_key.zeroize();
    }
}

// ── Phase 1: Key derivation ────────────────────────────────────────────────

/// Derive name_key (32 bytes) and data_key (32 bytes) from password and
/// optional salt (password2). Compatible with rclone's scrypt parameters.
pub fn derive_keys(password: &str, salt: &str) -> Result<([u8; 32], [u8; 32]), String> {
    let params = ScryptParams::new(SCRYPT_LOG_N, SCRYPT_R, SCRYPT_P, SCRYPT_KEY_LEN)
        .map_err(|e| format!("invalid scrypt params: {}", e))?;

    let mut key_bytes = [0u8; SCRYPT_KEY_LEN];
    scrypt(
        password.as_bytes(),
        salt.as_bytes(),
        &params,
        &mut key_bytes,
    )
    .map_err(|e| format!("scrypt failed: {}", e))?;

    let mut name_key = [0u8; 32];
    let mut data_key = [0u8; 32];
    name_key.copy_from_slice(&key_bytes[..32]);
    data_key.copy_from_slice(&key_bytes[32..]);

    key_bytes.zeroize();
    Ok((name_key, data_key))
}

// ── Phase 1: File content decryption ───────────────────────────────────────

/// Decrypt an rclone-crypt encrypted file.
///
/// `data` must include the full file: magic header + nonce + encrypted chunks.
/// Returns the decrypted plaintext. Empty files (header-only) return empty vec.
pub fn decrypt_file_content(data: &[u8], data_key: &[u8; 32]) -> Result<Vec<u8>, String> {
    if data.len() > MAX_DECRYPT_INPUT_BYTES {
        return Err(format!(
            "encrypted input too large for in-memory decrypt path ({} bytes > {} bytes)",
            data.len(),
            MAX_DECRYPT_INPUT_BYTES
        ));
    }

    // Validate header
    if data.len() < HEADER_SIZE {
        return Err("file too short for rclone crypt header".into());
    }
    if &data[..8] != RCLONE_MAGIC {
        return Err("invalid rclone crypt magic header".into());
    }

    // Read file nonce
    let mut file_nonce = [0u8; FILE_NONCE_SIZE];
    file_nonce.copy_from_slice(&data[8..HEADER_SIZE]);

    // Create cipher
    let cipher = XSalsa20Poly1305::new(data_key.into());

    // Decrypt chunks
    let chunk_data = &data[HEADER_SIZE..];
    if chunk_data.is_empty() {
        return Ok(Vec::new()); // empty file
    }

    let mut plaintext = Vec::new();
    let mut offset = 0;
    let mut chunk_num: u64 = 0;

    while offset < chunk_data.len() {
        let remaining = chunk_data.len() - offset;
        let chunk_size = remaining.min(CHUNK_CIPHER_SIZE);

        // Minimum valid chunk: tag (16) + 1 byte plaintext = 17
        if chunk_size <= CHUNK_TAG_SIZE {
            return Err(format!(
                "chunk {} truncated ({} bytes, need > {})",
                chunk_num, chunk_size, CHUNK_TAG_SIZE
            ));
        }

        let chunk = &chunk_data[offset..offset + chunk_size];

        // Compute per-chunk nonce: file_nonce + chunk_num (LE addition on first 8 bytes)
        let nonce = chunk_nonce(&file_nonce, chunk_num);

        let decrypted = cipher
            .decrypt((&nonce).into(), chunk)
            .map_err(|_| format!("chunk {} decrypt failed (wrong key?)", chunk_num))?;

        plaintext.extend_from_slice(&decrypted);
        offset += chunk_size;
        chunk_num += 1;
    }

    Ok(plaintext)
}

/// Encrypt plaintext into the rclone-crypt file format.
///
/// Output layout: magic header + 24-byte file nonce + XSalsa20-Poly1305 chunks.
/// The per-chunk nonce follows rclone's counter semantics and is shared with
/// the decrypt path through `chunk_nonce`.
pub fn encrypt_file_content(plaintext: &[u8], data_key: &[u8; 32]) -> Result<Vec<u8>, String> {
    let cipher = XSalsa20Poly1305::new(data_key.into());

    let mut file_nonce = [0u8; FILE_NONCE_SIZE];
    rand::rngs::OsRng.fill_bytes(&mut file_nonce);

    let mut output = Vec::with_capacity(
        HEADER_SIZE + plaintext.len() + ((plaintext.len() / CHUNK_DATA_SIZE) + 1) * CHUNK_TAG_SIZE,
    );
    output.extend_from_slice(RCLONE_MAGIC);
    output.extend_from_slice(&file_nonce);

    for (chunk_num, chunk) in plaintext.chunks(CHUNK_DATA_SIZE).enumerate() {
        let nonce = chunk_nonce(&file_nonce, chunk_num as u64);
        let encrypted = cipher
            .encrypt((&nonce).into(), chunk)
            .map_err(|_| format!("chunk {} encrypt failed", chunk_num))?;
        output.extend_from_slice(&encrypted);
    }

    Ok(output)
}

/// Compute the nonce for a specific chunk by adding chunk_num to the file nonce.
/// Matches rclone's nonce.add() — treats first 8 bytes as LE u64 counter.
fn chunk_nonce(file_nonce: &[u8; FILE_NONCE_SIZE], chunk_num: u64) -> [u8; FILE_NONCE_SIZE] {
    let mut nonce = *file_nonce;
    let base = u64::from_le_bytes(nonce[..8].try_into().unwrap());
    let new_val = base.wrapping_add(chunk_num);
    nonce[..8].copy_from_slice(&new_val.to_le_bytes());
    nonce
}

// ── Phase 2: Filename decryption ───────────────────────────────────────────

/// Decrypt a filename encrypted with rclone's `standard` mode.
///
/// Flow: Base32-decode -> EME-decrypt with name_key + dir_iv -> PKCS#7 unpad -> UTF-8.
pub fn decrypt_name(
    name_key: &[u8; 32],
    dir_iv: &[u8; 16],
    encrypted_name: &str,
) -> Result<String, String> {
    // 1. Base32hex decode (rclone uses uppercase base32hex, no padding)
    let ciphertext = base32hex_decode(encrypted_name)?;

    if ciphertext.is_empty() || ciphertext.len() % AES_BLOCK != 0 {
        return Err(format!(
            "ciphertext length {} not a multiple of {}",
            ciphertext.len(),
            AES_BLOCK
        ));
    }

    // 2. EME decrypt
    let padded = eme_decrypt(name_key, dir_iv, &ciphertext)?;

    // 3. PKCS#7 unpad
    let plain = pkcs7_unpad(&padded)?;

    // 4. UTF-8
    String::from_utf8(plain).map_err(|e| format!("filename not valid UTF-8: {}", e))
}

/// Encrypt a filename with rclone's `standard` mode.
pub fn encrypt_name(
    name_key: &[u8; 32],
    dir_iv: &[u8; 16],
    plain_name: &str,
) -> Result<String, String> {
    // 1. PKCS#7 pad
    let padded = pkcs7_pad(plain_name.as_bytes());

    // 2. EME encrypt
    let ciphertext = eme_encrypt(name_key, dir_iv, &padded)?;

    // 3. Base32hex encode (uppercase, no padding)
    Ok(base32hex_encode(&ciphertext))
}

// ── EME (ECB-Mix-ECB) wide-block cipher ────────────────────────────────────

/// EME-decrypt: decrypts data that is a multiple of 16 bytes using the
/// EME (Halevi-Rogaway) wide-block cipher with AES-256.
fn eme_decrypt(key: &[u8; 32], tweak: &[u8; 16], data: &[u8]) -> Result<Vec<u8>, String> {
    eme_transform(key, tweak, data, false)
}

/// EME-encrypt: encrypts data that is a multiple of 16 bytes.
fn eme_encrypt(key: &[u8; 32], tweak: &[u8; 16], data: &[u8]) -> Result<Vec<u8>, String> {
    eme_transform(key, tweak, data, true)
}

/// Core EME transform (encrypt or decrypt).
/// Ported verbatim from rfjakob/eme (Go), Halevi-Rogaway 2003.
fn eme_transform(
    key: &[u8; 32],
    tweak: &[u8; 16],
    data: &[u8],
    encrypt: bool,
) -> Result<Vec<u8>, String> {
    let m = data.len() / AES_BLOCK;
    if m == 0 || data.len() % AES_BLOCK != 0 {
        return Err("EME: data must be a non-empty multiple of 16 bytes".into());
    }

    let bc = Aes256::new(key.into());

    // L = E_K(0^128), then build L table: L_table[j] = 2^(j+1) * L
    let mut l_init = [0u8; AES_BLOCK];
    bc.encrypt_block((&mut l_init).into());
    let l_table = tabulate_l(&l_init, m);

    // C is our working buffer (same size as input)
    let mut c = vec![0u8; data.len()];

    // Steps 1-2: PPj = Pj XOR L_table[j], then PPPj = AES(K, PPj) or AES_dec
    let mut ppj = [0u8; AES_BLOCK];
    for j in 0..m {
        xor_into(
            &mut ppj,
            &data[j * AES_BLOCK..(j + 1) * AES_BLOCK],
            &l_table[j],
        );
        let mut block = ppj;
        if encrypt {
            bc.encrypt_block((&mut block).into());
        } else {
            bc.decrypt_block((&mut block).into());
        }
        c[j * AES_BLOCK..(j + 1) * AES_BLOCK].copy_from_slice(&block);
    }

    // Step 3: MP = T XOR PPP[0] XOR PPP[1] XOR ... XOR PPP[m-1]
    let mut mp = [0u8; AES_BLOCK];
    xor_into(&mut mp, &c[0..AES_BLOCK], tweak);
    for j in 1..m {
        xor_mut(&mut mp, &c[j * AES_BLOCK..(j + 1) * AES_BLOCK]);
    }

    // Step 4: MC = AES(K, MP) — same direction as overall transform
    let mut mc = mp;
    if encrypt {
        bc.encrypt_block((&mut mc).into());
    } else {
        bc.decrypt_block((&mut mc).into());
    }

    // Step 5: M = MP XOR MC
    let mut m_val = [0u8; AES_BLOCK];
    xor_into(&mut m_val, &mp, &mc);

    // Step 6: For j=1..m-1: M = 2*M, CCC[j] = PPP[j] XOR M
    for j in 1..m {
        m_val = gf128_double(&m_val);
        let mut cccj = [0u8; AES_BLOCK];
        xor_into(&mut cccj, &c[j * AES_BLOCK..(j + 1) * AES_BLOCK], &m_val);
        c[j * AES_BLOCK..(j + 1) * AES_BLOCK].copy_from_slice(&cccj);
    }

    // Step 7: CCC[0] = MC XOR T XOR CCC[1] XOR ... XOR CCC[m-1]
    let mut ccc0 = [0u8; AES_BLOCK];
    xor_into(&mut ccc0, &mc, tweak);
    for j in 1..m {
        xor_mut(&mut ccc0, &c[j * AES_BLOCK..(j + 1) * AES_BLOCK]);
    }
    c[0..AES_BLOCK].copy_from_slice(&ccc0);

    // Step 8: For j=0..m-1: CC[j] = AES(K, CCC[j]), C[j] = CC[j] XOR L_table[j]
    for j in 0..m {
        let mut block = [0u8; AES_BLOCK];
        block.copy_from_slice(&c[j * AES_BLOCK..(j + 1) * AES_BLOCK]);
        if encrypt {
            bc.encrypt_block((&mut block).into());
        } else {
            bc.decrypt_block((&mut block).into());
        }
        xor_mut(&mut block, &l_table[j]);
        c[j * AES_BLOCK..(j + 1) * AES_BLOCK].copy_from_slice(&block);
    }

    Ok(c)
}

/// Build a table of L * 2^i for i = 1..n in GF(2^128).
fn tabulate_l(l: &[u8; AES_BLOCK], n: usize) -> Vec<[u8; AES_BLOCK]> {
    let mut table = Vec::with_capacity(n);
    let mut current = *l;
    for _ in 0..n {
        current = gf128_double(&current);
        table.push(current);
    }
    table
}

/// Multiply by 2 in GF(2^128) using the EME/rfjakob convention:
/// byte 0 = least significant, byte 15 = most significant.
/// Reduction polynomial: x^128 + x^7 + x^2 + x + 1 (0x87 into byte 0).
fn gf128_double(val: &[u8; AES_BLOCK]) -> [u8; AES_BLOCK] {
    let mut result = [0u8; AES_BLOCK];
    // Byte 0: shift left, then conditionally XOR reduction if byte 15 MSB was set
    result[0] = val[0] << 1;
    if val[AES_BLOCK - 1] & 0x80 != 0 {
        result[0] ^= 0x87;
    }
    // Bytes 1..15: shift left with carry from previous byte's MSB
    for j in 1..AES_BLOCK {
        result[j] = (val[j] << 1) | (val[j - 1] >> 7);
    }
    result
}

/// XOR: out = a XOR b (slice version, both must be AES_BLOCK length).
fn xor_into(out: &mut [u8; AES_BLOCK], a: &[u8], b: &[u8; AES_BLOCK]) {
    for i in 0..AES_BLOCK {
        out[i] = a[i] ^ b[i];
    }
}

/// XOR: a ^= b (in-place, slice version).
fn xor_mut(a: &mut [u8; AES_BLOCK], b: &[u8]) {
    for i in 0..AES_BLOCK {
        a[i] ^= b[i];
    }
}

// ── PKCS#7 padding ─────────────────────────────────────────────────────────

fn pkcs7_pad(data: &[u8]) -> Vec<u8> {
    let pad_len = AES_BLOCK - (data.len() % AES_BLOCK);
    let mut padded = Vec::with_capacity(data.len() + pad_len);
    padded.extend_from_slice(data);
    padded.resize(data.len() + pad_len, pad_len as u8);
    padded
}

fn pkcs7_unpad(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.is_empty() {
        return Err("pkcs7: empty data".into());
    }
    let pad_byte = *data.last().unwrap();
    if pad_byte == 0 || pad_byte as usize > AES_BLOCK || pad_byte as usize > data.len() {
        return Err(format!("pkcs7: invalid padding byte {}", pad_byte));
    }
    // Verify all padding bytes
    for &b in &data[data.len() - pad_byte as usize..] {
        if b != pad_byte {
            return Err("pkcs7: inconsistent padding".into());
        }
    }
    Ok(data[..data.len() - pad_byte as usize].to_vec())
}

// ── Base32hex encoding (rclone-compatible) ─────────────────────────────────

/// Base32hex encode (uppercase, no padding) — matches rclone's filename encoding.
fn base32hex_encode(data: &[u8]) -> String {
    data_encoding::BASE32HEX_NOPAD.encode(data)
}

/// Base32hex decode (case-insensitive, no padding).
fn base32hex_decode(s: &str) -> Result<Vec<u8>, String> {
    // rclone uses uppercase but we accept both
    let upper = s.to_uppercase();
    data_encoding::BASE32HEX_NOPAD
        .decode(upper.as_bytes())
        .map_err(|e| format!("base32hex decode failed: {}", e))
}

// ── Tauri state and commands (Phase 3) ─────────────────────────────────────

use std::collections::HashMap;
use tokio::sync::Mutex;

/// Info returned after unlock.
#[derive(Debug, Clone, Serialize)]
pub struct RcloneCryptVaultInfo {
    pub vault_id: String,
    pub filename_encryption: FilenameEncryption,
    pub directory_name_encryption: bool,
}

/// Managed state holding all unlocked rclone crypt remotes.
pub struct RcloneCryptState {
    pub vaults: Mutex<HashMap<String, RcloneCryptKeys>>,
}

impl RcloneCryptState {
    pub fn new() -> Self {
        Self {
            vaults: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for RcloneCryptState {
    fn default() -> Self {
        Self::new()
    }
}

/// Unlock an rclone crypt remote by deriving keys from password (and optional salt).
#[tauri::command]
pub async fn rclone_crypt_unlock(
    state: tauri::State<'_, RcloneCryptState>,
    password: String,
    salt: Option<String>,
    filename_encryption: Option<String>,
    directory_name_encryption: Option<bool>,
) -> Result<RcloneCryptVaultInfo, String> {
    if matches!(filename_encryption.as_deref(), Some("obfuscate")) {
        return Err("filename_encryption=obfuscate is not supported in this MVP".to_string());
    }

    let secret_pwd = secrecy::SecretString::from(password);
    let salt_str = salt.unwrap_or_default();

    let (name_key, data_key) =
        derive_keys(secrecy::ExposeSecret::expose_secret(&secret_pwd), &salt_str)?;

    let fe = match filename_encryption.as_deref() {
        Some("off") => FilenameEncryption::Off,
        Some("obfuscate") => FilenameEncryption::Obfuscate,
        _ => FilenameEncryption::Standard,
    };
    let dne = directory_name_encryption.unwrap_or(true);

    let vault_id = uuid::Uuid::new_v4().to_string();
    let keys = RcloneCryptKeys {
        name_key,
        data_key,
        filename_encryption: fe,
        directory_name_encryption: dne,
    };

    let info = RcloneCryptVaultInfo {
        vault_id: vault_id.clone(),
        filename_encryption: fe,
        directory_name_encryption: dne,
    };

    state.vaults.lock().await.insert(vault_id, keys);
    Ok(info)
}

/// Lock (forget) an unlocked rclone crypt remote, zeroizing keys.
#[tauri::command]
pub async fn rclone_crypt_lock(
    state: tauri::State<'_, RcloneCryptState>,
    vault_id: String,
) -> Result<(), String> {
    let mut vaults = state.vaults.lock().await;
    if vaults.remove(&vault_id).is_none() {
        return Err("Vault not found or already locked".to_string());
    }
    // Keys are zeroized via Drop impl
    Ok(())
}

/// Decrypt a single filename using the unlocked keys and a directory IV.
#[tauri::command]
pub async fn rclone_crypt_decrypt_name(
    state: tauri::State<'_, RcloneCryptState>,
    vault_id: String,
    dir_iv_base64: String,
    encrypted_name: String,
) -> Result<String, String> {
    let vaults = state.vaults.lock().await;
    let keys = vaults.get(&vault_id).ok_or("Vault not unlocked")?;

    if keys.filename_encryption == FilenameEncryption::Off {
        return Ok(encrypted_name);
    }
    if keys.filename_encryption == FilenameEncryption::Obfuscate {
        return Err("filename_encryption=obfuscate is not supported in this MVP".to_string());
    }

    let dir_iv = parse_dir_iv(&dir_iv_base64)?;
    decrypt_name(&keys.name_key, &dir_iv, &encrypted_name)
}

/// Encrypt a single filename using the unlocked keys and a directory IV.
#[tauri::command]
pub async fn rclone_crypt_encrypt_name(
    state: tauri::State<'_, RcloneCryptState>,
    vault_id: String,
    dir_iv_base64: String,
    plain_name: String,
) -> Result<String, String> {
    let vaults = state.vaults.lock().await;
    let keys = vaults.get(&vault_id).ok_or("Vault not unlocked")?;

    if keys.filename_encryption == FilenameEncryption::Off {
        return Ok(plain_name);
    }
    if keys.filename_encryption == FilenameEncryption::Obfuscate {
        return Err("filename_encryption=obfuscate is not supported in this MVP".to_string());
    }

    let dir_iv = parse_dir_iv(&dir_iv_base64)?;
    encrypt_name(&keys.name_key, &dir_iv, &plain_name)
}

/// Decrypt file content. Takes raw encrypted bytes (base64-encoded from frontend),
/// returns decrypted bytes as base64.
#[tauri::command]
pub async fn rclone_crypt_decrypt_file(
    state: tauri::State<'_, RcloneCryptState>,
    vault_id: String,
    encrypted_data_base64: String,
    output_path: String,
) -> Result<String, String> {
    use base64::Engine;

    let vaults = state.vaults.lock().await;
    let keys = vaults.get(&vault_id).ok_or("Vault not unlocked")?;

    let encrypted_data = base64::engine::general_purpose::STANDARD
        .decode(&encrypted_data_base64)
        .map_err(|e| format!("base64 decode failed: {}", e))?;

    let plaintext = decrypt_file_content(&encrypted_data, &keys.data_key)?;
    let guard = OutputPathGuard::new(&output_path)?;
    guard.write_all(&plaintext)
}

/// Decrypt file from a local encrypted file path to a local decrypted output path.
#[tauri::command]
pub async fn rclone_crypt_decrypt_file_path(
    state: tauri::State<'_, RcloneCryptState>,
    vault_id: String,
    encrypted_file_path: String,
    output_path: String,
) -> Result<String, String> {
    crate::filesystem::validate_path(&encrypted_file_path)?;

    let encrypted_meta = std::fs::symlink_metadata(Path::new(&encrypted_file_path))
        .map_err(|e| format!("failed to inspect encrypted input file: {}", e))?;
    if encrypted_meta.file_type().is_symlink() {
        return Err("Encrypted input path cannot be a symlink".to_string());
    }
    if !encrypted_meta.is_file() {
        return Err("Encrypted input path must be a regular file".to_string());
    }
    if encrypted_meta.len() > MAX_DECRYPT_INPUT_BYTES as u64 {
        return Err(format!(
            "encrypted input too large for MVP decrypt path ({} bytes > {} bytes)",
            encrypted_meta.len(),
            MAX_DECRYPT_INPUT_BYTES
        ));
    }

    let vaults = state.vaults.lock().await;
    let keys = vaults.get(&vault_id).ok_or("Vault not unlocked")?;

    let encrypted_data = std::fs::read(&encrypted_file_path)
        .map_err(|e| format!("failed to read encrypted file: {}", e))?;

    let plaintext = decrypt_file_content(&encrypted_data, &keys.data_key)?;
    let guard = OutputPathGuard::new(&output_path)?;
    guard.write_all(&plaintext)
}

/// Encrypt a local plaintext file to a local rclone-crypt formatted file.
#[tauri::command]
pub async fn rclone_crypt_encrypt_file_path(
    state: tauri::State<'_, RcloneCryptState>,
    vault_id: String,
    plaintext_file_path: String,
    encrypted_output_path: String,
) -> Result<String, String> {
    crate::filesystem::validate_path(&plaintext_file_path)?;

    let plaintext_meta = std::fs::symlink_metadata(Path::new(&plaintext_file_path))
        .map_err(|e| format!("failed to inspect plaintext input file: {}", e))?;
    if plaintext_meta.file_type().is_symlink() {
        return Err("Plaintext input path cannot be a symlink".to_string());
    }
    if !plaintext_meta.is_file() {
        return Err("Plaintext input path must be a regular file".to_string());
    }
    if plaintext_meta.len() > MAX_DECRYPT_INPUT_BYTES as u64 {
        return Err(format!(
            "plaintext input too large for MVP encrypt path ({} bytes > {} bytes)",
            plaintext_meta.len(),
            MAX_DECRYPT_INPUT_BYTES
        ));
    }

    let data_key = {
        let vaults = state.vaults.lock().await;
        let keys = vaults.get(&vault_id).ok_or("Vault not unlocked")?;
        keys.data_key
    };

    let plaintext =
        std::fs::read(&plaintext_file_path).map_err(|e| format!("failed to read file: {}", e))?;

    let encrypted = encrypt_file_content(&plaintext, &data_key)?;
    let guard = OutputPathGuard::new(&encrypted_output_path)?;
    guard.write_all(&encrypted)
}

/// Parse a 16-byte dirIV from base64.
fn parse_dir_iv(base64_str: &str) -> Result<[u8; 16], String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64_str)
        .map_err(|e| format!("dirIV base64 decode failed: {}", e))?;
    if bytes.len() != 16 {
        return Err(format!("dirIV must be 16 bytes, got {}", bytes.len()));
    }
    let mut iv = [0u8; 16];
    iv.copy_from_slice(&bytes);
    Ok(iv)
}

/// Helper for cryptcheck: stream-decrypts a remote file and computes its hash (e.g. MD5 or SHA-256).
pub fn decrypt_and_hash<H: sha2::digest::Digest>(
    blob: &[u8],
    data_key: &[u8; 32],
) -> Result<(generic_array::GenericArray<u8, H::OutputSize>, u64), String> {
    if blob.len() < 32 {
        return Err("Blob too short (missing header/nonce)".to_string());
    }
    if &blob[..8] != b"RCLONE\x00\x00" {
        return Err("Invalid Rclone crypt header".to_string());
    }
    let mut file_nonce = [0u8; 24];
    file_nonce.copy_from_slice(&blob[8..32]);

    let mut hasher = H::new();
    let mut offset = 32;
    let mut chunk_num = 0u64;
    let mut total_len = 0u64;

    while offset < blob.len() {
        let chunk_size = std::cmp::min(blob.len() - offset, CHUNK_DATA_SIZE + MAC_SIZE);
        let chunk = &blob[offset..offset + chunk_size];
        let plain = decrypt_chunk(data_key, &file_nonce, chunk_num, chunk)?;
        hasher.update(&plain);
        total_len += plain.len() as u64;
        offset += chunk_size;
        chunk_num += 1;
    }

    Ok((hasher.finalize(), total_len))
}

/// Helper for cryptcheck: async stream-decrypts a remote reader and computes its hash.
pub async fn decrypt_and_hash_async<R: tokio::io::AsyncRead + Unpin, H: sha2::digest::Digest>(
    mut reader: R,
    data_key: &[u8; 32],
) -> Result<(generic_array::GenericArray<u8, H::OutputSize>, u64), String> {
    use tokio::io::AsyncReadExt;
    let mut header = [0u8; 8];
    reader.read_exact(&mut header).await.map_err(|e| e.to_string())?;
    if &header != b"RCLONE\x00\x00" {
        return Err("Invalid Rclone crypt header".to_string());
    }
    
    let mut file_nonce = [0u8; 24];
    reader.read_exact(&mut file_nonce).await.map_err(|e| e.to_string())?;

    let mut hasher = H::new();
    let mut chunk_num = 0u64;
    let mut total_len = 0u64;

    loop {
        let mut chunk_buf = vec![0u8; CHUNK_DATA_SIZE + MAC_SIZE];
        let mut chunk_len = 0;
        while chunk_len < CHUNK_DATA_SIZE + MAC_SIZE {
            let n = reader.read(&mut chunk_buf[chunk_len..]).await.map_err(|e| e.to_string())?;
            if n == 0 { break; }
            chunk_len += n;
        }
        if chunk_len == 0 { break; }
        
        let plain = decrypt_chunk(data_key, &file_nonce, chunk_num, &chunk_buf[..chunk_len])?;
        hasher.update(&plain);
        total_len += plain.len() as u64;
        chunk_num += 1;
        
        if chunk_len < CHUNK_DATA_SIZE + MAC_SIZE { break; }
    }

    Ok((hasher.finalize(), total_len))
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;

    // Golden vectors copied from rclone/backend/crypt/cipher_test.go
    // (TestEncryptData + TestStandardEncryptFileNameBase32).
    const RCLONE_GOLDEN_FILE0: &[u8] = &[
        0x52, 0x43, 0x4c, 0x4f, 0x4e, 0x45, 0x00, 0x00,
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
        0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10,
        0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18,
    ];
    const RCLONE_GOLDEN_FILE1: &[u8] = &[
        0x52, 0x43, 0x4c, 0x4f, 0x4e, 0x45, 0x00, 0x00,
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
        0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10,
        0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18,
        0x09, 0x5b, 0x44, 0x6c, 0xd6, 0x23, 0x7b, 0xbc,
        0xb0, 0x8d, 0x09, 0xfb, 0x52, 0x4c, 0xe5, 0x65,
        0xaa,
    ];
    const RCLONE_GOLDEN_FILE16: &[u8] = &[
        0x52, 0x43, 0x4c, 0x4f, 0x4e, 0x45, 0x00, 0x00,
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
        0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10,
        0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18,
        0xb9, 0xc4, 0x55, 0x2a, 0x27, 0x10, 0x06, 0x29,
        0x18, 0x96, 0x0a, 0x3e, 0x60, 0x8c, 0x29, 0xb9,
        0xaa, 0x8a, 0x5e, 0x1e, 0x16, 0x5b, 0x6d, 0x07,
        0x5d, 0xe4, 0xe9, 0xbb, 0x36, 0x7f, 0xd6, 0xd4,
    ];

    // ── Phase 1 tests ──

    #[test]
    fn derive_keys_produces_64_bytes() {
        let (name_key, data_key) = derive_keys("testpassword", "testsalt").unwrap();
        // Keys should be 32 bytes each and non-zero
        assert_ne!(name_key, [0u8; 32]);
        assert_ne!(data_key, [0u8; 32]);
        // Different inputs should produce different keys
        let (name_key2, data_key2) = derive_keys("other", "salt2").unwrap();
        assert_ne!(name_key, name_key2);
        assert_ne!(data_key, data_key2);
    }

    #[test]
    fn derive_keys_deterministic() {
        let (nk1, dk1) = derive_keys("password", "salt").unwrap();
        let (nk2, dk2) = derive_keys("password", "salt").unwrap();
        assert_eq!(nk1, nk2);
        assert_eq!(dk1, dk2);
    }

    #[test]
    fn derive_keys_empty_salt() {
        // rclone allows empty password2 (salt)
        let (nk, dk) = derive_keys("password", "").unwrap();
        assert_ne!(nk, [0u8; 32]);
        assert_ne!(dk, [0u8; 32]);
    }

    #[test]
    fn chunk_nonce_zero() {
        let file_nonce = [0x42u8; FILE_NONCE_SIZE];
        let nonce = chunk_nonce(&file_nonce, 0);
        assert_eq!(nonce, file_nonce); // no change for chunk 0
    }

    #[test]
    fn chunk_nonce_increment() {
        let mut file_nonce = [0u8; FILE_NONCE_SIZE];
        file_nonce[0] = 0x10;
        file_nonce[8] = 0xFF; // upper bytes stay unchanged

        let nonce = chunk_nonce(&file_nonce, 1);
        assert_eq!(nonce[0], 0x11); // 0x10 + 1
        assert_eq!(nonce[8], 0xFF); // upper half unchanged
    }

    #[test]
    fn chunk_nonce_wrapping() {
        let mut file_nonce = [0u8; FILE_NONCE_SIZE];
        file_nonce[0] = 0xFF;
        file_nonce[1] = 0x00;

        let nonce = chunk_nonce(&file_nonce, 1);
        assert_eq!(nonce[0], 0x00); // 0xFF + 1 = 0x100, wraps
        assert_eq!(nonce[1], 0x01); // carry
    }

    #[test]
    fn reject_short_header() {
        let data = b"RCLONE"; // too short
        let key = [0u8; 32];
        assert!(decrypt_file_content(data, &key).is_err());
    }

    #[test]
    fn reject_bad_magic() {
        let mut data = [0u8; HEADER_SIZE + 17]; // header + min chunk
        data[..6].copy_from_slice(b"BADMAG");
        let key = [0u8; 32];
        assert!(decrypt_file_content(&data, &key).is_err());
    }

    #[test]
    fn empty_file_decrypts_to_empty() {
        // Valid rclone crypt file with zero content (header only)
        let mut data = [0u8; HEADER_SIZE];
        data[..8].copy_from_slice(RCLONE_MAGIC);
        let key = [0u8; 32];
        let result = decrypt_file_content(&data, &key).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn encrypt_empty_file_roundtrip() {
        let (_, data_key) = derive_keys("empty", "").unwrap();
        let encrypted = encrypt_file_content(&[], &data_key).unwrap();
        assert_eq!(&encrypted[..8], RCLONE_MAGIC);
        assert_eq!(encrypted.len(), HEADER_SIZE);

        let decrypted = decrypt_file_content(&encrypted, &data_key).unwrap();
        assert!(decrypted.is_empty());
    }

    #[test]
    fn golden_rclone_decrypt_file_vectors() {
        let key = [0u8; 32];

        let out0 = decrypt_file_content(RCLONE_GOLDEN_FILE0, &key).unwrap();
        assert!(out0.is_empty());

        let out1 = decrypt_file_content(RCLONE_GOLDEN_FILE1, &key).unwrap();
        assert_eq!(out1, vec![0x01]);

        let out16 = decrypt_file_content(RCLONE_GOLDEN_FILE16, &key).unwrap();
        assert_eq!(out16, (1u8..=16u8).collect::<Vec<u8>>());
    }

    #[test]
    fn end_to_end_single_chunk() {
        // Derive keys, encrypt a small payload manually, then decrypt
        let (_, data_key) = derive_keys("test", "").unwrap();
        let cipher = XSalsa20Poly1305::new((&data_key).into());

        let plaintext = b"Hello, rclone crypt!";
        let file_nonce = [0xABu8; FILE_NONCE_SIZE];
        let nonce0 = chunk_nonce(&file_nonce, 0);
        let encrypted_chunk = cipher
            .encrypt((&nonce0).into(), plaintext.as_ref())
            .unwrap();

        // Build file: magic + nonce + chunk
        let mut file_data = Vec::new();
        file_data.extend_from_slice(RCLONE_MAGIC);
        file_data.extend_from_slice(&file_nonce);
        file_data.extend_from_slice(&encrypted_chunk);

        let decrypted = decrypt_file_content(&file_data, &data_key).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn end_to_end_multi_chunk() {
        let (_, data_key) = derive_keys("multipass", "salty").unwrap();
        let cipher = XSalsa20Poly1305::new((&data_key).into());
        let file_nonce = [0x01u8; FILE_NONCE_SIZE];

        // Create plaintext larger than one chunk
        let plaintext: Vec<u8> = (0..CHUNK_DATA_SIZE + 100)
            .map(|i| (i % 256) as u8)
            .collect();

        let mut file_data = Vec::new();
        file_data.extend_from_slice(RCLONE_MAGIC);
        file_data.extend_from_slice(&file_nonce);

        // Chunk 0: full 64KB
        let nonce0 = chunk_nonce(&file_nonce, 0);
        let enc0 = cipher
            .encrypt((&nonce0).into(), &plaintext[..CHUNK_DATA_SIZE])
            .unwrap();
        file_data.extend_from_slice(&enc0);

        // Chunk 1: remaining 100 bytes
        let nonce1 = chunk_nonce(&file_nonce, 1);
        let enc1 = cipher
            .encrypt((&nonce1).into(), &plaintext[CHUNK_DATA_SIZE..])
            .unwrap();
        file_data.extend_from_slice(&enc1);

        let decrypted = decrypt_file_content(&file_data, &data_key).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn encrypt_file_content_roundtrip_single_chunk() {
        let (_, data_key) = derive_keys("write-single", "salt").unwrap();
        let plaintext = b"rclone crypt write path";

        let encrypted = encrypt_file_content(plaintext, &data_key).unwrap();
        assert_eq!(&encrypted[..8], RCLONE_MAGIC);
        assert!(encrypted.len() > HEADER_SIZE);

        let decrypted = decrypt_file_content(&encrypted, &data_key).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn encrypt_file_content_roundtrip_multi_chunk() {
        let (_, data_key) = derive_keys("write-multi", "salt").unwrap();
        let plaintext: Vec<u8> = (0..(CHUNK_DATA_SIZE * 2 + 257))
            .map(|i| (i % 251) as u8)
            .collect();

        let encrypted = encrypt_file_content(&plaintext, &data_key).unwrap();
        assert_eq!(&encrypted[..8], RCLONE_MAGIC);
        assert!(encrypted.len() > plaintext.len());

        let decrypted = decrypt_file_content(&encrypted, &data_key).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn encrypt_file_content_uses_fresh_nonce() {
        let (_, data_key) = derive_keys("nonce", "salt").unwrap();
        let plaintext = b"same plaintext";

        let encrypted_a = encrypt_file_content(plaintext, &data_key).unwrap();
        let encrypted_b = encrypt_file_content(plaintext, &data_key).unwrap();

        assert_ne!(&encrypted_a[8..HEADER_SIZE], &encrypted_b[8..HEADER_SIZE]);
        assert_ne!(encrypted_a, encrypted_b);
        assert_eq!(
            decrypt_file_content(&encrypted_a, &data_key).unwrap(),
            plaintext
        );
        assert_eq!(
            decrypt_file_content(&encrypted_b, &data_key).unwrap(),
            plaintext
        );
    }

    // ── Phase 2 tests ──

    #[test]
    fn pkcs7_pad_unpad_roundtrip() {
        let data = b"test";
        let padded = pkcs7_pad(data);
        assert_eq!(padded.len(), 16); // padded to block size
        assert_eq!(padded[4..], [12u8; 12]); // 12 bytes of padding
        let unpadded = pkcs7_unpad(&padded).unwrap();
        assert_eq!(unpadded, data);
    }

    #[test]
    fn pkcs7_pad_exact_block() {
        let data = [0u8; 16]; // exactly one block
        let padded = pkcs7_pad(&data);
        assert_eq!(padded.len(), 32); // adds full block of padding
        let unpadded = pkcs7_unpad(&padded).unwrap();
        assert_eq!(unpadded, data);
    }

    #[test]
    fn pkcs7_unpad_invalid() {
        assert!(pkcs7_unpad(&[]).is_err());
        assert!(pkcs7_unpad(&[0]).is_err()); // pad byte 0 invalid
        assert!(pkcs7_unpad(&[5, 5, 5, 3]).is_err()); // inconsistent
    }

    #[test]
    fn base32hex_roundtrip() {
        let data = b"test filename";
        let encoded = base32hex_encode(data);
        let decoded = base32hex_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn base32hex_case_insensitive() {
        let data = b"hello";
        let encoded = base32hex_encode(data);
        let decoded_lower = base32hex_decode(&encoded.to_lowercase()).unwrap();
        assert_eq!(decoded_lower, data);
    }

    #[test]
    fn gf128_double_zero() {
        let zero = [0u8; AES_BLOCK];
        let doubled = gf128_double(&zero);
        assert_eq!(doubled, zero);
    }

    #[test]
    fn gf128_double_one() {
        let mut one = [0u8; AES_BLOCK];
        one[0] = 0x01;
        let doubled = gf128_double(&one);
        assert_eq!(doubled[0], 0x02);
    }

    #[test]
    fn eme_encrypt_decrypt_roundtrip() {
        let key = [0x42u8; 32];
        let tweak = [0x01u8; 16];
        let plaintext = [0xABu8; 32]; // 2 blocks

        let encrypted = eme_encrypt(&key, &tweak, &plaintext).unwrap();
        assert_ne!(encrypted, plaintext.to_vec());

        let decrypted = eme_decrypt(&key, &tweak, &encrypted).unwrap();
        assert_eq!(decrypted, plaintext.to_vec());
    }

    #[test]
    fn eme_different_tweaks() {
        let key = [0x42u8; 32];
        let tweak1 = [0x01u8; 16];
        let tweak2 = [0x02u8; 16];
        let plaintext = [0xABu8; 16]; // 1 block

        let enc1 = eme_encrypt(&key, &tweak1, &plaintext).unwrap();
        let enc2 = eme_encrypt(&key, &tweak2, &plaintext).unwrap();
        assert_ne!(enc1, enc2); // different tweaks produce different output
    }

    #[test]
    fn name_encrypt_decrypt_roundtrip() {
        let (name_key, _) = derive_keys("nametest", "").unwrap();
        let dir_iv = [0x55u8; 16];
        let name = "my-document.txt";

        let encrypted = encrypt_name(&name_key, &dir_iv, name).unwrap();
        let decrypted = decrypt_name(&name_key, &dir_iv, &encrypted).unwrap();
        assert_eq!(decrypted, name);
    }

    #[test]
    fn name_encrypt_decrypt_unicode() {
        let (name_key, _) = derive_keys("unicode", "salt").unwrap();
        let dir_iv = [0xAAu8; 16];
        let name = "foto_2026_è.txt";

        let encrypted = encrypt_name(&name_key, &dir_iv, name).unwrap();
        let decrypted = decrypt_name(&name_key, &dir_iv, &encrypted).unwrap();
        assert_eq!(decrypted, name);
    }

    #[test]
    fn golden_rclone_filename_standard_vectors() {
        let name_key = [0u8; 32];
        let dir_iv = [0u8; 16];

        let encrypted_1 = encrypt_name(&name_key, &dir_iv, "1").unwrap();
        assert_eq!(encrypted_1.to_lowercase(), "p0e52nreeaj0a5ea7s64m4j72s");
        assert_eq!(
            decrypt_name(&name_key, &dir_iv, "p0e52nreeaj0a5ea7s64m4j72s").unwrap(),
            "1"
        );

        let encrypted_12 = encrypt_name(&name_key, &dir_iv, "12").unwrap();
        assert_eq!(encrypted_12.to_lowercase(), "l42g6771hnv3an9cgc8cr2n1ng");
        assert_eq!(
            decrypt_name(&name_key, &dir_iv, "l42g6771hnv3an9cgc8cr2n1ng").unwrap(),
            "12"
        );

        let encrypted_123 = encrypt_name(&name_key, &dir_iv, "123").unwrap();
        assert_eq!(encrypted_123.to_lowercase(), "qgm4avr35m5loi1th53ato71v0");
        assert_eq!(
            decrypt_name(&name_key, &dir_iv, "qgm4avr35m5loi1th53ato71v0").unwrap(),
            "123"
        );
    }

    #[test]
    fn name_decrypt_rejects_invalid_base32() {
        let key = [0u8; 32];
        let iv = [0u8; 16];
        assert!(decrypt_name(&key, &iv, "!!!invalid!!!").is_err());
    }

    #[test]
    fn decrypt_and_hash_roundtrip_sha256() {
        let (_, data_key) = derive_keys("hash-test", "salt").unwrap();
        let plaintext = b"streaming hash test";
        let encrypted = encrypt_file_content(plaintext, &data_key).unwrap();
        
        let (hash, len) = decrypt_and_hash::<sha2::Sha256>(&encrypted, &data_key).unwrap();
        let expected_hash = sha2::Sha256::digest(plaintext);
        
        assert_eq!(hash, expected_hash);
        assert_eq!(len, plaintext.len() as u64);
    }

    #[test]
    fn decrypt_and_hash_empty_file() {
        let (_, data_key) = derive_keys("empty-hash", "").unwrap();
        let encrypted = encrypt_file_content(&[], &data_key).unwrap();
        
        let (hash, len) = decrypt_and_hash::<sha2::Sha256>(&encrypted, &data_key).unwrap();
        let expected_hash = sha2::Sha256::digest(b"");
        
        assert_eq!(hash, expected_hash);
        assert_eq!(len, 0);
    }

    #[tokio::test]
    async fn decrypt_and_hash_async_roundtrip() {
        let (_, data_key) = derive_keys("async-hash-test", "salt").unwrap();
        let plaintext: Vec<u8> = (0..(CHUNK_DATA_SIZE * 2 + 100))
            .map(|i| (i % 251) as u8)
            .collect();
        let encrypted = encrypt_file_content(&plaintext, &data_key).unwrap();
        
        let cursor = std::io::Cursor::new(encrypted);
        let (hash, len) = decrypt_and_hash_async::<_, sha2::Sha256>(cursor, &data_key).await.unwrap();
        let expected_hash = sha2::Sha256::digest(&plaintext);
        
        assert_eq!(hash, expected_hash);
        assert_eq!(len, plaintext.len() as u64);
    }
}
