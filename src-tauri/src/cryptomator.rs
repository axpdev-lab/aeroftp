//! Cryptomator Vault Format 8 Support
//!
//! Implements reading and writing Cryptomator-compatible encrypted vaults.
//! Uses scrypt KDF, AES Key Wrap (RFC 3394), AES-SIV for filenames,
//! and AES-GCM for file content encryption.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit};
use data_encoding::BASE32;
use secrecy::zeroize::Zeroize;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tokio::sync::Mutex;

/// Cryptomator masterkey.cryptomator file format
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct MasterkeyFile {
    scrypt_salt: String,
    scrypt_cost_param: u32,
    scrypt_block_size: u32,
    primary_master_key: String,
    hmac_master_key: String,
    version_mac: String,
}

/// Vault config from vault.cryptomator JWT payload
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VaultConfig {
    format: u32,
    cipher_combo: String,
    shortening_threshold: u32,
}

/// An unlocked vault with decrypted master keys
#[allow(dead_code)]
pub struct UnlockedVault {
    enc_key: [u8; 32],
    mac_key: [u8; 32],
    vault_path: PathBuf,
    shortening_threshold: u32,
}

/// Zeroize master keys when vault is dropped (M64/A7-S5 fix)
impl Drop for UnlockedVault {
    fn drop(&mut self) {
        self.enc_key.zeroize();
        self.mac_key.zeroize();
    }
}

/// A decrypted directory entry
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CryptomatorEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub dir_id: Option<String>,
}

/// Summary of a recursive ingest (used when creating a vault from a selection).
/// `skipped` carries a human-readable "path: reason" for every item that could
/// not be added (symlinks, unreadable entries, per-file failures) so the caller
/// can surface a partial-failure summary instead of silently dropping items.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CryptomatorIngestReport {
    pub files: u32,
    pub dirs: u32,
    pub skipped: Vec<String>,
}

/// Info returned after unlocking
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CryptomatorVaultInfo {
    pub vault_id: String,
    pub name: String,
    pub format: u32,
}

/// Global state holding unlocked vaults
pub struct CryptomatorState {
    pub vaults: Mutex<HashMap<String, UnlockedVault>>,
}

impl CryptomatorState {
    pub fn new() -> Self {
        Self {
            vaults: Mutex::new(HashMap::new()),
        }
    }
}

// ─── Crypto primitives ────────────────────────────────────────────────────────

/// Derive KEK from password using scrypt
fn derive_kek(password: &str, salt: &[u8], n: u32, r: u32) -> Result<[u8; 32], String> {
    use scrypt::{scrypt, Params};

    if !n.is_power_of_two() {
        return Err(format!(
            "Unsupported scrypt cost parameter: {n} is not a power of two"
        ));
    }
    let log_n = n.trailing_zeros();
    // Cap log2(N) at 20: with r=8 that bounds the scrypt working set to ~1 GiB
    // so a hostile masterkey.cryptomator cannot force a multi-GiB allocation on
    // unlock (CLAUDE-AV-017). Cryptomator's own default is log2(N)=15.
    if !(14..=20).contains(&log_n) {
        return Err(format!(
            "Unsupported scrypt cost parameter: log2(N)={} outside 14..=20",
            log_n
        ));
    }
    if r != 8 {
        return Err(format!(
            "Unsupported scrypt block size: r={} (expected 8)",
            r
        ));
    }

    let params = Params::new(log_n as u8, r, 1, 32).map_err(|e| format!("scrypt params: {}", e))?;

    let mut kek = [0u8; 32];
    scrypt(password.as_bytes(), salt, &params, &mut kek)
        .map_err(|e| format!("scrypt derive: {}", e))?;

    Ok(kek)
}

/// Unwrap an AES-KW wrapped key
fn unwrap_key(kek: &[u8; 32], wrapped: &[u8]) -> Result<[u8; 32], String> {
    use aes_kw::Kek;

    let kek_obj: Kek<aes_gcm::aes::Aes256> = Kek::from(*kek);
    let mut buf = [0u8; 32];
    kek_obj
        .unwrap(wrapped, &mut buf)
        .map_err(|e| format!("AES-KW unwrap: {}", e))?;
    Ok(buf)
}

/// Wrap a 256-bit key with AES Key Wrap (RFC 3394)
fn wrap_key(kek: &[u8; 32], key: &[u8; 32]) -> Result<Vec<u8>, String> {
    use aes_kw::Kek;

    let kek_obj: Kek<aes_gcm::aes::Aes256> = Kek::from(*kek);
    let mut buf = [0u8; 40]; // 32 + 8 byte integrity check
    kek_obj
        .wrap(key, &mut buf)
        .map_err(|e| format!("AES-KW wrap: {}", e))?;
    Ok(buf.to_vec())
}

/// Encrypt data with AES-SIV (deterministic authenticated encryption)
fn aes_siv_encrypt(
    enc_key: &[u8; 32],
    mac_key: &[u8; 32],
    plaintext: &[u8],
    associated_data: &[u8],
) -> Result<Vec<u8>, String> {
    use aes_siv::siv::Aes256Siv;
    use aes_siv::KeyInit as SivKeyInit;

    // AES-SIV uses a 64-byte key: mac_key || enc_key
    let mut combined_key = [0u8; 64];
    combined_key[..32].copy_from_slice(mac_key);
    combined_key[32..].copy_from_slice(enc_key);

    let mut cipher = Aes256Siv::new(&combined_key.into());
    cipher
        .encrypt([associated_data], plaintext)
        .map_err(|e| format!("AES-SIV encrypt: {}", e))
}

/// Encrypt data with AES-SIV passing ZERO associated-data components.
///
/// This is distinct from `aes_siv_encrypt(.., b"")`: the RustCrypto wrapper used
/// elsewhere always supplies exactly one associated-data header, and in AES-SIV
/// (RFC 5297, the S2V construction) "no headers" and "one empty header" yield
/// different synthetic IVs. Cryptomator hashes directory IDs with the dir ID as
/// the plaintext and NO associated data, so directory paths only match an
/// independent Cryptomator implementation when hashed exactly this way.
fn aes_siv_encrypt_no_ad(
    enc_key: &[u8; 32],
    mac_key: &[u8; 32],
    plaintext: &[u8],
) -> Result<Vec<u8>, String> {
    use aes_siv::siv::Aes256Siv;
    use aes_siv::KeyInit as SivKeyInit;

    let mut combined_key = [0u8; 64];
    combined_key[..32].copy_from_slice(mac_key);
    combined_key[32..].copy_from_slice(enc_key);

    let mut cipher = Aes256Siv::new(&combined_key.into());
    let headers: [&[u8]; 0] = [];
    cipher
        .encrypt(headers, plaintext)
        .map_err(|e| format!("AES-SIV encrypt (no AD): {}", e))
}

