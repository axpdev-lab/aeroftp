//! Deterministic filename encryption for AeroCrypt overlays.
//!
//! Lifted (P1) from the CLI `crypt_overlay` module so the native overlay codec
//! and the future GUI share one implementation. AES-256-SIV (RFC 5297) is
//! deterministic on purpose: the same plaintext name always maps to the same
//! obfuscated name, which is what keeps an encrypted scope listable and
//! syncable object by object.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use aes_siv::aead::KeyInit;
use base64::Engine as _;
use hkdf::Hkdf;
use sha2::Sha256;

use super::KEY_SIZE;

/// HKDF salt + info that derive the 512-bit AES-SIV name key from the master
/// key. Stable strings: changing either re-maps every obfuscated name.
const NAME_HKDF_SALT: &[u8] = b"aeroftp-name-salt";
const NAME_HKDF_INFO: &[u8] = b"aeroftp-crypt-name-key";
/// AES-256-SIV needs a 512-bit key (two 256-bit halves).
const NAME_KEY_SIZE: usize = 64;

/// Derive the AES-SIV filename key from the overlay master key.
fn derive_name_key(master_key: &[u8; KEY_SIZE]) -> Result<[u8; NAME_KEY_SIZE], String> {
    let hk = Hkdf::<Sha256>::new(Some(NAME_HKDF_SALT), master_key);
    let mut name_key = [0u8; NAME_KEY_SIZE];
    hk.expand(NAME_HKDF_INFO, &mut name_key)
        .map_err(|_| "HKDF name-key expand failed".to_string())?;
    Ok(name_key)
}

/// Encrypt a plaintext filename to a deterministic base64url (no padding) token.
pub fn encrypt_filename(
    master_key: &[u8; KEY_SIZE],
    plaintext_name: &str,
) -> Result<String, String> {
    let name_key = derive_name_key(master_key)?;
    let mut cipher = aes_siv::siv::Aes256Siv::new((&name_key).into());
    let ciphertext = cipher
        .encrypt([&[] as &[u8]], plaintext_name.as_bytes())
        .map_err(|_| "AES-SIV filename encrypt failed".to_string())?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(ciphertext))
}

/// Decrypt a base64url filename token. Returns `None` if it is not a valid
/// AeroCrypt name (lets callers skip foreign entries like the config file).
pub fn decrypt_filename(master_key: &[u8; KEY_SIZE], encrypted_name: &str) -> Option<String> {
    let name_key = derive_name_key(master_key).ok()?;
    let mut cipher = aes_siv::siv::Aes256Siv::new((&name_key).into());
    let ciphertext = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encrypted_name)
        .ok()?;
    let plaintext = cipher.decrypt([&[] as &[u8]], &ciphertext).ok()?;
    String::from_utf8(plaintext).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_round_trip_is_deterministic() {
        let master = [5u8; KEY_SIZE];
        let a = encrypt_filename(&master, "report 2026.pdf").unwrap();
        let b = encrypt_filename(&master, "report 2026.pdf").unwrap();
        assert_eq!(a, b, "AES-SIV names must be deterministic");
        assert_ne!(a, "report 2026.pdf");
        assert_eq!(
            decrypt_filename(&master, &a).as_deref(),
            Some("report 2026.pdf")
        );
    }

    #[test]
    fn wrong_key_or_token_fails_closed() {
        let master = [5u8; KEY_SIZE];
        let token = encrypt_filename(&master, "secret.txt").unwrap();
        assert_eq!(decrypt_filename(&[6u8; KEY_SIZE], &token), None);
        assert_eq!(decrypt_filename(&master, "not-base64-$$$"), None);
        assert_eq!(decrypt_filename(&master, ".aeroftp-crypt.json"), None);
    }
}
