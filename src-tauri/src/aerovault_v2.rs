//! AeroVault v2 — Military-Grade Encrypted Containers
//!
//! Security features:
//! - AES-256-GCM-SIV: Nonce misuse-resistant content encryption (RFC 8452)
//! - AES-256-KW: Key wrapping for master key protection (RFC 3394)
//! - AES-SIV: Deterministic filename encryption
//! - Argon2id: Memory-hard KDF with 128MiB/t=4/p=4 (OWASP 2024)
//! - HMAC-SHA512: Header integrity verification
//! - ChaCha20-Poly1305: Optional cascade mode for defense-in-depth
//! - 64KB chunks: Optimal balance of security and performance
//!
//! Format version: 2
//! File extension: .aerovault (backwards compatible detection via magic bytes)

use aes_gcm_siv::{
    aead::{Aead, KeyInit, Payload},
    Aes256GcmSiv, Nonce,
};
use aes_kw::KekAes256;
use aes_siv::Aes256SivAead;
use argon2::{Argon2, Params, Version};
use chacha20poly1305::{ChaCha20Poly1305, aead::Aead as ChaChaAead};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::{rngs::OsRng, RngCore};
use secrecy::{zeroize::Zeroize, ExposeSecret, SecretBox, SecretString};
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Sha512};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};

// ============================================================================
// Constants
// ============================================================================

/// Magic bytes identifying AeroVault v2 format
const MAGIC: &[u8; 10] = b"AEROVAULT2";
/// Current format version
const VERSION: u8 = 2;
/// Default chunk size (64KB)
const DEFAULT_CHUNK_SIZE: usize = 64 * 1024;
/// Nonce size for AES-GCM-SIV (96 bits)
const NONCE_SIZE: usize = 12;
/// Authentication tag size (128 bits)
const TAG_SIZE: usize = 16;
/// Master key size (256 bits)
const MASTER_KEY_SIZE: usize = 32;
/// MAC key size (256 bits)
const MAC_KEY_SIZE: usize = 32;
/// Salt size for Argon2
const SALT_SIZE: usize = 32;
/// Wrapped key size (32-byte key + 8-byte AES-KW overhead = 40 bytes)
const WRAPPED_KEY_SIZE: usize = 40;
/// Header size (fixed)
const HEADER_SIZE: usize = 512;

// Argon2id parameters (OWASP 2024 high security)
const ARGON2_M_COST: u32 = 128 * 1024; // 128 MiB
const ARGON2_T_COST: u32 = 4;          // 4 iterations
const ARGON2_P_COST: u32 = 4;          // 4 parallelism

// ============================================================================
// Data Structures
// ============================================================================

/// Encryption mode flag
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
#[allow(dead_code)]
pub enum EncryptionMode {
    /// AES-256-GCM-SIV only (default, fast)
    Standard = 0,
    /// AES-256-GCM-SIV + ChaCha20-Poly1305 cascade (paranoid, slower)
    Cascade = 1,
}

/// Header flags
#[derive(Clone, Copy, Debug)]
pub struct HeaderFlags {
    pub cascade_mode: bool,
    pub hidden_volume: bool,
    pub keyfile_required: bool,
}

impl HeaderFlags {
    fn to_byte(self) -> u8 {
        let mut flags = 0u8;
        if self.cascade_mode { flags |= 0x01; }
        if self.hidden_volume { flags |= 0x02; }
        if self.keyfile_required { flags |= 0x04; }
        flags
    }

    fn from_byte(byte: u8) -> Self {
        Self {
            cascade_mode: (byte & 0x01) != 0,
            hidden_volume: (byte & 0x02) != 0,
            keyfile_required: (byte & 0x04) != 0,
        }
    }
}

/// Vault header (512 bytes, fixed size)
#[derive(Clone)]
pub struct VaultHeader {
    /// Magic bytes "AEROVAULT2"
    pub magic: [u8; 10],
    /// Format version (2)
    pub version: u8,
    /// Flags (cascade, hidden, keyfile)
    pub flags: HeaderFlags,
    /// Salt for Argon2id (32 bytes)
    pub salt: [u8; SALT_SIZE],
    /// Wrapped master key (48 bytes = 32 key + 8 padding via AES-KW)
    pub wrapped_master_key: [u8; WRAPPED_KEY_SIZE],
    /// Wrapped MAC key (48 bytes)
    pub wrapped_mac_key: [u8; WRAPPED_KEY_SIZE],
    /// Chunk size (default 64KB)
    pub chunk_size: u32,
    /// Header HMAC-SHA512 (64 bytes)
    pub header_mac: [u8; 64],
}

impl VaultHeader {
    /// Serialize header to bytes (512 bytes)
    fn to_bytes(&self) -> Vec<u8> {
        let mut buf = vec![0u8; HEADER_SIZE];
        let mut pos = 0;

        // Magic (10 bytes)
        buf[pos..pos + 10].copy_from_slice(&self.magic);
        pos += 10;

        // Version (1 byte)
        buf[pos] = self.version;
        pos += 1;

        // Flags (1 byte)
        buf[pos] = self.flags.to_byte();
        pos += 1;

        // Salt (32 bytes)
        buf[pos..pos + SALT_SIZE].copy_from_slice(&self.salt);
        pos += SALT_SIZE;

        // Wrapped master key (48 bytes)
        buf[pos..pos + WRAPPED_KEY_SIZE].copy_from_slice(&self.wrapped_master_key);
        pos += WRAPPED_KEY_SIZE;

        // Wrapped MAC key (48 bytes)
        buf[pos..pos + WRAPPED_KEY_SIZE].copy_from_slice(&self.wrapped_mac_key);
        pos += WRAPPED_KEY_SIZE;

        // Chunk size (4 bytes, little-endian)
        buf[pos..pos + 4].copy_from_slice(&self.chunk_size.to_le_bytes());

        // Reserved space until MAC position (zeros by default)
        // ... zeros already

        // Header MAC at the end (64 bytes)
        buf[HEADER_SIZE - 64..].copy_from_slice(&self.header_mac);

        buf
    }

    /// Parse header from bytes
    fn from_bytes(buf: &[u8]) -> Result<Self, String> {
        if buf.len() < HEADER_SIZE {
            return Err("Invalid header size".into());
        }

        let mut pos = 0;

        // Magic
        let mut magic = [0u8; 10];
        magic.copy_from_slice(&buf[pos..pos + 10]);
        if &magic != MAGIC {
            return Err("Invalid magic bytes - not an AeroVault v2 file".into());
        }
        pos += 10;

        // Version
        let version = buf[pos];
        if version != VERSION {
            return Err(format!("Unsupported version: {}", version));
        }
        pos += 1;

        // Flags
        let flags = HeaderFlags::from_byte(buf[pos]);
        pos += 1;

        // Salt
        let mut salt = [0u8; SALT_SIZE];
        salt.copy_from_slice(&buf[pos..pos + SALT_SIZE]);
        pos += SALT_SIZE;

        // Wrapped master key
        let mut wrapped_master_key = [0u8; WRAPPED_KEY_SIZE];
        wrapped_master_key.copy_from_slice(&buf[pos..pos + WRAPPED_KEY_SIZE]);
        pos += WRAPPED_KEY_SIZE;

        // Wrapped MAC key
        let mut wrapped_mac_key = [0u8; WRAPPED_KEY_SIZE];
        wrapped_mac_key.copy_from_slice(&buf[pos..pos + WRAPPED_KEY_SIZE]);
        pos += WRAPPED_KEY_SIZE;

        // Chunk size
        let chunk_size = u32::from_le_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]);

        // Header MAC
        let mut header_mac = [0u8; 64];
        header_mac.copy_from_slice(&buf[HEADER_SIZE - 64..]);

        Ok(Self {
            magic,
            version,
            flags,
            salt,
            wrapped_master_key,
            wrapped_mac_key,
            chunk_size,
            header_mac,
        })
    }

    /// Compute HMAC-SHA512 of header (excluding the MAC field itself)
    fn compute_mac(&self, mac_key: &[u8]) -> [u8; 64] {
        let mut bytes = self.to_bytes();
        // Zero out the MAC field before computing
        bytes[HEADER_SIZE - 64..].fill(0);

        // SAFETY: HMAC-SHA512 accepts keys of any size per RFC 2104 — new_from_slice never fails
        let mut mac = <Hmac<Sha512> as Mac>::new_from_slice(mac_key)
            .expect("HMAC-SHA512 accepts any key size");
        mac.update(&bytes);
        let result = mac.finalize();

        let mut output = [0u8; 64];
        output.copy_from_slice(&result.into_bytes());
        output
    }
}

/// Encrypted file manifest entry
#[derive(Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// Encrypted filename (AES-SIV, base64)
    pub encrypted_name: String,
    /// Original filename (only in memory, not stored)
    #[serde(skip)]
    pub name: String,
    /// File size (encrypted)
    pub size: u64,
    /// Offset in data section
    pub offset: u64,
    /// Number of chunks
    pub chunk_count: u32,
    /// Is directory
    pub is_dir: bool,
    /// Modified timestamp (ISO 8601)
    pub modified: String,
}

/// Vault manifest (encrypted with AES-SIV)
#[derive(Clone, Serialize, Deserialize)]
pub struct VaultManifest {
    pub created: String,
    pub modified: String,
    pub description: Option<String>,
    pub entries: Vec<ManifestEntry>,
}

/// Unlocked vault state
#[allow(dead_code)]
pub struct UnlockedVaultV2 {
    pub path: String,
    pub header: VaultHeader,
    pub master_key: SecretBox<Vec<u8>>,
    pub mac_key: SecretBox<Vec<u8>>,
    pub manifest: VaultManifest,
}

// ============================================================================
// Key Derivation
// ============================================================================

/// Derive encryption key from password using Argon2id (OWASP 2024 params)
fn derive_key(password: &SecretString, salt: &[u8]) -> Result<SecretBox<Vec<u8>>, String> {
    let params = Params::new(
        ARGON2_M_COST,
        ARGON2_T_COST,
        ARGON2_P_COST,
        Some(MASTER_KEY_SIZE),
    ).map_err(|e| format!("Invalid Argon2 params: {}", e))?;

    let argon2 = Argon2::new(argon2::Algorithm::Argon2id, Version::V0x13, params);

    let mut output = vec![0u8; MASTER_KEY_SIZE];
    argon2.hash_password_into(
        password.expose_secret().as_bytes(),
        salt,
        &mut output,
    ).map_err(|e| format!("Key derivation failed: {}", e))?;

    Ok(SecretBox::new(Box::new(output)))
}

/// Derive separate KEKs for master key and MAC key using HKDF (ISSUE-005 fix)
/// This provides proper key separation instead of using the same KEK for both wraps
fn derive_kek_pair(base_kek: &[u8]) -> ([u8; 32], [u8; 32]) {
    let hk = Hkdf::<Sha256>::new(None, base_kek);

    let mut kek_master = [0u8; 32];
    let mut kek_mac = [0u8; 32];

    // SAFETY: HKDF-SHA256 output up to 255*32=8160 bytes; 32 bytes always succeeds
    hk.expand(b"AeroVault v2 KEK for master key", &mut kek_master)
        .expect("32 bytes is valid HKDF-SHA256 output length");
    hk.expand(b"AeroVault v2 KEK for MAC key", &mut kek_mac)
        .expect("32 bytes is valid HKDF-SHA256 output length");

    (kek_master, kek_mac)
}

