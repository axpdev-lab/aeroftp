//! L0 encryption helpers (stand-in for vault encryption).
//!
//! For the spike we use a user-provided "pairing secret" (copied from listen to dial,
//! or encoded in a future QR) to derive a session key.
//!
//! This simulates the E2EE that will come from the vault + per-user DEK once
//! MU-VAULT lands. The real implementation will use the proper user_crypto
//! primitives and long-term peer identity keys.
//!
//! We keep it simple and auditable for the gate:
//! - HKDF (SHA-256) from the pairing secret + context (NodeIDs + "aeroftp-peer-l0").
//! - AES-256-GCM for the payload (standard, well-reviewed).
//! - 12-byte nonce per blob (random).

use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Nonce,
};
use anyhow::{bail, Context, Result};
use hkdf::Hkdf;
use rand::RngCore;
use sha2::Sha256;

/// Size of the pairing secret we recommend users copy (16 bytes = 128 bit).
/// Can be shown as base64 for copy-paste or QR.
pub const PAIRING_SECRET_LEN: usize = 16;

/// Derive a 32-byte AES key from a pairing secret + context.
/// Context should include both NodeIDs and a version string to bind the key.
pub fn derive_session_key(secret: &[u8], local_node: &str, remote_node: &str) -> [u8; 32] {
    // L0 fix (Linux 2): the session key MUST be identical on both peers, but each
    // side calls this with its OWN NodeId as `local` and the peer's as `remote`
    // (i.e. the two arguments are swapped between dial and listen). Binding them in
    // call order produced two different keys and AES-GCM decryption failed on the
    // receiver. Order the pair canonically (sorted) so both sides derive the same
    // key regardless of who dialed.
    let (first, second) = if local_node <= remote_node {
        (local_node, remote_node)
    } else {
        (remote_node, local_node)
    };
    let info = format!("aeroftp-peer-l0-v1|{}|{}", first, second);
    let hk = Hkdf::<Sha256>::new(None, secret);
    let mut key = [0u8; 32];
    // We ignore the error in practice because 32 bytes is always ok for HKDF-SHA256.
    let _ = hk.expand(info.as_bytes(), &mut key);
    key
}

/// L1 drive key: HKDF over the pairing secret, domain-separated by the drive NamespaceId.
/// Both publish and replicate know the NamespaceId (publish creates it; replicate gets it from the
/// ticket via doc.id()), so both derive the SAME 32-byte key.
pub fn derive_drive_key(secret: &[u8], namespace_id: &str) -> [u8; 32] {
    let info = format!("aeroftp-peer-l1-drive|{}", namespace_id);
    let hk = Hkdf::<Sha256>::new(None, secret);
    let mut key = [0u8; 32];
    let _ = hk.expand(info.as_bytes(), &mut key);
    key
}

/// Generate a fresh random pairing secret (for the listen side to display).
pub fn generate_pairing_secret() -> [u8; PAIRING_SECRET_LEN] {
    let mut secret = [0u8; PAIRING_SECRET_LEN];
    OsRng.fill_bytes(&mut secret); // or rand::thread_rng(), but OsRng is fine
    secret
}

/// Encode a secret for display / copy-paste (base64, no padding for shortness).
pub fn encode_secret(secret: &[u8]) -> String {
    base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, secret)
}

/// Decode a secret the user pasted.
pub fn decode_secret(s: &str) -> Result<Vec<u8>> {
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, s.trim())
        .context("invalid base64 pairing secret")?;
    if bytes.len() != PAIRING_SECRET_LEN {
        bail!("pairing secret must be {} bytes", PAIRING_SECRET_LEN);
    }
    Ok(bytes)
}

/// Encrypt the plaintext blob. Returns (nonce, ciphertext).
pub fn encrypt_blob(key: &[u8; 32], plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let key = aes_gcm::Key::<Aes256Gcm>::from_slice(key);
    let cipher = Aes256Gcm::new(key);

    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| anyhow::anyhow!("aes-gcm encryption failed: {e}"))?;

    Ok((nonce_bytes.to_vec(), ciphertext))
}

/// Decrypt. The caller must have already verified the plaintext hash after this.
pub fn decrypt_blob(key: &[u8; 32], nonce: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
    if nonce.len() != 12 {
        bail!("invalid nonce length");
    }
    let key = aes_gcm::Key::<Aes256Gcm>::from_slice(key);
    let cipher = Aes256Gcm::new(key);
    let nonce = Nonce::from_slice(nonce);

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow::anyhow!("aes-gcm decryption failed (wrong secret or corrupted data): {e}"))?;

    Ok(plaintext)
}