/// Decrypt data with AES-SIV
fn aes_siv_decrypt(
    enc_key: &[u8; 32],
    mac_key: &[u8; 32],
    ciphertext: &[u8],
    associated_data: &[u8],
) -> Result<Vec<u8>, String> {
    use aes_siv::siv::Aes256Siv;
    use aes_siv::KeyInit as SivKeyInit;

    let mut combined_key = [0u8; 64];
    combined_key[..32].copy_from_slice(mac_key);
    combined_key[32..].copy_from_slice(enc_key);

    let mut cipher = Aes256Siv::new(&combined_key.into());
    cipher
        .decrypt([associated_data], ciphertext)
        .map_err(|e| format!("AES-SIV decrypt: {}", e))
}

// ─── Vault operations ─────────────────────────────────────────────────────────

/// Hash a directory ID to its physical path in the vault
fn hash_dir_id(vault: &UnlockedVault, dir_id: &str) -> Result<PathBuf, String> {
    // Cryptomator spec: hash = SHA1(AES-SIV(plaintext = dirId, associatedData = none)).
    let encrypted = aes_siv_encrypt_no_ad(&vault.enc_key, &vault.mac_key, dir_id.as_bytes())?;

    // SHA-1 hash
    let mut hasher = Sha1::new();
    hasher.update(&encrypted);
    let hash = hasher.finalize();

    // Base32 encode
    let encoded = BASE32.encode(&hash);

    // Split into d/XX/REMAINING/
    let (prefix, rest) = encoded.split_at(2.min(encoded.len()));
    Ok(vault.vault_path.join("d").join(prefix).join(rest))
}

/// Encrypt a cleartext filename
fn encrypt_filename(vault: &UnlockedVault, dir_id: &str, name: &str) -> Result<String, String> {
    use data_encoding::BASE64URL;

    let encrypted = aes_siv_encrypt(
        &vault.enc_key,
        &vault.mac_key,
        name.as_bytes(),
        dir_id.as_bytes(),
    )?;
    // Cryptomator encodes encrypted names with PADDED base64url (e.g. "...==.c9r").
    // BASE64URL_NOPAD would drop the "=" padding and produce names a real
    // Cryptomator implementation cannot find (interop break for any name whose
    // ciphertext length is not a multiple of 3).
    let encoded = BASE64URL.encode(&encrypted);

    Ok(format!("{}.c9r", encoded))
}

/// Decrypt an encrypted filename (strip .c9r, base64url decode, AES-SIV decrypt)
fn decrypt_filename(
    vault: &UnlockedVault,
    dir_id: &str,
    encrypted_name: &str,
) -> Result<String, String> {
    use data_encoding::BASE64URL;

    let name_part = encrypted_name
        .strip_suffix(".c9r")
        .ok_or_else(|| format!("Not a .c9r entry: {}", encrypted_name))?;

    let ciphertext = BASE64URL
        .decode(name_part.as_bytes())
        .map_err(|e| format!("Base64url decode: {}", e))?;

    let plaintext = aes_siv_decrypt(
        &vault.enc_key,
        &vault.mac_key,
        &ciphertext,
        dir_id.as_bytes(),
    )?;

    String::from_utf8(plaintext).map_err(|e| format!("UTF-8 decode: {}", e))
}

/// Unlock a Cryptomator vault
fn unlock_vault_inner(
    vault_path: &Path,
    password: &str,
) -> Result<(UnlockedVault, VaultConfig), String> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;

    // Read masterkey.cryptomator
    let masterkey_path = vault_path.join("masterkey.cryptomator");
    let masterkey_json = fs::read_to_string(&masterkey_path)
        .map_err(|e| format!("Failed to read masterkey.cryptomator: {}", e))?;
    let masterkey: MasterkeyFile = serde_json::from_str(&masterkey_json)
        .map_err(|e| format!("Invalid masterkey format: {}", e))?;

    // Read vault.cryptomator (JWT)
    let vault_config_path = vault_path.join("vault.cryptomator");
    let jwt_str = fs::read_to_string(&vault_config_path)
        .map_err(|e| format!("Failed to read vault.cryptomator: {}", e))?;

    // Decode the JWT payload for the config fields. The HS256 signature is
    // verified after the master keys are unwrapped (CLAUDE-AV-016), since the
    // signing key is derived from them.
    let parts: Vec<&str> = jwt_str.trim().split('.').collect();
    if parts.len() != 3 {
        return Err("Invalid vault.cryptomator JWT format".to_string());
    }
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|e| format!("JWT decode: {}", e))?;
    let config: VaultConfig =
        serde_json::from_slice(&payload).map_err(|e| format!("Invalid vault config: {}", e))?;

    if config.format != 8 {
        return Err(format!(
            "Unsupported vault format: {} (only format 8 supported)",
            config.format
        ));
    }
    if config.cipher_combo != "SIV_GCM" {
        return Err(format!("Unsupported cipher combo: {}", config.cipher_combo));
    }

    // Derive KEK
    let salt = b64
        .decode(&masterkey.scrypt_salt)
        .map_err(|e| format!("Salt decode: {}", e))?;
    let kek = derive_kek(
        password,
        &salt,
        masterkey.scrypt_cost_param,
        masterkey.scrypt_block_size,
    )?;

    // Unwrap master keys
    let wrapped_enc = b64
        .decode(&masterkey.primary_master_key)
        .map_err(|e| format!("Enc key decode: {}", e))?;
    let wrapped_mac = b64
        .decode(&masterkey.hmac_master_key)
        .map_err(|e| format!("MAC key decode: {}", e))?;

    let mut enc_key = unwrap_key(&kek, &wrapped_enc)?;
    let mut mac_key = unwrap_key(&kek, &wrapped_mac)?;

    // Verify the vault.cryptomator HS256 signature with the unwrapped master
    // keys before trusting any config field (CLAUDE-AV-016).
    if let Err(e) = verify_vault_jwt_signature(&jwt_str, &enc_key, &mac_key) {
        enc_key.zeroize();
        mac_key.zeroize();
        return Err(e);
    }

    Ok((
        UnlockedVault {
            enc_key,
            mac_key,
            vault_path: vault_path.to_path_buf(),
            shortening_threshold: config.shortening_threshold,
        },
        config,
    ))
}

