# AeroVault v2 — Binary Container Format Specification

> **Version**: 2.0 &bull; **Status**: Stable &bull; **Last Updated**: March 2026

---

## Overview

AeroVault v2 is a military-grade encrypted container format designed for secure file storage and transport. It bundles multiple files and directories into a single `.aerovault` binary with authenticated encryption, nonce-misuse resistance, and defense-in-depth through optional cascade encryption.

This document provides a complete specification for implementors who wish to read or write `.aerovault` files in any programming language.

### Design Goals

1. **Nonce-misuse resistance**: AES-256-GCM-SIV (RFC 8452) prevents catastrophic failure on nonce reuse
2. **Memory-hard KDF**: Argon2id at OWASP 2024 high-security parameters resists GPU/ASIC attacks
3. **Key separation**: HKDF-SHA256 domain separation ensures independent keys for encryption, MAC, and filenames
4. **Tamper detection**: HMAC-SHA512 header integrity with constant-time comparison
5. **Metadata privacy**: Filenames encrypted with AES-256-SIV (deterministic, no nonce management)
6. **Crash safety**: Atomic write pattern protects against data loss during mutation operations
7. **Optional cascade**: ChaCha20-Poly1305 second layer for defense-in-depth

### Cryptographic Primitives

| Primitive | Algorithm | Standard |
|-----------|-----------|----------|
| Key Derivation | Argon2id | RFC 9106 |
| Key Expansion | HKDF-SHA256 | RFC 5869 |
| Key Wrapping | AES-256-KW | RFC 3394 / NIST SP 800-38F |
| Content Encryption | AES-256-GCM-SIV | RFC 8452 |
| Cascade Encryption | ChaCha20-Poly1305 | RFC 8439 |
| Filename Encryption | AES-256-SIV | RFC 5297 |
| Header Integrity | HMAC-SHA512 | RFC 2104 |

---

## 1. File Layout

An `.aerovault` file consists of three contiguous sections:

```
+========================+
|    Header (512 bytes)  |
+========================+
| Manifest Length (4 B)  |
| Encrypted Manifest     |
+========================+
|    Data Section        |
|  (encrypted chunks)   |
+========================+
```

---

## 2. Header (512 bytes, fixed)

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0 | 10 | Magic | ASCII `AEROVAULT2` |
| 10 | 1 | Version | `0x02` |
| 11 | 1 | Flags | Bitfield (see below) |
| 12 | 32 | Salt | Argon2id salt (cryptographically random) |
| 44 | 40 | Wrapped Master Key | AES-256-KW wrapped 32-byte master key |
| 84 | 40 | Wrapped MAC Key | AES-256-KW wrapped 32-byte MAC key |
| 124 | 4 | Chunk Size | `uint32` little-endian, default 65536 (64 KB) |
| 128 | 320 | Reserved | Zero-filled, reserved for future use |
| 448 | 64 | Header HMAC | HMAC-SHA512 integrity tag |

### 2.1 Flags (byte 11)

| Bit | Name | Description |
|-----|------|-------------|
| 0 | `cascade_mode` | Enable ChaCha20-Poly1305 second encryption layer |
| 1 | `hidden_volume` | Reserved for future hidden volume support |
| 2 | `keyfile_required` | Reserved for future keyfile support |
| 3-7 | — | Reserved, must be zero |

### 2.2 Validation

On open, implementations MUST verify:

1. Bytes 0-9 equal ASCII `AEROVAULT2`
2. Byte 10 equals `0x02`
3. Chunk size > 0 (if zero or negative, default to 65536)

---

## 3. Key Derivation

### 3.1 Argon2id (base KEK)

Derive a 32-byte base Key Encryption Key from the user password:

```
Algorithm:    Argon2id
Version:      1.3 (0x13)
Memory (m):   131072 KB (128 MiB)
Iterations:   4
Parallelism:  4
Salt:         header bytes [12..44] (32 bytes)
Output:       32 bytes
```

These parameters meet OWASP 2024 **high-security** recommendations. The 128 MiB memory cost makes GPU/ASIC attacks economically infeasible.

### 3.2 HKDF-SHA256 Domain Separation