/// Wrap a key using AES-256-KW (RFC 3394)
fn wrap_key(kek: &[u8], key: &[u8]) -> Result<[u8; WRAPPED_KEY_SIZE], String> {
    let kek_array: [u8; 32] = kek.try_into()
        .map_err(|_| "Invalid KEK size: expected 32 bytes")?;
    let kek = KekAes256::from(kek_array);

    let mut output = [0u8; WRAPPED_KEY_SIZE];
    kek.wrap(key, &mut output)
        .map_err(|e| format!("Key wrap failed: {:?}", e))?;

    Ok(output)
}

/// Unwrap a key using AES-256-KW
fn unwrap_key(kek: &[u8], wrapped: &[u8]) -> Result<SecretBox<Vec<u8>>, String> {
    let kek_array: [u8; 32] = kek.try_into()
        .map_err(|_| "Invalid KEK size: expected 32 bytes")?;
    let kek = KekAes256::from(kek_array);

    let mut output = vec![0u8; MASTER_KEY_SIZE];
    kek.unwrap(wrapped, &mut output)
        .map_err(|_| "Key unwrap failed - wrong password?")?;

    Ok(SecretBox::new(Box::new(output)))
}

// ============================================================================
// Filename Encryption (AES-SIV)
// ============================================================================

/// Derive 64-byte AES-SIV key from master key using HKDF-SHA256 (RFC 5869)
/// This replaces the insecure XOR derivation with cryptographically proper key expansion
fn derive_siv_key(master_key: &[u8]) -> [u8; 64] {
    let hk = Hkdf::<Sha256>::new(None, master_key);
    let mut full_key = [0u8; 64];
    // SAFETY: 64 bytes < HKDF-SHA256 max (8160 bytes). Info string provides domain separation.
    hk.expand(b"AeroVault v2 AES-SIV filename encryption", &mut full_key)
        .expect("64 bytes is valid HKDF-SHA256 output length");
    full_key
}

/// Encrypt filename using AES-SIV (deterministic, no IV needed)
fn encrypt_filename(key: &[u8], filename: &str) -> Result<String, String> {
    // AES-SIV-AEAD needs 64 bytes key (two 32-byte keys for encryption and MAC)
    // Use HKDF to properly derive the full key from the 32-byte master key
    let mut full_key = derive_siv_key(key);

    let cipher = Aes256SivAead::new_from_slice(&full_key)
        .map_err(|_| "Invalid SIV key")?;

    // Use empty nonce for deterministic encryption
    let nonce = aes_siv::Nonce::default();
    let ciphertext = cipher.encrypt(&nonce, filename.as_bytes())
        .map_err(|e| format!("Filename encryption failed: {:?}", e))?;

    // Zeroize the derived key after use
    full_key.zeroize();

    Ok(data_encoding::BASE64URL_NOPAD.encode(&ciphertext))
}

/// Decrypt filename using AES-SIV
fn decrypt_filename(key: &[u8], encrypted: &str) -> Result<String, String> {
    let ciphertext = data_encoding::BASE64URL_NOPAD
        .decode(encrypted.as_bytes())
        .map_err(|_| "Invalid base64")?;

    // Use HKDF to properly derive the full key
    let mut full_key = derive_siv_key(key);

    let cipher = Aes256SivAead::new_from_slice(&full_key)
        .map_err(|_| "Invalid SIV key")?;

    let nonce = aes_siv::Nonce::default();
    let plaintext = cipher.decrypt(&nonce, ciphertext.as_ref())
        .map_err(|_| "Filename decryption failed - wrong password?")?;

    // Zeroize the derived key after use
    full_key.zeroize();

    String::from_utf8(plaintext)
        .map_err(|_| "Invalid UTF-8 in filename".into())
}

// ============================================================================
// Content Encryption (AES-256-GCM-SIV)
// ============================================================================

/// Encrypt a chunk using AES-256-GCM-SIV with chunk index in AAD.
/// GAP-D03: Chunk index bound to AAD prevents chunk reordering/duplication attacks.
fn encrypt_chunk(key: &[u8], chunk: &[u8], chunk_index: u32) -> Result<Vec<u8>, String> {
    let cipher = Aes256GcmSiv::new_from_slice(key)
        .map_err(|_| "Invalid encryption key")?;

    // Generate random nonce
    let mut nonce_bytes = [0u8; NONCE_SIZE];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    // AAD: chunk index as little-endian u32 (prevents reordering attacks)
    let aad = chunk_index.to_le_bytes();
    let payload = Payload { msg: chunk, aad: &aad };

    let ciphertext = cipher.encrypt(nonce, payload)
        .map_err(|e| format!("Encryption failed: {:?}", e))?;

    // Output: nonce || ciphertext (includes tag)
    let mut output = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
    output.extend_from_slice(&nonce_bytes);
    output.extend_from_slice(&ciphertext);

    Ok(output)
}

/// Decrypt a chunk using AES-256-GCM-SIV with chunk index in AAD.
/// GAP-D03: Chunk index verified via AAD — tampered index causes auth failure.
fn decrypt_chunk(key: &[u8], encrypted: &[u8], chunk_index: u32) -> Result<Vec<u8>, String> {
    if encrypted.len() < NONCE_SIZE + TAG_SIZE {
        return Err("Encrypted chunk too small".into());
    }

    let cipher = Aes256GcmSiv::new_from_slice(key)
        .map_err(|_| "Invalid decryption key")?;

    let nonce = Nonce::from_slice(&encrypted[..NONCE_SIZE]);
    let ciphertext = &encrypted[NONCE_SIZE..];

    // AAD: chunk index as little-endian u32 (must match encryption AAD)
    let aad = chunk_index.to_le_bytes();
    let payload = Payload { msg: ciphertext, aad: &aad };

    cipher.decrypt(nonce, payload)
        .map_err(|_| "Decryption failed - corrupted or wrong password".into())
}

/// Apply cascade encryption (AES-GCM-SIV then ChaCha20-Poly1305)
#[allow(dead_code)]
fn encrypt_chunk_cascade(aes_key: &[u8], chacha_key: &[u8], chunk: &[u8], chunk_index: u32) -> Result<Vec<u8>, String> {
    // First layer: AES-256-GCM-SIV (with chunk index AAD)
    let aes_encrypted = encrypt_chunk(aes_key, chunk, chunk_index)?;

    // Second layer: ChaCha20-Poly1305
    let cipher = ChaCha20Poly1305::new_from_slice(chacha_key)
        .map_err(|_| "Invalid ChaCha key")?;

    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = chacha20poly1305::Nonce::from_slice(&nonce_bytes);

    let ciphertext = ChaChaAead::encrypt(&cipher, nonce, aes_encrypted.as_ref())
        .map_err(|e| format!("ChaCha encryption failed: {:?}", e))?;

    let mut output = Vec::with_capacity(12 + ciphertext.len());
    output.extend_from_slice(&nonce_bytes);
    output.extend_from_slice(&ciphertext);

    Ok(output)
}

/// Decrypt cascade (ChaCha20-Poly1305 then AES-GCM-SIV)
#[allow(dead_code)]
fn decrypt_chunk_cascade(aes_key: &[u8], chacha_key: &[u8], encrypted: &[u8], chunk_index: u32) -> Result<Vec<u8>, String> {
    if encrypted.len() < 12 + 16 {
        return Err("Encrypted chunk too small".into());
    }

    // First: remove ChaCha20-Poly1305 layer
    let cipher = ChaCha20Poly1305::new_from_slice(chacha_key)
        .map_err(|_| "Invalid ChaCha key")?;
    let nonce = chacha20poly1305::Nonce::from_slice(&encrypted[..12]);
    let ciphertext = &encrypted[12..];

    let aes_encrypted = ChaChaAead::decrypt(&cipher, nonce, ciphertext)
        .map_err(|_| "ChaCha decryption failed".to_string())?;

    // Second: remove AES-GCM-SIV layer (with chunk index AAD)
    decrypt_chunk(aes_key, &aes_encrypted, chunk_index)
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Create a new AeroVault v2
#[tauri::command]
pub async fn vault_v2_create(
    vault_path: String,
    password: String,
    description: Option<String>,
    cascade_mode: bool,
) -> Result<String, String> {
    let pwd = SecretString::from(password);

    // Generate random keys
    let mut master_key = [0u8; MASTER_KEY_SIZE];
    let mut mac_key = [0u8; MAC_KEY_SIZE];
    let mut salt = [0u8; SALT_SIZE];
    OsRng.fill_bytes(&mut master_key);
    OsRng.fill_bytes(&mut mac_key);
    OsRng.fill_bytes(&mut salt);

    // Derive base KEK from password, then derive separate KEKs for each key (ISSUE-005 fix)
    let base_kek = derive_key(&pwd, &salt)?;
    let (mut kek_master, mut kek_mac) = derive_kek_pair(base_kek.expose_secret());

    // Wrap keys with their respective KEKs
    let wrapped_master = wrap_key(&kek_master, &master_key)?;
    let wrapped_mac = wrap_key(&kek_mac, &mac_key)?;

    // Zeroize derived KEKs
    kek_master.zeroize();
    kek_mac.zeroize();

    // Create header
    let mut header = VaultHeader {
        magic: *MAGIC,
        version: VERSION,
        flags: HeaderFlags {
            cascade_mode,
            hidden_volume: false,
            keyfile_required: false,
        },
        salt,
        wrapped_master_key: wrapped_master,
        wrapped_mac_key: wrapped_mac,
        chunk_size: DEFAULT_CHUNK_SIZE as u32,
        header_mac: [0u8; 64],
    };

    // Compute header MAC
    header.header_mac = header.compute_mac(&mac_key);

    // Create empty manifest
    let manifest = VaultManifest {
        created: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        modified: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        description,
        entries: vec![],
    };

    // Encrypt manifest with AES-SIV
    let manifest_json = serde_json::to_string(&manifest)
        .map_err(|e| format!("Failed to serialize manifest: {}", e))?;
    let encrypted_manifest = encrypt_filename(&master_key, &manifest_json)?;
    let manifest_bytes = encrypted_manifest.as_bytes();

    // Write vault file
    let file = File::create(&vault_path)
        .map_err(|e| format!("Failed to create vault: {}", e))?;
    let mut writer = BufWriter::new(file);

    // Write header
    writer.write_all(&header.to_bytes())
        .map_err(|e| format!("Failed to write header: {}", e))?;

    // Write manifest length (4 bytes) + manifest
    let manifest_len = manifest_bytes.len() as u32;
    writer.write_all(&manifest_len.to_le_bytes())
        .map_err(|e| format!("Failed to write manifest length: {}", e))?;
    writer.write_all(manifest_bytes)
        .map_err(|e| format!("Failed to write manifest: {}", e))?;

    writer.flush()
        .map_err(|e| format!("Failed to flush: {}", e))?;

    // Zeroize sensitive data
    master_key.zeroize();
    mac_key.zeroize();

    Ok(vault_path)
}

/// Open an AeroVault v2 and return its metadata
#[tauri::command]
pub async fn vault_v2_open(
    vault_path: String,
    password: String,
) -> Result<serde_json::Value, String> {
    let pwd = SecretString::from(password);

    let file = File::open(&vault_path)
        .map_err(|e| format!("Failed to open vault: {}", e))?;
    let mut reader = BufReader::new(file);

    // Read header
    let mut header_buf = [0u8; HEADER_SIZE];
    reader.read_exact(&mut header_buf)
        .map_err(|e| format!("Failed to read header: {}", e))?;

    let header = VaultHeader::from_bytes(&header_buf)?;

    // Derive base KEK from password, then derive separate KEKs (ISSUE-005 fix)
    let base_kek = derive_key(&pwd, &header.salt)?;
    let (mut kek_master, mut kek_mac) = derive_kek_pair(base_kek.expose_secret());

    // Unwrap keys with their respective KEKs
    let master_key = unwrap_key(&kek_master, &header.wrapped_master_key)?;
    let mac_key = unwrap_key(&kek_mac, &header.wrapped_mac_key)?;

    // Zeroize derived KEKs immediately after use (ISSUE-006 fix)
    kek_master.zeroize();
    kek_mac.zeroize();

    // Verify header MAC
    let computed_mac = header.compute_mac(mac_key.expose_secret());
    if computed_mac != header.header_mac {
        return Err("Header integrity check failed - file may be corrupted".into());
    }

    // Read and decrypt manifest
    let mut manifest_len_buf = [0u8; 4];
    reader.read_exact(&mut manifest_len_buf)
        .map_err(|e| format!("Failed to read manifest length: {}", e))?;
    let manifest_len = u32::from_le_bytes(manifest_len_buf) as usize;

    let mut manifest_encrypted = vec![0u8; manifest_len];
    reader.read_exact(&mut manifest_encrypted)
        .map_err(|e| format!("Failed to read manifest: {}", e))?;

    let manifest_json = decrypt_filename(
        master_key.expose_secret(),
        &String::from_utf8_lossy(&manifest_encrypted),
    )?;

    // Zeroize encrypted manifest buffer (ISSUE-006 fix)
    manifest_encrypted.zeroize();

    let mut manifest: VaultManifest = serde_json::from_str(&manifest_json)
        .map_err(|e| format!("Failed to parse manifest: {}", e))?;

    // Decrypt filenames
    for entry in &mut manifest.entries {
        entry.name = decrypt_filename(master_key.expose_secret(), &entry.encrypted_name)?;
    }

    // Return vault info (keys are auto-zeroized on drop via SecretBox)
    Ok(serde_json::json!({
        "version": header.version,
        "cascade_mode": header.flags.cascade_mode,
        "chunk_size": header.chunk_size,
        "created": manifest.created,
        "modified": manifest.modified,
        "description": manifest.description,
        "file_count": manifest.entries.len(),
        "files": manifest.entries.iter().map(|e| serde_json::json!({
            "name": e.name,
            "size": e.size,
            "is_dir": e.is_dir,
            "modified": e.modified,
        })).collect::<Vec<_>>()
    }))
}

/// Check if a file is AeroVault v2 format
#[tauri::command]
pub async fn is_vault_v2(path: String) -> Result<bool, String> {
    let file = File::open(&path).map_err(|e| format!("Failed to open: {}", e))?;
    let mut reader = BufReader::new(file);

    let mut magic = [0u8; 10];
    if reader.read_exact(&mut magic).is_err() {
        return Ok(false);
    }

    Ok(&magic == MAGIC)
}

/// Peek at vault header to get security info without password
/// This reads only the unencrypted header fields (magic, version, flags)
#[tauri::command]
pub async fn vault_v2_peek(path: String) -> Result<serde_json::Value, String> {
    let file = File::open(&path).map_err(|e| format!("Failed to open: {}", e))?;
    let mut reader = BufReader::new(file);

    let mut header_buf = [0u8; HEADER_SIZE];
    reader.read_exact(&mut header_buf)
        .map_err(|e| format!("Failed to read header: {}", e))?;

    // Verify magic
    if &header_buf[0..10] != MAGIC {
        return Err("Not an AeroVault v2 file".into());
    }

    let version = header_buf[10];
    let flags = HeaderFlags::from_byte(header_buf[11]);

    Ok(serde_json::json!({
        "version": version,
        "cascade_mode": flags.cascade_mode,
        "security_level": if flags.cascade_mode { "paranoid" } else { "advanced" }
    }))
}

/// Get AeroVault v2 security info for UI display
#[tauri::command]
pub async fn vault_v2_security_info() -> serde_json::Value {
    serde_json::json!({
        "version": "2.0",
        "encryption": {
            "content": "AES-256-GCM-SIV (RFC 8452)",
            "filenames": "AES-256-SIV",
            "key_wrap": "AES-256-KW (RFC 3394)",
            "cascade": "ChaCha20-Poly1305 (optional)"
        },
        "kdf": {
            "algorithm": "Argon2id",
            "memory": "128 MiB",
            "iterations": 4,
            "parallelism": 4
        },
        "integrity": {
            "header": "HMAC-SHA512",
            "chunks": "GCM-SIV authentication tag"
        },
        "chunk_size": "64 KB",
        "features": [
            "Nonce misuse resistance",
            "Memory-hard key derivation",
            "Encrypted filenames",
            "Header integrity verification",
            "Optional cascade encryption"
        ]
    })
}

/// Derive ChaCha20 key from master key using HKDF for cascade mode
fn derive_chacha_key(master_key: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, master_key);
    let mut chacha_key = [0u8; 32];
    // SAFETY: 32 bytes < HKDF-SHA256 max (8160 bytes); expand never fails for this size
    hk.expand(b"AeroVault v2 ChaCha20-Poly1305 cascade", &mut chacha_key)
        .expect("32 bytes is valid HKDF-SHA256 output length");
    chacha_key
}