/// Verify the `vault.cryptomator` JWT (HS256) signature. Cryptomator signs the
/// vault config with HMAC-SHA256 keyed by `enc_key || mac_key` (64 bytes), so a
/// tampered config is rejected once the master keys are unwrapped from the
/// password (CLAUDE-AV-016).
fn verify_vault_jwt_signature(
    jwt: &str,
    enc_key: &[u8; 32],
    mac_key: &[u8; 32],
) -> Result<(), String> {
    use base64::Engine;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;

    let parts: Vec<&str> = jwt.trim().split('.').collect();
    if parts.len() != 3 {
        return Err("Invalid vault.cryptomator JWT format".to_string());
    }

    let header_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[0])
        .map_err(|e| format!("JWT header decode: {}", e))?;
    let header: serde_json::Value =
        serde_json::from_slice(&header_bytes).map_err(|e| format!("Invalid JWT header: {}", e))?;
    if header.get("alg").and_then(|v| v.as_str()) != Some("HS256") {
        return Err("Unsupported vault.cryptomator JWT algorithm (expected HS256)".to_string());
    }

    let provided_sig = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[2])
        .map_err(|e| format!("JWT signature decode: {}", e))?;

    let mut signing_key = Vec::with_capacity(64);
    signing_key.extend_from_slice(enc_key);
    signing_key.extend_from_slice(mac_key);
    // Fully-qualified: `KeyInit` (via aes-gcm) and `Mac` both expose
    // `new_from_slice`, so disambiguate to the MAC impl.
    let mut mac = <HmacSha256 as Mac>::new_from_slice(&signing_key)
        .map_err(|e| format!("HMAC init: {}", e))?;
    signing_key.zeroize();

    // Signing input is `base64url(header).base64url(payload)`.
    let signing_input = format!("{}.{}", parts[0], parts[1]);
    mac.update(signing_input.as_bytes());

    mac.verify_slice(&provided_sig)
        .map_err(|_| "vault.cryptomator signature verification failed".to_string())
}