From the base KEK, derive purpose-specific keys using HKDF-SHA256 (RFC 5869).

**HKDF implementation**: Extract-then-Expand.

- **Extract**: `PRK = HMAC-SHA256(salt=0x00*32, IKM=base_kek)`
- **Expand**: Standard HKDF-Expand with counter byte

Four domain-separated keys:

| Key | HKDF Info String (UTF-8) | Output Length |
|-----|--------------------------|---------------|
| KEK for master key | `AeroVault v2 KEK for master key` | 32 bytes |
| KEK for MAC key | `AeroVault v2 KEK for MAC key` | 32 bytes |
| SIV key (filenames) | `AeroVault v2 AES-SIV filename encryption` | 64 bytes |
| ChaCha20 key (cascade) | `AeroVault v2 ChaCha20-Poly1305 cascade` | 32 bytes |

The SIV key is 64 bytes because AES-SIV requires two 32-byte subkeys internally.

### 3.3 AES-256-KW Key Unwrapping (RFC 3394)

The master key and MAC key are stored wrapped in the header:

```
kek_master  = HKDF-Expand(base_kek, "AeroVault v2 KEK for master key", 32)
kek_mac     = HKDF-Expand(base_kek, "AeroVault v2 KEK for MAC key", 32)
master_key  = AES-256-KW-Unwrap(kek_master, header[44..84])   // 40 → 32 bytes
mac_key     = AES-256-KW-Unwrap(kek_mac,    header[84..124])  // 40 → 32 bytes
```

AES-KW adds 8 bytes of integrity (IV check). If unwrapping fails, the password is wrong.

> **Security note**: After use, `base_kek`, `kek_master`, and `kek_mac` MUST be securely zeroed.

---

## 4. Header Integrity Verification

After key derivation, verify the header has not been tampered with:

1. Copy the 512-byte header
2. Zero bytes 448-511 in the copy (where the HMAC is stored)
3. Compute `HMAC-SHA512(mac_key, zeroed_header)`
4. Compare with stored HMAC at header bytes 448-511

> **CRITICAL**: The comparison MUST be constant-time to prevent timing attacks.
> Use `crypto_verify_64`, `subtle.ConstantTimeCompare`, `MessageDigest.isEqual`,
> or equivalent.

If verification fails: wrong password or tampered file. Abort and zero all keys.

---

## 5. Manifest

### 5.1 Reading the Manifest

Immediately after the 512-byte header:

```
Offset 512:     uint32 LE — manifest_len (encrypted manifest size in bytes)
Offset 516:     manifest_len bytes — encrypted manifest blob
```

**Validation**: `manifest_len` MUST be > 0 and <= 67,108,864 (64 MiB). Reject larger values to prevent OOM attacks.

### 5.2 Manifest Decryption

The manifest blob is encrypted with **AES-256-SIV** (deterministic authenticated encryption):

```
siv_key          = HKDF-Expand(master_key, "AeroVault v2 AES-SIV filename encryption", 64)
manifest_decoded = Base64URL-Decode(manifest_blob)
manifest_json    = AES-SIV-Decrypt(siv_key, manifest_decoded, aad=empty)
```

> **Note**: The manifest blob is Base64URL-encoded (no padding) before storage.
> Decode it before decryption.

### 5.3 Manifest JSON Schema

```json
{
  "created": "2026-01-15T10:30:00Z",
  "modified": "2026-01-15T10:30:00Z",
  "description": "Optional vault description",
  "entries": [
    {
      "encrypted_name": "<base64url-encoded AES-SIV ciphertext>",
      "size": 12345,
      "offset": 0,
      "chunk_count": 1,
      "is_dir": false,
      "modified": "2026-01-15T10:30:00Z"
    }
  ]
}
```

#### Entry Fields

| Field | Type | Description |
|-------|------|-------------|
| `encrypted_name` | string | Base64URL-encoded AES-SIV ciphertext of the filename/path |
| `size` | integer | Original file size in bytes (0 for directories) |
| `offset` | integer | Byte offset within the data section |
| `chunk_count` | integer | Number of encrypted chunks (0 for directories) |
| `is_dir` | boolean | `true` for directory entries |
| `modified` | string | ISO 8601 timestamp |