/// Add files to an existing AeroVault v2
#[tauri::command]
pub async fn vault_v2_add_files(
    vault_path: String,
    password: String,
    file_paths: Vec<String>,
) -> Result<serde_json::Value, String> {
    let pwd = SecretString::from(password);

    // Open and read the entire vault
    let file = File::open(&vault_path)
        .map_err(|e| format!("Failed to open vault: {}", e))?;
    let mut reader = BufReader::new(file);

    // Read header
    let mut header_buf = [0u8; HEADER_SIZE];
    reader.read_exact(&mut header_buf)
        .map_err(|e| format!("Failed to read header: {}", e))?;

    let header = VaultHeader::from_bytes(&header_buf)?;
    let cascade_mode = header.flags.cascade_mode;
    let chunk_size = header.chunk_size as usize;

    // Derive keys
    let base_kek = derive_key(&pwd, &header.salt)?;
    let (mut kek_master, mut kek_mac) = derive_kek_pair(base_kek.expose_secret());

    let master_key = unwrap_key(&kek_master, &header.wrapped_master_key)?;
    let mac_key = unwrap_key(&kek_mac, &header.wrapped_mac_key)?;

    // Zeroize KEKs
    kek_master.zeroize();
    kek_mac.zeroize();

    // Verify header MAC
    let computed_mac = header.compute_mac(mac_key.expose_secret());
    if computed_mac != header.header_mac {
        return Err("Header integrity check failed - wrong password?".into());
    }

    // Derive ChaCha key for cascade mode if needed
    let mut chacha_key = if cascade_mode {
        derive_chacha_key(master_key.expose_secret())
    } else {
        [0u8; 32]
    };

    // Read manifest
    let mut manifest_len_buf = [0u8; 4];
    reader.read_exact(&mut manifest_len_buf)
        .map_err(|e| format!("Failed to read manifest length: {}", e))?;
    let manifest_len = u32::from_le_bytes(manifest_len_buf) as usize;

    let mut manifest_encrypted = vec![0u8; manifest_len];
    reader.read_exact(&mut manifest_encrypted)
        .map_err(|e| format!("Failed to read manifest: {}", e))?;

    let manifest_json = decrypt_filename(
        master_key.expose_secret(),
        &String::from_utf8_lossy(&manifest_encrypted),
    )?;
    manifest_encrypted.zeroize();

    let mut manifest: VaultManifest = serde_json::from_str(&manifest_json)
        .map_err(|e| format!("Failed to parse manifest: {}", e))?;

    // Read existing data section
    let mut existing_data = Vec::new();
    reader.read_to_end(&mut existing_data)
        .map_err(|e| format!("Failed to read data: {}", e))?;

    // Current data offset for new files
    let mut data_offset = existing_data.len() as u64;
    let mut new_data = Vec::new();
    let mut added_count = 0;

    // Process each file to add
    for file_path in &file_paths {
        let source_file = File::open(file_path)
            .map_err(|e| format!("Failed to open source file '{}': {}", file_path, e))?;
        let metadata = source_file.metadata()
            .map_err(|e| format!("Failed to read metadata: {}", e))?;

        // Get filename from path
        let filename = std::path::Path::new(file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("Invalid filename: {}", file_path))?;

        // Check for duplicate
        let encrypted_name = encrypt_filename(master_key.expose_secret(), filename)?;
        if manifest.entries.iter().any(|e| e.encrypted_name == encrypted_name) {
            continue; // Skip duplicates
        }

        // Read and encrypt file content in chunks
        let mut source_reader = BufReader::new(source_file);
        let mut chunk_count = 0u32;
        let entry_offset = data_offset;

        loop {
            let mut chunk = vec![0u8; chunk_size];
            let bytes_read = source_reader.read(&mut chunk)
                .map_err(|e| format!("Failed to read source: {}", e))?;

            if bytes_read == 0 {
                break;
            }
            chunk.truncate(bytes_read);

            // Encrypt chunk (with cascade if enabled, chunk index in AAD)
            let encrypted_chunk = if cascade_mode {
                encrypt_chunk_cascade(master_key.expose_secret(), &chacha_key, &chunk, chunk_count)?
            } else {
                encrypt_chunk(master_key.expose_secret(), &chunk, chunk_count)?
            };

            // Chunk format: length (4 bytes) + encrypted data
            let chunk_len = encrypted_chunk.len() as u32;
            new_data.extend_from_slice(&chunk_len.to_le_bytes());
            new_data.extend_from_slice(&encrypted_chunk);

            data_offset += 4 + encrypted_chunk.len() as u64;
            chunk_count += 1;

            // Zeroize plaintext chunk
            chunk.zeroize();
        }

        // Add manifest entry
        let modified = metadata.modified()
            .map(|t| {
                let datetime: chrono::DateTime<chrono::Utc> = t.into();
                datetime.format("%Y-%m-%dT%H:%M:%SZ").to_string()
            })
            .unwrap_or_else(|_| chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string());

        manifest.entries.push(ManifestEntry {
            encrypted_name,
            name: String::new(), // Not stored
            size: metadata.len(),
            offset: entry_offset,
            chunk_count,
            is_dir: false,
            modified,
        });

        added_count += 1;
    }

    // Update manifest timestamp
    manifest.modified = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    // Re-encrypt manifest
    let manifest_json = serde_json::to_string(&manifest)
        .map_err(|e| format!("Failed to serialize manifest: {}", e))?;
    let encrypted_manifest = encrypt_filename(master_key.expose_secret(), &manifest_json)?;
    let manifest_bytes = encrypted_manifest.as_bytes();

    // Zeroize ChaCha key
    chacha_key.zeroize();

    // Write updated vault
    let file = File::create(&vault_path)
        .map_err(|e| format!("Failed to create vault: {}", e))?;
    let mut writer = BufWriter::new(file);

    // Write header (unchanged)
    writer.write_all(&header_buf)
        .map_err(|e| format!("Failed to write header: {}", e))?;

    // Write manifest
    let manifest_len = manifest_bytes.len() as u32;
    writer.write_all(&manifest_len.to_le_bytes())
        .map_err(|e| format!("Failed to write manifest length: {}", e))?;
    writer.write_all(manifest_bytes)
        .map_err(|e| format!("Failed to write manifest: {}", e))?;

    // Write existing data + new data
    writer.write_all(&existing_data)
        .map_err(|e| format!("Failed to write existing data: {}", e))?;
    writer.write_all(&new_data)
        .map_err(|e| format!("Failed to write new data: {}", e))?;

    writer.flush()
        .map_err(|e| format!("Failed to flush: {}", e))?;

    Ok(serde_json::json!({
        "added": added_count,
        "total": manifest.entries.len()
    }))
}