/// List a directory in the vault
fn list_dir_inner(vault: &UnlockedVault, dir_id: &str) -> Result<Vec<CryptomatorEntry>, String> {
    let dir_path = hash_dir_id(vault, dir_id)?;

    if !dir_path.exists() {
        return Err(format!("Directory not found in vault: {:?}", dir_path));
    }

    let mut entries = Vec::new();
    let read_dir =
        fs::read_dir(&dir_path).map_err(|e| format!("Failed to read vault directory: {}", e))?;

    for entry_result in read_dir {
        let entry = entry_result.map_err(|e| format!("Read dir entry: {}", e))?;
        let file_name = entry.file_name().to_string_lossy().to_string();

        // Handle .c9r files (regular files or directories)
        if file_name.ends_with(".c9r") {
            let entry_path = entry.path();
            let is_dir = entry_path.is_dir();

            // For directories, the .c9r is a folder containing dir.c9r
            if is_dir {
                // It's a directory: read dir.c9r to get the child dir_id
                let dir_c9r_path = entry_path.join("dir.c9r");
                let child_dir_id = if dir_c9r_path.exists() {
                    fs::read_to_string(&dir_c9r_path)
                        .map_err(|e| format!("Failed to read dir.c9r: {}", e))?
                        .trim()
                        .to_string()
                } else {
                    String::new()
                };

                match decrypt_filename(vault, dir_id, &file_name) {
                    Ok(name) => entries.push(CryptomatorEntry {
                        name,
                        is_dir: true,
                        size: 0,
                        dir_id: Some(child_dir_id),
                    }),
                    Err(_) => continue, // Skip entries we can't decrypt
                }
            } else {
                // Regular file
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                match decrypt_filename(vault, dir_id, &file_name) {
                    Ok(name) => entries.push(CryptomatorEntry {
                        name,
                        is_dir: false,
                        size,
                        dir_id: None,
                    }),
                    Err(_) => continue,
                }
            }
        }

        // Handle .c9s (shortened names)
        if file_name.ends_with(".c9s") {
            let entry_path = entry.path();
            if entry_path.is_dir() {
                let name_path = entry_path.join("name.c9s");
                if name_path.exists() {
                    let full_encrypted = fs::read_to_string(&name_path)
                        .map_err(|e| format!("Failed to read name.c9s: {}", e))?;
                    let full_name = full_encrypted.trim();

                    // Check if it's a directory (has dir.c9r inside contents.c9r/)
                    let contents_dir = entry_path.join("contents.c9r");
                    let is_dir = contents_dir.is_dir() && contents_dir.join("dir.c9r").exists();

                    let child_dir_id = if is_dir {
                        fs::read_to_string(contents_dir.join("dir.c9r"))
                            .unwrap_or_default()
                            .trim()
                            .to_string()
                    } else {
                        String::new()
                    };

                    match decrypt_filename(vault, dir_id, full_name) {
                        Ok(name) => entries.push(CryptomatorEntry {
                            name,
                            is_dir,
                            size: 0,
                            dir_id: if is_dir { Some(child_dir_id) } else { None },
                        }),
                        Err(_) => continue,
                    }
                }
            }
        }
    }

    entries.sort_by(|a, b| {
        if a.is_dir != b.is_dir {
            return if a.is_dir {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
        a.name.to_lowercase().cmp(&b.name.to_lowercase())
    });

    Ok(entries)
}

/// Decrypt a file from the vault
/// No-progress convenience over `decrypt_file_inner_with_progress`, used by the unit
/// tests (the GUI command path passes `Some(app)` directly).
#[cfg(test)]
fn decrypt_file_inner(
    vault: &UnlockedVault,
    dir_id: &str,
    filename: &str,
    output_path: &Path,
) -> Result<(), String> {
    decrypt_file_inner_with_progress(vault, dir_id, filename, output_path, None)
}

/// As `decrypt_file_inner`, but emits byte-true `archive_progress` while writing the
/// plaintext when an `AppHandle` is supplied (GUI path). The CLI and tests pass
/// `None`. The decrypt crypto is in-app (no crate dependency), so this is part of the
/// app-only progress pass (HANDOFF section 3.3.A).
fn decrypt_file_inner_with_progress(
    vault: &UnlockedVault,
    dir_id: &str,
    filename: &str,
    output_path: &Path,
    app: Option<tauri::AppHandle>,
) -> Result<(), String> {
    use std::io::Write;

    // Find the encrypted file
    let encrypted_name = encrypt_filename(vault, dir_id, filename)?;
    let dir_path = hash_dir_id(vault, dir_id)?;
    let file_path = dir_path.join(&encrypted_name);

    if !file_path.exists() {
        return Err(format!("Encrypted file not found: {:?}", file_path));
    }

    let data = fs::read(&file_path).map_err(|e| format!("Failed to read encrypted file: {}", e))?;

    if data.len() < 68 {
        return Err("File too small to contain a valid header".to_string());
    }

    // Header: 12-byte nonce + (8 reserved + 32 content key) encrypted with GCM + 16-byte tag = 68 bytes
    let header_nonce = &data[0..12];
    let header_payload = &data[12..68];

    // Decrypt header to get content key
    let cipher = Aes256Gcm::new(GenericArray::from_slice(&vault.enc_key));
    let header_nonce = GenericArray::from_slice(header_nonce);
    let decrypted_header = cipher
        .decrypt(header_nonce, header_payload)
        .map_err(|_| "Failed to decrypt file header: wrong key?".to_string())?;

    if decrypted_header.len() < 40 {
        return Err("Invalid decrypted header size".to_string());
    }

    // Content key is bytes 8..40 (first 8 are reserved)
    let mut content_key = [0u8; 32];
    content_key.copy_from_slice(&decrypted_header[8..40]);

    let content_cipher = Aes256Gcm::new(GenericArray::from_slice(&content_key));

    // Decrypt content chunks (each: 12-byte nonce + ciphertext + 16-byte GCM tag)
    // Chunk cleartext size: 32768 bytes (32 KiB)
    // Chunk overhead: 12 (nonce) + 16 (tag) = 28 bytes
    let chunk_size = 32768 + 28;
    let content = &data[68..];

    // Exact plaintext total: full 32 KiB chunks plus the last partial chunk's payload
    // (each chunk is 12-byte nonce + ciphertext + 16-byte tag = 28 bytes of overhead).
    let full_chunks = content.len() / chunk_size;
    let rem = content.len() % chunk_size;
    let last_plain = if rem > 0 { rem.saturating_sub(28) } else { 0 };
    // u64 arithmetic so the intermediate cannot overflow `usize` on a 32-bit target
    // for a very large (>128 GB) vault file.
    let plaintext_total = full_chunks as u64 * 32768 + last_plain as u64;
    let mut progress = app.map(|a| {
        crate::archive_progress::ArchiveProgress::for_app(
            a,
            crate::archive_progress::phase::DECRYPTING,
            plaintext_total,
        )
    });

    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create output directory: {}", e))?;
    }

    let mut outfile = fs::File::create(output_path)
        .map_err(|e| format!("Failed to create output file: {}", e))?;

    let mut chunk_num: u64 = 0;
    let mut offset = 0;

    while offset < content.len() {
        let end = (offset + chunk_size).min(content.len());
        let chunk = &content[offset..end];

        if chunk.len() < 28 {
            return Err("Truncated chunk".to_string());
        }

        let chunk_nonce = &chunk[0..12];

        // AAD: chunk number (big-endian u64) + header nonce
        let mut aad = Vec::with_capacity(20);
        aad.extend_from_slice(&chunk_num.to_be_bytes());
        aad.extend_from_slice(header_nonce.as_slice());

        let nonce = GenericArray::from_slice(chunk_nonce);
        let decrypted = content_cipher
            .decrypt(
                nonce,
                aes_gcm::aead::Payload {
                    msg: &chunk[12..],
                    aad: &aad,
                },
            )
            .map_err(|_| format!("Failed to decrypt chunk {}", chunk_num))?;

        outfile
            .write_all(&decrypted)
            .map_err(|e| format!("Failed to write chunk: {}", e))?;
        if let Some(p) = progress.as_mut() {
            p.add(decrypted.len() as u64);
        }

        chunk_num += 1;
        offset = end;
    }

    if let Some(mut p) = progress {
        p.finish();
    }

    Ok(())
}

/// Encrypt a file into the vault
fn encrypt_file_inner(
    vault: &UnlockedVault,
    dir_id: &str,
    input_path: &Path,
) -> Result<String, String> {
    use rand::RngCore;
    use std::io::Write;

    let filename = input_path
        .file_name()
        .ok_or("Invalid input filename")?
        .to_string_lossy()
        .to_string();

    let plaintext = fs::read(input_path).map_err(|e| format!("Failed to read input: {}", e))?;

    let encrypted_name = encrypt_filename(vault, dir_id, &filename)?;
    let dir_path = hash_dir_id(vault, dir_id)?;
    fs::create_dir_all(&dir_path)
        .map_err(|e| format!("Failed to create vault directory: {}", e))?;

    let file_path = dir_path.join(&encrypted_name);
    let mut outfile = fs::File::create(&file_path)
        .map_err(|e| format!("Failed to create encrypted file: {}", e))?;

    // Generate random content key and header nonce (using OsRng for cryptographic security)
    let mut content_key = [0u8; 32];
    let mut header_nonce_bytes = [0u8; 12];
    let mut rng = rand::rngs::OsRng;
    rng.fill_bytes(&mut content_key);
    rng.fill_bytes(&mut header_nonce_bytes);

    // Build header payload: 8 reserved bytes + 32 content key
    let mut header_payload = [0u8; 40];
    header_payload[8..40].copy_from_slice(&content_key);

    // Encrypt header with master enc_key
    let cipher = Aes256Gcm::new(GenericArray::from_slice(&vault.enc_key));
    let header_nonce = GenericArray::from_slice(&header_nonce_bytes);
    let encrypted_header = cipher
        .encrypt(header_nonce, header_payload.as_ref())
        .map_err(|_| "Failed to encrypt file header".to_string())?;

    // Write header: nonce + encrypted payload (includes GCM tag)
    outfile
        .write_all(&header_nonce_bytes)
        .map_err(|e| format!("Write header nonce: {}", e))?;
    outfile
        .write_all(&encrypted_header)
        .map_err(|e| format!("Write header: {}", e))?;

    // Encrypt content in 32KiB chunks
    let content_cipher = Aes256Gcm::new(GenericArray::from_slice(&content_key));
    let chunk_cleartext_size = 32768;
    let mut chunk_num: u64 = 0;
    let mut offset = 0;

    while offset < plaintext.len() || (offset == 0 && plaintext.is_empty()) {
        let end = (offset + chunk_cleartext_size).min(plaintext.len());
        let chunk = &plaintext[offset..end];

        let mut chunk_nonce = [0u8; 12];
        rng.fill_bytes(&mut chunk_nonce);

        // AAD: chunk number + header nonce
        let mut aad = Vec::with_capacity(20);
        aad.extend_from_slice(&chunk_num.to_be_bytes());
        aad.extend_from_slice(&header_nonce_bytes);

        let nonce = GenericArray::from_slice(&chunk_nonce);
        let encrypted_chunk = content_cipher
            .encrypt(
                nonce,
                aes_gcm::aead::Payload {
                    msg: chunk,
                    aad: &aad,
                },
            )
            .map_err(|_| format!("Failed to encrypt chunk {}", chunk_num))?;

        outfile
            .write_all(&chunk_nonce)
            .map_err(|e| format!("Write chunk nonce: {}", e))?;
        outfile
            .write_all(&encrypted_chunk)
            .map_err(|e| format!("Write chunk: {}", e))?;

        chunk_num += 1;
        offset = end;

        if plaintext.is_empty() {
            break;
        }
    }

    Ok(encrypted_name)
}

/// Create a subdirectory inside the vault under `parent_dir_id` and return the
/// freshly minted child `dir_id`.
///
/// Cryptomator format 8 represents a directory as a `<encrypted_name>.c9r`
/// FOLDER (sibling of file entries) that contains a single `dir.c9r` holding the
/// child directory's UUID in cleartext. The child's contents then live under the
/// hashed path of that UUID (see `hash_dir_id` / `list_dir_inner`).
fn create_dir_inner(
    vault: &UnlockedVault,
    parent_dir_id: &str,
    name: &str,
) -> Result<String, String> {
    // A fresh random dir_id for the new directory (cleartext UUID, like Cryptomator).
    let new_dir_id = uuid::Uuid::new_v4().to_string();

    let encrypted_name = encrypt_filename(vault, parent_dir_id, name)?;
    let parent_path = hash_dir_id(vault, parent_dir_id)?;
    fs::create_dir_all(&parent_path)
        .map_err(|e| format!("Failed to create parent vault directory: {}", e))?;

    // The directory entry is itself a folder containing dir.c9r.
    let dir_entry_path = parent_path.join(&encrypted_name);
    fs::create_dir_all(&dir_entry_path)
        .map_err(|e| format!("Failed to create directory entry: {}", e))?;
    fs::write(dir_entry_path.join("dir.c9r"), new_dir_id.as_bytes())
        .map_err(|e| format!("Failed to write dir.c9r: {}", e))?;

    // Materialize the child's (initially empty) content directory.
    let child_path = hash_dir_id(vault, &new_dir_id)?;
    fs::create_dir_all(&child_path)
        .map_err(|e| format!("Failed to create child content directory: {}", e))?;

    Ok(new_dir_id)
}

/// Recursively ingest a single host path (file or directory) into the vault
/// under `parent_dir_id`. Files are encrypted in place; directories are created
/// via `create_dir_inner` and their children ingested under the new dir_id.
///
/// Symlinks are never followed (skipped with a reason) so ingest cannot be
/// tricked into walking outside the selection. Per-entry failures are recorded
/// in `report.skipped` rather than aborting the whole operation, so one bad file
/// does not lose the rest of the selection.
fn ingest_path_inner(
    vault: &UnlockedVault,
    parent_dir_id: &str,
    path: &Path,
    report: &mut CryptomatorIngestReport,
) {
    // symlink_metadata does NOT follow symlinks.
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) => {
            report.skipped.push(format!("{}: {}", path.display(), e));
            return;
        }
    };

    if meta.file_type().is_symlink() {
        report
            .skipped
            .push(format!("{}: symlink (not supported)", path.display()));
        return;
    }

    if meta.is_dir() {
        let name = match path.file_name() {
            Some(n) => n.to_string_lossy().to_string(),
            None => {
                report
                    .skipped
                    .push(format!("{}: invalid directory name", path.display()));
                return;
            }
        };
        let child_dir_id = match create_dir_inner(vault, parent_dir_id, &name) {
            Ok(id) => id,
            Err(e) => {
                report.skipped.push(format!("{}: {}", path.display(), e));
                return;
            }
        };
        report.dirs += 1;

        let read_dir = match fs::read_dir(path) {
            Ok(rd) => rd,
            Err(e) => {
                report.skipped.push(format!("{}: {}", path.display(), e));
                return;
            }
        };
        for entry in read_dir {
            match entry {
                Ok(entry) => ingest_path_inner(vault, &child_dir_id, &entry.path(), report),
                Err(e) => report.skipped.push(format!("{}: {}", path.display(), e)),
            }
        }
    } else if meta.is_file() {
        match encrypt_file_inner(vault, parent_dir_id, path) {
            Ok(_) => report.files += 1,
            Err(e) => report.skipped.push(format!("{}: {}", path.display(), e)),
        }
    } else {
        report.skipped.push(format!(
            "{}: not a regular file or directory",
            path.display()
        ));
    }
}

