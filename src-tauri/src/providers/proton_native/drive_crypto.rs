//! Drive cryptography: node keys, names, extended attributes, content keys,
//! blocks and manifests.
//!
//! Ported from `client/js/src/crypto/driveCrypto.ts` of the Proton Drive SDK
//! at commit [`super::SDK_REFERENCE_COMMIT`] (MIT, Copyright (c) 2025-2026
//! Proton AG, see `LICENSE-PROTON-DRIVE-SDK.md`). The OpenPGP work is done by
//! Proton's `proton-crypto` crate; this file only arranges the calls the way
//! the SDK does.
//!
//! The SDK's rule, kept here everywhere: a failed DECRYPTION is an error (or
//! degrades that one field), a failed signature VERIFICATION never is. It is
//! reported as [`Verified`] next to the data and the caller decides what to
//! show; the data stays usable.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use hmac::{Hmac, Mac};
use proton_crypto_account::proton_crypto::crypto::{
    DataEncoding, Decryptor, DecryptorSync, DetachedSignatureVariant, Encryptor, EncryptorSync,
    KeyGenerator, KeyGeneratorAlgorithm, KeyGeneratorSync, PGPMessage, PGPProviderSync, SessionKey,
    Signer, SignerSync, VerificationError, VerificationResult, VerifiedData, Verifier,
    VerifierSync,
};
use rand::RngCore;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::error::ProtonError;

/// Outcome of a signature check, kept next to the data it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verified {
    /// Signed by one of the expected keys.
    Valid,
    /// No signature at all. Some legacy data is legitimately unsigned; the
    /// caller decides whether that counts (see the node rules).
    NotSigned,
    /// A signature was expected and did not verify.
    Invalid(String),
}

impl Verified {
    fn from_result(result: VerificationResult) -> Self {
        match result {
            Ok(_) => Verified::Valid,
            Err(VerificationError::NotSigned(_)) => Verified::NotSigned,
            Err(error) => Verified::Invalid(error.to_string()),
        }
    }

    pub fn is_valid(&self) -> bool {
        matches!(self, Verified::Valid)
    }
}

/// `driveCrypto.ts::decryptKey`: decrypts the passphrase with
/// `decryption_keys`, checks its detached signature against
/// `verification_keys`, then unlocks `armored_key` with it.
///
/// A missing signature is reported as invalid, not as unsigned, as the SDK
/// does ("Signature is missing").
pub fn decrypt_key<P: PGPProviderSync>(
    provider: &P,
    armored_key: &str,
    armored_passphrase: &str,
    armored_passphrase_signature: Option<&str>,
    decryption_keys: &[&P::PrivateKey],
    verification_keys: &[&P::PublicKey],
) -> Result<(P::PrivateKey, Verified), ProtonError> {
    let decryptor = provider
        .new_decryptor()
        .with_decryption_keys(decryption_keys.iter().copied())
        .with_verification_keys(verification_keys.iter().copied());
    let decryptor = match armored_passphrase_signature {
        Some(signature) => decryptor.with_detached_signature_ref(
            signature.as_bytes(),
            DetachedSignatureVariant::Plaintext,
            true,
        ),
        None => decryptor,
    };
    let decrypted = decryptor
        .decrypt(armored_passphrase.as_bytes(), DataEncoding::Armor)
        .map_err(|error| ProtonError::Crypto(format!("node passphrase: {error}")))?;
    let verified = match armored_passphrase_signature {
        Some(_) => Verified::from_result(decrypted.verification_result()),
        None => Verified::Invalid("signature is missing".into()),
    };
    // The SDK decodes the passphrase as strict UTF-8 and unlocks the key with
    // that string, i.e. its UTF-8 bytes.
    let passphrase = Zeroizing::new(decrypted.into_vec());
    std::str::from_utf8(&passphrase)
        .map_err(|_| ProtonError::Crypto("node passphrase is not UTF-8".into()))?;
    let key = provider
        .private_key_import(
            armored_key.as_bytes(),
            passphrase.as_slice(),
            DataEncoding::Armor,
        )
        .map_err(|error| ProtonError::Crypto(format!("node key unlock: {error}")))?;
    Ok((key, verified))
}