/// Extract a single entry from AeroVault v2
#[tauri::command]
pub async fn vault_v2_extract_entry(
    vault_path: String,
    password: String,
    entry_name: String,
    dest_path: String,
) -> Result<String, String> {
    let pwd = SecretString::from(password);

    let file = File::open(&vault_path)
        .map_err(|e| format!("Failed to open vault: {}", e))?;
    let mut reader = BufReader::new(file);

    // Read header
    let mut header_buf = [0u8; HEADER_SIZE];
    reader.read_exact(&mut header_buf)
        .map_err(|e| format!("Failed to read header: {}", e))?;

    let header = VaultHeader::from_bytes(&header_buf)?;
    let cascade_mode = header.flags.cascade_mode;

    // Derive keys
    let base_kek = derive_key(&pwd, &header.salt)?;
    let (mut kek_master, mut kek_mac) = derive_kek_pair(base_kek.expose_secret());

    let master_key = unwrap_key(&kek_master, &header.wrapped_master_key)?;
    let mac_key = unwrap_key(&kek_mac, &header.wrapped_mac_key)?;

    // Zeroize KEKs
    kek_master.zeroize();
    kek_mac.zeroize();

    // Verify header MAC
    let computed_mac = header.compute_mac(mac_key.expose_secret());
    if computed_mac != header.header_mac {
        return Err("Header integrity check failed - wrong password?".into());
    }

    // Derive ChaCha key for cascade mode if needed
    let mut chacha_key = if cascade_mode {
        derive_chacha_key(master_key.expose_secret())
    } else {
        [0u8; 32]
    };

    // Read manifest
    let mut manifest_len_buf = [0u8; 4];
    reader.read_exact(&mut manifest_len_buf)
        .map_err(|e| format!("Failed to read manifest length: {}", e))?;
    let manifest_len = u32::from_le_bytes(manifest_len_buf) as usize;

    let mut manifest_encrypted = vec![0u8; manifest_len];
    reader.read_exact(&mut manifest_encrypted)
        .map_err(|e| format!("Failed to read manifest: {}", e))?;

    let manifest_json = decrypt_filename(
        master_key.expose_secret(),
        &String::from_utf8_lossy(&manifest_encrypted),
    )?;
    manifest_encrypted.zeroize();

    let manifest: VaultManifest = serde_json::from_str(&manifest_json)
        .map_err(|e| format!("Failed to parse manifest: {}", e))?;

    // Find the entry by decrypted name
    let mut target_entry: Option<ManifestEntry> = None;
    for entry in &manifest.entries {
        let decrypted_name = decrypt_filename(master_key.expose_secret(), &entry.encrypted_name)?;
        if decrypted_name == entry_name {
            target_entry = Some(entry.clone());
            break;
        }
    }

    let entry = target_entry.ok_or_else(|| format!("Entry '{}' not found in vault", entry_name))?;

    if entry.is_dir {
        // Create directory
        std::fs::create_dir_all(&dest_path)
            .map_err(|e| format!("Failed to create directory: {}", e))?;
        return Ok(dest_path);
    }

    // Calculate manifest end position (where data section starts)
    let data_start = HEADER_SIZE as u64 + 4 + manifest_len as u64;

    // Seek to the entry's offset in the data section
    let file = File::open(&vault_path)
        .map_err(|e| format!("Failed to reopen vault: {}", e))?;
    let mut reader = BufReader::new(file);

    use std::io::Seek;
    reader.seek(std::io::SeekFrom::Start(data_start + entry.offset))
        .map_err(|e| format!("Failed to seek: {}", e))?;

    // Create output file
    let out_file = File::create(&dest_path)
        .map_err(|e| format!("Failed to create output: {}", e))?;
    let mut writer = BufWriter::new(out_file);

    // Read and decrypt each chunk (chunk index in AAD for integrity)
    for chunk_idx in 0..entry.chunk_count {
        // Read chunk length
        let mut len_buf = [0u8; 4];
        reader.read_exact(&mut len_buf)
            .map_err(|e| format!("Failed to read chunk length: {}", e))?;
        let chunk_len = u32::from_le_bytes(len_buf) as usize;

        // Read encrypted chunk
        let mut encrypted_chunk = vec![0u8; chunk_len];
        reader.read_exact(&mut encrypted_chunk)
            .map_err(|e| format!("Failed to read chunk: {}", e))?;

        // Decrypt chunk (chunk index verified via AAD)
        let mut plaintext = if cascade_mode {
            decrypt_chunk_cascade(master_key.expose_secret(), &chacha_key, &encrypted_chunk, chunk_idx)?
        } else {
            decrypt_chunk(master_key.expose_secret(), &encrypted_chunk, chunk_idx)?
        };

        // Write to output
        writer.write_all(&plaintext)
            .map_err(|e| format!("Failed to write: {}", e))?;

        // Zeroize plaintext
        plaintext.zeroize();
        encrypted_chunk.zeroize();
    }

    writer.flush()
        .map_err(|e| format!("Failed to flush: {}", e))?;

    // Zeroize ChaCha key
    chacha_key.zeroize();

    Ok(dest_path)
}

/// Change the password of an AeroVault v2
/// This re-wraps the master and MAC keys with a new KEK derived from the new password
/// The encrypted content remains unchanged (only the header is modified)
#[tauri::command]
pub async fn vault_v2_change_password(
    vault_path: String,
    old_password: String,
    new_password: String,
) -> Result<String, String> {
    let old_pwd = SecretString::from(old_password);
    let new_pwd = SecretString::from(new_password);

    // Read the entire vault
    let mut vault_data = std::fs::read(&vault_path)
        .map_err(|e| format!("Failed to read vault: {}", e))?;

    // Parse header
    if vault_data.len() < HEADER_SIZE {
        return Err("Invalid vault file: too small".into());
    }
    let header = VaultHeader::from_bytes(&vault_data[..HEADER_SIZE])?;

    // Derive old KEKs
    let old_base_kek = derive_key(&old_pwd, &header.salt)?;
    let (mut old_kek_master, mut old_kek_mac) = derive_kek_pair(old_base_kek.expose_secret());

    // Unwrap keys with old password
    let master_key = unwrap_key(&old_kek_master, &header.wrapped_master_key)
        .map_err(|_| "Wrong password - cannot unwrap master key")?;
    let mac_key = unwrap_key(&old_kek_mac, &header.wrapped_mac_key)
        .map_err(|_| "Wrong password - cannot unwrap MAC key")?;

    // Zeroize old KEKs
    old_kek_master.zeroize();
    old_kek_mac.zeroize();

    // Verify header MAC with old password
    let computed_mac = header.compute_mac(mac_key.expose_secret());
    if computed_mac != header.header_mac {
        return Err("Header integrity check failed - wrong password?".into());
    }

    // Generate new salt for new password
    let mut new_salt = [0u8; SALT_SIZE];
    OsRng.fill_bytes(&mut new_salt);

    // Derive new KEKs from new password
    let new_base_kek = derive_key(&new_pwd, &new_salt)?;
    let (mut new_kek_master, mut new_kek_mac) = derive_kek_pair(new_base_kek.expose_secret());

    // Re-wrap keys with new KEKs
    let new_wrapped_master = wrap_key(&new_kek_master, master_key.expose_secret())?;
    let new_wrapped_mac = wrap_key(&new_kek_mac, mac_key.expose_secret())?;

    // Zeroize new KEKs
    new_kek_master.zeroize();
    new_kek_mac.zeroize();

    // Create new header with same flags but new salt and wrapped keys
    let mut new_header = VaultHeader {
        magic: *MAGIC,
        version: VERSION,
        flags: header.flags,
        salt: new_salt,
        wrapped_master_key: new_wrapped_master,
        wrapped_mac_key: new_wrapped_mac,
        chunk_size: header.chunk_size,
        header_mac: [0u8; 64],
    };

    // Compute new header MAC
    new_header.header_mac = new_header.compute_mac(mac_key.expose_secret());

    // Write new header to vault data (in-place, first 512 bytes)
    let header_bytes = new_header.to_bytes();
    vault_data[..HEADER_SIZE].copy_from_slice(&header_bytes);

    // Write updated vault back to disk
    std::fs::write(&vault_path, &vault_data)
        .map_err(|e| format!("Failed to write vault: {}", e))?;

    // Zeroize vault data buffer
    vault_data.zeroize();

    Ok("Password changed successfully".to_string())
}

/// Delete a file entry from an AeroVault v2
/// This removes the entry from the manifest; data remains but becomes orphaned
/// A future compaction command could reclaim the space
#[tauri::command]
pub async fn vault_v2_delete_entry(
    vault_path: String,
    password: String,
    entry_name: String,
) -> Result<serde_json::Value, String> {
    let pwd = SecretString::from(password);

    // Read the entire vault
    let vault_data = std::fs::read(&vault_path)
        .map_err(|e| format!("Failed to read vault: {}", e))?;

    if vault_data.len() < HEADER_SIZE {
        return Err("Invalid vault file: too small".into());
    }

    // Parse header
    let header = VaultHeader::from_bytes(&vault_data[..HEADER_SIZE])?;

    // Derive keys
    let base_kek = derive_key(&pwd, &header.salt)?;
    let (mut kek_master, mut kek_mac) = derive_kek_pair(base_kek.expose_secret());

    let master_key = unwrap_key(&kek_master, &header.wrapped_master_key)?;
    let mac_key = unwrap_key(&kek_mac, &header.wrapped_mac_key)?;

    // Zeroize KEKs
    kek_master.zeroize();
    kek_mac.zeroize();

    // Verify header MAC
    let computed_mac = header.compute_mac(mac_key.expose_secret());
    if computed_mac != header.header_mac {
        return Err("Header integrity check failed - wrong password?".into());
    }

    // Read manifest
    let mut pos = HEADER_SIZE;
    if vault_data.len() < pos + 4 {
        return Err("Invalid vault: missing manifest length".into());
    }
    let manifest_len = u32::from_le_bytes([
        vault_data[pos], vault_data[pos + 1], vault_data[pos + 2], vault_data[pos + 3]
    ]) as usize;
    pos += 4;

    if vault_data.len() < pos + manifest_len {
        return Err("Invalid vault: manifest truncated".into());
    }
    let manifest_encrypted = &vault_data[pos..pos + manifest_len];
    pos += manifest_len;

    let manifest_json = decrypt_filename(
        master_key.expose_secret(),
        &String::from_utf8_lossy(manifest_encrypted),
    )?;

    let mut manifest: VaultManifest = serde_json::from_str(&manifest_json)
        .map_err(|e| format!("Failed to parse manifest: {}", e))?;

    // Find and remove the entry by decrypted name
    let original_count = manifest.entries.len();
    let mut found = false;

    manifest.entries.retain(|entry| {
        match decrypt_filename(master_key.expose_secret(), &entry.encrypted_name) {
            Ok(name) => {
                if name == entry_name {
                    found = true;
                    false // Remove this entry
                } else {
                    true // Keep
                }
            }
            Err(_) => true, // Keep entries we can't decrypt (shouldn't happen)
        }
    });

    if !found {
        return Err(format!("Entry '{}' not found in vault", entry_name));
    }

    // Update manifest timestamp
    manifest.modified = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    // Re-encrypt manifest
    let new_manifest_json = serde_json::to_string(&manifest)
        .map_err(|e| format!("Failed to serialize manifest: {}", e))?;
    let new_encrypted_manifest = encrypt_filename(master_key.expose_secret(), &new_manifest_json)?;
    let new_manifest_bytes = new_encrypted_manifest.as_bytes();

    // Rebuild vault: header + new manifest + existing data section
    let data_section = &vault_data[pos..];

    let file = File::create(&vault_path)
        .map_err(|e| format!("Failed to create vault: {}", e))?;
    let mut writer = BufWriter::new(file);

    // Write header (unchanged)
    writer.write_all(&vault_data[..HEADER_SIZE])
        .map_err(|e| format!("Failed to write header: {}", e))?;

    // Write new manifest
    let new_manifest_len = new_manifest_bytes.len() as u32;
    writer.write_all(&new_manifest_len.to_le_bytes())
        .map_err(|e| format!("Failed to write manifest length: {}", e))?;
    writer.write_all(new_manifest_bytes)
        .map_err(|e| format!("Failed to write manifest: {}", e))?;

    // Write data section (unchanged - orphan data will remain until compaction)
    writer.write_all(data_section)
        .map_err(|e| format!("Failed to write data: {}", e))?;

    writer.flush()
        .map_err(|e| format!("Failed to flush: {}", e))?;

    Ok(serde_json::json!({
        "deleted": entry_name,
        "remaining": manifest.entries.len(),
        "removed_count": original_count - manifest.entries.len()
    }))
}

