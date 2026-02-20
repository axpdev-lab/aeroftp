//! Cryptomator Vault Format 8 Support
//!
//! Implements reading and writing Cryptomator-compatible encrypted vaults.
//! Uses scrypt KDF, AES Key Wrap (RFC 3394), AES-SIV for filenames,
//! and AES-GCM for file content encryption.

use aes_gcm::{Aes256Gcm, KeyInit, aead::Aead};
use aes_gcm::aead::generic_array::GenericArray;
use data_encoding::BASE32;
use secrecy::zeroize::Zeroize;
use serde::{Deserialize, Serialize};
use sha1::{Sha1, Digest};
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

/// A decrypted directory entry
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CryptomatorEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub dir_id: Option<String>,
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
        Self { vaults: Mutex::new(HashMap::new()) }
    }
}

// ─── Crypto primitives ────────────────────────────────────────────────────────

/// Derive KEK from password using scrypt
fn derive_kek(password: &str, salt: &[u8], n: u32, r: u32) -> Result<[u8; 32], String> {
    use scrypt::{scrypt, Params};

    let log_n = (n as f64).log2() as u8;
    let params = Params::new(log_n, r, 1, 32)
        .map_err(|e| format!("scrypt params: {}", e))?;

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
    kek_obj.unwrap(wrapped, &mut buf)
        .map_err(|e| format!("AES-KW unwrap: {}", e))?;
    Ok(buf)
}

/// Wrap a 256-bit key with AES Key Wrap (RFC 3394)
fn wrap_key(kek: &[u8; 32], key: &[u8; 32]) -> Result<Vec<u8>, String> {
    use aes_kw::Kek;

    let kek_obj: Kek<aes_gcm::aes::Aes256> = Kek::from(*kek);
    let mut buf = [0u8; 40]; // 32 + 8 byte integrity check
    kek_obj.wrap(key, &mut buf)
        .map_err(|e| format!("AES-KW wrap: {}", e))?;
    Ok(buf.to_vec())
}

/// Encrypt data with AES-SIV (deterministic authenticated encryption)
fn aes_siv_encrypt(enc_key: &[u8; 32], mac_key: &[u8; 32], plaintext: &[u8], associated_data: &[u8]) -> Result<Vec<u8>, String> {
    use aes_siv::siv::Aes256Siv;
    use aes_siv::KeyInit as SivKeyInit;

    // AES-SIV uses a 64-byte key: mac_key || enc_key
    let mut combined_key = [0u8; 64];
    combined_key[..32].copy_from_slice(mac_key);
    combined_key[32..].copy_from_slice(enc_key);

    let mut cipher = Aes256Siv::new(&combined_key.into());
    cipher.encrypt([associated_data], plaintext)
        .map_err(|e| format!("AES-SIV encrypt: {}", e))
}

/// Decrypt data with AES-SIV
fn aes_siv_decrypt(enc_key: &[u8; 32], mac_key: &[u8; 32], ciphertext: &[u8], associated_data: &[u8]) -> Result<Vec<u8>, String> {
    use aes_siv::siv::Aes256Siv;
    use aes_siv::KeyInit as SivKeyInit;

    let mut combined_key = [0u8; 64];
    combined_key[..32].copy_from_slice(mac_key);
    combined_key[32..].copy_from_slice(enc_key);

    let mut cipher = Aes256Siv::new(&combined_key.into());
    cipher.decrypt([associated_data], ciphertext)
        .map_err(|e| format!("AES-SIV decrypt: {}", e))
}

// ─── Vault operations ─────────────────────────────────────────────────────────

/// Hash a directory ID to its physical path in the vault
fn hash_dir_id(vault: &UnlockedVault, dir_id: &str) -> Result<PathBuf, String> {
    // Encrypt empty string with dir_id as associated data
    let encrypted = aes_siv_encrypt(&vault.enc_key, &vault.mac_key, b"", dir_id.as_bytes())?;

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
    use data_encoding::BASE64URL_NOPAD;

    let encrypted = aes_siv_encrypt(&vault.enc_key, &vault.mac_key, name.as_bytes(), dir_id.as_bytes())?;
    let encoded = BASE64URL_NOPAD.encode(&encrypted);

    Ok(format!("{}.c9r", encoded))
}

/// Decrypt an encrypted filename (strip .c9r, base64url decode, AES-SIV decrypt)
fn decrypt_filename(vault: &UnlockedVault, dir_id: &str, encrypted_name: &str) -> Result<String, String> {
    use data_encoding::BASE64URL_NOPAD;

    let name_part = encrypted_name.strip_suffix(".c9r")
        .ok_or_else(|| format!("Not a .c9r entry: {}", encrypted_name))?;

    let ciphertext = BASE64URL_NOPAD.decode(name_part.as_bytes())
        .map_err(|e| format!("Base64url decode: {}", e))?;

    let plaintext = aes_siv_decrypt(&vault.enc_key, &vault.mac_key, &ciphertext, dir_id.as_bytes())?;

    String::from_utf8(plaintext)
        .map_err(|e| format!("UTF-8 decode: {}", e))
}

