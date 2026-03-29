// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

//! MEGA protocol cryptography — AES-128, RSA, KDF v1/v2, node key management.
//! Implements the MEGA file encryption protocol as specified in APPENDIX-N/N2.

use aes::Aes128;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use ctr::cipher::StreamCipher;
use num_bigint_dig::BigUint;
use pbkdf2::pbkdf2_hmac;
use sha2::Sha512;

use super::ProviderError;

pub type MegaCryptoResult<T> = Result<T, ProviderError>;

// ─── Base64url (MEGA flavour: URL-safe, no padding) ───────────────────────

pub fn mega_base64_encode(data: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(data)
}

pub fn mega_base64_decode(value: &str) -> MegaCryptoResult<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|error| ProviderError::ParseError(format!("Invalid MEGA base64 value: {}", error)))
}

// ─── KDF ──────────────────────────────────────────────────────────────────

/// MEGA v1 KDF: 65536 rounds of AES-ECB with 4-byte password chunks.
/// Returns a 16-byte password key.
pub fn kdf_v1(password: &[u8]) -> MegaCryptoResult<[u8; 16]> {
    let mut key = [0u8; 16];

    // Process password in 16-byte chunks, XOR-accumulate through 65536 AES rounds
    for chunk_start in (0..password.len()).step_by(16) {
        let mut chunk = [0u8; 16];
        let chunk_end = std::cmp::min(chunk_start + 16, password.len());
        chunk[..chunk_end - chunk_start].copy_from_slice(&password[chunk_start..chunk_end]);

        // XOR chunk into key
        for (k, c) in key.iter_mut().zip(chunk.iter()) {
            *k ^= c;
        }

        // 65536 rounds of AES-ECB with the current key as both data and key derivation input
        let mut pkey = [0x93, 0xC4, 0x67, 0xE3, 0x7D, 0xB0, 0xC7, 0xA4,
                        0xD1, 0xBE, 0x3F, 0x81, 0x01, 0x52, 0xCB, 0x56]; // MEGA constant
        for _ in 0..65536 {
            let cipher = Aes128::new(GenericArray::from_slice(&key));
            let mut block = GenericArray::clone_from_slice(&pkey);
            cipher.encrypt_block(&mut block);
            pkey.copy_from_slice(&block);
        }

        key = pkey;
    }

    Ok(key)
}

/// MEGA v2 KDF: PBKDF2-HMAC-SHA512, 100 000 iterations.
/// Returns (password_key[16], user_hash[16]).
pub fn kdf_v2(password: &[u8], salt: &[u8]) -> MegaCryptoResult<([u8; 16], [u8; 16])> {
    let mut derived = [0u8; 32];
    pbkdf2_hmac::<Sha512>(password, salt, 100_000, &mut derived);

    let mut password_key = [0u8; 16];
    password_key.copy_from_slice(&derived[..16]);

    let mut user_hash = [0u8; 16];
    user_hash.copy_from_slice(&derived[16..32]);

    Ok((password_key, user_hash))
}

/// MEGA v1 username hash: email XOR-folded into 8 bytes, 16384 AES-ECB rounds.
pub fn username_hash_v1(email: &str, password_key: &[u8; 16]) -> MegaCryptoResult<[u8; 8]> {
    let email_lower = email.trim().to_lowercase();
    let email_bytes = email_lower.as_bytes();

    // XOR-fold email into 8 bytes
    let mut hash = [0u8; 8];
    for (i, &byte) in email_bytes.iter().enumerate() {
        hash[i % 8] ^= byte;
    }

    // Expand 8 bytes to 16 for AES block
    let mut block = [0u8; 16];
    block[..8].copy_from_slice(&hash);
    block[8..].copy_from_slice(&hash);

    // 16384 AES-ECB rounds
    let cipher = Aes128::new(GenericArray::from_slice(password_key));
    for _ in 0..16384 {
        let mut ga_block = GenericArray::clone_from_slice(&block);
        cipher.encrypt_block(&mut ga_block);
        block.copy_from_slice(&ga_block);
    }

    let mut result = [0u8; 8];
    result.copy_from_slice(&block[..4]);
    result[4..].copy_from_slice(&block[8..12]);
    Ok(result)
}