/// Add files to a specific directory inside an AeroVault v2
/// The target_dir specifies the parent directory path (e.g. "docs/notes")
/// Files are stored as "target_dir/filename" in the manifest
#[tauri::command]
pub async fn vault_v2_add_files_to_dir(
    vault_path: String,
    password: String,
    file_paths: Vec<String>,
    target_dir: String,
) -> Result<serde_json::Value, String> {
    let target_dir = target_dir.trim().trim_matches('/').to_string();

    // Validate target_dir
    if target_dir.contains("..") {
        return Err("Directory path cannot contain '..'".into());
    }

    let pwd = SecretString::from(password);

    // Open and read the entire vault
    let file = File::open(&vault_path)
        .map_err(|e| format!("Failed to open vault: {}", e))?;
    let mut reader = BufReader::new(file);

    // Read header
    let mut header_buf = [0u8; HEADER_SIZE];
    reader.read_exact(&mut header_buf)
        .map_err(|e| format!("Failed to read header: {}", e))?;

    let header = VaultHeader::from_bytes(&header_buf)?;
    let cascade_mode = header.flags.cascade_mode;
    let chunk_size = header.chunk_size as usize;

    // Derive keys
    let base_kek = derive_key(&pwd, &header.salt)?;
    let (mut kek_master, mut kek_mac) = derive_kek_pair(base_kek.expose_secret());

    let master_key = unwrap_key(&kek_master, &header.wrapped_master_key)?;
    let mac_key = unwrap_key(&kek_mac, &header.wrapped_mac_key)?;

    kek_master.zeroize();
    kek_mac.zeroize();

    // Verify header MAC
    let computed_mac = header.compute_mac(mac_key.expose_secret());
    if computed_mac != header.header_mac {
        return Err("Header integrity check failed - wrong password?".into());
    }

    // Derive ChaCha key for cascade mode if needed
    let mut chacha_key = if cascade_mode {
        derive_chacha_key(master_key.expose_secret())
    } else {
        [0u8; 32]
    };

    // Read manifest
    let mut manifest_len_buf = [0u8; 4];
    reader.read_exact(&mut manifest_len_buf)
        .map_err(|e| format!("Failed to read manifest length: {}", e))?;
    let manifest_len = u32::from_le_bytes(manifest_len_buf) as usize;

    let mut manifest_encrypted = vec![0u8; manifest_len];
    reader.read_exact(&mut manifest_encrypted)
        .map_err(|e| format!("Failed to read manifest: {}", e))?;

    let manifest_json = decrypt_filename(
        master_key.expose_secret(),
        &String::from_utf8_lossy(&manifest_encrypted),
    )?;
    manifest_encrypted.zeroize();

    let mut manifest: VaultManifest = serde_json::from_str(&manifest_json)
        .map_err(|e| format!("Failed to parse manifest: {}", e))?;

    // Verify target directory exists (if non-empty)
    if !target_dir.is_empty() {
        let target_encrypted = encrypt_filename(master_key.expose_secret(), &target_dir)?;
        let dir_exists = manifest.entries.iter().any(|e| e.encrypted_name == target_encrypted && e.is_dir);
        if !dir_exists {
            return Err(format!("Target directory '{}' does not exist in vault", target_dir));
        }
    }

    // Read existing data section
    let mut existing_data = Vec::new();
    reader.read_to_end(&mut existing_data)
        .map_err(|e| format!("Failed to read data: {}", e))?;

    // Current data offset for new files
    let mut data_offset = existing_data.len() as u64;
    let mut new_data = Vec::new();
    let mut added_count = 0;

    // Process each file to add
    for file_path in &file_paths {
        let source_file = File::open(file_path)
            .map_err(|e| format!("Failed to open source file '{}': {}", file_path, e))?;
        let metadata = source_file.metadata()
            .map_err(|e| format!("Failed to read metadata: {}", e))?;

        // Get filename from path
        let filename = std::path::Path::new(file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("Invalid filename: {}", file_path))?;

        // Build full vault path: "target_dir/filename" or just "filename"
        let vault_name = if target_dir.is_empty() {
            filename.to_string()
        } else {
            format!("{}/{}", target_dir, filename)
        };

        // Check for duplicate
        let encrypted_name = encrypt_filename(master_key.expose_secret(), &vault_name)?;
        if manifest.entries.iter().any(|e| e.encrypted_name == encrypted_name) {
            continue; // Skip duplicates
        }

        // Read and encrypt file content in chunks
        let mut source_reader = BufReader::new(source_file);
        let mut chunk_count = 0u32;
        let entry_offset = data_offset;

        loop {
            let mut chunk = vec![0u8; chunk_size];
            let bytes_read = source_reader.read(&mut chunk)
                .map_err(|e| format!("Failed to read source: {}", e))?;

            if bytes_read == 0 {
                break;
            }
            chunk.truncate(bytes_read);

            let encrypted_chunk = if cascade_mode {
                encrypt_chunk_cascade(master_key.expose_secret(), &chacha_key, &chunk, chunk_count)?
            } else {
                encrypt_chunk(master_key.expose_secret(), &chunk, chunk_count)?
            };

            let chunk_len = encrypted_chunk.len() as u32;
            new_data.extend_from_slice(&chunk_len.to_le_bytes());
            new_data.extend_from_slice(&encrypted_chunk);

            data_offset += 4 + encrypted_chunk.len() as u64;
            chunk_count += 1;

            chunk.zeroize();
        }

        // Add manifest entry
        let modified = metadata.modified()
            .map(|t| {
                let datetime: chrono::DateTime<chrono::Utc> = t.into();
                datetime.format("%Y-%m-%dT%H:%M:%SZ").to_string()
            })
            .unwrap_or_else(|_| chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string());

        manifest.entries.push(ManifestEntry {
            encrypted_name,
            name: String::new(),
            size: metadata.len(),
            offset: entry_offset,
            chunk_count,
            is_dir: false,
            modified,
        });

        added_count += 1;
    }

    // Update manifest timestamp
    manifest.modified = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    // Re-encrypt manifest
    let manifest_json = serde_json::to_string(&manifest)
        .map_err(|e| format!("Failed to serialize manifest: {}", e))?;
    let encrypted_manifest = encrypt_filename(master_key.expose_secret(), &manifest_json)?;
    let manifest_bytes = encrypted_manifest.as_bytes();

    chacha_key.zeroize();

    // Write updated vault
    let file = File::create(&vault_path)
        .map_err(|e| format!("Failed to create vault: {}", e))?;
    let mut writer = BufWriter::new(file);

    writer.write_all(&header_buf)
        .map_err(|e| format!("Failed to write header: {}", e))?;

    let manifest_len = manifest_bytes.len() as u32;
    writer.write_all(&manifest_len.to_le_bytes())
        .map_err(|e| format!("Failed to write manifest length: {}", e))?;
    writer.write_all(manifest_bytes)
        .map_err(|e| format!("Failed to write manifest: {}", e))?;

    writer.write_all(&existing_data)
        .map_err(|e| format!("Failed to write existing data: {}", e))?;
    writer.write_all(&new_data)
        .map_err(|e| format!("Failed to write new data: {}", e))?;

    writer.flush()
        .map_err(|e| format!("Failed to flush: {}", e))?;

    Ok(serde_json::json!({
        "added": added_count,
        "total": manifest.entries.len()
    }))
}

/// Create a directory inside an AeroVault v2
/// Directories are manifest-only entries with is_dir=true, no data section
/// Supports nested paths (e.g. "docs/notes") — intermediate directories are created automatically
#[tauri::command]
pub async fn vault_v2_create_directory(
    vault_path: String,
    password: String,
    dir_name: String,
) -> Result<serde_json::Value, String> {
    // Validate directory name
    let dir_name = dir_name.trim().trim_matches('/').to_string();
    if dir_name.is_empty() {
        return Err("Directory name cannot be empty".into());
    }
    if dir_name.contains("..") {
        return Err("Directory name cannot contain '..'".into());
    }
    if dir_name.len() > 4096 {
        return Err("Directory name too long".into());
    }

    let pwd = SecretString::from(password);

    // Read the entire vault
    let vault_data = std::fs::read(&vault_path)
        .map_err(|e| format!("Failed to read vault: {}", e))?;

    if vault_data.len() < HEADER_SIZE {
        return Err("Invalid vault file: too small".into());
    }

    // Parse header
    let header = VaultHeader::from_bytes(&vault_data[..HEADER_SIZE])?;

    // Derive keys
    let base_kek = derive_key(&pwd, &header.salt)?;
    let (mut kek_master, mut kek_mac) = derive_kek_pair(base_kek.expose_secret());

    let master_key = unwrap_key(&kek_master, &header.wrapped_master_key)?;
    let mac_key = unwrap_key(&kek_mac, &header.wrapped_mac_key)?;

    kek_master.zeroize();
    kek_mac.zeroize();

    // Verify header MAC
    let computed_mac = header.compute_mac(mac_key.expose_secret());
    if computed_mac != header.header_mac {
        return Err("Header integrity check failed - wrong password?".into());
    }

    // Read manifest
    let mut pos = HEADER_SIZE;
    if vault_data.len() < pos + 4 {
        return Err("Invalid vault: missing manifest length".into());
    }
    let manifest_len = u32::from_le_bytes([
        vault_data[pos], vault_data[pos + 1], vault_data[pos + 2], vault_data[pos + 3]
    ]) as usize;
    pos += 4;

    if vault_data.len() < pos + manifest_len {
        return Err("Invalid vault: manifest truncated".into());
    }
    let manifest_encrypted = &vault_data[pos..pos + manifest_len];
    pos += manifest_len;

    let manifest_json = decrypt_filename(
        master_key.expose_secret(),
        &String::from_utf8_lossy(manifest_encrypted),
    )?;

    let mut manifest: VaultManifest = serde_json::from_str(&manifest_json)
        .map_err(|e| format!("Failed to parse manifest: {}", e))?;

    // Decrypt all existing names to check for duplicates
    let mut existing_names: Vec<String> = Vec::new();
    for entry in &manifest.entries {
        let name = decrypt_filename(master_key.expose_secret(), &entry.encrypted_name)?;
        existing_names.push(name);
    }

    // Collect all directories to create (including intermediate ones)
    // e.g. "a/b/c" → ["a", "a/b", "a/b/c"]
    let parts: Vec<&str> = dir_name.split('/').collect();
    let mut dirs_to_create: Vec<String> = Vec::new();
    for i in 1..=parts.len() {
        let path = parts[..i].join("/");
        if !existing_names.contains(&path) {
            dirs_to_create.push(path);
        }
    }

    if dirs_to_create.is_empty() {
        return Ok(serde_json::json!({
            "created": 0,
            "message": "Directory already exists"
        }));
    }

    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    // Add directory entries to manifest
    for dir_path in &dirs_to_create {
        let encrypted_name = encrypt_filename(master_key.expose_secret(), dir_path)?;
        manifest.entries.push(ManifestEntry {
            encrypted_name,
            name: String::new(),
            size: 0,
            offset: 0,
            chunk_count: 0,
            is_dir: true,
            modified: now.clone(),
        });
    }

    // Update manifest timestamp
    manifest.modified = now;

    // Re-encrypt manifest
    let new_manifest_json = serde_json::to_string(&manifest)
        .map_err(|e| format!("Failed to serialize manifest: {}", e))?;
    let new_encrypted_manifest = encrypt_filename(master_key.expose_secret(), &new_manifest_json)?;
    let new_manifest_bytes = new_encrypted_manifest.as_bytes();

    // Rebuild vault: header + new manifest + existing data section
    let data_section = &vault_data[pos..];

    let file = File::create(&vault_path)
        .map_err(|e| format!("Failed to create vault: {}", e))?;
    let mut writer = BufWriter::new(file);

    writer.write_all(&vault_data[..HEADER_SIZE])
        .map_err(|e| format!("Failed to write header: {}", e))?;

    let new_manifest_len = new_manifest_bytes.len() as u32;
    writer.write_all(&new_manifest_len.to_le_bytes())
        .map_err(|e| format!("Failed to write manifest length: {}", e))?;
    writer.write_all(new_manifest_bytes)
        .map_err(|e| format!("Failed to write manifest: {}", e))?;

    writer.write_all(data_section)
        .map_err(|e| format!("Failed to write data: {}", e))?;

    writer.flush()
        .map_err(|e| format!("Failed to flush: {}", e))?;

    let created_count = dirs_to_create.len();
    Ok(serde_json::json!({
        "created": created_count,
        "directories": dirs_to_create,
        "total": manifest.entries.len()
    }))
}