// ─── Tauri Commands ───────────────────────────────────────────────────────────

#[tauri::command]
pub async fn cryptomator_unlock(
    state: tauri::State<'_, CryptomatorState>,
    vault_path: String,
    password: String,
) -> Result<CryptomatorVaultInfo, String> {
    crate::filesystem::validate_path(&vault_path)?;
    // A7-07: Wrap password in SecretString for automatic zeroization on drop
    let secret_pwd = secrecy::SecretString::from(password);
    let path = Path::new(&vault_path);
    let result = unlock_vault_inner(path, secrecy::ExposeSecret::expose_secret(&secret_pwd));
    let (vault, config) = result?;

    let vault_id = uuid::Uuid::new_v4().to_string();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "Vault".to_string());

    let info = CryptomatorVaultInfo {
        vault_id: vault_id.clone(),
        name,
        format: config.format,
    };

    state.vaults.lock().await.insert(vault_id, vault);

    Ok(info)
}

#[tauri::command]
pub async fn cryptomator_lock(
    state: tauri::State<'_, CryptomatorState>,
    vault_id: String,
) -> Result<(), String> {
    let mut vaults = state.vaults.lock().await;
    if vaults.remove(&vault_id).is_none() {
        return Err("Vault not found or already locked".to_string());
    }
    Ok(())
}

#[tauri::command]
pub async fn cryptomator_list(
    state: tauri::State<'_, CryptomatorState>,
    vault_id: String,
    dir_id: String,
) -> Result<Vec<CryptomatorEntry>, String> {
    let vaults = state.vaults.lock().await;
    let vault = vaults.get(&vault_id).ok_or("Vault not unlocked")?;
    list_dir_inner(vault, &dir_id)
}

#[tauri::command]
pub async fn cryptomator_decrypt_file(
    state: tauri::State<'_, CryptomatorState>,
    vault_id: String,
    dir_id: String,
    filename: String,
    output_path: String,
    app: tauri::AppHandle,
) -> Result<String, String> {
    crate::filesystem::validate_path(&output_path)?;
    let vaults = state.vaults.lock().await;
    let vault = vaults.get(&vault_id).ok_or("Vault not unlocked")?;
    decrypt_file_inner_with_progress(
        vault,
        &dir_id,
        &filename,
        Path::new(&output_path),
        Some(app),
    )?;
    Ok(output_path)
}

#[tauri::command]
pub async fn cryptomator_encrypt_file(
    state: tauri::State<'_, CryptomatorState>,
    vault_id: String,
    dir_id: String,
    input_path: String,
) -> Result<String, String> {
    crate::filesystem::validate_path(&input_path)?;
    let vaults = state.vaults.lock().await;
    let vault = vaults.get(&vault_id).ok_or("Vault not unlocked")?;
    encrypt_file_inner(vault, &dir_id, Path::new(&input_path))
}