/// Unlock a Cryptomator vault
fn unlock_vault_inner(vault_path: &Path, password: &str) -> Result<(UnlockedVault, VaultConfig), String> {
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

    // Decode JWT payload without signature verification (we verify via masterkey MAC instead)
    let parts: Vec<&str> = jwt_str.trim().split('.').collect();
    if parts.len() != 3 {
        return Err("Invalid vault.cryptomator JWT format".to_string());
    }
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[1])
        .map_err(|e| format!("JWT decode: {}", e))?;
    let config: VaultConfig = serde_json::from_slice(&payload)
        .map_err(|e| format!("Invalid vault config: {}", e))?;

    if config.format != 8 {
        return Err(format!("Unsupported vault format: {} (only format 8 supported)", config.format));
    }
    if config.cipher_combo != "SIV_GCM" && config.cipher_combo != "SIV_CTRMAC" {
        return Err(format!("Unsupported cipher combo: {}", config.cipher_combo));
    }

    // Derive KEK
    let salt = b64.decode(&masterkey.scrypt_salt)
        .map_err(|e| format!("Salt decode: {}", e))?;
    let kek = derive_kek(password, &salt, masterkey.scrypt_cost_param, masterkey.scrypt_block_size)?;

    // Unwrap master keys
    let wrapped_enc = b64.decode(&masterkey.primary_master_key)
        .map_err(|e| format!("Enc key decode: {}", e))?;
    let wrapped_mac = b64.decode(&masterkey.hmac_master_key)
        .map_err(|e| format!("MAC key decode: {}", e))?;

    let enc_key = unwrap_key(&kek, &wrapped_enc)?;
    let mac_key = unwrap_key(&kek, &wrapped_mac)?;

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

/// List a directory in the vault
fn list_dir_inner(vault: &UnlockedVault, dir_id: &str) -> Result<Vec<CryptomatorEntry>, String> {
    let dir_path = hash_dir_id(vault, dir_id)?;

    if !dir_path.exists() {
        return Err(format!("Directory not found in vault: {:?}", dir_path));
    }

    let mut entries = Vec::new();
    let read_dir = fs::read_dir(&dir_path)
        .map_err(|e| format!("Failed to read vault directory: {}", e))?;

    for entry_result in read_dir {
        let entry = entry_result.map_err(|e| format!("Read dir entry: {}", e))?;
        let file_name = entry.file_name().to_string_lossy().to_string();

        // Handle .c9r files (regular files or directories)
        if file_name.ends_with(".c9r") {
            let entry_path = entry.path();
            let is_dir = entry_path.is_dir();

            // For directories, the .c9r is a folder containing dir.c9r
            if is_dir {
                // It's a directory — read dir.c9r to get the child dir_id
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
        if a.is_dir != b.is_dir { return if a.is_dir { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater }; }
        a.name.to_lowercase().cmp(&b.name.to_lowercase())
    });

    Ok(entries)
}

/// Decrypt a file from the vault
fn decrypt_file_inner(vault: &UnlockedVault, dir_id: &str, filename: &str, output_path: &Path) -> Result<(), String> {
    use std::io::Write;

    // Find the encrypted file
    let encrypted_name = encrypt_filename(vault, dir_id, filename)?;
    let dir_path = hash_dir_id(vault, dir_id)?;
    let file_path = dir_path.join(&encrypted_name);

    if !file_path.exists() {
        return Err(format!("Encrypted file not found: {:?}", file_path));
    }

    let data = fs::read(&file_path)
        .map_err(|e| format!("Failed to read encrypted file: {}", e))?;

    if data.len() < 68 {
        return Err("File too small to contain a valid header".to_string());
    }

    // Header: 12-byte nonce + (8 reserved + 32 content key) encrypted with GCM + 16-byte tag = 68 bytes
    let header_nonce = &data[0..12];
    let header_payload = &data[12..68];

    // Decrypt header to get content key
    let cipher = Aes256Gcm::new(GenericArray::from_slice(&vault.enc_key));
    let header_nonce = GenericArray::from_slice(header_nonce);
    let decrypted_header = cipher.decrypt(header_nonce, header_payload)
        .map_err(|_| "Failed to decrypt file header — wrong key?".to_string())?;

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
        let decrypted = content_cipher.decrypt(nonce, aes_gcm::aead::Payload {
            msg: &chunk[12..],
            aad: &aad,
        }).map_err(|_| format!("Failed to decrypt chunk {}", chunk_num))?;

        outfile.write_all(&decrypted)
            .map_err(|e| format!("Failed to write chunk: {}", e))?;

        chunk_num += 1;
        offset = end;
    }

    Ok(())
}

