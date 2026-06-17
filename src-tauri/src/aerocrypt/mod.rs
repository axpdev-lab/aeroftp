//! Shared AeroCrypt codec primitives.
//!
//! The audited crypto core extracted (P0, pure refactor) from `aerovault_v3.rs`
//! so the AeroVault container and the native AeroCrypt overlay can share one
//! implementation instead of maintaining two parallel cipher stacks
//! (#276 "one codec, one audit pass"). This module owns only the
//! format-agnostic primitives; the per-format key schedule (HKDF labels, AAD
//! domains, header layout) stays with each consumer.
//!
//! - Content AEAD: AES-256-GCM-SIV (RFC 8452, nonce-misuse resistant).
//! - Key wrap: AES-256-KW (RFC 3394).
//! - KDF: Argon2id (RFC 9106) at the audited AeroVault profile.
//! - Subkey derivation: HKDF-SHA256.
//! - CSPRNG helper over `OsRng`.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use aes_gcm_siv::aead::{Aead, Payload};
use aes_gcm_siv::{Aes256GcmSiv, KeyInit, Nonce};
use aes_kw::Kek;
#[cfg(not(feature = "test-vectors"))]
use rand::RngCore;

pub mod names;
pub mod overlay;

/// AES-256 key length, in bytes.
pub const KEY_SIZE: usize = 32;
/// Argon2id salt length, in bytes.
pub const SALT_SIZE: usize = 32;
/// AES-GCM-SIV nonce length, in bytes.
pub const NONCE_SIZE: usize = 12;
/// Length of an AES-KW-wrapped 256-bit key (key + 8-byte integrity check).
pub const WRAPPED_KEY_SIZE: usize = 40;

/// The audited AeroVault Argon2id profile: 128 MiB / t=4 / p=4
/// (RFC 9106, exceeds OWASP 2024). Tuned once, shared by every consumer.
const ARGON2_MEM_KIB: u32 = 128 * 1024;
const ARGON2_TIME: u32 = 4;
const ARGON2_LANES: u32 = 4;

/// Argon2id memory cost (KiB) of the shared profile. Exposed so consumers can
/// bind the KDF parameters into authenticated metadata (e.g. the overlay config
/// MAC) without re-hardcoding them.
pub fn argon2_mem_kib() -> u32 {
    ARGON2_MEM_KIB
}
/// Argon2id time cost (passes) of the shared profile.
pub fn argon2_time() -> u32 {
    ARGON2_TIME
}
/// Argon2id parallelism (lanes) of the shared profile.
pub fn argon2_lanes() -> u32 {
    ARGON2_LANES
}

/// Fill an `N`-byte array from the OS CSPRNG.
#[cfg(not(feature = "test-vectors"))]
pub fn random_array<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    rand::rngs::OsRng.fill_bytes(&mut out);
    out
}

// --- Deterministic test-vector mode (feature `test-vectors`) -----------------
//
// Mirror of the `aerovault` crate's deterministic generator. The seed string
// and per-call fill rule MUST stay byte-identical to the crate's copy
// (aerovault/src/aerocrypt.rs) or the AEROVAULT3 cross-impl golden diverges.
// Never enabled in a production build.
#[cfg(feature = "test-vectors")]
const TEST_VECTOR_SEED: &[u8] = b"AEROVAULT3 test-vectors v1";

#[cfg(feature = "test-vectors")]
thread_local! {
    static TEST_VECTOR_COUNTER: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Rewind the deterministic test-vector stream to its start. Only present under
/// the `test-vectors` feature.
#[cfg(feature = "test-vectors")]
pub fn reset_test_vectors() {
    TEST_VECTOR_COUNTER.with(|c| c.set(0));
}

#[cfg(feature = "test-vectors")]
pub fn random_array<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    let mut off = 0usize;
    while off < N {
        let ctr = TEST_VECTOR_COUNTER.with(|c| {
            let v = c.get();
            c.set(v + 1);
            v
        });
        let mut input = TEST_VECTOR_SEED.to_vec();
        input.extend_from_slice(&ctr.to_le_bytes());
        let block = blake3::hash(&input);
        let take = core::cmp::min(32, N - off);
        out[off..off + take].copy_from_slice(&block.as_bytes()[..take]);
        off += take;
    }
    out
}