### 5.4 Filename Decryption

Each `encrypted_name` is independently encrypted with AES-SIV:

```
siv_key    = HKDF-Expand(master_key, "AeroVault v2 AES-SIV filename encryption", 64)
name_bytes = Base64URL-Decode(entry.encrypted_name)
plaintext  = AES-SIV-Decrypt(siv_key, name_bytes, aad=empty)
filename   = UTF-8-Decode(plaintext)
```

**Path convention**: Nested files use `/` as separator (e.g., `documents/report.pdf`).

**Directory entries**: Have `is_dir: true`, `size: 0`, `offset: 0`, `chunk_count: 0`. They occupy no space in the data section.

---

## 6. Data Section

The data section starts immediately after the manifest:

```
data_offset = 512 + 4 + manifest_len
```

### 6.1 Chunk Layout

Each file is split into chunks of `chunk_size` bytes (last chunk may be smaller). Each chunk is independently encrypted:

```
For each chunk [i = 0, 1, 2, ...]:
  +---------------------------------+
  | chunk_len: uint32 LE (4 bytes)  |
  +---------------------------------+
  | encrypted_chunk (chunk_len B)   |
  |   = nonce (12B) ||              |
  |     ciphertext ||               |
  |     auth_tag (16B)              |
  +---------------------------------+
```

The `chunk_len` field contains the total size of the encrypted chunk blob (nonce + ciphertext + tag).

### 6.2 Chunk Decryption (AES-256-GCM-SIV)

```
aead = AES-256-GCM-SIV(master_key)

For chunk index i:
  aad       = uint32_le(i)          // 4-byte little-endian chunk index
  plaintext = aead.decrypt(encrypted_chunk, aad)
```

The AAD binding to chunk index prevents chunk reordering and duplication attacks.

The `encrypted_chunk` blob format follows the convention:

```
[12-byte nonce] [ciphertext] [16-byte authentication tag]
```

Most AES-GCM-SIV libraries (Tink, RustCrypto, libsodium) handle nonce prepending internally.

### 6.3 File Reconstruction

To extract a file at manifest entry `e`:

```
seek to: data_offset + e.offset
for i in 0..e.chunk_count:
    read 4 bytes → chunk_len (uint32 LE)
    read chunk_len bytes → encrypted_chunk
    plaintext = aead.decrypt(encrypted_chunk, aad=uint32_le(i))
    append plaintext to output
```

### 6.4 Cascade Mode (Optional)

When header flag bit 0 is set, each chunk undergoes **double encryption**:

```
Layer 1: AES-256-GCM-SIV (master_key)     — nonce-misuse resistant
Layer 2: ChaCha20-Poly1305 (chacha_key)    — algorithmic diversity
```

**Encryption order**: AES-GCM-SIV first, then ChaCha20-Poly1305.

**Decryption order**: ChaCha20-Poly1305 first (outer layer), then AES-GCM-SIV (inner layer).

```
chacha_key = HKDF-Expand(master_key, "AeroVault v2 ChaCha20-Poly1305 cascade", 32)

// Decryption:
outer_plaintext = ChaCha20Poly1305.decrypt(chacha_key, encrypted_chunk, aad=uint32_le(i))
final_plaintext = AesGcmSiv.decrypt(master_key, outer_plaintext, aad=uint32_le(i))
```

ChaCha20-Poly1305 uses a 12-byte nonce and 16-byte tag, prepended in the same format as GCM-SIV.

---

## 7. Vault Creation

### 7.1 Key Generation

```
salt        = random(32)                    // CSPRNG
master_key  = random(32)                    // CSPRNG
mac_key     = random(32)                    // CSPRNG
base_kek    = Argon2id(password, salt)      // 128 MiB, t=4, p=4
kek_master  = HKDF(base_kek, info_master)
kek_mac     = HKDF(base_kek, info_mac)
wrapped_mk  = AES-256-KW-Wrap(kek_master, master_key)   // 32 → 40 bytes
wrapped_mac = AES-256-KW-Wrap(kek_mac, mac_key)          // 32 → 40 bytes
```

### 7.2 Header Assembly