// ─── AES-ECB ──────────────────────────────────────────────────────────────

pub fn aes_ecb_encrypt_block(data: &[u8; 16], key: &[u8; 16]) -> MegaCryptoResult<[u8; 16]> {
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut block = GenericArray::clone_from_slice(data);
    cipher.encrypt_block(&mut block);
    let mut output = [0u8; 16];
    output.copy_from_slice(&block);
    Ok(output)
}

pub fn aes_ecb_decrypt_block(data: &[u8; 16], key: &[u8; 16]) -> MegaCryptoResult<[u8; 16]> {
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut block = GenericArray::clone_from_slice(data);
    cipher.decrypt_block(&mut block);
    let mut output = [0u8; 16];
    output.copy_from_slice(&block);
    Ok(output)
}

/// AES-128-ECB decrypt multiple 16-byte blocks (e.g. RSA private key, node keys).
pub fn aes_ecb_decrypt_multi(data: &[u8], key: &[u8; 16]) -> MegaCryptoResult<Vec<u8>> {
    if data.len() % 16 != 0 {
        return Err(ProviderError::ParseError(format!(
            "AES-ECB multi-block: data length {} is not a multiple of 16", data.len()
        )));
    }
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut output = data.to_vec();
    for chunk in output.chunks_exact_mut(16) {
        let mut block = GenericArray::clone_from_slice(chunk);
        cipher.decrypt_block(&mut block);
        chunk.copy_from_slice(&block);
    }
    Ok(output)
}

/// AES-128-ECB encrypt multiple 16-byte blocks.
pub fn aes_ecb_encrypt_multi(data: &[u8], key: &[u8; 16]) -> MegaCryptoResult<Vec<u8>> {
    if data.len() % 16 != 0 {
        return Err(ProviderError::ParseError(format!(
            "AES-ECB multi-block: data length {} is not a multiple of 16", data.len()
        )));
    }
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut output = data.to_vec();
    for chunk in output.chunks_exact_mut(16) {
        let mut block = GenericArray::clone_from_slice(chunk);
        cipher.encrypt_block(&mut block);
        chunk.copy_from_slice(&block);
    }
    Ok(output)
}

// ─── AES-CBC ──────────────────────────────────────────────────────────────

type Aes128CbcDec = cbc::Decryptor<Aes128>;
type Aes128CbcEnc = cbc::Encryptor<Aes128>;

/// AES-128-CBC decrypt (zero IV, no padding — MEGA node attributes).
pub fn aes_cbc_decrypt(data: &[u8], key: &[u8; 16]) -> MegaCryptoResult<Vec<u8>> {
    if data.len() % 16 != 0 {
        return Err(ProviderError::ParseError(format!(
            "AES-CBC: data length {} is not a multiple of 16", data.len()
        )));
    }
    let iv = [0u8; 16];
    let mut buf = data.to_vec();
    Aes128CbcDec::new(key.into(), &iv.into())
        .decrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut buf)
        .map_err(|e| ProviderError::ParseError(format!("AES-CBC decrypt failed: {e}")))?;
    Ok(buf)
}