/// Delete entries from an AeroVault v2 with recursive directory support
/// If the entry is a directory and recursive=true, all children are also removed
/// If recursive=false and the directory has children, returns an error
#[tauri::command]
pub async fn vault_v2_delete_entries(
    vault_path: String,
    password: String,
    entry_names: Vec<String>,
    recursive: bool,
) -> Result<serde_json::Value, String> {
    let pwd = SecretString::from(password);

    // Read the entire vault
    let vault_data = std::fs::read(&vault_path)
        .map_err(|e| format!("Failed to read vault: {}", e))?;

    if vault_data.len() < HEADER_SIZE {
        return Err("Invalid vault file: too small".into());
    }

    // Parse header
    let header = VaultHeader::from_bytes(&vault_data[..HEADER_SIZE])?;

    // Derive keys
    let base_kek = derive_key(&pwd, &header.salt)?;
    let (mut kek_master, mut kek_mac) = derive_kek_pair(base_kek.expose_secret());

    let master_key = unwrap_key(&kek_master, &header.wrapped_master_key)?;
    let mac_key = unwrap_key(&kek_mac, &header.wrapped_mac_key)?;

    kek_master.zeroize();
    kek_mac.zeroize();

    // Verify header MAC
    let computed_mac = header.compute_mac(mac_key.expose_secret());
    if computed_mac != header.header_mac {
        return Err("Header integrity check failed - wrong password?".into());
    }

    // Read manifest
    let mut pos = HEADER_SIZE;
    if vault_data.len() < pos + 4 {
        return Err("Invalid vault: missing manifest length".into());
    }
    let manifest_len = u32::from_le_bytes([
        vault_data[pos], vault_data[pos + 1], vault_data[pos + 2], vault_data[pos + 3]
    ]) as usize;
    pos += 4;

    if vault_data.len() < pos + manifest_len {
        return Err("Invalid vault: manifest truncated".into());
    }
    let manifest_encrypted = &vault_data[pos..pos + manifest_len];
    pos += manifest_len;

    let manifest_json = decrypt_filename(
        master_key.expose_secret(),
        &String::from_utf8_lossy(manifest_encrypted),
    )?;

    let mut manifest: VaultManifest = serde_json::from_str(&manifest_json)
        .map_err(|e| format!("Failed to parse manifest: {}", e))?;

    // Decrypt all names first
    let mut decrypted_names: Vec<String> = Vec::with_capacity(manifest.entries.len());
    for entry in &manifest.entries {
        let name = decrypt_filename(master_key.expose_secret(), &entry.encrypted_name)?;
        decrypted_names.push(name);
    }

    // Build set of entries to remove
    let mut to_remove: Vec<bool> = vec![false; manifest.entries.len()];
    let mut removed_names: Vec<String> = Vec::new();

    for target_name in &entry_names {
        let mut found = false;

        for (i, name) in decrypted_names.iter().enumerate() {
            if name == target_name {
                found = true;

                // If it's a directory, check children
                if manifest.entries[i].is_dir {
                    let prefix = format!("{}/", target_name);
                    let has_children = decrypted_names.iter().any(|n| n.starts_with(&prefix));

                    if has_children && !recursive {
                        return Err(format!(
                            "Directory '{}' is not empty. Use recursive delete to remove it and its contents.",
                            target_name
                        ));
                    }

                    // Mark children for removal if recursive
                    if recursive {
                        for (j, child_name) in decrypted_names.iter().enumerate() {
                            if child_name.starts_with(&prefix) && !to_remove[j] {
                                to_remove[j] = true;
                                removed_names.push(child_name.clone());
                            }
                        }
                    }
                }

                if !to_remove[i] {
                    to_remove[i] = true;
                    removed_names.push(name.clone());
                }
                break;
            }
        }

        if !found {
            return Err(format!("Entry '{}' not found in vault", target_name));
        }
    }

    let original_count = manifest.entries.len();

    // Remove marked entries (iterate in reverse to preserve indices)
    let mut new_entries = Vec::new();
    for (i, entry) in manifest.entries.into_iter().enumerate() {
        if !to_remove[i] {
            new_entries.push(entry);
        }
    }
    manifest.entries = new_entries;

    // Update manifest timestamp
    manifest.modified = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    // Re-encrypt manifest
    let new_manifest_json = serde_json::to_string(&manifest)
        .map_err(|e| format!("Failed to serialize manifest: {}", e))?;
    let new_encrypted_manifest = encrypt_filename(master_key.expose_secret(), &new_manifest_json)?;
    let new_manifest_bytes = new_encrypted_manifest.as_bytes();

    // Rebuild vault: header + new manifest + existing data section
    let data_section = &vault_data[pos..];

    let file = File::create(&vault_path)
        .map_err(|e| format!("Failed to create vault: {}", e))?;
    let mut writer = BufWriter::new(file);

    writer.write_all(&vault_data[..HEADER_SIZE])
        .map_err(|e| format!("Failed to write header: {}", e))?;

    let new_manifest_len = new_manifest_bytes.len() as u32;
    writer.write_all(&new_manifest_len.to_le_bytes())
        .map_err(|e| format!("Failed to write manifest length: {}", e))?;
    writer.write_all(new_manifest_bytes)
        .map_err(|e| format!("Failed to write manifest: {}", e))?;

    writer.write_all(data_section)
        .map_err(|e| format!("Failed to write data: {}", e))?;

    writer.flush()
        .map_err(|e| format!("Failed to flush: {}", e))?;

    let removed_count = original_count - manifest.entries.len();
    Ok(serde_json::json!({
        "deleted": removed_names,
        "remaining": manifest.entries.len(),
        "removed_count": removed_count
    }))
}

/// Extract all entries from AeroVault v2 to a directory
#[tauri::command]
pub async fn vault_v2_extract_all(
    vault_path: String,
    password: String,
    dest_dir: String,
) -> Result<serde_json::Value, String> {
    let pwd = SecretString::from(password);

    let file = File::open(&vault_path)
        .map_err(|e| format!("Failed to open vault: {}", e))?;
    let mut reader = BufReader::new(file);

    // Read header
    let mut header_buf = [0u8; HEADER_SIZE];
    reader.read_exact(&mut header_buf)
        .map_err(|e| format!("Failed to read header: {}", e))?;

    let header = VaultHeader::from_bytes(&header_buf)?;

    // Derive keys
    let base_kek = derive_key(&pwd, &header.salt)?;
    let (mut kek_master, mut kek_mac) = derive_kek_pair(base_kek.expose_secret());

    let master_key = unwrap_key(&kek_master, &header.wrapped_master_key)?;
    let mac_key = unwrap_key(&kek_mac, &header.wrapped_mac_key)?;

    kek_master.zeroize();
    kek_mac.zeroize();

    // Verify header MAC
    let computed_mac = header.compute_mac(mac_key.expose_secret());
    if computed_mac != header.header_mac {
        return Err("Header integrity check failed - wrong password?".into());
    }

    // Read manifest
    let mut manifest_len_buf = [0u8; 4];
    reader.read_exact(&mut manifest_len_buf)
        .map_err(|e| format!("Failed to read manifest length: {}", e))?;
    let manifest_len = u32::from_le_bytes(manifest_len_buf) as usize;

    let mut manifest_encrypted = vec![0u8; manifest_len];
    reader.read_exact(&mut manifest_encrypted)
        .map_err(|e| format!("Failed to read manifest: {}", e))?;

    let manifest_json = decrypt_filename(
        master_key.expose_secret(),
        &String::from_utf8_lossy(&manifest_encrypted),
    )?;
    manifest_encrypted.zeroize();

    let manifest: VaultManifest = serde_json::from_str(&manifest_json)
        .map_err(|e| format!("Failed to parse manifest: {}", e))?;

    // Create destination directory
    std::fs::create_dir_all(&dest_dir)
        .map_err(|e| format!("Failed to create destination: {}", e))?;

    // Get decrypted names
    let mut entries_with_names: Vec<(ManifestEntry, String)> = Vec::new();
    for entry in &manifest.entries {
        let name = decrypt_filename(master_key.expose_secret(), &entry.encrypted_name)?;
        entries_with_names.push((entry.clone(), name));
    }

    // Extract each entry
    let mut extracted = 0;
    let total = entries_with_names.len();

    for (_entry, name) in entries_with_names {
        let dest_path = std::path::Path::new(&dest_dir).join(&name);

        // Call extract_entry for each file
        match vault_v2_extract_entry(
            vault_path.clone(),
            pwd.expose_secret().to_string(),
            name.clone(),
            dest_path.to_string_lossy().to_string(),
        ).await {
            Ok(_) => extracted += 1,
            Err(e) => tracing::error!("Failed to extract '{}': {}", name, e),
        }
    }

    Ok(serde_json::json!({
        "extracted": extracted,
        "total": total
    }))
}

// ============================================================================
// Vault Compaction
// ============================================================================

/// Result of a vault compaction operation
#[derive(Serialize)]
pub struct CompactResult {
    /// Original vault file size in bytes
    pub original_size: u64,
    /// Compacted vault file size in bytes
    pub compacted_size: u64,
    /// Bytes saved by compaction
    pub saved_bytes: u64,
    /// Number of file entries preserved
    pub file_count: usize,
}