/// Decrypts an armored message with an inline signature: node names, folder
/// hash keys and extended attributes (`decryptNodeName`,
/// `decryptNodeHashKey`, `decryptExtendedAttributes`). Compressed payloads
/// are inflated by the decryptor.
pub fn decrypt_inline<P: PGPProviderSync>(
    provider: &P,
    armored: &str,
    decryption_key: &P::PrivateKey,
    verification_keys: &[&P::PublicKey],
) -> Result<(Vec<u8>, Verified), ProtonError> {
    let decrypted = provider
        .new_decryptor()
        .with_decryption_key(decryption_key)
        .with_verification_keys(verification_keys.iter().copied())
        .decrypt(armored.as_bytes(), DataEncoding::Armor)
        .map_err(|error| ProtonError::Crypto(error.to_string()))?;
    let verified = Verified::from_result(decrypted.verification_result());
    Ok((decrypted.into_vec(), verified))
}

/// The session key of an armored message's key packets: used to re-encrypt a
/// node name or passphrase on rename and move without changing its content.
pub fn message_session_key<P: PGPProviderSync>(
    provider: &P,
    armored_message: &str,
    decryption_key: &P::PrivateKey,
) -> Result<P::SessionKey, ProtonError> {
    use proton_crypto_account::proton_crypto::crypto::PGPMessage;
    let message = provider
        .pgp_message_import(armored_message.as_bytes(), DataEncoding::Armor)
        .map_err(|error| ProtonError::Crypto(format!("message import: {error}")))?;
    provider
        .new_decryptor()
        .with_decryption_key(decryption_key)
        .decrypt_session_key(message.as_key_packets())
        .map_err(|error| ProtonError::Crypto(format!("message session key: {error}")))
}

/// `driveCrypto.ts::decryptAndVerifySessionKey`: the file content key from
/// the base64 `ContentKeyPacket`, decrypted with the node key, and its
/// detached signature over the raw session key bytes.
pub fn decrypt_content_key<P: PGPProviderSync>(
    provider: &P,
    content_key_packet_b64: &str,
    armored_signature: Option<&str>,
    node_key: &P::PrivateKey,
    verification_keys: &[&P::PublicKey],
) -> Result<(P::SessionKey, Option<Verified>), ProtonError> {
    let packet = BASE64
        .decode(content_key_packet_b64.trim())
        .map_err(|_| ProtonError::Crypto("content key packet is not base64".into()))?;
    let session_key = provider
        .new_decryptor()
        .with_decryption_key(node_key)
        .decrypt_session_key(&packet)
        .map_err(|error| ProtonError::Crypto(format!("content key: {error}")))?;
    let verified = armored_signature.map(|signature| {
        verify_detached(
            provider,
            session_key.export().as_ref(),
            signature,
            verification_keys,
        )
    });
    Ok((session_key, verified))
}

/// `driveCrypto.ts::decryptBlock`: a content block decrypted with the file's
/// session key. The SDK does not verify block signatures ("We do not verify
/// signatures on blocks"); integrity comes from the block hash and the
/// manifest signature.
pub fn decrypt_block<P: PGPProviderSync>(
    provider: &P,
    encrypted_block: &[u8],
    session_key: &P::SessionKey,
) -> Result<Vec<u8>, ProtonError> {
    provider
        .new_decryptor()
        .with_session_key_ref(session_key)
        .decrypt(encrypted_block, DataEncoding::Bytes)
        .map(|data| data.into_vec())
        .map_err(|error| ProtonError::Crypto(format!("block: {error}")))
}

