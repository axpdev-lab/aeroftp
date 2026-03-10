# AeroVault v2 Format Specification

**Version**: 2
**Status**: Stable
**Date**: 2026-03-10
**Authors**: AXP Development

> Canonical source: [`aerovault` crate on crates.io](https://crates.io/crates/aerovault)
> Reference implementation: [github.com/axpnet/aerovault](https://github.com/axpnet/aerovault)

---

## 1. Overview

AeroVault v2 is a single-file encrypted container format designed for maximum security with practical usability. A `.aerovault` file encapsulates an arbitrary number of files and directories in a single encrypted archive, using layered cryptography to provide defense-in-depth.

### 1.1 Design Goals

- **Single-file portability**: One `.aerovault` file contains everything
- **Nonce misuse resistance**: AES-256-GCM-SIV (RFC 8452) tolerates nonce reuse without catastrophic failure
- **Password-based access**: No key files required, Argon2id KDF exceeds OWASP 2024 recommendations
- **Atomic operations**: All mutations use temp+rename to prevent corruption on crash/power loss
- **Optional cascade mode**: Double encryption (AES-256-GCM-SIV + ChaCha20-Poly1305) for defense-in-depth
- **Deterministic filename encryption**: AES-256-SIV enables efficient duplicate detection

### 1.2 Cryptographic Primitives

| Purpose | Algorithm | Standard |
|---------|-----------|----------|
| Key Derivation | Argon2id | RFC 9106 |
| Key Wrapping | AES-256-KW | RFC 3394 |
| Content Encryption | AES-256-GCM-SIV | RFC 8452 |
| Cascade Encryption | ChaCha20-Poly1305 | RFC 8439 |
| Filename Encryption | AES-256-SIV | RFC 5297 |
| Header Integrity | HMAC-SHA512 | RFC 2104 |
| Key Separation | HKDF-SHA256 | RFC 5869 |

---

## 2. File Structure

An AeroVault v2 file consists of three contiguous sections:

```
┌─────────────────────────────────────┐  offset 0
│           Header (512 bytes)        │
├─────────────────────────────────────┤  offset 512
│      Manifest Length (4 bytes)      │
├─────────────────────────────────────┤  offset 516
│   AES-SIV Encrypted Manifest       │
│   (variable length)                │
├─────────────────────────────────────┤  offset 516 + manifest_len
│       Encrypted Data Chunks         │
│   [chunk_len:4][encrypted:N]        │
│   [chunk_len:4][encrypted:N]        │
│              ...                    │
└─────────────────────────────────────┘
```

---

## 3. Header (512 bytes)

The header is a fixed-size structure at offset 0. All multi-byte integers are **little-endian**.

### 3.1 Layout

| Offset | Size (bytes) | Field | Description |
|--------|-------------|-------|-------------|
| 0 | 10 | `magic` | ASCII `AEROVAULT2` |
| 10 | 1 | `version` | Format version (`0x02`) |
| 11 | 1 | `flags` | Bit field (see 3.2) |
| 12 | 32 | `salt` | Argon2id salt (random) |
| 44 | 40 | `wrapped_master_key` | AES-KW wrapped master key |
| 84 | 40 | `wrapped_mac_key` | AES-KW wrapped MAC key |
| 124 | 4 | `chunk_size` | Plaintext chunk size in bytes (LE u32) |
| 128 | 64 | `header_mac` | HMAC-SHA512 over bytes 0..128 |
| 192 | 320 | `reserved` | Zero-filled, reserved for future use |

**Total**: 512 bytes

### 3.2 Flags (byte offset 11)

| Bit | Name | Description |
|-----|------|-------------|
| 0 | `cascade_mode` | 1 = cascade encryption enabled |
| 1-7 | reserved | Must be 0 |

### 3.3 Magic Bytes

The magic string is the ASCII encoding of `AEROVAULT2` (10 bytes):

```
41 45 52 4F 56 41 55 4C 54 32
```

Implementations MUST reject files where the first 10 bytes do not match this sequence.

### 3.4 Wrapped Keys

Each wrapped key is 40 bytes: the original 32-byte key + 8-byte AES-KW integrity check value (ICV). The wrapping uses AES-256-KW per RFC 3394.

The `wrapped_master_key` protects the 256-bit master key used for content and filename encryption.

The `wrapped_mac_key` protects the 256-bit MAC key used for HMAC-SHA512 header integrity.

### 3.5 Header MAC

The `header_mac` field contains an HMAC-SHA512 computed over the first 128 bytes of the header (offsets 0-127, which includes everything except the MAC itself and the reserved area).

The MAC key used is the unwrapped `mac_key`.

Implementations MUST verify the header MAC using **constant-time comparison** before proceeding with any other operations.

---

## 4. Key Derivation

### 4.1 Argon2id Parameters

| Parameter | Value | Description |
|-----------|-------|-------------|
| Algorithm | Argon2id | Hybrid: side-channel resistant (Argon2i) + GPU resistant (Argon2d) |
| Version | 0x13 (19) | Current Argon2 version |
| Memory (`m_cost`) | 131072 KiB (128 MiB) | Memory required |
| Time (`t_cost`) | 4 | Number of iterations |
| Parallelism (`p_cost`) | 4 | Degree of parallelism |
| Output length | 32 bytes | 256-bit base KEK |
| Salt | 32 bytes | Random, stored in header |

These parameters exceed the OWASP 2024 recommendation of 64 MiB / t=3 / p=1.

### 4.2 Key Derivation Chain

```
password (UTF-8 bytes)
    │
    ▼ Argon2id(salt, m=128MiB, t=4, p=4)
base_kek (32 bytes)
    │
    ├─► HKDF-SHA256(salt=∅, info="AeroVault v2 KEK for master key") → kek_master (32 bytes)
    │       │
    │       ▼ AES-256-KW unwrap(wrapped_master_key)
    │   master_key (32 bytes)
    │       │
    │       ├─► HKDF-SHA256(info="AeroVault v2 AES-SIV filename encryption") → siv_key (64 bytes)
    │       │       └─► AES-256-SIV filename/manifest encryption
    │       │
    │       └─► HKDF-SHA256(info="AeroVault v2 ChaCha20-Poly1305 cascade") → chacha_key (32 bytes)
    │               └─► ChaCha20-Poly1305 cascade layer (if enabled)
    │
    └─► HKDF-SHA256(salt=∅, info="AeroVault v2 KEK for MAC key") → kek_mac (32 bytes)
            │
            ▼ AES-256-KW unwrap(wrapped_mac_key)
        mac_key (32 bytes)
            └─► HMAC-SHA512 header verification
```

### 4.3 HKDF Domain Separation

All HKDF derivations use SHA-256 with:
- **Salt**: None (empty)
- **IKM**: The source key material
- **Info**: Domain-specific ASCII label

| Label | Output Size | Purpose |
|-------|-------------|---------|
| `AeroVault v2 KEK for master key` | 32 bytes | KEK for unwrapping master key |
| `AeroVault v2 KEK for MAC key` | 32 bytes | KEK for unwrapping MAC key |
| `AeroVault v2 AES-SIV filename encryption` | 64 bytes | AES-256-SIV key for filenames |
| `AeroVault v2 ChaCha20-Poly1305 cascade` | 32 bytes | ChaCha20-Poly1305 key for cascade |

> **Note**: These labels match the constants in the [reference implementation](https://github.com/axpnet/aerovault/blob/main/aerovault/src/constants.rs). Third-party implementations MUST use these exact byte strings.

---

## 5. Manifest

### 5.1 Structure

The manifest is a JSON object encrypted with AES-256-SIV and stored as hex-encoded ciphertext immediately after the header.

**On-disk layout**:

| Offset | Size | Content |
|--------|------|---------|
| 512 | 4 | Manifest length in bytes (LE u32) |
| 516 | N | Hex-encoded AES-SIV ciphertext |

### 5.2 Manifest Length Validation

Implementations MUST validate the manifest length before allocation. The maximum allowed value is **67,108,864 bytes (64 MiB)**. Values exceeding this limit MUST be rejected to prevent denial-of-service.

### 5.3 Plaintext JSON Schema

```json
{
  "created": "2026-03-07T12:00:00Z",
  "modified": "2026-03-07T12:30:00Z",
  "entries": [
    {
      "encrypted_name": "<hex-encoded AES-SIV ciphertext>",
      "size": 1048576,
      "offset": 0,
      "chunk_count": 16,
      "is_dir": false,
      "modified": "2026-03-07T12:00:00Z"
    }
  ]
}
```

### 5.4 Entry Fields

| Field | Type | Description |
|-------|------|-------------|
| `encrypted_name` | string | Hex-encoded AES-SIV ciphertext of the filename |
| `size` | u64 | Original plaintext size in bytes (0 for directories) |
| `offset` | u64 | Byte offset of first chunk in the data section |
| `chunk_count` | u32 | Number of encrypted chunks |
| `is_dir` | bool | `true` for directory entries (default: `false`) |
| `modified` | string | ISO 8601 timestamp (UTC) |

### 5.5 Filename Encryption

Filenames are encrypted using AES-256-SIV with a 64-byte key derived from the master key via HKDF (see 4.3). The ciphertext is hex-encoded for JSON compatibility.

AES-SIV is **deterministic**: the same plaintext always produces the same ciphertext. This property enables efficient duplicate detection by comparing encrypted names without decryption.

**AES-SIV Associated Data**: Implementations pass the associated data headers in the order `[aad, nonce]` per RFC 5297 Section 3 (S2V). The AAD is empty (`new byte[0]`) and the nonce is a 16-byte zero-filled array. This order is critical for cross-platform interoperability.

### 5.6 Directory Entries

Directories are manifest-only entries with `is_dir: true`, `size: 0`, `offset: 0`, and `chunk_count: 0`. They have no corresponding data in the data section.

Nested directories use `/` as the path separator (e.g., `docs/notes`). Implementations SHOULD create intermediate directories automatically.

### 5.7 Path Constraints

- Path separator: `/` (forward slash)
- Maximum path length: 4096 bytes
- Forbidden sequences: `..` (parent traversal)
- Leading/trailing slashes are stripped

---

## 6. Data Section

### 6.1 Chunk Format

The data section starts immediately after the manifest and consists of a sequence of length-prefixed encrypted chunks:

```
┌──────────────────────────────────────┐
│  chunk_length (4 bytes, LE u32)     │
├──────────────────────────────────────┤
│  encrypted_chunk (chunk_length bytes)│
│    = nonce (12) || ciphertext || tag │
└──────────────────────────────────────┘
```

Each entry's chunks are stored contiguously starting at the entry's `offset`.

### 6.2 Standard Mode (AES-256-GCM-SIV)

Each chunk is encrypted as:

```
nonce (12 bytes, random) || AES-256-GCM-SIV(key=master_key, nonce, aad=chunk_index_le32, plaintext)
```

- **Nonce**: 12 random bytes (OsRng)
- **AAD**: Chunk index as 4-byte little-endian u32
- **Tag**: 16 bytes (appended by AEAD)

The AAD binding prevents chunk reordering attacks: a chunk encrypted at index 0 cannot be placed at index 5 without authentication failure.

### 6.3 Cascade Mode (AES-256-GCM-SIV + ChaCha20-Poly1305)

When cascade mode is enabled (flag bit 0), each chunk undergoes double encryption:

**Layer 1 — AES-256-GCM-SIV** (same as standard mode):
```
inner = nonce_aes (12) || AES-GCM-SIV(master_key, nonce_aes, aad=chunk_index, plaintext)
```

**Layer 2 — ChaCha20-Poly1305**:
```
outer = nonce_chacha (12) || ChaCha20-Poly1305(chacha_key, nonce_chacha, aad=chunk_index, inner)
```

The `chacha_key` is derived from `master_key` via HKDF (see 4.3).

Decryption reverses the order: peel ChaCha20-Poly1305 first, then AES-256-GCM-SIV.

### 6.4 Default Chunk Size

The default plaintext chunk size is **65,536 bytes (64 KiB)**. This provides a good balance between:

- Memory usage during encryption/decryption
- Overhead ratio (nonce + tag per chunk)
- Seeking granularity for future random-access support

The actual chunk size is stored in the header and may differ from the default.

### 6.5 Encrypted Chunk Size

For standard mode:
```
encrypted_size = 12 (nonce) + plaintext_size + 16 (tag) = plaintext_size + 28
```

For cascade mode:
```
inner_size = plaintext_size + 28     (AES-GCM-SIV layer)
outer_size = 12 + inner_size + 16    (ChaCha20 layer)
           = plaintext_size + 56
```

The last chunk of a file may be smaller than `chunk_size`.

---

## 7. Operations

### 7.1 Vault Creation

1. Generate 32-byte random salt
2. Generate 32-byte random master key
3. Generate 32-byte random MAC key
4. Derive `base_kek` from password + salt via Argon2id
5. Derive `kek_master` and `kek_mac` from `base_kek` via HKDF
6. Wrap `master_key` with `kek_master` via AES-256-KW
7. Wrap `mac_key` with `kek_mac` via AES-256-KW
8. Build header with magic, version, flags, salt, wrapped keys, chunk size
9. Compute HMAC-SHA512 of header bytes 0..128 using `mac_key`
10. Create empty manifest, encrypt with AES-SIV using `master_key`
11. Write: header (512) + manifest_len (4) + encrypted_manifest

### 7.2 Vault Opening

1. Read and parse 512-byte header
2. Verify magic bytes and version
3. Derive `base_kek` from password + salt via Argon2id
4. Derive `kek_master` and `kek_mac` via HKDF
5. Unwrap `master_key` and `mac_key` via AES-256-KW (fails if wrong password)
6. Verify header MAC using constant-time comparison (fails if tampered)
7. Vault is unlocked — manifest can now be read and decrypted

### 7.3 Adding Files

1. Read current vault: header + manifest + existing data
2. For each file to add:
   a. Compute encrypted filename
   b. Skip if duplicate (deterministic SIV comparison)
   c. Read plaintext in chunks of `chunk_size`
   d. Encrypt each chunk (standard or cascade)
   e. Append `[chunk_len:4][encrypted_chunk:N]` to new data buffer
   f. Create manifest entry with offset, chunk_count, size
3. Re-encrypt manifest with updated entries
4. Write vault atomically: temp file → rename

### 7.4 Extracting Files

1. Open vault and decrypt manifest
2. Find entry by decrypting all filenames (SIV comparison)
3. Seek to entry's data offset
4. Read and decrypt `chunk_count` chunks
5. Write plaintext to output file

### 7.5 Password Change

1. Open vault with current password (verifies access)
2. Generate new 32-byte salt
3. Derive new KEK pair from new password + new salt
4. Re-wrap existing master_key and mac_key with new KEKs
5. Rebuild header with new salt, wrapped keys, and MAC
6. Write atomically: only the header changes, data section is untouched

### 7.6 Entry Deletion

1. Open vault and decrypt manifest
2. Remove entry from manifest (by decrypting and matching name)
3. Re-encrypt manifest
4. Rewrite vault: header + new manifest + same data section
5. Orphaned data remains until compaction (future feature)

### 7.7 Atomic Write Pattern

All mutations follow the crash-safe pattern:

1. Write complete new vault to `<path>.tmp`
2. Rename original to `<path>.bak`
3. Rename `<path>.tmp` to `<path>`
4. Delete `<path>.bak`

If step 3 fails, step 2 is rolled back (`.bak` → original).

---

## 8. Security Properties

### 8.1 Nonce Misuse Resistance

AES-256-GCM-SIV (RFC 8452) is the primary content cipher. Unlike AES-GCM, if a nonce is accidentally reused, only the equality of plaintexts encrypted under the same nonce is revealed — no key material is compromised.

### 8.2 Chunk Binding

Each chunk's authentication tag covers the chunk index as AAD. This prevents:

- **Reordering**: Moving chunk 0 to position 5 causes authentication failure
- **Truncation**: Missing chunks are detected by `chunk_count` mismatch
- **Duplication**: Inserting a copy of chunk 0 at position 1 fails AAD verification

### 8.3 Header Integrity

HMAC-SHA512 protects the header against modification. An attacker cannot:

- Change the salt (would change derived keys, but MAC would mismatch)
- Replace wrapped keys (MAC covers the wrapped key bytes)
- Alter flags or chunk size (MAC covers bytes 0..128)

### 8.4 Key Separation

HKDF with distinct `info` labels ensures that:

- The master key KEK and MAC key KEK are independent
- The SIV key for filenames is independent of the content encryption key
- The ChaCha key for cascade mode is independent of the GCM-SIV key

Compromise of any single derived key does not compromise the others.

### 8.5 Password Strength

- Minimum password length: 8 characters (enforced at API level)
- Argon2id with 128 MiB memory makes GPU/ASIC brute-force impractical
- Each vault has a unique random salt, preventing rainbow table attacks

### 8.6 Memory Safety

Implementations SHOULD:

- Zeroize all key material when no longer needed
- Use `SecretString` / `SecretVec` types to prevent accidental logging
- Zeroize decrypted plaintext buffers after writing to output

---

## 9. Comparison with Cryptomator

| Feature | AeroVault v2 | Cryptomator (v8) |
|---------|-------------|------------------|
| KDF | Argon2id (128 MiB) | scrypt (64 MiB) |
| Content cipher | AES-256-GCM-SIV (RFC 8452) | AES-256-GCM |
| Nonce misuse resistance | Yes (inherent) | No |
| Cascade mode | Optional (+ ChaCha20-Poly1305) | No |
| Filename encryption | AES-256-SIV | AES-256-SIV |
| Key wrapping | AES-256-KW (RFC 3394) | AES-256-KW |
| Header integrity | HMAC-SHA512 | JWT (HMAC-SHA256) |
| Storage model | Single file | Directory tree |
| Implementation | Rust (native) | Java/JVM |
| Chunk size | 64 KiB (configurable) | 32 KiB (fixed) |

---

## 10. Constants Summary

```
MAGIC                = "AEROVAULT2" (10 bytes)
VERSION              = 0x02
HEADER_SIZE          = 512
SALT_SIZE            = 32
WRAPPED_KEY_SIZE     = 40  (32-byte key + 8-byte AES-KW overhead)
NONCE_SIZE           = 12  (GCM-SIV and ChaCha20)
TAG_SIZE             = 16  (GCM-SIV and ChaCha20)
KEY_SIZE             = 32  (master key and MAC key)
MAC_SIZE             = 64  (HMAC-SHA512)
DEFAULT_CHUNK_SIZE   = 65536  (64 KiB)
MAX_MANIFEST_SIZE    = 67108864  (64 MiB)
MIN_PASSWORD_LENGTH  = 8

ARGON2_MEMORY        = 131072 KiB  (128 MiB)
ARGON2_ITERATIONS    = 4
ARGON2_PARALLELISM   = 4
ARGON2_OUTPUT_LEN    = 32
ARGON2_VERSION       = 0x13  (v1.3)
```

---

## 11. Test Vectors

### 11.1 Magic Bytes

```
Hex: 41 45 52 4F 56 41 55 4C 54 32
```

### 11.2 Header Structure (bytes 0-511)

```
00-09:  magic           AEROVAULT2
0A:     version         02
0B:     flags           00 (standard) or 01 (cascade)
0C-2B:  salt            32 random bytes
2C-53:  wrapped_master  40 bytes (AES-KW)
54-7B:  wrapped_mac     40 bytes (AES-KW)
7C-7F:  chunk_size      00 00 01 00 (65536 LE)
80-BF:  header_mac      64 bytes (HMAC-SHA512 over 00-7F)
C0-1FF: reserved        320 zero bytes
```

### 11.3 AAD for Chunk Index 0

```
Hex: 00 00 00 00    (uint32 LE)
```

### 11.4 AAD for Chunk Index 42

```
Hex: 2a 00 00 00    (uint32 LE)
```

---

## 12. File Extension and MIME Type

- **Extension**: `.aerovault`
- **MIME Type**: `application/x-aerovault` (not registered with IANA)
- **Magic detection**: First 10 bytes = `AEROVAULT2`

---

## 13. Reference Implementations

| Language | Implementation | Status |
|----------|---------------|--------|
| **Rust** | [`aerovault` crate](https://crates.io/crates/aerovault) | Production |
| **Rust** | [AeroFTP Desktop](https://github.com/axpnet/aeroftp) (GUI integration) | Production |
| **Java** | [AeroFTP Mobile](https://github.com/axpnet/aeroftp-mobile) `VaultPlugin.java` | Production |

---

## 14. Version History

| Version | Date | Changes |
|---------|------|---------|
| 2.0 | March 2026 | Initial specification |
| 2.0.1 | March 2026 | Extracted to standalone [`aerovault`](https://crates.io/crates/aerovault) crate with `MIME_TYPE` and `ICON_SVG` constants |

---

## License

This specification is released under the [GPL-3.0 License](../LICENSE). The `aerovault` crate is also GPL-3.0 licensed. Implementations of the AeroVault v2 format are free to use in both open-source and commercial software.

---

*AeroVault v2 is part of the [AeroFTP](https://github.com/axpnet/aeroftp) ecosystem by AXP Development.*