/// Compact an AeroVault v2 by rewriting it without orphaned data.
///
/// When files are deleted from a vault, their encrypted data remains in the file
/// (only the manifest entry is removed). Compaction rewrites the vault with only
/// the data referenced by current manifest entries, reclaiming the orphaned space.
///
/// The operation is crash-safe: data is written to a temporary file first, then
/// atomically renamed over the original via a `.bak` intermediate.
#[tauri::command]
pub async fn vault_v2_compact(
    vault_path: String,
    password: String,
) -> Result<CompactResult, String> {
    let pwd = SecretString::from(password);

    // Get original file size before any work
    let original_size = std::fs::metadata(&vault_path)
        .map_err(|e| format!("Failed to stat vault: {}", e))?
        .len();

    // Open and read the vault
    let file = File::open(&vault_path)
        .map_err(|e| format!("Failed to open vault: {}", e))?;
    let mut reader = BufReader::new(file);

    // Read header
    let mut header_buf = [0u8; HEADER_SIZE];
    reader.read_exact(&mut header_buf)
        .map_err(|e| format!("Failed to read header: {}", e))?;

    let header = VaultHeader::from_bytes(&header_buf)?;
    let cascade_mode = header.flags.cascade_mode;

    // Derive keys
    let base_kek = derive_key(&pwd, &header.salt)?;
    let (mut kek_master, mut kek_mac) = derive_kek_pair(base_kek.expose_secret());

    let master_key = unwrap_key(&kek_master, &header.wrapped_master_key)?;
    let mac_key = unwrap_key(&kek_mac, &header.wrapped_mac_key)?;

    // Zeroize KEKs
    kek_master.zeroize();
    kek_mac.zeroize();

    // Verify header MAC
    let computed_mac = header.compute_mac(mac_key.expose_secret());
    if computed_mac != header.header_mac {
        return Err("Header integrity check failed - wrong password?".into());
    }

    // Derive ChaCha key for cascade mode if needed
    let mut chacha_key = if cascade_mode {
        derive_chacha_key(master_key.expose_secret())
    } else {
        [0u8; 32]
    };

    // Read manifest
    let mut manifest_len_buf = [0u8; 4];
    reader.read_exact(&mut manifest_len_buf)
        .map_err(|e| format!("Failed to read manifest length: {}", e))?;
    let manifest_len = u32::from_le_bytes(manifest_len_buf) as usize;

    // Prevent OOM from corrupted or malicious manifest_len
    if manifest_len > 64 * 1024 * 1024 {
        return Err("Manifest exceeds maximum allowed size (64 MB)".to_string());
    }

    let mut manifest_encrypted = vec![0u8; manifest_len];
    reader.read_exact(&mut manifest_encrypted)
        .map_err(|e| format!("Failed to read manifest: {}", e))?;

    let manifest_json = decrypt_filename(
        master_key.expose_secret(),
        &String::from_utf8_lossy(&manifest_encrypted),
    )?;
    manifest_encrypted.zeroize();

    let mut manifest: VaultManifest = serde_json::from_str(&manifest_json)
        .map_err(|e| format!("Failed to parse manifest: {}", e))?;

    // Calculate where the data section starts in the original file
    let data_start = HEADER_SIZE as u64 + 4 + manifest_len as u64;

    // Drop the sequential reader — we will use seek-based reads per entry
    drop(reader);

    // Prepare the temporary compacted vault file
    let tmp_path = format!("{}.compact.tmp", vault_path);
    let tmp_file = File::create(&tmp_path)
        .map_err(|e| format!("Failed to create temp file: {}", e))?;
    let mut writer = BufWriter::new(tmp_file);

    // Write header (unchanged)
    writer.write_all(&header_buf)
        .map_err(|e| format!("Failed to write header to temp: {}", e))?;

    // Reserve space for manifest (we will come back and write it after we know offsets)
    // Strategy: write a placeholder manifest length + empty bytes, then rewrite later.
    // But it is simpler to: first write all data, then compute manifest, then write
    // the complete file. We will buffer the new data section in memory.
    //
    // Actually, a cleaner approach: accumulate all new data, then write header + manifest + data.
    // This avoids seeking back. The vault should fit in memory since we already read it.

    // Reset writer — we will write the complete file at the end
    drop(writer);

    // Re-open the original vault for seek-based chunk reading
    let orig_file = File::open(&vault_path)
        .map_err(|e| format!("Failed to reopen vault: {}", e))?;
    let mut orig_reader = BufReader::new(orig_file);

    // Accumulate compacted data section
    let mut compacted_data: Vec<u8> = Vec::new();
    let mut new_data_offset: u64 = 0;
    let file_count = manifest.entries.len();

    use std::io::Seek;

    for entry in &mut manifest.entries {
        // Directory entries have no data — just reset offset
        if entry.is_dir || entry.chunk_count == 0 {
            entry.offset = 0;
            continue;
        }

        // Record new offset for this entry
        let entry_new_offset = new_data_offset;

        // Seek to the entry's data in the original vault
        orig_reader.seek(std::io::SeekFrom::Start(data_start + entry.offset))
            .map_err(|e| format!("Failed to seek to entry data: {}", e))?;

        // Read each chunk, decrypt, re-encrypt, write to compacted data
        for chunk_idx in 0..entry.chunk_count {
            // Read chunk length (4 bytes)
            let mut len_buf = [0u8; 4];
            orig_reader.read_exact(&mut len_buf)
                .map_err(|e| format!("Failed to read chunk length: {}", e))?;
            let encrypted_chunk_len = u32::from_le_bytes(len_buf) as usize;

            // Read encrypted chunk
            let mut encrypted_chunk = vec![0u8; encrypted_chunk_len];
            orig_reader.read_exact(&mut encrypted_chunk)
                .map_err(|e| format!("Failed to read chunk data: {}", e))?;

            // Decrypt chunk
            let mut plaintext = if cascade_mode {
                decrypt_chunk_cascade(
                    master_key.expose_secret(),
                    &chacha_key,
                    &encrypted_chunk,
                    chunk_idx,
                )?
            } else {
                decrypt_chunk(master_key.expose_secret(), &encrypted_chunk, chunk_idx)?
            };

            encrypted_chunk.zeroize();

            // Re-encrypt chunk with fresh nonces
            let new_encrypted = if cascade_mode {
                encrypt_chunk_cascade(
                    master_key.expose_secret(),
                    &chacha_key,
                    &plaintext,
                    chunk_idx,
                )?
            } else {
                encrypt_chunk(master_key.expose_secret(), &plaintext, chunk_idx)?
            };

            plaintext.zeroize();

            // Write chunk to compacted data: length (4 bytes) + encrypted data
            let new_chunk_len = new_encrypted.len() as u32;
            compacted_data.extend_from_slice(&new_chunk_len.to_le_bytes());
            compacted_data.extend_from_slice(&new_encrypted);

            new_data_offset += 4 + new_encrypted.len() as u64;
        }

        // Update entry offset to point to its new position
        entry.offset = entry_new_offset;
    }

    drop(orig_reader);

    // Update manifest timestamp
    manifest.modified = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    // Re-encrypt manifest with updated offsets
    let new_manifest_json = serde_json::to_string(&manifest)
        .map_err(|e| format!("Failed to serialize manifest: {}", e))?;
    let new_encrypted_manifest = encrypt_filename(master_key.expose_secret(), &new_manifest_json)?;
    let new_manifest_bytes = new_encrypted_manifest.as_bytes();

    // Zeroize ChaCha key
    chacha_key.zeroize();

    // Write complete compacted vault to temp file
    let tmp_file = File::create(&tmp_path)
        .map_err(|e| format!("Failed to create temp file: {}", e))?;
    let mut writer = BufWriter::new(tmp_file);

    // Write header (unchanged — same salt, wrapped keys, MAC)
    writer.write_all(&header_buf)
        .map_err(|e| format!("Failed to write header: {}", e))?;

    // Write manifest length + manifest
    let new_manifest_len = new_manifest_bytes.len() as u32;
    writer.write_all(&new_manifest_len.to_le_bytes())
        .map_err(|e| format!("Failed to write manifest length: {}", e))?;
    writer.write_all(new_manifest_bytes)
        .map_err(|e| format!("Failed to write manifest: {}", e))?;

    // Write compacted data section
    writer.write_all(&compacted_data)
        .map_err(|e| format!("Failed to write data: {}", e))?;

    writer.flush()
        .map_err(|e| format!("Failed to flush temp file: {}", e))?;
    drop(writer);

    // Atomic rename: original → .bak, temp → original, delete .bak
    let bak_path = format!("{}.bak", vault_path);

    std::fs::rename(&vault_path, &bak_path)
        .map_err(|e| format!("Failed to backup original vault: {}", e))?;

    if let Err(e) = std::fs::rename(&tmp_path, &vault_path) {
        // Rollback: restore original from backup
        let _ = std::fs::rename(&bak_path, &vault_path);
        return Err(format!("Failed to replace vault with compacted file: {}", e));
    }

    // Remove backup (non-fatal if this fails)
    let _ = std::fs::remove_file(&bak_path);

    // Get compacted size
    let compacted_size = std::fs::metadata(&vault_path)
        .map_err(|e| format!("Failed to stat compacted vault: {}", e))?
        .len();

    let saved_bytes = original_size.saturating_sub(compacted_size);

    Ok(CompactResult {
        original_size,
        compacted_size,
        saved_bytes,
        file_count,
    })
}

// ============================================================================
// Vault Bidirectional Sync
// ============================================================================

/// A conflict entry where the file exists in both vault and local with different content
#[derive(Serialize)]
pub struct VaultSyncConflict {
    pub name: String,
    pub vault_modified: String,
    pub local_modified: String,
    pub vault_size: u64,
    pub local_size: u64,
}

/// Comparison result between vault contents and a local directory
#[derive(Serialize)]
pub struct VaultSyncComparison {
    pub vault_only: Vec<String>,
    pub local_only: Vec<String>,
    pub conflicts: Vec<VaultSyncConflict>,
    pub unchanged: usize,
}