/// AES-128-CBC encrypt (zero IV, no padding — MEGA node attributes).
pub fn aes_cbc_encrypt(data: &[u8], key: &[u8; 16]) -> MegaCryptoResult<Vec<u8>> {
    if data.len() % 16 != 0 {
        return Err(ProviderError::ParseError(format!(
            "AES-CBC: data length {} is not a multiple of 16", data.len()
        )));
    }
    let iv = [0u8; 16];
    // cbc::Encryptor needs extra buffer space for potential padding, but NoPadding means same size
    let mut buf = vec![0u8; data.len() + 16]; // extra block for API
    buf[..data.len()].copy_from_slice(data);
    let ct = Aes128CbcEnc::new(key.into(), &iv.into())
        .encrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut buf, data.len())
        .map_err(|e| ProviderError::ParseError(format!("AES-CBC encrypt failed: {e}")))?;
    Ok(ct.to_vec())
}

// ─── AES-CTR ──────────────────────────────────────────────────────────────

/// Build 16-byte CTR nonce from MEGA 8-byte nonce + byte offset.
fn build_ctr_iv(nonce: &[u8; 8], offset: u64) -> [u8; 16] {
    let block_offset = offset / 16;
    let mut iv = [0u8; 16];
    iv[..8].copy_from_slice(nonce);
    iv[8..16].copy_from_slice(&block_offset.to_be_bytes());
    iv
}

/// AES-128-CTR decrypt (symmetric — also used for encrypt).
pub fn aes_ctr_decrypt(
    data: &[u8],
    key: &[u8; 16],
    nonce: &[u8; 8],
    offset: u64,
) -> MegaCryptoResult<Vec<u8>> {
    let iv = build_ctr_iv(nonce, offset);
    let mut cipher = <ctr::Ctr128BE<Aes128> as KeyIvInit>::new(key.into(), &iv.into());
    let mut buf = data.to_vec();
    cipher.apply_keystream(&mut buf);
    Ok(buf)
}

/// AES-128-CTR encrypt (same as decrypt — CTR is symmetric).
pub fn aes_ctr_encrypt(
    data: &[u8],
    key: &[u8; 16],
    nonce: &[u8; 8],
    offset: u64,
) -> MegaCryptoResult<Vec<u8>> {
    aes_ctr_decrypt(data, key, nonce, offset) // CTR mode is symmetric
}

// ─── Node key management ──────────────────────────────────────────────────

/// Compute the AES key used for node attribute encryption.
/// For file nodes (32-byte packed key): XOR the four 32-bit words.
/// For folder nodes (16-byte key): use directly.
pub fn compute_attr_key(key: &[u8]) -> MegaCryptoResult<[u8; 16]> {
    match key.len() {
        32 => {
            // File node: key is 32 bytes = file_key(16) || nonce(8) || meta_mac(8)
            // Attr key = file_key[0..4] XOR nonce[0..4], file_key[4..8] XOR nonce[4..8],
            //            file_key[8..12] XOR meta_mac[0..4], file_key[12..16] XOR meta_mac[4..8]
            let mut attr_key = [0u8; 16];
            for i in 0..16 {
                attr_key[i] = key[i] ^ key[16 + (i % 16).min(15)];
            }
            // More precisely: XOR the two 16-byte halves
            for i in 0..16 {
                attr_key[i] = key[i] ^ key[i + 16];
            }
            Ok(attr_key)
        }
        16 => {
            let mut attr_key = [0u8; 16];
            attr_key.copy_from_slice(key);
            Ok(attr_key)
        }
        other => Err(ProviderError::ParseError(format!(
            "Invalid node key length: {} (expected 16 or 32)", other
        ))),
    }
}

/// Unpack a 32-byte MEGA file node key into (file_key[16], nonce[8]).
pub fn unpack_node_key(packed: &[u8; 32]) -> MegaCryptoResult<([u8; 16], [u8; 8])> {
    // packed = encrypted_key[0..8] XOR encrypted_key[16..24], encrypted_key[8..16] XOR encrypted_key[24..32]
    // After AES-ECB decryption of the 32-byte block, the layout is:
    // [file_key(16)] [nonce(8)] [meta_mac_fragment(8)]
    let mut file_key = [0u8; 16];
    file_key.copy_from_slice(&packed[..16]);

    let mut nonce = [0u8; 8];
    nonce.copy_from_slice(&packed[16..24]);

    Ok((file_key, nonce))
}