/// A detached, armored signature over `data` (manifests, content keys).
pub fn verify_detached<P: PGPProviderSync>(
    provider: &P,
    data: &[u8],
    armored_signature: &str,
    verification_keys: &[&P::PublicKey],
) -> Verified {
    Verified::from_result(
        provider
            .new_verifier()
            .with_verification_keys(verification_keys.iter().copied())
            .verify_detached(data, armored_signature.as_bytes(), DataEncoding::Armor),
    )
}

/// `driveCrypto.ts::generateLookupHash`: HMAC-SHA256 of the UTF-8 name keyed
/// with the parent folder's decrypted hash key bytes, used as they come out
/// of the message (never base64-decoded), as lowercase hex.
pub fn lookup_hash(parent_hash_key: &[u8], name: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(parent_hash_key)
        .expect("HMAC-SHA256 accepts keys of any length");
    mac.update(name.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Base64 SHA-256 of an encrypted block, as the API reports it in `Hash`.
pub fn encrypted_block_hash(encrypted_block: &[u8]) -> (Vec<u8>, String) {
    let digest = Sha256::digest(encrypted_block).to_vec();
    let encoded = BASE64.encode(&digest);
    (digest, encoded)
}

/// User ID of node keys, as the C# SDK writes it (`NodeOperations.cs`).
const NODE_KEY_USER_NAME: &str = "Drive key";
const NODE_KEY_USER_EMAIL: &str = "no-reply@proton.me";

/// `openPGPCrypto.ts::generatePassphrase`: 32 random bytes as padded base64.
/// The 44-character ASCII string itself is the passphrase.
pub fn generate_passphrase() -> Zeroizing<String> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    rand::rngs::OsRng.fill_bytes(&mut bytes[..]);
    Zeroizing::new(BASE64.encode(&bytes[..]))
}

fn armored(message: &impl PGPMessage) -> Result<String, ProtonError> {
    let bytes = message
        .armor()
        .map_err(|error| ProtonError::Crypto(format!("armor: {error}")))?;
    String::from_utf8(bytes).map_err(|_| ProtonError::Crypto("armor is not UTF-8".into()))
}

fn armored_bytes(bytes: impl AsRef<[u8]>) -> Result<String, ProtonError> {
    String::from_utf8(bytes.as_ref().to_vec())
        .map_err(|_| ProtonError::Crypto("armor is not UTF-8".into()))
}

/// A new node, as `driveCrypto.ts::generateKey` makes it: an ECC v4 key
/// (EdDSA legacy plus ECDH Curve25519 legacy), locked with a fresh
/// passphrase; the passphrase encrypted to `encryption_key` (the parent's)
/// and its detached signature by `signing_key` (the address key).
pub struct GeneratedNodeKey<P: PGPProviderSync> {
    pub key: P::PrivateKey,
    pub armored_key: String,
    pub armored_passphrase: String,
    pub armored_passphrase_signature: String,
}

pub fn generate_node_key<P: PGPProviderSync>(
    provider: &P,
    encryption_key: &P::PublicKey,
    signing_key: &P::PrivateKey,
) -> Result<GeneratedNodeKey<P>, ProtonError> {
    let passphrase = generate_passphrase();
    let key = provider
        .new_key_generator()
        .with_user_id(NODE_KEY_USER_NAME, NODE_KEY_USER_EMAIL)
        .with_algorithm(KeyGeneratorAlgorithm::ECC)
        .generate()
        .map_err(|error| ProtonError::Crypto(format!("key generation: {error}")))?;
    let armored_key = armored_bytes(
        provider
            .private_key_export(&key, passphrase.as_bytes(), DataEncoding::Armor)
            .map_err(|error| ProtonError::Crypto(format!("key lock: {error}")))?,
    )?;
    let (armored_passphrase, armored_passphrase_signature) =
        encrypt_passphrase(provider, passphrase.as_bytes(), encryption_key, signing_key)?;
    Ok(GeneratedNodeKey {
        key,
        armored_key,
        armored_passphrase,
        armored_passphrase_signature,
    })
}

/// `driveCrypto.ts::encryptPassphrase`: armored message to the parent key
/// plus an armored detached signature over the plaintext.
pub fn encrypt_passphrase<P: PGPProviderSync>(
    provider: &P,
    passphrase: &[u8],
    encryption_key: &P::PublicKey,
    signing_key: &P::PrivateKey,
) -> Result<(String, String), ProtonError> {
    let message = provider
        .new_encryptor()
        .with_encryption_key(encryption_key)
        .encrypt(passphrase)
        .map_err(|error| ProtonError::Crypto(format!("passphrase: {error}")))?;
    let signature = sign_detached(provider, passphrase, signing_key)?;
    Ok((armored(&message)?, signature))
}

/// Armored detached signature (manifests, passphrases).
pub fn sign_detached<P: PGPProviderSync>(
    provider: &P,
    data: &[u8],
    signing_key: &P::PrivateKey,
) -> Result<String, ProtonError> {
    armored_bytes(
        provider
            .new_signer()
            .with_signing_key(signing_key)
            .sign_detached(data, DataEncoding::Armor)
            .map_err(|error| ProtonError::Crypto(format!("signature: {error}")))?,
    )
}

/// `driveCrypto.ts::encryptNodeName`: binary literal data with an inline
/// signature by the address key, armored, encrypted to the parent key. On
/// rename and move the existing name session key is reused.
pub fn encrypt_node_name<P: PGPProviderSync>(
    provider: &P,
    name: &str,
    session_key: Option<&P::SessionKey>,
    encryption_key: &P::PublicKey,
    signing_key: &P::PrivateKey,
) -> Result<String, ProtonError> {
    let encryptor = provider
        .new_encryptor()
        .with_encryption_key(encryption_key)
        .with_signing_key(signing_key);
    let encryptor = match session_key {
        Some(session_key) => encryptor.with_session_key_ref(session_key),
        None => encryptor,
    };
    let message = encryptor
        .encrypt(name.as_bytes())
        .map_err(|error| ProtonError::Crypto(format!("name: {error}")))?;
    armored(&message)
}

/// Re-encrypts `data` with an existing session key and a key packet for a
/// new recipient (the node passphrase on move): the content and its old
/// signature stay valid.
pub fn reencrypt_with_session_key<P: PGPProviderSync>(
    provider: &P,
    data: &[u8],
    session_key: &P::SessionKey,
    encryption_key: &P::PublicKey,
) -> Result<String, ProtonError> {
    let message = provider
        .new_encryptor()
        .with_session_key_ref(session_key)
        .with_encryption_key(encryption_key)
        .encrypt(data)
        .map_err(|error| ProtonError::Crypto(format!("re-encryption: {error}")))?;
    armored(&message)
}

/// `driveCrypto.ts::generateHashKey`: a fresh passphrase-style string,
/// encrypted to and signed by the new folder key, armored.
pub fn generate_hash_key<P: PGPProviderSync>(
    provider: &P,
    node_key: &P::PrivateKey,
) -> Result<(Zeroizing<Vec<u8>>, String), ProtonError> {
    let hash_key = generate_passphrase();
    let public = provider
        .private_key_to_public_key(node_key)
        .map_err(|error| ProtonError::Crypto(error.to_string()))?;
    let message = provider
        .new_encryptor()
        .with_encryption_key(&public)
        .with_signing_key(node_key)
        .encrypt(hash_key.as_bytes())
        .map_err(|error| ProtonError::Crypto(format!("hash key: {error}")))?;
    Ok((
        Zeroizing::new(hash_key.as_bytes().to_vec()),
        armored(&message)?,
    ))
}

/// `driveCrypto.ts::generateContentKey`: a session key for the file content,
/// its key packet to the node key (base64) and a detached signature by the
/// node key over the raw session key bytes. `proton-crypto` makes a SEIPDv1
/// AES-256 key with a v3 key packet: the legacy format every Proton client
/// reads (see the appendix note on AEAD).
pub fn generate_content_key<P: PGPProviderSync>(
    provider: &P,
    node_public: &P::PublicKey,
    node_key: &P::PrivateKey,
) -> Result<(P::SessionKey, String, String), ProtonError> {
    let session_key = provider
        .new_encryptor()
        .with_encryption_key(node_public)
        .generate_session_key()
        .map_err(|error| ProtonError::Crypto(format!("content key: {error}")))?;
    let packets = provider
        .new_encryptor()
        .with_encryption_key(node_public)
        .encrypt_session_key(&session_key)
        .map_err(|error| ProtonError::Crypto(format!("content key packet: {error}")))?;
    let signature = sign_detached(provider, session_key.export().as_ref(), node_key)?;
    Ok((session_key, BASE64.encode(packets), signature))
}

/// `driveCrypto.ts::encryptBlock`: the block encrypted with the content
/// session key alone (no key packet), binary.
pub fn encrypt_block<P: PGPProviderSync>(
    provider: &P,
    data: &[u8],
    session_key: &P::SessionKey,
) -> Result<Vec<u8>, ProtonError> {
    let message = provider
        .new_encryptor()
        .with_session_key_ref(session_key)
        .encrypt(data)
        .map_err(|error| ProtonError::Crypto(format!("block: {error}")))?;
    Ok(message.as_data_packet().to_vec())
}

/// `driveCrypto.ts::encryptBlock` + `encryptSignature`: a detached signature
/// by the address key over the plaintext, encrypted with the content session
/// key plus a key packet to the node key, armored (`EncSignature`).
pub fn encrypt_block_signature<P: PGPProviderSync>(
    provider: &P,
    data: &[u8],
    signing_key: &P::PrivateKey,
    session_key: &P::SessionKey,
    node_public: &P::PublicKey,
) -> Result<String, ProtonError> {
    let signature = provider
        .new_signer()
        .with_signing_key(signing_key)
        .sign_detached(data, DataEncoding::Bytes)
        .map_err(|error| ProtonError::Crypto(format!("block signature: {error}")))?;
    let message = provider
        .new_encryptor()
        .with_session_key_ref(session_key)
        .with_encryption_key(node_public)
        .encrypt(&signature)
        .map_err(|error| ProtonError::Crypto(format!("block signature: {error}")))?;
    armored(&message)
}

/// `driveCrypto.ts::encryptExtendedAttributes`: encrypted to the node key,
/// signed inline by the address key, compressed, armored.
pub fn encrypt_extended_attributes<P: PGPProviderSync>(
    provider: &P,
    json: &str,
    node_public: &P::PublicKey,
    signing_key: &P::PrivateKey,
) -> Result<String, ProtonError> {
    let message = provider
        .new_encryptor()
        .with_encryption_key(node_public)
        .with_signing_key(signing_key)
        .with_compression()
        .encrypt(json.as_bytes())
        .map_err(|error| ProtonError::Crypto(format!("extended attributes: {error}")))?;
    armored(&message)
}

/// A session key from a base64 key packet, without a signature check (the
/// verification key packet of an upload, `blockVerifier.ts`).
pub fn decrypt_session_key_b64<P: PGPProviderSync>(
    provider: &P,
    key_packet_b64: &str,
    decryption_key: &P::PrivateKey,
) -> Result<P::SessionKey, ProtonError> {
    decrypt_content_key(provider, key_packet_b64, None, decryption_key, &[]).map(|(key, _)| key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4231 test case 2 for HMAC-SHA256, then the lowercase hex shape.
    #[test]
    fn lookup_hash_is_hmac_sha256_lower_hex() {
        assert_eq!(
            lookup_hash(b"Jefe", "what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn block_hash_is_base64_sha256() {
        let (raw, encoded) = encrypted_block_hash(b"abc");
        assert_eq!(raw.len(), 32);
        assert_eq!(encoded, "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=");
    }
}
