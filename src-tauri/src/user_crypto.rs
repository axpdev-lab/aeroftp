//! Per-user cryptographic primitives for local profile partitions.
//!
//! This module mirrors the application's existing keystore posture:
//! Argon2id for human passphrases, AES-KW for O(1) DEK wrapping,
//! AES-256-GCM for payload encryption, HMAC-SHA256 for keyed metadata tags,
//! and zeroize-on-drop secret key storage.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use aes::cipher::generic_array::GenericArray;
use aes_kw::KekAes256;
use argon2::Argon2;
use hmac::{Hmac, Mac};
use rand::rngs::OsRng;
use rand::RngCore;
use secrecy::zeroize::{Zeroize, Zeroizing};
use secrecy::{ExposeSecret, SecretBox};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;

pub type SecretKey = SecretBox<[u8; 32]>;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Argon2Params {
    pub memory_cost_kib: u32,
    pub time_cost: u32,
    pub parallelism: u32,
}

impl Default for Argon2Params {
    fn default() -> Self {
        Self {
            memory_cost_kib: 131_072,
            time_cost: 4,
            parallelism: 4,
        }
    }
}

impl Argon2Params {
    #[cfg(test)]
    pub fn fast_for_tests() -> Self {
        Self {
            memory_cost_kib: 1_024,
            time_cost: 1,
            parallelism: 1,
        }
    }
}

pub fn params_to_json(params: &Argon2Params) -> Result<String, String> {
    serde_json::to_string(params).map_err(|e| format!("Serialize Argon2 params: {e}"))
}

pub fn params_from_json(json: &str) -> Result<Argon2Params, String> {
    serde_json::from_str(json).map_err(|e| format!("Parse Argon2 params: {e}"))
}

pub fn generate_dek() -> SecretKey {
    SecretBox::<[u8; 32]>::init_with_mut(|key| OsRng.fill_bytes(key))
}

pub fn secret_key_from_bytes(bytes: &[u8; 32]) -> SecretKey {
    SecretBox::init_with(|| *bytes)
}

pub fn random_salt() -> [u8; 16] {
    let mut salt = [0u8; 16];
    OsRng.fill_bytes(&mut salt);
    salt
}

pub fn derive_wrapping_key(
    passphrase: &str,
    salt: &[u8; 16],
    params: &Argon2Params,
) -> Result<SecretKey, String> {
    let argon_params = argon2::Params::new(
        params.memory_cost_kib,
        params.time_cost,
        params.parallelism,
        Some(32),
    )
    .map_err(|e| format!("Argon2 params: {e}"))?;
    let argon2 = Argon2::new(
        argon2::Algorithm::Argon2id,
        argon2::Version::V0x13,
        argon_params,
    );

    let mut key = [0u8; 32];
    if let Err(e) = argon2.hash_password_into(passphrase.as_bytes(), salt, &mut key) {
        key.zeroize();
        return Err(format!("Argon2 derive: {e}"));
    }
    let secret = SecretBox::init_with(|| key);
    key.zeroize();
    Ok(secret)
}

pub fn wrap_dek(wrapping_key: &SecretKey, dek: &SecretKey) -> Result<Vec<u8>, String> {
    let kek = KekAes256::new(GenericArray::from_slice(wrapping_key.expose_secret()));
    kek.wrap_vec(dek.expose_secret())
        .map_err(|e| format!("Wrap user data key: {e}"))
}

pub fn unwrap_dek(wrapping_key: &SecretKey, wrapped_dek: &[u8]) -> Result<SecretKey, String> {
    let kek = KekAes256::new(GenericArray::from_slice(wrapping_key.expose_secret()));
    let mut unwrapped = kek
        .unwrap_vec(wrapped_dek)
        .map_err(|e| format!("Unwrap user data key: {e}"))?;
    if unwrapped.len() != 32 {
        unwrapped.zeroize();
        return Err("INVALID_DEK_SIZE".to_string());
    }
    let mut dek = [0u8; 32];
    dek.copy_from_slice(&unwrapped);
    unwrapped.zeroize();
    let secret = SecretBox::init_with(|| dek);
    dek.zeroize();
    Ok(secret)
}

pub fn compute_dek_verifier(dek: &SecretKey) -> Result<[u8; 16], String> {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(dek.expose_secret())
        .map_err(|e| format!("Create DEK verifier: {e}"))?;
    mac.update(b"aeroftp.user.verify");
    let tag = mac.finalize().into_bytes();
    let mut verifier = [0u8; 16];
    verifier.copy_from_slice(&tag[..16]);
    Ok(verifier)
}