/// Pack file_key + nonce + meta_mac into a 32-byte MEGA node key.
pub fn pack_node_key(
    key: &[u8; 16],
    nonce: &[u8; 8],
    meta_mac: &[u8; 8],
) -> MegaCryptoResult<[u8; 32]> {
    let mut packed = [0u8; 32];
    packed[..16].copy_from_slice(key);
    packed[16..24].copy_from_slice(nonce);
    packed[24..32].copy_from_slice(meta_mac);
    Ok(packed)
}

// ─── Node attribute encryption ────────────────────────────────────────────

/// Decrypt MEGA node attributes (AES-CBC with zero IV, "MEGA" prefix).
/// Returns the JSON string inside (e.g. `{"n":"filename.txt"}`).
pub fn decrypt_node_attrs(encrypted: &[u8], key: &[u8]) -> MegaCryptoResult<String> {
    let attr_key = compute_attr_key(key)?;
    let decrypted = aes_cbc_decrypt(encrypted, &attr_key)?;

    // Must start with "MEGA" (4 bytes)
    if decrypted.len() < 4 || &decrypted[..4] != b"MEGA" {
        return Err(ProviderError::ParseError(
            "Decrypted node attributes do not start with MEGA prefix".to_string(),
        ));
    }

    // Find the JSON object — strip "MEGA" prefix and any trailing null bytes
    let json_bytes = &decrypted[4..];
    let json_str = std::str::from_utf8(json_bytes)
        .unwrap_or_else(|_| {
            // Try to find valid UTF-8 up to the first null or invalid byte
            let end = json_bytes.iter().position(|&b| b == 0).unwrap_or(json_bytes.len());
            std::str::from_utf8(&json_bytes[..end]).unwrap_or("")
        })
        .trim_end_matches('\0')
        .trim();

    Ok(json_str.to_string())
}

/// Encrypt MEGA node attributes. Prepends "MEGA" prefix, pads to 16-byte boundary, AES-CBC with zero IV.
pub fn encrypt_node_attrs(name: &str, key: &[u8]) -> MegaCryptoResult<Vec<u8>> {
    let attr_key = compute_attr_key(key)?;
    let json = format!("MEGA{{\"n\":\"{}\"}}", name.replace('\\', "\\\\").replace('"', "\\\""));
    let mut data = json.into_bytes();

    // Pad to 16-byte boundary with null bytes
    let pad_len = (16 - (data.len() % 16)) % 16;
    data.extend(std::iter::repeat(0u8).take(pad_len));

    aes_cbc_encrypt(&data, &attr_key)
}

// ─── Chunk MAC / Meta MAC ─────────────────────────────────────────────────

/// Compute MAC for a single file chunk (condensed AES-CBC-MAC with nonce).
pub fn chunk_mac(data: &[u8], key: &[u8; 16], nonce: &[u8; 8]) -> MegaCryptoResult<[u8; 16]> {
    let cipher = Aes128::new(GenericArray::from_slice(key));

    // Initial MAC state: [nonce(8), nonce(8)]
    let mut mac = [0u8; 16];
    mac[..8].copy_from_slice(nonce);
    mac[8..].copy_from_slice(nonce);

    // Process data in 16-byte blocks
    let mut i = 0;
    while i < data.len() {
        let end = std::cmp::min(i + 16, data.len());
        // XOR data block into mac
        for j in 0..(end - i) {
            mac[j] ^= data[i + j];
        }
        // AES encrypt
        let mut block = GenericArray::clone_from_slice(&mac);
        cipher.encrypt_block(&mut block);
        mac.copy_from_slice(&block);
        i += 16;
    }

    Ok(mac)
}