```
header = [0u8; 512]
header[0..10]   = "AEROVAULT2"
header[10]      = 0x02
header[11]      = flags
header[12..44]  = salt
header[44..84]  = wrapped_mk
header[84..124] = wrapped_mac
header[124..128] = uint32_le(chunk_size)    // default: 65536
// header[128..448] = reserved (zeros)
// Zero bytes 448..512 for HMAC computation
hmac = HMAC-SHA512(mac_key, header)
header[448..512] = hmac
```

### 7.3 Manifest Assembly

```
manifest_json = JSON.stringify({ created, modified, description, entries })
siv_key       = HKDF(master_key, info_siv, 64)
encrypted     = AES-SIV-Encrypt(siv_key, manifest_json_bytes, aad=empty)
encoded       = Base64URL-Encode-NoPad(encrypted)
```

### 7.4 File Encryption

For each file:

```
siv_key        = HKDF(master_key, info_siv, 64)
encrypted_name = Base64URL-NoPad(AES-SIV-Encrypt(siv_key, filename_bytes, aad=empty))

aead = AES-256-GCM-SIV(master_key)
for each chunk (up to chunk_size bytes):
    nonce = random(12)
    aad   = uint32_le(chunk_index)
    encrypted_chunk = aead.encrypt(nonce, plaintext_chunk, aad)
    write uint32_le(len(encrypted_chunk))
    write encrypted_chunk
```

---

## 8. Security Considerations

### 8.1 Zeroization

Implementations MUST securely zero all key material after use:

- Base KEK (Argon2id output)
- Domain-separated KEKs (kek_master, kek_mac)
- SIV key (64 bytes)
- ChaCha20 key (if cascade mode)
- Decrypted plaintext chunks (if privacy-sensitive)

Use `explicit_bzero()`, `SecureZeroMemory()`, `Arrays.fill()`, `sodium_memzero()`,
or equivalent non-optimizable zeroing.

### 8.2 Constant-Time Operations

- HMAC verification MUST use constant-time comparison
- AES-KW unwrap inherently provides integrity checking

### 8.3 Path Traversal Prevention

When extracting files, implementations MUST:

1. Reject filenames containing `..`
2. Reject absolute paths (starting with `/` or drive letter)
3. Reject null bytes in filenames
4. Canonicalize the destination path and verify it remains within the target directory

### 8.4 Manifest Size Limits

Reject manifests larger than 64 MiB to prevent memory exhaustion attacks.

### 8.5 Crash Safety

Mutation operations (add, delete, change password) SHOULD use atomic write patterns:

1. Write new vault to a temporary file
2. Rename original to `.bak`
3. Rename temporary to original
4. Delete `.bak`

---

## 9. Implementation Guide

### 9.1 Required Libraries by Language

#### Rust

```toml
[dependencies]
argon2 = "0.5"            # Argon2id
hkdf = "0.12"             # HKDF-SHA256
aes-kw = "0.2"            # AES Key Wrap (RFC 3394)
aes-gcm-siv = "0.11"      # AES-256-GCM-SIV (RFC 8452)
aes-siv = "0.7"           # AES-256-SIV (RFC 5297)
chacha20poly1305 = "0.10"  # ChaCha20-Poly1305 (cascade)
hmac = "0.12"              # HMAC-SHA512
sha2 = "0.10"              # SHA-256 / SHA-512
data-encoding = "2"        # Base64URL
secrecy = "0.10"           # Secure zeroization
```

#### Java / Android

```gradle
// BouncyCastle — Argon2id, HKDF, HMAC
implementation 'org.bouncycastle:bcprov-jdk18on:1.80'

// Google Tink — AES-GCM-SIV, AES-SIV
implementation 'com.google.crypto.tink:tink-android:1.15.0'
// For non-Android: 'com.google.crypto.tink:tink:1.15.0'
```

Key classes:
- `com.google.crypto.tink.aead.subtle.AesGcmSiv` for AES-256-GCM-SIV
- `com.google.crypto.tink.subtle.AesSiv` for AES-256-SIV (requires 64-byte key)
- `javax.crypto.Cipher` with `"AESWrap"` for AES-KW
- `org.bouncycastle.crypto.generators.Argon2BytesGenerator` for Argon2id