/// Derive the base key-encryption key from a password + salt via Argon2id.
pub fn derive_base_kek(password: &str, salt: &[u8; SALT_SIZE]) -> Result<[u8; KEY_SIZE], String> {
    let params = argon2::Params::new(ARGON2_MEM_KIB, ARGON2_TIME, ARGON2_LANES, Some(KEY_SIZE))
        .map_err(|e| format!("Argon2 params: {e}"))?;
    let argon2 = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut key = [0u8; KEY_SIZE];
    argon2
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| format!("Argon2 derive: {e}"))?;
    Ok(key)
}

/// HKDF-SHA256 expand of `ikm` under a domain-separating `label`.
pub fn hkdf_expand<const N: usize>(ikm: &[u8], label: &[u8]) -> Result<[u8; N], String> {
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(None, ikm);
    let mut out = [0u8; N];
    hk.expand(label, &mut out)
        .map_err(|_| "HKDF expand failed".to_string())?;
    Ok(out)
}

/// Wrap a 256-bit key under `kek` (AES-256-KW).
pub fn wrap_key(
    kek: &[u8; KEY_SIZE],
    key: &[u8; KEY_SIZE],
) -> Result<[u8; WRAPPED_KEY_SIZE], String> {
    let kek = Kek::from(*kek);
    let mut out = [0u8; WRAPPED_KEY_SIZE];
    kek.wrap(key, &mut out)
        .map_err(|_| "AES-KW wrap failed".to_string())?;
    Ok(out)
}

/// Unwrap a 256-bit key wrapped under `kek` (AES-256-KW).
pub fn unwrap_key(
    kek: &[u8; KEY_SIZE],
    wrapped: &[u8; WRAPPED_KEY_SIZE],
) -> Result<[u8; KEY_SIZE], String> {
    let kek = Kek::from(*kek);
    let mut out = [0u8; KEY_SIZE];
    kek.unwrap(wrapped, &mut out)
        .map_err(|_| "AES-KW unwrap failed".to_string())?;
    Ok(out)
}

/// Encrypt `plaintext` with AES-256-GCM-SIV under `key`, binding `aad`.
/// The fresh random nonce is prefixed to the returned ciphertext.
pub fn encrypt_with_aad(
    key: &[u8; KEY_SIZE],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, String> {
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

/// Decrypt a nonce-prefixed AES-256-GCM-SIV payload under `key`, binding `aad`.
pub fn decrypt_with_aad(
    key: &[u8; KEY_SIZE],
    encrypted: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gcm_siv_round_trip_binds_aad() {
        let key = [7u8; KEY_SIZE];
        let msg = b"AeroCrypt shared codec round trip";
        let ct = encrypt_with_aad(&key, msg, b"domain-a").unwrap();
        assert_eq!(decrypt_with_aad(&key, &ct, b"domain-a").unwrap(), msg);
        // Wrong AAD must fail closed.
        assert!(decrypt_with_aad(&key, &ct, b"domain-b").is_err());
        // Wrong key must fail closed.
        assert!(decrypt_with_aad(&[8u8; KEY_SIZE], &ct, b"domain-a").is_err());
    }

    #[test]
    fn gcm_siv_rejects_short_payload() {
        assert!(decrypt_with_aad(&[0u8; KEY_SIZE], &[0u8; 4], b"").is_err());
    }

    #[test]
    fn aes_kw_wrap_unwrap_round_trip() {
        let kek = [3u8; KEY_SIZE];
        let key = [9u8; KEY_SIZE];
        let wrapped = wrap_key(&kek, &key).unwrap();
        assert_eq!(wrapped.len(), WRAPPED_KEY_SIZE);
        assert_eq!(unwrap_key(&kek, &wrapped).unwrap(), key);
        // A different KEK must not unwrap.
        assert!(unwrap_key(&[4u8; KEY_SIZE], &wrapped).is_err());
    }

    #[test]
    fn argon2_and_hkdf_are_deterministic() {
        let salt = [1u8; SALT_SIZE];
        let a = derive_base_kek("correct horse battery staple", &salt).unwrap();
        let b = derive_base_kek("correct horse battery staple", &salt).unwrap();
        assert_eq!(a, b);
        let c = derive_base_kek("different password", &salt).unwrap();
        assert_ne!(a, c);

        let k1 = hkdf_expand::<KEY_SIZE>(&a, b"label-1").unwrap();
        let k2 = hkdf_expand::<KEY_SIZE>(&a, b"label-1").unwrap();
        let k3 = hkdf_expand::<KEY_SIZE>(&a, b"label-2").unwrap();
        assert_eq!(k1, k2);
        assert_ne!(k1, k3);
    }
}