/// Recursively ingest a list of host paths (files and/or directories) into the
/// vault under `dir_id`. Used by the "Create Cryptomator Vault..." flow to add
/// the user's selection into a freshly created vault (#322). Returns a summary
/// of what was added and what was skipped.
#[tauri::command]
pub async fn cryptomator_encrypt_paths(
    state: tauri::State<'_, CryptomatorState>,
    vault_id: String,
    dir_id: String,
    input_paths: Vec<String>,
) -> Result<CryptomatorIngestReport, String> {
    for p in &input_paths {
        crate::filesystem::validate_path(p)?;
    }
    let vaults = state.vaults.lock().await;
    let vault = vaults.get(&vault_id).ok_or("Vault not unlocked")?;

    let mut report = CryptomatorIngestReport::default();
    for p in &input_paths {
        ingest_path_inner(vault, &dir_id, Path::new(p), &mut report);
    }
    Ok(report)
}

#[tauri::command]
pub async fn cryptomator_create(vault_path: String, password: String) -> Result<String, String> {
    use base64::Engine;
    use hmac::{Hmac, Mac};
    use rand::RngCore;
    use sha2::Sha256;

    let b64 = base64::engine::general_purpose::STANDARD;

    // Validate path with centralized validator (includes traversal, null bytes, length)
    crate::filesystem::validate_path(&vault_path)?;

    // A7-07: Wrap password in SecretString for automatic zeroization on drop
    let secret_pwd = secrecy::SecretString::from(password);
    let password = secrecy::ExposeSecret::expose_secret(&secret_pwd);

    // Validate password minimum length
    if password.len() < 8 {
        return Err("Password must be at least 8 characters".to_string());
    }

    let vault_dir = Path::new(&vault_path);

    // Check that a vault doesn't already exist at this path
    if vault_dir.join("masterkey.cryptomator").exists() {
        return Err("A Cryptomator vault already exists at this path".to_string());
    }

    // Step 1: Create vault directory
    fs::create_dir_all(vault_dir)
        .map_err(|e| format!("Failed to create vault directory: {}", e))?;

    // Step 2: Generate two random 256-bit master keys (using OsRng for cryptographic security)
    let mut enc_key = [0u8; 32];
    let mut mac_key = [0u8; 32];
    let mut rng = rand::rngs::OsRng;
    rng.fill_bytes(&mut enc_key);
    rng.fill_bytes(&mut mac_key);

    // Step 3: Generate random 32-byte scrypt salt
    let mut salt = [0u8; 32];
    rng.fill_bytes(&mut salt);

    // Step 4: Derive KEK via scrypt (N=32768, r=8, p=1)
    let mut kek = derive_kek(password, &salt, 32768, 8)?;

    // Step 5: Wrap both keys with AES-KW
    let wrapped_enc = wrap_key(&kek, &enc_key)?;
    let wrapped_mac = wrap_key(&kek, &mac_key)?;

    // Step 6: Compute versionMac = HMAC-SHA256(mac_key, version_bytes)
    let version: i32 = 999;
    let version_bytes = version.to_be_bytes();
    let mut hmac_obj =
        <Hmac<Sha256> as Mac>::new_from_slice(&mac_key).map_err(|e| format!("HMAC init: {}", e))?;
    hmac_obj.update(&version_bytes);
    let version_mac = hmac_obj.finalize().into_bytes();

    // Step 7: Write masterkey.cryptomator JSON
    let masterkey_json = serde_json::json!({
        "version": 999,
        "scryptSalt": b64.encode(salt),
        "scryptCostParam": 32768,
        "scryptBlockSize": 8,
        "primaryMasterKey": b64.encode(&wrapped_enc),
        "hmacMasterKey": b64.encode(&wrapped_mac),
        "versionMac": b64.encode(version_mac)
    });

    let masterkey_path = vault_dir.join("masterkey.cryptomator");
    fs::write(
        &masterkey_path,
        serde_json::to_string_pretty(&masterkey_json)
            .map_err(|e| format!("JSON serialize: {}", e))?,
    )
    .map_err(|e| format!("Failed to write masterkey.cryptomator: {}", e))?;

    // Step 8: Generate JWT vault.cryptomator
    // Header: {"kid":"masterkeyfile:masterkey.cryptomator","typ":"JWT","alg":"HS256"}
    // Payload: {"format":8,"cipherCombo":"SIV_GCM","shorteningThreshold":220}
    // Sign with raw 512-bit key (enc_key || mac_key)
    let mut jwt_signing_key = [0u8; 64];
    jwt_signing_key[..32].copy_from_slice(&enc_key);
    jwt_signing_key[32..].copy_from_slice(&mac_key);

    let mut jwt_header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256);
    jwt_header.kid = Some("masterkeyfile:masterkey.cryptomator".to_string());
    jwt_header.typ = Some("JWT".to_string());

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct VaultJwtPayload {
        format: u32,
        cipher_combo: String,
        shortening_threshold: u32,
    }

    let payload = VaultJwtPayload {
        format: 8,
        cipher_combo: "SIV_GCM".to_string(),
        shortening_threshold: 220,
    };

    let encoding_key = jsonwebtoken::EncodingKey::from_secret(&jwt_signing_key);
    let jwt_token = jsonwebtoken::encode(&jwt_header, &payload, &encoding_key)
        .map_err(|e| format!("JWT encode: {}", e))?;

    let vault_config_path = vault_dir.join("vault.cryptomator");
    fs::write(&vault_config_path, &jwt_token)
        .map_err(|e| format!("Failed to write vault.cryptomator: {}", e))?;

    // Step 9: Create d/ directory
    let d_dir = vault_dir.join("d");
    fs::create_dir_all(&d_dir).map_err(|e| format!("Failed to create d/ directory: {}", e))?;

    // Step 10: Create root directory using hash_dir_id with empty dir_id
    let temp_vault = UnlockedVault {
        enc_key,
        mac_key,
        vault_path: vault_dir.to_path_buf(),
        shortening_threshold: 220,
    };

    let root_dir_path = hash_dir_id(&temp_vault, "")?;
    fs::create_dir_all(&root_dir_path)
        .map_err(|e| format!("Failed to create root directory: {}", e))?;

    // Zeroize all key material before returning (password is auto-zeroized via SecretString drop)
    enc_key.zeroize();
    mac_key.zeroize();
    kek.zeroize();
    salt.zeroize();
    jwt_signing_key.zeroize();

    Ok("Vault created successfully".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Regression test for #322: AeroFile must unlock a Cryptomator vault from
    // the directory that holds masterkey.cryptomator. The GUI bug was passing
    // the wrong directory; this proves the backend contract the fix relies on:
    // the vault root is the directory CONTAINING masterkey.cryptomator, and any
    // other directory yields the exact "Failed to read masterkey.cryptomator"
    // error the user reported.
    #[tokio::test]
    async fn unlock_from_vault_root_succeeds_wrong_dir_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let vault_root = tmp.path().join("MyVault");
        let password = "correcthorse";

        // Build a real format-8 SIV_GCM vault.
        cryptomator_create(
            vault_root.to_string_lossy().to_string(),
            password.to_string(),
        )
        .await
        .expect("vault creation");

        assert!(vault_root.join("masterkey.cryptomator").exists());

        // Correct directory (the parent of masterkey.cryptomator) unlocks.
        unlock_vault_inner(&vault_root, password).expect("unlock from vault root");

        // The parent directory (what the user wrongly selected) does NOT hold a
        // masterkey and must fail with the reported message.
        let err = match unlock_vault_inner(tmp.path(), password) {
            Ok(_) => panic!("unlock from a non-vault directory must fail"),
            Err(e) => e,
        };
        assert!(
            err.contains("Failed to read masterkey.cryptomator"),
            "unexpected error: {err}"
        );

        // Wrong password on the right directory must also fail (sanity).
        assert!(unlock_vault_inner(&vault_root, "wrong-password").is_err());
    }

    // Regression test for #322 (the empty-vault report): "Create Cryptomator
    // Vault..." from a selection must actually INGEST the selected items, not
    // just build an empty skeleton. This locks the create -> ingest -> read-back
    // round-trip for both files AND a nested folder, including byte-identical
    // decryption.
    #[tokio::test]
    async fn create_from_selection_ingests_files_and_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let password = "correcthorse";

        // Build a source selection on disk: two top-level files + a folder that
        // itself holds a file (to exercise directory recursion).
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        let file_a = src.join("alpha.txt");
        let file_b = src.join("beta.bin");
        let bytes_a = b"hello cryptomator round-trip".to_vec();
        let bytes_b: Vec<u8> = (0u8..=255).cycle().take(5000).collect(); // binary content
        fs::write(&file_a, &bytes_a).unwrap();
        fs::write(&file_b, &bytes_b).unwrap();
        let sub = src.join("nested");
        fs::create_dir_all(&sub).unwrap();
        let file_c = sub.join("gamma.txt");
        let bytes_c = b"deep inside a subfolder".to_vec();
        fs::write(&file_c, &bytes_c).unwrap();

        // Create the vault skeleton, then ingest the selection into root (dir_id "").
        // When AERO_VAULT_OUT is set, build it there (persisted) so an INDEPENDENT
        // reader (real Cryptomator / cryptomator-cli) can open it for the
        // cross-implementation proof; otherwise inside the auto-deleted tempdir.
        let vault_root = match std::env::var("AERO_VAULT_OUT") {
            Ok(p) => {
                fs::create_dir_all(&p).unwrap();
                Path::new(&p).join("MyVault")
            }
            Err(_) => tmp.path().join("MyVault"),
        };
        cryptomator_create(
            vault_root.to_string_lossy().to_string(),
            password.to_string(),
        )
        .await
        .expect("vault creation");

        let (vault, _config) =
            unlock_vault_inner(&vault_root, password).expect("unlock from vault root");

        let mut report = CryptomatorIngestReport::default();
        for p in [&file_a, &file_b, &sub] {
            ingest_path_inner(&vault, "", p, &mut report);
        }
        assert!(
            report.skipped.is_empty(),
            "unexpected skips: {:?}",
            report.skipped
        );
        // files is a recursive total: alpha.txt + beta.bin + nested/gamma.txt.
        assert_eq!(report.files, 3, "all three files ingested (incl. nested)");
        assert_eq!(report.dirs, 1, "one folder ingested");

        // Read the vault root back: it must contain exactly alpha.txt, beta.bin,
        // and the nested/ folder (NOT empty).
        let root = list_dir_inner(&vault, "").expect("list root");
        let mut names: Vec<String> = root.iter().map(|e| e.name.clone()).collect();
        names.sort();
        assert_eq!(names, vec!["alpha.txt", "beta.bin", "nested"]);

        // Decrypt the two root files and assert byte-identical to the originals.
        for (entry_name, expected) in [("alpha.txt", &bytes_a), ("beta.bin", &bytes_b)] {
            let out = tmp.path().join(format!("out-{entry_name}"));
            decrypt_file_inner(&vault, "", entry_name, &out).expect("decrypt root file");
            assert_eq!(
                &fs::read(&out).unwrap(),
                expected,
                "{entry_name} round-trip"
            );
        }

        // Recurse into the nested folder via its child dir_id and decrypt gamma.txt.
        let nested = root
            .iter()
            .find(|e| e.name == "nested")
            .expect("nested dir entry");
        assert!(nested.is_dir);
        let child_dir_id = nested.dir_id.clone().expect("nested dir_id");
        let children = list_dir_inner(&vault, &child_dir_id).expect("list nested");
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].name, "gamma.txt");
        let out_c = tmp.path().join("out-gamma.txt");
        decrypt_file_inner(&vault, &child_dir_id, "gamma.txt", &out_c)
            .expect("decrypt nested file");
        assert_eq!(fs::read(&out_c).unwrap(), bytes_c, "gamma.txt round-trip");

        // Optional: persist originals + decrypted outputs for external sha256
        // verification (the "verifiable tests executed" evidence for #322).
        // Enabled only when AERO_E2E_DIR is set, so normal CI runs stay clean.
        if let Ok(dir) = std::env::var("AERO_E2E_DIR") {
            let d = Path::new(&dir);
            fs::create_dir_all(d).unwrap();
            for (orig, dec, label) in [
                (&file_a, &tmp.path().join("out-alpha.txt"), "alpha.txt"),
                (&file_b, &tmp.path().join("out-beta.bin"), "beta.bin"),
                (&file_c, &out_c, "gamma.txt"),
            ] {
                fs::copy(orig, d.join(format!("orig-{label}"))).unwrap();
                fs::copy(dec, d.join(format!("dec-{label}"))).unwrap();
            }
        }
    }

    // Reverse cross-implementation harness for #322: read a vault that an
    // INDEPENDENT Cryptomator implementation wrote. Gated on AERO_READ_VAULT
    // (vault dir), AERO_READ_PW (password) and AERO_READ_DUMP (output dir); a
    // no-op in normal CI. Recursively decrypts every file so the caller can
    // sha256-compare against the originals.
    #[tokio::test]
    async fn read_external_vault_dump() {
        let (vault_path, pw, dump) = match (
            std::env::var("AERO_READ_VAULT"),
            std::env::var("AERO_READ_PW"),
            std::env::var("AERO_READ_DUMP"),
        ) {
            (Ok(v), Ok(p), Ok(d)) => (v, p, d),
            _ => return, // not requested
        };

        let (vault, _c) = unlock_vault_inner(Path::new(&vault_path), &pw).expect("unlock external");

        fn walk(vault: &UnlockedVault, dir_id: &str, rel: &Path, out: &Path) {
            for e in list_dir_inner(vault, dir_id).expect("list") {
                if e.is_dir {
                    let child = e.dir_id.clone().unwrap();
                    walk(vault, &child, &rel.join(&e.name), out);
                } else {
                    let dest = out.join(rel).join(&e.name);
                    fs::create_dir_all(dest.parent().unwrap()).unwrap();
                    decrypt_file_inner(vault, dir_id, &e.name, &dest).expect("decrypt external");
                }
            }
        }
        fs::create_dir_all(&dump).unwrap();
        walk(&vault, "", Path::new(""), Path::new(&dump));
    }

    // Edge-case sweep for #322: exercises the limit conditions the GUI flow can
    // hit (empty file, unicode/spaced names, multi-level nesting, empty folder,
    // symlink, overlong name) and proves the ingest neither panics nor silently
    // drops items, and that every encrypted file decrypts byte-identically.
    #[tokio::test]
    async fn ingest_edge_cases_roundtrip_and_skip_reporting() {
        let tmp = tempfile::tempdir().unwrap();
        let password = "correcthorse";
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();

        // (1) empty file
        let empty = src.join("empty.txt");
        fs::write(&empty, b"").unwrap();
        // (2) unicode + spaces in the name
        let uni = src.join("caffè è bello 日本語.dat");
        let uni_bytes = "naïve ☕ 🚀 содержимое".as_bytes().to_vec();
        fs::write(&uni, &uni_bytes).unwrap();
        // (3) multi-level nesting a/b/deep.txt + a sibling file
        let a = src.join("a");
        let ab = a.join("b");
        fs::create_dir_all(&ab).unwrap();
        let deep = ab.join("deep.txt");
        let deep_bytes = b"two levels down".to_vec();
        fs::write(&deep, &deep_bytes).unwrap();
        fs::write(a.join("top.txt"), b"top of a").unwrap();
        // (4) empty folder
        let emptydir = src.join("emptydir");
        fs::create_dir_all(&emptydir).unwrap();
        // (5) symlink (unix only: Windows symlink creation is privileged and
        //     std::os::unix is absent there). Must be skipped, not followed.
        #[cfg(unix)]
        let link = {
            let link = src.join("link.txt");
            std::os::unix::fs::symlink(&deep, &link).unwrap();
            link
        };
        // (6) overlong filename: encrypted name blows past the 255-byte path
        //     limit, so File::create fails and it is reported as skipped, never
        //     a panic and never a silent drop.
        let overlong = src.join(format!("{}.txt", "x".repeat(250)));
        fs::write(&overlong, b"too long to store").unwrap();

        let vault_root = tmp.path().join("EdgeVault");
        cryptomator_create(
            vault_root.to_string_lossy().to_string(),
            password.to_string(),
        )
        .await
        .expect("vault creation");
        let (vault, _c) = unlock_vault_inner(&vault_root, password).expect("unlock");

        let mut report = CryptomatorIngestReport::default();
        #[allow(unused_mut)]
        let mut targets: Vec<&Path> = vec![
            empty.as_path(),
            uni.as_path(),
            a.as_path(),
            emptydir.as_path(),
            overlong.as_path(),
        ];
        #[cfg(unix)]
        targets.push(link.as_path());
        for p in targets {
            ingest_path_inner(&vault, "", p, &mut report);
        }

        // The overlong file is always skipped; the symlink adds one more on unix.
        // Both are reported with a reason, never silently dropped.
        let expected_skips = if cfg!(unix) { 2 } else { 1 };
        assert_eq!(
            report.skipped.len(),
            expected_skips,
            "skips: {:?}",
            report.skipped
        );
        #[cfg(unix)]
        assert!(report.skipped.iter().any(|s| s.contains("symlink")));
        assert!(report.skipped.iter().any(|s| s.contains(&"x".repeat(10)))); // the overlong entry
                                                                             // Folders ingested: a, a/b, emptydir = 3. Files: empty, uni, deep, top = 4.
        assert_eq!(report.dirs, 3, "dirs");
        assert_eq!(report.files, 4, "files");

        // Root listing: empty.txt, the unicode file, folder a, folder emptydir.
        let mut root: Vec<String> = list_dir_inner(&vault, "")
            .unwrap()
            .iter()
            .map(|e| e.name.clone())
            .collect();
        root.sort();
        assert_eq!(
            root,
            vec!["a", "caffè è bello 日本語.dat", "empty.txt", "emptydir"]
        );

        // Empty file round-trips to zero bytes.
        let out_empty = tmp.path().join("o-empty");
        decrypt_file_inner(&vault, "", "empty.txt", &out_empty).expect("decrypt empty");
        assert!(
            fs::read(&out_empty).unwrap().is_empty(),
            "empty stays empty"
        );

        // Unicode-named file round-trips byte-identically.
        let out_uni = tmp.path().join("o-uni");
        decrypt_file_inner(&vault, "", "caffè è bello 日本語.dat", &out_uni)
            .expect("decrypt unicode");
        assert_eq!(fs::read(&out_uni).unwrap(), uni_bytes, "unicode round-trip");

        // The empty folder exists in the vault and lists as empty.
        let entries = list_dir_inner(&vault, "").unwrap();
        let edir = entries.iter().find(|e| e.name == "emptydir").unwrap();
        assert!(edir.is_dir);
        let edir_id = edir.dir_id.clone().unwrap();
        assert!(list_dir_inner(&vault, &edir_id).unwrap().is_empty());

        // Drill two levels: a -> b -> deep.txt, byte-identical.
        let a_entry = entries.iter().find(|e| e.name == "a").unwrap();
        let a_id = a_entry.dir_id.clone().unwrap();
        let a_children = list_dir_inner(&vault, &a_id).unwrap();
        let b_entry = a_children.iter().find(|e| e.name == "b").unwrap();
        let b_id = b_entry.dir_id.clone().unwrap();
        let out_deep = tmp.path().join("o-deep");
        decrypt_file_inner(&vault, &b_id, "deep.txt", &out_deep).expect("decrypt deep");
        assert_eq!(fs::read(&out_deep).unwrap(), deep_bytes, "deep round-trip");

        // The overlong file must NOT have leaked into the vault root.
        assert!(!entries.iter().any(|e| e.name.starts_with("xxxx")));
    }
}