pub fn verify_dek(dek: &SecretKey, expected: &[u8]) -> Result<bool, String> {
    if expected.len() != 16 {
        return Ok(false);
    }
    let actual = compute_dek_verifier(dek)?;
    Ok(actual.ct_eq(expected).into())
}

pub fn metadata_tag(key: &SecretKey, label: &[u8], value: &str) -> Result<String, String> {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key.expose_secret())
        .map_err(|e| format!("Create metadata tag: {e}"))?;
    mac.update(b"aeroftp.user_partitions.metadata.v1");
    mac.update(label);
    mac.update(&[0]);
    mac.update(value.as_bytes());
    Ok(format!(
        "hmac-sha256:{}",
        hex::encode(mac.finalize().into_bytes())
    ))
}

pub fn encrypt_blob(dek: &SecretKey, plaintext: &[u8]) -> Result<(Vec<u8>, [u8; 12]), String> {
    let mut nonce_vec = crate::crypto::random_bytes(12);
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&nonce_vec);
    nonce_vec.zeroize();
    let encrypted = crate::crypto::encrypt_aes_gcm(dek.expose_secret(), &nonce, plaintext)?;
    Ok((encrypted, nonce))
}

pub fn decrypt_blob(
    dek: &SecretKey,
    nonce: &[u8],
    encrypted: &[u8],
) -> Result<Zeroizing<Vec<u8>>, String> {
    if nonce.len() != 12 {
        return Err("INVALID_NONCE_SIZE".to_string());
    }
    crate::crypto::decrypt_aes_gcm(dek.expose_secret(), nonce, encrypted).map(Zeroizing::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dek_wrap_unwrap_and_verifier_round_trip() {
        let wrapping_key = generate_dek();
        let dek = generate_dek();
        let verifier = compute_dek_verifier(&dek).expect("verifier");
        assert!(verify_dek(&dek, &verifier).expect("verify"));

        let wrapped = wrap_dek(&wrapping_key, &dek).expect("wrap");
        assert_eq!(wrapped.len(), 40);
        let unwrapped = unwrap_dek(&wrapping_key, &wrapped).expect("unwrap");
        assert!(verify_dek(&unwrapped, &verifier).expect("verify unwrapped"));
    }

    #[test]
    fn argon2_passphrase_key_unlocks_wrapped_dek() {
        let params = Argon2Params::fast_for_tests();
        let salt = random_salt();
        let wrapping_key =
            derive_wrapping_key("correct horse battery staple", &salt, &params).expect("derive");
        let dek = generate_dek();
        let verifier = compute_dek_verifier(&dek).expect("verifier");
        let wrapped = wrap_dek(&wrapping_key, &dek).expect("wrap");

        let same_key =
            derive_wrapping_key("correct horse battery staple", &salt, &params).expect("derive");
        let unwrapped = unwrap_dek(&same_key, &wrapped).expect("unwrap");
        assert!(verify_dek(&unwrapped, &verifier).expect("verify"));

        let wrong_key = derive_wrapping_key("wrong", &salt, &params).expect("derive wrong");
        assert!(unwrap_dek(&wrong_key, &wrapped).is_err());
    }

    #[test]
    fn encrypt_decrypt_blob_round_trip() {
        let dek = generate_dek();
        let plaintext = br#"{"name":"Production","host":"example.com"}"#;
        let (encrypted, nonce) = encrypt_blob(&dek, plaintext).expect("encrypt");
        assert!(!encrypted
            .windows(b"Production".len())
            .any(|w| w == b"Production"));
        let decrypted = decrypt_blob(&dek, &nonce, &encrypted).expect("decrypt");
        assert_eq!(&*decrypted, plaintext);
    }

    #[test]
    fn metadata_tag_is_keyed_and_deterministic() {
        let key = generate_dek();
        let tag1 = metadata_tag(&key, b"dedup-key", "sftp:example.com:user").expect("tag1");
        let tag2 = metadata_tag(&key, b"dedup-key", "sftp:example.com:user").expect("tag2");
        assert_eq!(tag1, tag2);
        assert!(tag1.starts_with("hmac-sha256:"));
        assert!(!tag1.contains("example.com"));
    }
}