#### Python

```python
# pip install argon2-cffi cryptography pysodium
from argon2.low_level import hash_secret_raw, Type  # Argon2id
from cryptography.hazmat.primitives.keywrap import aes_key_unwrap  # AES-KW
from cryptography.hazmat.primitives.ciphers.aead import AESGCMSIV  # GCM-SIV
from miscreant import SIV  # AES-SIV (pip install miscreant)
import hmac, hashlib  # HMAC-SHA512, HKDF
```

#### Go

```go
import (
    "golang.org/x/crypto/argon2"           // Argon2id
    "golang.org/x/crypto/hkdf"             // HKDF-SHA256
    "crypto/cipher"                         // AES-KW (manual or via NaCl)
    "github.com/miscreant/miscreant.go"     // AES-SIV
    // AES-GCM-SIV: use Tink Go or manual implementation
)
```

#### C / C++

```c
// libsodium provides: Argon2id, ChaCha20-Poly1305, HMAC-SHA512
// OpenSSL 3.x provides: AES-GCM-SIV (via EVP), AES-KW, HKDF
// For AES-SIV: use libaes-siv or implement per RFC 5297
```

#### JavaScript / TypeScript (Node.js)

```javascript
// npm install argon2 @noble/ciphers @noble/hashes
import { argon2id } from 'argon2';
import { gcm_siv } from '@noble/ciphers/aes';      // AES-GCM-SIV
import { siv } from '@noble/ciphers/aes';           // AES-SIV
import { hkdf } from '@noble/hashes/hkdf';          // HKDF-SHA256
import { hmac } from '@noble/hashes/hmac';           // HMAC-SHA512
// AES-KW: crypto.subtle.unwrapKey() with "AES-KW" algorithm
```

### 9.2 Pseudocode: Opening a Vault (Read-Only)

```
function open_vault(file_path, password):
    f = open(file_path, "rb")

    // 1. Read and validate header
    header = f.read(512)
    assert header[0:10] == "AEROVAULT2"
    assert header[10] == 0x02
    flags       = header[11]
    salt        = header[12:44]
    wrapped_mk  = header[44:84]
    wrapped_mac = header[84:124]
    chunk_size  = uint32_le(header[124:128])
    stored_hmac = header[448:512]
    cascade     = (flags & 0x01) != 0

    // 2. Key derivation
    base_kek   = argon2id(password, salt, m=128*1024, t=4, p=4, len=32)
    kek_master = hkdf_expand_sha256(base_kek, "AeroVault v2 KEK for master key", 32)
    kek_mac    = hkdf_expand_sha256(base_kek, "AeroVault v2 KEK for MAC key", 32)
    secure_zero(base_kek)

    // 3. Unwrap keys
    master_key = aes_kw_unwrap(kek_master, wrapped_mk)   // fails if wrong password
    mac_key    = aes_kw_unwrap(kek_mac, wrapped_mac)
    secure_zero(kek_master, kek_mac)

    // 4. Verify header integrity
    header_copy = copy(header)
    header_copy[448:512] = zeros
    computed_hmac = hmac_sha512(mac_key, header_copy)
    assert constant_time_equal(stored_hmac, computed_hmac)

    // 5. Read manifest
    f.seek(512)
    manifest_len = uint32_le(f.read(4))
    assert 0 < manifest_len <= 64 * 1024 * 1024
    manifest_blob = f.read(manifest_len)

    // 6. Decrypt manifest
    siv_key = hkdf_expand_sha256(master_key, "AeroVault v2 AES-SIV filename encryption", 64)
    manifest_decoded = base64url_decode(manifest_blob)
    manifest_json = aes_siv_decrypt(siv_key, manifest_decoded, aad=empty)
    manifest = json_parse(manifest_json)

    // 7. Decrypt filenames
    for entry in manifest.entries:
        name_ct = base64url_decode(entry.encrypted_name)
        entry.name = utf8_decode(aes_siv_decrypt(siv_key, name_ct, aad=empty))
    secure_zero(siv_key)

    data_offset = 512 + 4 + manifest_len
    return { master_key, mac_key, manifest, data_offset, chunk_size, cascade }
```

