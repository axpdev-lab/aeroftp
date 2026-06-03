# AeroVault On-Disk Formats

AeroFTP can read and write three distinct encrypted-container families. They are
**deliberately separate formats with distinct magic bytes** and are **not
mutually interoperable**: each parser fail-closes when handed a container of a
different family. This document is the authoritative format/naming contract
(addresses the dual-audit finding CODEX-AV-005).

## TL;DR

| GUI level / CLI flag | On-disk family | Magic (bytes 0..10) | Internal version byte | Header size | Implemented in |
|----------------------|----------------|---------------------|-----------------------|-------------|----------------|
| "Standard"* / `vault_create` | v1 ZIP (WinZip AE) | `PK\x03\x04` (ZIP) | n/a | ZIP metadata | `aerovault.rs` |
| "Advanced", "Paranoid" / CLI `-V v2` | **`AEROVAULT2`** | `AEROVAULT2` | `3` (current) or `2` (legacy read-only) | 512 bytes | `aerovault` crate (`aerovault_v2.rs` wrapper) |
| "Experimental" / CLI `-V v3` | **`AEROVAULT3`** | `AEROVAULT3` | `3` | 1024 bytes | `aerovault_v3.rs` (app-native) |

\* As of the 2026-06-03 remediation the GUI "Standard" level creates an
`AEROVAULT2` container (Argon2id + AES-256-GCM-SIV), not a v1 ZIP. The v1 ZIP
path remains only for **opening** legacy `.aerovault` files.

## Why two things are both called "v3"

This is the naming trap the audit flagged, stated plainly so nobody trips on it
again:

- The **`aerovault` crate** uses an internal *format version byte* whose current
  value is `3`. That byte lives inside an **`AEROVAULT2`**-magic container. The
  `3` denotes the hardened per-chunk AAD binding (file id + chunk count + chunk
  index, the CRYPTO-01 fix); it does **not** mean "AeroVault format 3".
- The **app-native `AEROVAULT3`** format is an entirely different container
  (1024-byte header, content-defined chunking, dedup, manifest wrapper chain).

So "`AEROVAULT2` version 3" (crate) and "`AEROVAULT3`" (app) are **different
formats that share the digit 3 by coincidence of versioning, not by design.**

- CLI `aeroftp-cli vault create -V v2` → `AEROVAULT2` container (internal version
  byte `3`, the current hardened crate format).
- CLI `aeroftp-cli vault create -V v3` → `AEROVAULT3` container (app-native).

## Cross-open behaviour (intentional, fail-closed)

| Open with \ Container | `AEROVAULT2` | `AEROVAULT3` | v1 ZIP |
|-----------------------|--------------|--------------|--------|
| crate parser (v2)     | opens        | rejected: "invalid magic bytes (not an AeroVault v2 file)" | rejected |
| app parser (v3)       | rejected: "Not an AeroVault v3 file" | opens | rejected |
| ZIP parser (v1)       | rejected     | rejected     | opens  |

Cross-family opens are **rejected before any allocation or decrypt** by a magic
check, so there is no cross-parser confusion or type-confusion primitive. This
non-interoperability is by design: the two binary layouts are not convertible in
place. To move data between families, extract from one and re-add to the other
(the GUI and CLI both expose extract + create).

## Header authentication

- **`AEROVAULT2` (crate):** 512-byte header, HMAC-SHA512 over the full 512 bytes
  with the 64-byte MAC field zeroed. The reserved region (bytes 128..448) is now
  carried through the struct so the MAC covers it and non-zero reserved bytes are
  rejected on read (AV-012).
- **`AEROVAULT3` (app):** 1024-byte header, HMAC-SHA512 with the MAC field at
  offset 960; data/manifest/extension ranges are bounds-checked after the MAC
  verifies. The wrapper-header version and the manifest wrapper algorithm ids are
  validated on open; unknown values are rejected rather than decoded with the
  hardcoded cipher stack (AV-024 / CODEX-AV-006).

## Chunk AAD sizes (for spec readers)

- `AEROVAULT2` v3 chunk AAD = `b"AeroVault v2 chunk aad v3"` (25) + file_id (16) +
  chunk_count (4) + chunk_index (4) = **49 bytes**.
- `AEROVAULT2` v2 (legacy) chunk AAD = 4-byte chunk index only.
- `AEROVAULT3` block AAD = `b"AeroVault v3 block"` + block_index (u64) + 32-byte
  keyed-blake3 chunk id.

_Last updated: 2026-06-03 (dual-independent audit remediation)._
