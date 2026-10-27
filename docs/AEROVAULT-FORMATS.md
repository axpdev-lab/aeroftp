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
| "Archive" / CLI `-V v3` | **`AEROVAULT3`** | `AEROVAULT3` | `3` | 1024 bytes | `aerovault_v3.rs` (app-native) |
| "AeroVault Zip" / CLI `archive` | **`AEROVAULT3` plaintext** | `AEROVAULT3` | `3` | 1024 bytes | `aerovault_v3.rs` + `cli_aerovz.rs` |

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

## The plaintext sibling: AeroVault Zip (`.aerozip`)

`.aerozip` (`application/x-aerozip`, GUI "AeroVault Zip", CLI
`aeroftp-cli archive create/list/extract`) is **not a fourth encrypted family**.
It is the **`AEROVAULT3` wrapper stack with the cipher layers turned off**: same
`AEROVAULT3` magic and 1024-byte header, content-defined chunking, per-chunk zstd
compression and optional Reed-Solomon recovery parity, but **no Argon2id key
derivation and no AES/ChaCha20 encryption**. It is integrity-and-recovery only and
**provides no confidentiality**: anyone with the file can read its contents.

- Recovery parity is on by default and opt-out: `--recovery-level 0` (also `off` /
  `none`) creates a smaller, parity-free archive.
- It is fail-closed against the encrypted lane in both directions: the `archive`
  parsers reject an encrypted `AEROVAULT3` ("it is encrypted"), and the `vault`
  parsers reject a plaintext one. A header flag distinguishes the two.

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
(the GUI and CLI both expose extract + create). The shipped **Change Mode** operation performs this extract-and-rebuild (v2<->v3, and v2 standard<->cascade). It is available from an open vault next to Change Password and through `aeroftp-cli vault change-mode`, keeping the password.

`repack_vault_blocking` extracts the full plaintext tree into a `.aerovault-repack-*` temporary directory beside the vault before building the replacement. The filesystem needs free space for both that plaintext tree and the rebuilt container, alongside the existing vault. Normal cleanup removes the scratch directory, but an abrupt interruption can leave plaintext behind; choose a suitable filesystem and check for leftovers after an interrupted operation.

Replacement uses two renames: the original is moved to `.<vault-name>.repack-bak-<pid>`, then the rebuilt container is moved to the original path. This is not an atomic swap. If the process stops between renames, the original path is absent and the original vault must be restored from the backup. A reported failure of the second rename attempts to restore that backup.

Change Mode does not automatically preserve recovery protection. A v3-to-v2 conversion drops embedded parity, and every successful repack attempts to delete the old default `<vault>.aerocorrect` sidecar because it describes the previous bytes. Deletion is best effort; any leftover old sidecar is stale and must not be reused. The CLI `aeroftp-cli vault change-mode` defaults `--error-correction` to false. For a v3 target, explicitly enable Error Correction in the GUI or pass `--security-level archive --error-correction` in the CLI to build fresh embedded parity. A v2 target has no embedded parity; generate a new standalone sidecar for the rebuilt file with `aeroftp-cli correct gen`. Keep a separate verified backup before conversion and verify the new recovery data afterward.

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
- `AEROVAULT3` block AAD = `b"AeroVault v3 block"` + block_index (u64 LE) + the
  128-bit keyed-BLAKE3 chunk id as its 32-character hex string.

_Last updated: 2026-10-26._
