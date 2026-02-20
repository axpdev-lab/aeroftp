// AeroFTP Shared Cryptographic Primitives
// Argon2id key derivation + AES-256-GCM authenticated encryption
// Shared by credential_store and profile_export modules
//
// Reviewed: 2026-02-02 — Claude Opus 4.5 audit — no issues found

use aes_gcm::{Aes256Gcm, KeyInit, aead::Aead};
use aes_gcm::aead::generic_array::GenericArray;
use argon2::Argon2;

pub const ARGON2_MEM_COST: u32 = 65536; // 64MB
pub const ARGON2_TIME_COST: u32 = 3;
pub const ARGON2_PARALLELISM: u32 = 4;

/// Derive a 256-bit key from password + salt using Argon2id
pub fn derive_key(password: &str, salt: &[u8]) -> Result<[u8; 32], String> {
    let params = argon2::Params::new(
        ARGON2_MEM_COST,
        ARGON2_TIME_COST,
        ARGON2_PARALLELISM,
        Some(32),
    ).map_err(|e| format!("Argon2 params: {}", e))?;

    let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);

    let mut key = [0u8; 32];
    argon2.hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| format!("Argon2 derive: {}", e))?;
    Ok(key)
}

/// Encrypt plaintext using AES-256-GCM
pub fn encrypt_aes_gcm(key: &[u8; 32], nonce: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let cipher = Aes256Gcm::new(GenericArray::from_slice(key));
    let nonce = GenericArray::from_slice(nonce);
    cipher.encrypt(nonce, plaintext)
        .map_err(|e| format!("AES-GCM encrypt: {}", e))
}

/// Decrypt ciphertext using AES-256-GCM
pub fn decrypt_aes_gcm(key: &[u8; 32], nonce: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    let cipher = Aes256Gcm::new(GenericArray::from_slice(key));
    let nonce = GenericArray::from_slice(nonce);
    cipher.decrypt(nonce, ciphertext)
        .map_err(|e| format!("AES-GCM decrypt: {}", e))
}

/// Derive vault key from high-entropy passphrase (64 bytes) using HKDF-SHA256
/// Used by Universal Vault — passphrase has 512 bits of entropy so HKDF is sufficient
pub fn derive_from_passphrase(passphrase: &[u8]) -> [u8; 32] {
    use hkdf::Hkdf;
    use sha2::Sha256;
    let hk = Hkdf::<Sha256>::new(None, passphrase);
    let mut key = [0u8; 32];
    hk.expand(b"aeroftp-vault-v2", &mut key).expect("HKDF expand");
    key
}

/// Derive master key from user password using Argon2id with strong parameters (128 MiB)
/// Used for master password mode where human password has low entropy
pub fn derive_key_strong(password: &str, salt: &[u8]) -> Result<[u8; 32], String> {
    let params = argon2::Params::new(
        131072, // 128 MiB
        4,      // t=4
        4,      // p=4
        Some(32),
    ).map_err(|e| format!("Argon2 params: {}", e))?;

    let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut key = [0u8; 32];
    argon2.hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| format!("Argon2 derive: {}", e))?;
    Ok(key)
}

/// Generate cryptographically secure random bytes using OS entropy
pub fn random_bytes(len: usize) -> Vec<u8> {
    use rand::rngs::OsRng;
    use rand::RngCore;
    let mut buf = vec![0u8; len];
    OsRng.fill_bytes(&mut buf);
    buf
}