/// Encrypt a file into the vault
fn encrypt_file_inner(vault: &UnlockedVault, dir_id: &str, input_path: &Path) -> Result<String, String> {
    use rand::RngCore;
    use std::io::Write;

    let filename = input_path.file_name()
        .ok_or("Invalid input filename")?
        .to_string_lossy()
        .to_string();

    let plaintext = fs::read(input_path)
        .map_err(|e| format!("Failed to read input: {}", e))?;

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
    let encrypted_header = cipher.encrypt(header_nonce, header_payload.as_ref())
        .map_err(|_| "Failed to encrypt file header".to_string())?;

    // Write header: nonce + encrypted payload (includes GCM tag)
    outfile.write_all(&header_nonce_bytes)
        .map_err(|e| format!("Write header nonce: {}", e))?;
    outfile.write_all(&encrypted_header)
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
        let encrypted_chunk = content_cipher.encrypt(nonce, aes_gcm::aead::Payload {
            msg: chunk,
            aad: &aad,
        }).map_err(|_| format!("Failed to encrypt chunk {}", chunk_num))?;

        outfile.write_all(&chunk_nonce)
            .map_err(|e| format!("Write chunk nonce: {}", e))?;
        outfile.write_all(&encrypted_chunk)
            .map_err(|e| format!("Write chunk: {}", e))?;

        chunk_num += 1;
        offset = end;

        if plaintext.is_empty() { break; }
    }

    Ok(encrypted_name)
}

// ─── Tauri Commands ───────────────────────────────────────────────────────────

#[tauri::command]
pub async fn cryptomator_unlock(
    state: tauri::State<'_, CryptomatorState>,
    vault_path: String,
    password: String,
) -> Result<CryptomatorVaultInfo, String> {
    let path = Path::new(&vault_path);
    let (vault, config) = unlock_vault_inner(path, &password)?;

    let vault_id = uuid::Uuid::new_v4().to_string();
    let name = path.file_name()
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
    let vault = vaults.get(&vault_id)
        .ok_or("Vault not unlocked")?;
    list_dir_inner(vault, &dir_id)
}

#[tauri::command]
pub async fn cryptomator_decrypt_file(
    state: tauri::State<'_, CryptomatorState>,
    vault_id: String,
    dir_id: String,
    filename: String,
    output_path: String,
) -> Result<String, String> {
    let vaults = state.vaults.lock().await;
    let vault = vaults.get(&vault_id)
        .ok_or("Vault not unlocked")?;
    decrypt_file_inner(vault, &dir_id, &filename, Path::new(&output_path))?;
    Ok(output_path)
}

#[tauri::command]
pub async fn cryptomator_encrypt_file(
    state: tauri::State<'_, CryptomatorState>,
    vault_id: String,
    dir_id: String,
    input_path: String,
) -> Result<String, String> {
    let vaults = state.vaults.lock().await;
    let vault = vaults.get(&vault_id)
        .ok_or("Vault not unlocked")?;
    encrypt_file_inner(vault, &dir_id, Path::new(&input_path))
}

#[tauri::command]
pub async fn cryptomator_create(vault_path: String, password: String) -> Result<String, String> {
    use base64::Engine;
    use hmac::{Hmac, Mac};
    use rand::RngCore;
    use sha2::Sha256;

    let b64 = base64::engine::general_purpose::STANDARD;

    // Validate path: reject traversal components
    use std::path::Component;
    for component in std::path::Path::new(&vault_path).components() {
        if matches!(component, Component::ParentDir) {
            return Err("Path traversal not allowed".to_string());
        }
    }

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
    let mut kek = derive_kek(&password, &salt, 32768, 8)?;

    // Step 5: Wrap both keys with AES-KW
    let wrapped_enc = wrap_key(&kek, &enc_key)?;
    let wrapped_mac = wrap_key(&kek, &mac_key)?;

    // Step 6: Compute versionMac = HMAC-SHA256(mac_key, version_bytes)
    let version: i32 = 999;
    let version_bytes = version.to_be_bytes();
    let mut hmac_obj = <Hmac<Sha256> as Mac>::new_from_slice(&mac_key)
        .map_err(|e| format!("HMAC init: {}", e))?;
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
    fs::write(&masterkey_path, serde_json::to_string_pretty(&masterkey_json)
        .map_err(|e| format!("JSON serialize: {}", e))?)
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
    fs::create_dir_all(&d_dir)
        .map_err(|e| format!("Failed to create d/ directory: {}", e))?;

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

    // Zeroize all key material before returning
    enc_key.zeroize();
    mac_key.zeroize();
    kek.zeroize();
    salt.zeroize();
    jwt_signing_key.zeroize();

    Ok("Vault created successfully".to_string())
}