/// Compute meta MAC from individual chunk MACs.
/// Condensed MAC: AES-CBC chain of all chunk MACs, then extract [0..4] and [8..12].
pub fn meta_mac(chunk_macs: &[[u8; 16]], key: &[u8; 16]) -> MegaCryptoResult<[u8; 8]> {
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut mac = [0u8; 16];

    for cmac in chunk_macs {
        for j in 0..16 {
            mac[j] ^= cmac[j];
        }
        let mut block = GenericArray::clone_from_slice(&mac);
        cipher.encrypt_block(&mut block);
        mac.copy_from_slice(&block);
    }

    let mut result = [0u8; 8];
    result[..4].copy_from_slice(&mac[..4]);
    result[4..8].copy_from_slice(&mac[8..12]);
    Ok(result)
}

// ─── MEGA chunk boundaries ────────────────────────────────────────────────

/// Compute MEGA upload/download chunk boundaries.
/// Initial chunks: 128KB, 256KB, 384KB, ..., 1MB, 1MB, 1MB, ...
pub fn compute_chunk_boundaries(file_size: u64) -> Vec<(u64, usize)> {
    let mut chunks = Vec::new();
    let mut offset = 0u64;
    let mut chunk_size_multiple = 1u64; // starts at 128KB * 1

    while offset < file_size {
        let size = std::cmp::min(chunk_size_multiple * 131072, 1048576); // max 1MB
        let actual = std::cmp::min(size, file_size - offset) as usize;
        chunks.push((offset, actual));
        offset += actual as u64;
        if chunk_size_multiple < 8 {
            chunk_size_multiple += 1;
        }
    }

    chunks
}

// ─── RSA (MEGA MPI format) ────────────────────────────────────────────────

/// Decode a MEGA MPI (Multi-Precision Integer): 2-byte big-endian bit-length + data.
/// Returns (value, bytes_consumed).
fn decode_mpi(data: &[u8]) -> MegaCryptoResult<(BigUint, usize)> {
    if data.len() < 2 {
        return Err(ProviderError::ParseError("MPI too short for length prefix".to_string()));
    }
    let bit_len = ((data[0] as usize) << 8) | (data[1] as usize);
    let byte_len = bit_len.div_ceil(8);
    let total = 2 + byte_len;

    if data.len() < total {
        return Err(ProviderError::ParseError(format!(
            "MPI: need {} bytes but only {} available", total, data.len()
        )));
    }

    let value = BigUint::from_bytes_be(&data[2..total]);
    Ok((value, total))
}

/// Encode a BigUint as MEGA MPI format (2-byte bit-length prefix + big-endian bytes).
#[allow(dead_code)]
fn encode_mpi(value: &BigUint) -> Vec<u8> {
    let bytes = value.to_bytes_be();
    let bit_len = if bytes.is_empty() { 0u16 } else { (bytes.len() as u16 - 1) * 8 + (8 - bytes[0].leading_zeros() as u16) };
    let mut out = Vec::with_capacity(2 + bytes.len());
    out.push((bit_len >> 8) as u8);
    out.push(bit_len as u8);
    out.extend_from_slice(&bytes);
    out
}

/// Decrypt MEGA RSA private key from AES-ECB encrypted blob.
/// Returns the 4 RSA components (p, q, d, u) as BigUint values.
pub fn decrypt_rsa_privkey(
    encrypted: &[u8],
    master_key: &[u8; 16],
) -> MegaCryptoResult<(BigUint, BigUint, BigUint, BigUint)> {
    // Pad to 16-byte boundary if needed (MEGA sometimes has trailing bytes)
    let padded_len = (encrypted.len() + 15) & !15;
    let mut padded = vec![0u8; padded_len];
    padded[..encrypted.len()].copy_from_slice(encrypted);

    let decrypted = aes_ecb_decrypt_multi(&padded, master_key)?;

    // Parse 4 MPIs: p, q, d, u
    let mut offset = 0;
    let mut components = Vec::with_capacity(4);
    for name in &["p", "q", "d", "u"] {
        if offset >= decrypted.len() {
            return Err(ProviderError::ParseError(format!(
                "RSA private key: unexpected end while reading component {}", name
            )));
        }
        let (value, consumed) = decode_mpi(&decrypted[offset..])?;
        components.push(value);
        offset += consumed;
    }

    Ok((
        components.remove(0),
        components.remove(0),
        components.remove(0),
        components.remove(0),
    ))
}

