# AeroVault v3 Format Specification

**Version**: Draft 0.1
**Status**: Implementation draft
**Date**: 2026-05-11
**Authors**: axpdev-lab

> **Roadmap attribution.** The wrapper-stack design this format implements (the wrapper-versus-step taxonomy, the corrected AES-256-GCM-SIV avalanche framing, algorithm versioning as a forward-compatibility clause, the small-file-packing model and the chunking trade analysis) is a sustained community design contribution by **Ehud Kirsh** in the AeroFTP [COMMUNITY ROADMAP thread (issue #162)](https://github.com/axpdev-lab/aeroftp/issues/162). The conversation shaped both the v3 architecture and this specification.

---

## 1. Purpose

AeroVault v3 is the first wrapper-stack vault format. It keeps the single-file `.aerovault` portability of v2 while adding compressed, content-addressed chunks and a forward-compatible extension directory for future recovery data.

The v3 design is intentionally shaped so that AeroVault v4 can be "v3 plus ECC", not a second incompatible archive format.

## 2. Wrapper Pipeline

The canonical v3 write pipeline is:

```text
plaintext files
  -> logical packing / small-file batching
  -> content-defined chunking
       -> keyed BLAKE3 chunk id
  -> zstd compress each chunk/frame
  -> AES-256-GCM-SIV encrypt each compressed chunk
  -> manifest + block table
  -> optional extension blocks (ECC in v4)
```

The ordering is deliberate:

- `packing` concatenates files smaller than the small-file threshold (v3 default: the CDC minimum, 256 KiB) into shared packs before chunking, so a tree of tiny files still yields multi-MiB chunks. The pack carries no per-file framing: the manifest is the index. Each packed file records the chunks that cover its byte span and `pack_offset`, the offset of its first byte inside the first covering chunk. Files at or above the threshold take the per-file path (`pack_offset` absent, equivalent to offset 0);
- chunking precedes compression so deduplication, resume, and future AeroSync range semantics stay chunk-aligned;
- compression is per chunk/frame so a reader can decompress one logical block without inflating the whole archive;
- encryption is last among v3 data-transforming wrappers;
- ECC is not part of v3, but the container has the extension slot v4 will use.

## 3. Wrapper And Algorithm IDs

Every wrapper layer has both an algorithm id and an algorithm version. Readers dispatch on these fields rather than on the container version alone.

| Wrapper | v3 default | Version |
|---|---|---|
| `packing` | `small-file-batching` | `1` |
| `chunking` | `gear-cdc` | `1` |
| `chunk_id` | `blake3-keyed-128` | `1` |
| `compression` | `zstd` | `1` |
| `crypt` | `aes-256-gcm-siv` | `1` |
| `cipher_hash` | `blake3-256` | `1` |
| `ecc` | absent in v3 | reserved |

The `chunking` wrapper carries an optional `bounds` object recording the
content-defined-chunking parameters the writer used:

```json
"chunking": { "algorithm_id": "gear-cdc", "algorithm_version": 1,
              "bounds": { "min": 262144, "avg": 1048576, "max": 4194304 } }
```

`avg` MUST be a power of two. A reader uses the recorded `bounds`; when the
field is absent (pre-GAP-5 v3 vaults, or any non-`chunking` wrapper) it falls
back to the const defaults `min = 256 KiB, avg = 1 MiB, max = 4 MiB`, which
keeps every existing vault byte-identical. Bounds only affect the write path
(chunk boundaries and therefore chunk ids); extraction never re-chunks.

The default zstd level and CDC bounds are profile based:

| Profile | Level | CDC min / avg / max | Intended use |
|---|---:|---|---|
| `fast` | 3 | 256 KiB / 1 MiB / 4 MiB | quick local work |
| `balanced` | 9 | 256 KiB / 1 MiB / 4 MiB | default v3 vaults |
| `archive` | 19 | 1 MiB / 4 MiB / 16 MiB | cold storage / export |

`archive` widens the per-chunk zstd window for ratio at the cost of
finer-grained dedup; the wrapper-id/version surface is unchanged so older v3
readers that predate GAP-5 still dispatch correctly (they just apply the const
bounds, which is wrong only for `archive` vaults and is a forward-compat
limitation noted here rather than a silent corruption: ids would differ on a
re-add, never on extraction).

## 4. File Layout

All multi-byte integers are little-endian.

```text
offset 0
┌─────────────────────────────────────┐
│ Header (1024 bytes)                 │
├─────────────────────────────────────┤
│ Encrypted chunk blocks              │
│   [block_len:u64][cipher_block:N]   │
│   ...                               │
├─────────────────────────────────────┤
│ Encrypted manifest                  │
├─────────────────────────────────────┤
│ Extension directory JSON            │
├─────────────────────────────────────┤
│ Extension payloads (v4+)            │
└─────────────────────────────────────┘
```

The data section always starts immediately after the header. This keeps existing encrypted blocks stable when the manifest grows: a writer can rebuild the header and manifest without shifting the data section.

## 5. Header

The fixed header is 1024 bytes.

| Offset | Size | Field |
|---:|---:|---|
| 0 | 10 | magic: `AEROVAULT3` |
| 10 | 1 | format major: `3` |
| 11 | 1 | header flags |
| 12 | 32 | Argon2id salt |
| 44 | 40 | AES-256-KW wrapped master key |
| 84 | 40 | AES-256-KW wrapped MAC key |
| 124 | 4 | header length (`1024`) |
| 128 | 8 | data offset |
| 136 | 8 | data length |
| 144 | 8 | manifest offset |
| 152 | 8 | manifest length |
| 160 | 8 | extension directory offset |
| 168 | 8 | extension directory length |
| 176 | 8 | extension payload offset |
| 184 | 8 | extension payload length |
| 192 | 2 | wrapper header version |
| 194 | 2 | reserved |
| 196 | 764 | reserved, zero-filled |
| 960 | 64 | HMAC-SHA512 over the full header with bytes 960..1024 zeroed |

Readers MUST reject unknown non-zero reserved fields until a future spec assigns them.

## 6. Extension Directory

The extension directory is a UTF-8 JSON array. v3 writers emit `[]`.

```json
[
  {
    "extension_id": "ecc.reed-solomon",
    "algorithm_id": "reed-solomon",
    "algorithm_version": 1,
    "critical": false,
    "offset": 123,
    "length": 456
  }
]
```

Unknown non-critical extensions are skipped. Unknown critical extensions make the vault unsupported. This is the v3/v4 compatibility contract: v4 ECC is expected to be a non-critical extension for data extraction and a critical extension only for workflows that promise repair guarantees.

## 7. Manifest

The manifest is AES-256-GCM-SIV encrypted. Its plaintext is JSON:

```json
{
  "format": 3,
  "created": "2026-05-11T00:00:00Z",
  "modified": "2026-05-11T00:00:00Z",
  "wrappers": {
    "packing": { "algorithm_id": "small-file-batching", "algorithm_version": 1 },
    "chunking": { "algorithm_id": "gear-cdc", "algorithm_version": 1 },
    "chunk_id": { "algorithm_id": "blake3-keyed-128", "algorithm_version": 1 },
    "compression": { "algorithm_id": "zstd", "algorithm_version": 1, "level": 9 },
    "crypt": { "algorithm_id": "aes-256-gcm-siv", "algorithm_version": 1 },
    "cipher_hash": { "algorithm_id": "blake3-256", "algorithm_version": 1 }
  },
  "entries": [],
  "chunks": {}
}
```

`entries` describe user-visible files and directories. Each file entry carries `path`, `size`, `modified`, `is_dir`, `chunks` (the ordered chunk ids that contain its bytes) and an optional `pack_offset`. `pack_offset` is the byte offset of the file inside the concatenation of its listed chunks; when absent the file owns its chunks whole from offset 0 (the per-file path and all pre-packing v3 vaults). `chunks` is keyed by the 128-bit keyed-BLAKE3 chunk id and stores block location metadata.

## 8. Hash Separation

v3 deliberately separates two hashes:

- `chunk_id`: keyed BLAKE3, truncated to 128 bits, over plaintext chunk bytes. It is used for content addressing and deduplication and is stored only inside the encrypted manifest.
- `cipher_hash`: full BLAKE3-256 over the encrypted block. It is used by scrub/ECC workflows to identify damaged stored bytes before decryption.

The chunk-id key is derived from the vault master key by HKDF. Chunk IDs are not raw public hashes of user content.

## 9. Backward Compatibility

Compatibility rules:

- v3-capable AeroFTP MUST continue to read v1 and v2 vaults through their existing readers.
- v4-capable AeroFTP MUST read v3 vaults without migration.
- v3 readers MUST be able to extract data from a v4 vault when all unknown extensions are non-critical.
- v3 readers MUST refuse a vault with an unknown critical extension rather than silently degrading a promised safety property.

## 10. AeroVault v2 Spec Correction

The v2 wire format stores the HMAC-SHA512 at bytes `448..512` and computes it over all 512 header bytes with that MAC field zeroed. Earlier prose in the v2 spec described the MAC as if it lived at `128..192`; that was documentation drift, not the implementation contract.

## 11. v4 Evolution Note (T-AEROVAULT-ECC, shipped)

v4 = v3 + non-critical "ecc.reed-solomon" extension (always critical=false). The extension carries a v2 Reed-Solomon payload (AVEC magic, version=2, K=10/P=2 fixed grid over the concatenated live-block stream, per-shard 16-byte truncated BLAKE3 for erasure localization, parity data). 

- Overhead target: ~P/K = 20% (clamped shard size; proven on real incompressible data).
- Damage model: per-shard cksums (not just per-block cipher_hash) so rot inside a large CDC chunk only erases the affected shards; a bad parity shard is detected and routed around.
- Repair contract (all-or-nothing): reconstruct, re-verify *every* repaired block against its manifest cipher_hash; persist the re-sealed vault only if *all* verify, else leave the file byte-for-byte untouched.
- scrub/repair exposed in GUI (draggable modals, template-faithful) and CLI (`vault scrub`, `vault repair [--dry-run]`).
- Receipt/VaultReport carries ecc_* fields (shards_generated, bytes_protected, overhead_pct, repair_events) for ops on ECC vaults.
- Forward-compat: pure v3 open/extract path ignores the non-critical ext and still works (magic stays AEROVAULT3 / format=3).
- Pipeline position: ECC is the *fourth* first-class wrapper, after crypt (see #276 wrapper-stack discussion).

Implementation, tests (22), live proof, surfaces (P3), docs and close tracked in `docs/dev/roadmap/APPENDIX-AEROVAULT-V4-ECC/`. "v3 + ECC = v4".

See also: CHANGELOG (Unreleased), SECURITY.md (formats table), CLI-GUIDE (vault subcommand), ROADMAP.