### 9.3 Pseudocode: Extracting a File

```
function extract_file(vault, entry, dest_path):
    f = vault.file
    f.seek(vault.data_offset + entry.offset)

    aead = AES_256_GCM_SIV(vault.master_key)

    if vault.cascade:
        chacha_key = hkdf_expand_sha256(
            vault.master_key,
            "AeroVault v2 ChaCha20-Poly1305 cascade", 32
        )
        chacha = ChaCha20Poly1305(chacha_key)

    out = open(dest_path, "wb")
    for i in 0..entry.chunk_count:
        chunk_len = uint32_le(f.read(4))
        encrypted = f.read(chunk_len)
        aad = uint32_le(i)

        if vault.cascade:
            encrypted = chacha.decrypt(encrypted, aad)

        plaintext = aead.decrypt(encrypted, aad)
        out.write(plaintext)

    out.close()
    if vault.cascade:
        secure_zero(chacha_key)
```

---

## 10. Test Vectors

### 10.1 HKDF Info Strings (exact bytes)

```
Master KEK:  41 65 72 6f 56 61 75 6c 74 20 76 32 20 4b 45 4b
             20 66 6f 72 20 6d 61 73 74 65 72 20 6b 65 79

MAC KEK:     41 65 72 6f 56 61 75 6c 74 20 76 32 20 4b 45 4b
             20 66 6f 72 20 4d 41 43 20 6b 65 79

SIV:         41 65 72 6f 56 61 75 6c 74 20 76 32 20 41 45 53
             2d 53 49 56 20 66 69 6c 65 6e 61 6d 65 20 65 6e
             63 72 79 70 74 69 6f 6e

ChaCha20:    41 65 72 6f 56 61 75 6c 74 20 76 32 20 43 68 61
             43 68 61 32 30 2d 50 6f 6c 79 31 33 30 35 20 63
             61 73 63 61 64 65
```

### 10.2 Magic Bytes

```
Hex:   41 45 52 4f 56 41 55 4c 54 32
ASCII: A  E  R  O  V  A  U  L  T  2
```

### 10.3 AAD for Chunk Index 0

```
Hex: 00 00 00 00    (uint32 LE)
```

### 10.4 AAD for Chunk Index 42

```
Hex: 2a 00 00 00    (uint32 LE)
```

---

## 11. Constants Summary

```
MAGIC                = "AEROVAULT2" (10 bytes)
VERSION              = 0x02
HEADER_SIZE          = 512
SALT_SIZE            = 32
WRAPPED_KEY_SIZE     = 40  (32-byte key + 8-byte AES-KW overhead)
NONCE_SIZE           = 12  (GCM-SIV and ChaCha20)
TAG_SIZE             = 16  (GCM-SIV and ChaCha20)
MASTER_KEY_SIZE      = 32
MAC_KEY_SIZE         = 32
DEFAULT_CHUNK_SIZE   = 65536  (64 KB)
MAX_MANIFEST_SIZE    = 67108864  (64 MiB)

ARGON2_MEMORY        = 131072 KB  (128 MiB)
ARGON2_ITERATIONS    = 4
ARGON2_PARALLELISM   = 4
ARGON2_OUTPUT_LEN    = 32
ARGON2_VERSION       = 0x13  (v1.3)
```

---

## 12. Reference Implementations

| Language | Implementation | Status |
|----------|---------------|--------|
| **Rust** | [AeroFTP Desktop](https://github.com/AXP-dev-team/aeroftp) `src-tauri/src/aerovault_v2.rs` | Production |
| **Java** | [AeroFTP Mobile](https://github.com/AXP-dev-team/aeroftp) `android/.../VaultPlugin.java` | Production |

---

## 13. Version History

| Version | Date | Changes |
|---------|------|---------|
| 2.0 | March 2026 | Initial public specification |

---

## License

This specification is released under the [MIT License](../LICENSE). Implementations of the AeroVault v2 format are free to use in both open-source and commercial software.

---

*AeroVault v2 is part of the [AeroFTP](https://github.com/AXP-dev-team/aeroftp) ecosystem by AXP Development.*