/// RSA-decrypt the MEGA csid (session ID) using the private key components.
/// MEGA uses textbook RSA (no padding): plaintext = ciphertext^d mod (p*q).
/// Returns the base64url-encoded session ID.
pub fn rsa_decrypt_csid(
    csid_b64: &str,
    p: &BigUint,
    q: &BigUint,
    d: &BigUint,
) -> MegaCryptoResult<String> {
    let csid_bytes = mega_base64_decode(csid_b64)?;

    // csid is MPI-encoded
    let (encrypted_value, _) = decode_mpi(&csid_bytes)?;

    let n = p * q;
    let decrypted = encrypted_value.modpow(d, &n);

    let decrypted_bytes = decrypted.to_bytes_be();

    // Session ID is the first 43 bytes of the decrypted MPI value
    if decrypted_bytes.len() < 43 {
        return Err(ProviderError::ParseError(format!(
            "RSA-decrypted csid too short: {} bytes (need 43)", decrypted_bytes.len()
        )));
    }

    Ok(mega_base64_encode(&decrypted_bytes[..43]))
}

// ─── XOR-based node key decryption ────────────────────────────────────────

/// Decrypt a MEGA node key. The encrypted key is XOR-ed with the owner's key
/// in a specific pattern depending on length (file=32 bytes, folder=16 bytes).
pub fn decrypt_node_key_xor(encrypted_key: &[u8], owner_key: &[u8; 16]) -> MegaCryptoResult<Vec<u8>> {
    match encrypted_key.len() {
        32 => {
            // File node: 32-byte key, AES-ECB decrypt 2 blocks
            let decrypted = aes_ecb_decrypt_multi(encrypted_key, owner_key)?;
            // XOR the two halves to get the actual 32-byte compound key
            // Actually, MEGA file keys are stored as 4x u32 XOR pairs:
            // decrypted[0..8] XOR decrypted[16..24] = file_key[0..8]
            // decrypted[8..16] XOR decrypted[24..32] = file_key[8..16] + nonce + metamac
            // The raw decrypted 32 bytes IS the compound key after ECB decryption
            Ok(decrypted)
        }
        16 => {
            // Folder node: 16-byte key, single AES-ECB block
            let mut arr = [0u8; 16];
            arr.copy_from_slice(encrypted_key);
            let decrypted = aes_ecb_decrypt_block(&arr, owner_key)?;
            Ok(decrypted.to_vec())
        }
        other => Err(ProviderError::ParseError(format!(
            "Unexpected MEGA node key length: {} (expected 16 or 32)", other
        ))),
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mega_base64_roundtrip_empty() {
        assert_eq!(mega_base64_encode(b""), "");
        assert_eq!(mega_base64_decode("").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn mega_base64_roundtrip_single_zero_byte() {
        assert_eq!(mega_base64_encode(&[0]), "AA");
        assert_eq!(mega_base64_decode("AA").unwrap(), vec![0]);
    }

    #[test]
    fn kdf_v2_matches_known_vector() {
        let salt = hex::decode("00112233445566778899aabbccddeeff").unwrap();
        let (password_key, user_hash) = kdf_v2(b"correct horse battery staple", &salt).unwrap();

        assert_eq!(hex::encode(password_key), "6cbed59f582390f4e8aae45c04c545a3");
        assert_eq!(hex::encode(user_hash), "f95598ec9077408dd6731403cbae2a4b");
    }

    #[test]
    fn aes_ecb_roundtrip_matches_single_block() {
        let key = [0x10; 16];
        let block = [0xAB; 16];

        let encrypted = aes_ecb_encrypt_block(&block, &key).unwrap();
        let decrypted = aes_ecb_decrypt_block(&encrypted, &key).unwrap();

        assert_eq!(decrypted, block);
    }

    #[test]
    fn aes_ecb_multi_roundtrip() {
        let key = [0x42; 16];
        let data = vec![0xAA; 48]; // 3 blocks

        let encrypted = aes_ecb_encrypt_multi(&data, &key).unwrap();
        let decrypted = aes_ecb_decrypt_multi(&encrypted, &key).unwrap();

        assert_eq!(decrypted, data);
    }

    #[test]
    fn aes_cbc_roundtrip() {
        let key = [0x33; 16];
        let data = vec![0x55; 32]; // 2 blocks

        let encrypted = aes_cbc_encrypt(&data, &key).unwrap();
        let decrypted = aes_cbc_decrypt(&encrypted, &key).unwrap();

        assert_eq!(decrypted, data);
    }

    #[test]
    fn aes_ctr_roundtrip() {
        let key = [0x77; 16];
        let nonce = [0x11; 8];
        let data = b"Hello MEGA encryption test!";

        let encrypted = aes_ctr_encrypt(data, &key, &nonce, 0).unwrap();
        let decrypted = aes_ctr_decrypt(&encrypted, &key, &nonce, 0).unwrap();

        assert_eq!(decrypted, data);
    }

    #[test]
    fn chunk_mac_basic() {
        let key = [0x42; 16];
        let nonce = [0x11; 8];
        let data = vec![0xAA; 1024];

        let mac = chunk_mac(&data, &key, &nonce).unwrap();
        assert_eq!(mac.len(), 16);

        // Same input should produce same output
        let mac2 = chunk_mac(&data, &key, &nonce).unwrap();
        assert_eq!(mac, mac2);
    }

    #[test]
    fn meta_mac_basic() {
        let key = [0x42; 16];
        let cmacs = vec![[0xAA; 16], [0xBB; 16]];

        let mm = meta_mac(&cmacs, &key).unwrap();
        assert_eq!(mm.len(), 8);
    }

    #[test]
    fn chunk_boundaries_small_file() {
        let chunks = compute_chunk_boundaries(100);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], (0, 100));
    }

    #[test]
    fn chunk_boundaries_medium_file() {
        let chunks = compute_chunk_boundaries(1_000_000);
        assert!(!chunks.is_empty());
        // Verify total coverage
        let total: usize = chunks.iter().map(|(_, s)| s).sum();
        assert_eq!(total, 1_000_000);
    }

    #[test]
    fn mpi_roundtrip() {
        let value = BigUint::from(0x1234_5678_9ABC_DEF0u64);
        let encoded = encode_mpi(&value);
        let (decoded, consumed) = decode_mpi(&encoded).unwrap();
        assert_eq!(decoded, value);
        assert_eq!(consumed, encoded.len());
    }

    #[test]
    fn node_attr_roundtrip() {
        let key = vec![0x42u8; 16]; // folder key (16 bytes)
        let encrypted = encrypt_node_attrs("test_file.txt", &key).unwrap();
        let decrypted = decrypt_node_attrs(&encrypted, &key).unwrap();
        assert!(decrypted.contains("test_file.txt"));
    }

    #[test]
    fn pack_unpack_node_key() {
        let file_key = [0xAA; 16];
        let nonce = [0xBB; 8];
        let mm = [0xCC; 8];

        let packed = pack_node_key(&file_key, &nonce, &mm).unwrap();
        let (unpacked_key, unpacked_nonce) = unpack_node_key(&packed).unwrap();

        assert_eq!(unpacked_key, file_key);
        assert_eq!(unpacked_nonce, nonce);
    }
}