/// Compare vault contents with a local directory to determine sync actions
#[tauri::command]
pub async fn vault_v2_sync_compare(
    vault_path: String,
    password: String,
    local_dir: String,
) -> Result<VaultSyncComparison, String> {
    // Validate local_dir
    let local_dir_path = std::path::Path::new(&local_dir);
    if !local_dir_path.is_dir() {
        return Err(format!("Local directory does not exist: {}", local_dir));
    }
    let local_dir_canonical = local_dir_path.canonicalize()
        .map_err(|e| format!("Failed to resolve local directory: {}", e))?;

    let pwd = SecretString::from(password);

    // Open vault and decrypt manifest
    let file = File::open(&vault_path)
        .map_err(|e| format!("Failed to open vault: {}", e))?;
    let mut reader = BufReader::new(file);

    let mut header_buf = [0u8; HEADER_SIZE];
    reader.read_exact(&mut header_buf)
        .map_err(|e| format!("Failed to read header: {}", e))?;

    let header = VaultHeader::from_bytes(&header_buf)?;

    let base_kek = derive_key(&pwd, &header.salt)?;
    let (mut kek_master, mut kek_mac) = derive_kek_pair(base_kek.expose_secret());

    let master_key = unwrap_key(&kek_master, &header.wrapped_master_key)?;
    let mac_key = unwrap_key(&kek_mac, &header.wrapped_mac_key)?;

    kek_master.zeroize();
    kek_mac.zeroize();

    let computed_mac = header.compute_mac(mac_key.expose_secret());
    if computed_mac != header.header_mac {
        return Err("Header integrity check failed - wrong password?".into());
    }

    let mut manifest_len_buf = [0u8; 4];
    reader.read_exact(&mut manifest_len_buf)
        .map_err(|e| format!("Failed to read manifest length: {}", e))?;
    let manifest_len = u32::from_le_bytes(manifest_len_buf) as usize;

    let mut manifest_encrypted = vec![0u8; manifest_len];
    reader.read_exact(&mut manifest_encrypted)
        .map_err(|e| format!("Failed to read manifest: {}", e))?;

    let manifest_json = decrypt_filename(
        master_key.expose_secret(),
        &String::from_utf8_lossy(&manifest_encrypted),
    )?;
    manifest_encrypted.zeroize();

    let manifest: VaultManifest = serde_json::from_str(&manifest_json)
        .map_err(|e| format!("Failed to parse manifest: {}", e))?;

    // Build vault entries map: name -> (size, modified)
    let mut vault_files: std::collections::HashMap<String, (u64, String)> = std::collections::HashMap::new();
    for entry in &manifest.entries {
        if entry.is_dir {
            continue;
        }
        let name = decrypt_filename(master_key.expose_secret(), &entry.encrypted_name)?;
        vault_files.insert(name, (entry.size, entry.modified.clone()));
    }

    // Walk local directory
    let mut local_files: std::collections::HashMap<String, (u64, String)> = std::collections::HashMap::new();
    for dir_entry in walkdir::WalkDir::new(&local_dir_canonical)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if dir_entry.file_type().is_dir() {
            continue;
        }
        let full_path = dir_entry.path();
        let rel_path = full_path.strip_prefix(&local_dir_canonical)
            .map_err(|_| "Failed to compute relative path")?;

        // Normalize path separators to forward slash
        let rel_str = rel_path.to_string_lossy().replace('\\', "/");
        if rel_str.is_empty() {
            continue;
        }

        let metadata = std::fs::metadata(full_path)
            .map_err(|e| format!("Failed to read metadata for {}: {}", rel_str, e))?;

        let modified = metadata.modified()
            .map(|t| {
                let datetime: chrono::DateTime<chrono::Utc> = t.into();
                datetime.format("%Y-%m-%dT%H:%M:%SZ").to_string()
            })
            .unwrap_or_default();

        local_files.insert(rel_str, (metadata.len(), modified));
    }

    // Compare
    let mut vault_only = Vec::new();
    let mut local_only = Vec::new();
    let mut conflicts = Vec::new();
    let mut unchanged: usize = 0;

    for (name, (v_size, v_modified)) in &vault_files {
        match local_files.get(name) {
            Some((l_size, l_modified)) => {
                if v_size == l_size && v_modified == l_modified {
                    unchanged += 1;
                } else {
                    conflicts.push(VaultSyncConflict {
                        name: name.clone(),
                        vault_modified: v_modified.clone(),
                        local_modified: l_modified.clone(),
                        vault_size: *v_size,
                        local_size: *l_size,
                    });
                }
            }
            None => {
                vault_only.push(name.clone());
            }
        }
    }

    for name in local_files.keys() {
        if !vault_files.contains_key(name) {
            local_only.push(name.clone());
        }
    }

    // Sort for deterministic output
    vault_only.sort();
    local_only.sort();
    conflicts.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(VaultSyncComparison {
        vault_only,
        local_only,
        conflicts,
        unchanged,
    })
}

/// Action to apply during vault sync
#[derive(Deserialize)]
pub struct VaultSyncAction {
    pub name: String,
    pub action: String, // "to_vault" | "to_local" | "skip"
}

/// Result of applying vault sync actions
#[derive(Serialize)]
pub struct VaultSyncResult {
    pub to_vault: usize,
    pub to_local: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}

/// Apply sync decisions between vault and local directory
#[tauri::command]
pub async fn vault_v2_sync_apply(
    vault_path: String,
    password: String,
    local_dir: String,
    actions: Vec<VaultSyncAction>,
) -> Result<VaultSyncResult, String> {
    let local_dir_path = std::path::Path::new(&local_dir);
    if !local_dir_path.is_dir() {
        return Err(format!("Local directory does not exist: {}", local_dir));
    }
    let local_dir_canonical = local_dir_path.canonicalize()
        .map_err(|e| format!("Failed to resolve local directory: {}", e))?;

    let mut to_vault_count: usize = 0;
    let mut to_local_count: usize = 0;
    let mut skipped_count: usize = 0;
    let mut errors: Vec<String> = Vec::new();

    // Separate actions by type
    let mut to_vault_files: Vec<String> = Vec::new();
    let mut to_local_files: Vec<String> = Vec::new();

    for action in &actions {
        match action.action.as_str() {
            "to_vault" => to_vault_files.push(action.name.clone()),
            "to_local" => to_local_files.push(action.name.clone()),
            "skip" => skipped_count += 1,
            other => errors.push(format!("Unknown action '{}' for '{}'", other, action.name)),
        }
    }

    // Process to_vault: add local files to vault
    if !to_vault_files.is_empty() {
        // Build absolute paths for all to_vault files
        let mut abs_paths: Vec<String> = Vec::new();
        // Group files by their parent directory within the vault
        let mut root_files: Vec<String> = Vec::new();
        let mut dir_files: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();

        for name in &to_vault_files {
            let local_path = local_dir_canonical.join(name);
            if !local_path.exists() {
                errors.push(format!("Local file not found: {}", name));
                continue;
            }
            // Validate the path is within local_dir (prevent traversal)
            let canonical = local_path.canonicalize()
                .map_err(|e| format!("Failed to resolve path {}: {}", name, e))?;
            if !canonical.starts_with(&local_dir_canonical) {
                errors.push(format!("Path traversal detected: {}", name));
                continue;
            }

            let abs_str = canonical.to_string_lossy().to_string();

            // Check if the file goes into a subdirectory
            if let Some(parent) = std::path::Path::new(name).parent() {
                let parent_str = parent.to_string_lossy().replace('\\', "/");
                if parent_str.is_empty() {
                    root_files.push(abs_str);
                } else {
                    dir_files.entry(parent_str).or_default().push(abs_str);
                }
            } else {
                root_files.push(abs_str);
            }
        }

        // First, ensure all parent directories exist in the vault
        // We collect all unique parent dirs and create them
        for dir_path in dir_files.keys() {
            match vault_v2_create_directory(
                vault_path.clone(),
                password.clone(),
                dir_path.clone(),
            ).await {
                Ok(_) => {} // Created or already exists
                Err(e) => errors.push(format!("Failed to create vault dir '{}': {}", dir_path, e)),
            }
        }

        // Add root-level files
        if !root_files.is_empty() {
            match vault_v2_add_files(
                vault_path.clone(),
                password.clone(),
                root_files.clone(),
            ).await {
                Ok(result) => {
                    if let Some(added) = result.get("added").and_then(|v| v.as_u64()) {
                        to_vault_count += added as usize;
                    }
                }
                Err(e) => errors.push(format!("Failed to add root files to vault: {}", e)),
            }
        }

        // Add directory-grouped files
        for (dir, file_paths) in &dir_files {
            match vault_v2_add_files_to_dir(
                vault_path.clone(),
                password.clone(),
                file_paths.clone(),
                dir.clone(),
            ).await {
                Ok(result) => {
                    if let Some(added) = result.get("added").and_then(|v| v.as_u64()) {
                        to_vault_count += added as usize;
                    }
                }
                Err(e) => errors.push(format!("Failed to add files to vault dir '{}': {}", dir, e)),
            }
        }

        // If files were added, we also need to handle vault entries that already exist
        // (conflicts resolved as to_vault). The existing add_files skips duplicates,
        // so we need to first delete the existing entry, then re-add.
        // For simplicity and safety, we handle this by deleting then re-adding.
        // But vault_v2_add_files skips duplicates by encrypted_name comparison.
        // Since the filename hasn't changed, it will be skipped.
        // We need to delete existing entries first for conflicts going to_vault.
        // Let's handle this: for to_vault files that already exist in the vault,
        // delete them first, then re-add.
        // We already collected to_vault_files. Let's check which ones exist in vault.
        abs_paths.clear();

        // Re-open vault to check what exists
        let pwd_check = SecretString::from(password.clone());
        let check_file = File::open(&vault_path)
            .map_err(|e| format!("Failed to open vault for conflict check: {}", e))?;
        let mut check_reader = BufReader::new(check_file);

        let mut hdr_buf = [0u8; HEADER_SIZE];
        check_reader.read_exact(&mut hdr_buf)
            .map_err(|e| format!("Failed to read header: {}", e))?;
        let hdr = VaultHeader::from_bytes(&hdr_buf)?;

        let base_kek = derive_key(&pwd_check, &hdr.salt)?;
        let (mut km, mut kc) = derive_kek_pair(base_kek.expose_secret());
        let mk = unwrap_key(&km, &hdr.wrapped_master_key)?;
        km.zeroize();
        kc.zeroize();

        // Check which to_vault files already have vault entries (these are conflicts)
        let mut ml_buf = [0u8; 4];
        check_reader.read_exact(&mut ml_buf).ok();
        let ml = u32::from_le_bytes(ml_buf) as usize;
        let mut me = vec![0u8; ml];
        check_reader.read_exact(&mut me).ok();

        if let Ok(mj) = decrypt_filename(mk.expose_secret(), &String::from_utf8_lossy(&me)) {
            if let Ok(mf) = serde_json::from_str::<VaultManifest>(&mj) {
                let mut existing_names: Vec<String> = Vec::new();
                for entry in &mf.entries {
                    if !entry.is_dir {
                        if let Ok(n) = decrypt_filename(mk.expose_secret(), &entry.encrypted_name) {
                            existing_names.push(n);
                        }
                    }
                }

                // Delete existing entries that are being overwritten
                let mut to_delete: Vec<String> = Vec::new();
                for name in &to_vault_files {
                    if existing_names.contains(name) {
                        to_delete.push(name.clone());
                    }
                }

                if !to_delete.is_empty() {
                    match vault_v2_delete_entries(
                        vault_path.clone(),
                        password.clone(),
                        to_delete.clone(),
                        false,
                    ).await {
                        Ok(_) => {
                            // Now re-add these files
                            let mut re_root: Vec<String> = Vec::new();
                            let mut re_dir: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();

                            for name in &to_delete {
                                let local_path = local_dir_canonical.join(name);
                                if let Ok(canonical) = local_path.canonicalize() {
                                    let abs_str = canonical.to_string_lossy().to_string();
                                    if let Some(parent) = std::path::Path::new(name).parent() {
                                        let ps = parent.to_string_lossy().replace('\\', "/");
                                        if ps.is_empty() {
                                            re_root.push(abs_str);
                                        } else {
                                            re_dir.entry(ps).or_default().push(abs_str);
                                        }
                                    } else {
                                        re_root.push(abs_str);
                                    }
                                }
                            }

                            if !re_root.is_empty() {
                                if let Err(e) = vault_v2_add_files(vault_path.clone(), password.clone(), re_root).await {
                                    errors.push(format!("Failed to re-add conflict files: {}", e));
                                }
                            }
                            for (d, fps) in &re_dir {
                                if let Err(e) = vault_v2_add_files_to_dir(vault_path.clone(), password.clone(), fps.clone(), d.clone()).await {
                                    errors.push(format!("Failed to re-add conflict files to '{}': {}", d, e));
                                }
                            }
                        }
                        Err(e) => errors.push(format!("Failed to delete existing entries for overwrite: {}", e)),
                    }
                }
            }
        }
        me.zeroize();
    }

    // Process to_local: extract vault entries to local dir
    for name in &to_local_files {
        // Validate name has no path traversal
        if name.contains("..") || name.starts_with('/') || name.starts_with('\\') {
            errors.push(format!("Invalid file name blocked: {}", name));
            skipped_count += 1;
            continue;
        }

        let dest = local_dir_canonical.join(name);

        // Create parent directories if needed
        if let Some(parent) = dest.parent() {
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    errors.push(format!("Failed to create directory for '{}': {}", name, e));
                    continue;
                }
            }
        }

        match vault_v2_extract_entry(
            vault_path.clone(),
            password.clone(),
            name.clone(),
            dest.to_string_lossy().to_string(),
        ).await {
            Ok(_) => to_local_count += 1,
            Err(e) => errors.push(format!("Failed to extract '{}': {}", name, e)),
        }
    }

    Ok(VaultSyncResult {
        to_vault: to_vault_count,
        to_local: to_local_count,
        skipped: skipped_count,
        errors,
    })
}
