# The AeroFTP Wrapper Stack

> Status: living document. Covers the at-rest transformation pipeline shared by
> AeroVault (static container) and, progressively, AeroSync (streaming engine).
>
> The structure of this page, the wrapper-versus-step taxonomy, the corrected
> avalanche framing, the algorithm-versioning clause, the small-file-packing
> model and the chunking trade analysis are a sustained design contribution by
> **Ehud Kirsh** in the AeroFTP COMMUNITY ROADMAP thread
> ([issue #162](https://github.com/axpdev-lab/aeroftp/issues/162)). The
> conversation shaped both the architecture and this document. Credit is his.

## Why this document exists

AeroFTP is not only a transfer tool. AeroVault is an encrypted archival format,
and the same transformations that protect a sealed `.aerovault` container are
the ones AeroSync needs for resilient cloud backup. Rather than implement four
ad-hoc features, AeroFTP treats them as a single ordered pipeline of
**wrappers**. One stack, one mental model, one audit pass.

## Wrappers and steps

A precise vocabulary, because it determines what is a public, versioned,
auditable surface and what is an internal helper:

- A **wrapper** is a transformation that is useful on its own. There are
  exactly four: **compression**, **chunking**, **encryption**, **error
  correction**.
- A **step** is a sub-component that is only meaningful inside a wrapper. It is
  never exposed as a standalone feature.

Examples of steps:

| Step | Belongs to | Role |
|---|---|---|
| Small-file packing (tape-archiving) | chunking / compression | concatenate small files so the chunker and compressor see a wide stream |
| RNG / nonce generation | encryption | produce the per-chunk nonce |
| Content-defined boundary detection | chunking | decide where one chunk ends and the next begins |
| Deduplication (index step) | chunking | use chunk ids to decide which chunks already exist at the destination |

Deduplication deserves a note because it is often mistaken for a wrapper. It
cannot exist without chunking: it consumes chunk identifiers and decides which
chunks to skip. It is the **index step of the chunking wrapper**, not a wrapper
and not a separate concern. It has two scopes, same mechanism, different
payoff:

- **Intra-file**: which chunks of this file changed since the last sync.
- **Cross-file / cross-snapshot**: this chunk already exists because another
  file or an earlier run produced it.

In the codebase the `Wrapper` trait is implemented only for the four real
wrappers; steps live as private helpers inside their wrapper. The public
surface stays honest, and so does this document.

## The order of the four wrappers, and why

```
plaintext
  -> packing (step: concatenate small files)
  -> chunking (content-defined boundaries; BLAKE3 chunk id; dedup index step)
  -> compression (per chunk)
  -> encryption (AEAD per chunk)
  -> error correction (parity over cipher blocks)   [AeroVault v4 / dedicated track]
  -> container / object store
```

The order is not arbitrary.

### Compression before encryption

Picture a spectrum. At one end, perfect order: a file of all zeros. At the
other end, complete chaos: data that looks fully random with no structure. A
compression algorithm saves space by recognising structure and patterns. It is
very effective near the ordered end and useless near the random end.

Encryption is designed to make its output statistically indistinguishable from
random. That is a security property, not a compression goal, but the side
effect is that ciphertext sits at the chaotic end of the spectrum and is
essentially incompressible. Therefore data must be **compressed before it is
encrypted**. Compressing after encrypting saves nothing.

A note on *why* the ciphertext looks random, because it is easy to state this
imprecisely:

> Cryptographic hashes (Argon2, scrypt, SHA-2/3, BLAKE2/3, RIPEMD) and block
> ciphers in feedback modes (AES-CBC, AES-CFB) exhibit a full avalanche effect:
> a single-bit change in the input produces a fully-changed output. AEAD modes
> built on top of stream-cipher constructions (AES-GCM, AES-GCM-SIV,
> ChaCha20-Poly1305) do not have this property in their ciphertext body: a
> single-bit plaintext change produces a single-bit ciphertext change. The
> avalanche surface in those modes lives in the authentication tag, which
> detects any tampering or corruption at decrypt time. AeroVault uses
> AES-256-GCM-SIV, so this is the distinction that applies to its at-rest
> format.

The "compress before encrypt" rule still holds, because the keystream itself is
statistically indistinguishable from random and the ciphertext inherits that
property regardless of mode. The justification routes through "the keystream
looks random", not "the cipher has avalanche on the body".

### Chunking before compression

A single compression stream over the whole input destroys content-defined
chunk boundaries: any byte shift in the source cascades through the compressed
stream and relocates every downstream boundary. That defeats deduplication,
defeats resume, and breaks the chunk-range semantics AeroSync depends on.
AeroFTP therefore chunks first, then compresses each chunk independently.
Per-chunk compression also keeps random access cheap: a reader inflates one
logical block instead of the whole stream.

The cost is some compression ratio, because each chunk's compressor sees less
context. For sealed containers where ratio matters most (the `archive`
profile), the lever is the chunk size, not the wrapper order (see below).

### Small-file packing comes first

Chunking and compression are both more effective the more data they can look
at. The first step is therefore to concatenate small files into a wider
stream, historically called tape-archiving. AeroFTP calls it
**small-file packing** in the internal architecture and "tape-archiving
(small-file batching)" once on first introduction in user docs. The pack is a
pure concatenation of the small files' bytes; the manifest is the index
(offset and length per file). There are no per-file frame headers inside the
pack: tar-ish framing without the metadata bloat. Files above an
engine-derived threshold keep a per-file path.

## Algorithm versioning is a forward-compatibility clause

A format that lives for years across many releases must be able to swap an
algorithm without breaking older artifacts. Every wrapper layer carries an
explicit `algorithm_id` and `algorithm_version` in the frame header. A reader
dispatches on those fields instead of hard-coding primitives, so a future build
can introduce a better compressor or a new Error Correction scheme and still read every old
vault.

The AeroVault v3 defaults are:

| Wrapper | v3 default | Version |
|---|---|---|
| `packing` | `small-file-batching` | 1 |
| `chunking` | `gear-cdc` | 1 |
| `chunk_id` | `blake3-keyed-128` | 1 |
| `compression` | `zstd` | 1 |
| `crypt` | `aes-256-gcm-siv` | 1 |
| `cipher_hash` | `blake3-256` | 1 |
| `ecc` | absent in v3 (reserved extension slot) | n/a |

v4 reuses the same header layout and only adds the `ecc` field, so
**v3 + error correction = v4** and a v3-only build opens a v4 vault for the
data it understands.

A teaching example most Linux users already have on disk: `.tar.gz` and
`.pkg.tar.zst`. Both tape-archive (`tar`) before compressing. The Arch package
format moved from an older compressor to Zstandard (`zst`) without changing
the "archive then compress" shape: exactly the algorithm-versioning move,
applied to a real-world format.

A deliberate non-claim: this document does not predict that AI or quantum
computing will reshape lossless general-purpose compression for backup
workloads. Lossless compression is bounded by source entropy, and backup data
is mostly already-compressed media and archives. The algorithm slot is
swappable regardless; the document versions the slot, it does not predict the
swap.

## Chunking in depth

### What chunking buys you

1. **Bypass per-file size caps on free tiers.** Chunking is what lets a
   multi-GB file land on a provider that caps single files. Representative
   free-tier single-file maxima:

   | Provider | Free-tier max single file |
   |---|---|
   | FileLu | 10 GB |
   | 4shared | 2 GB |
   | Yandex Disk | 1 GB |
   | Uploadcare | 500 MB |
   | Box | 250 MB |
   | OpenDrive | 100 MB |

2. **Metadata obfuscation.** An observer (provider, attacker, subpoena) sees
   similarly sized opaque chunks and cannot tell an executable from a video
   from a note, assuming names and contents are encrypted.
3. **Parallel transfer** of a large file's chunks.
4. **Deduplication** (the index step above): only changed chunks move.
5. **Cheap resume**: an interrupted transfer only repeats the in-flight chunk.
6. **Pooling (future, `T-POOLING`)**: because the manifest addresses chunks by
   id and does not care where a chunk physically lives, a placement policy can
   spread chunk ids across several saved provider profiles, RAID0-like across
   free tiers. Honest caveats: durability becomes the product of N providers,
   not the maximum (pair it with the error-correction wrapper), and a restore
   needs every pooled profile reachable. This is roadmap, not shipped.

### Ideal chunk size: a trade curve, not a constant

Smaller chunks: less bandwidth wasted on an interrupted transfer, finer dedup
granularity on small edits, but more per-object overhead (TLS, request
signing, provider rate limits), worse compression (less context per chunk), and
a larger manifest.

Larger chunks: better compression ratio, fewer requests, friendlier to rate
limits, but a small edit re-uploads a whole large chunk, an interrupted
transfer wastes more, and more RAM is held per in-flight chunk.

A hard floor overrides preference: S3 multipart requires a 5 MiB minimum for
every part except the last, so any S3-backed path cannot go below that for
multipart uploads. AeroVault's at-rest content-defined chunker runs separately
(256 KiB min / 1 MiB avg / 4 MiB max by default; the `archive` profile widens
the bounds for ratio without changing the wrapper order).

A counter-intuitive point worth stating explicitly, because the same word
"chunk" hides two opposite RAM curves:

- **Transfer-only chunking** (for example rclone's `--drive-chunk-size`) costs
  *more* RAM as the chunk grows: the buffer per in-flight part scales with the
  part size.
- **At-rest chunking** costs more RAM as the chunk *shrinks*: the cost is not
  the chunk buffer, it is the manifest. More chunks means a larger chunk-id
  table and a larger in-memory index.

### Transfer chunk-size flags vs an at-rest chunking wrapper

When an at-rest chunking wrapper produces the objects, a transfer-level
chunk-size flag is redundant: the unit on the wire is already the wrapper's
chunk, and the transfer layer must not re-window it. When both are set AeroFTP
keeps the wrapper boundary authoritative and logs a one-line notice that the
transfer flag is inert. The transfer flag stays useful only for the
no-wrapper case (a plain large-file upload to a provider with a part-size
sweet spot).

## Error correction (AeroVault v4 / dedicated track)

Error correction is the fourth wrapper. It is structurally different from the
other three: chunking, compression and encryption transform the data;
error correction adds parity *alongside* it, so `v3 + Error Correction = v4` and a v3
reader simply skips the parity it does not understand.

It sits as the outermost layer, over the cipher blocks. It repairs damage
*before* decryption; AES-256-GCM-SIV remains the sole authority on tampering.
Redundancy is for recovery, not for trust. On cloud backends durability is
already redundant, so the value is marginal there; on USB sticks, consumer NAS
disks, optical media and cold-storage archives it is the difference between
"an encrypted backup survives a bad sector" and "it is gone". The scheme is
**decided and shipped**: Reed-Solomon parity, kept either embedded in the
container or in a detached, content-SHA-bound `.aerocorrect` sidecar (the same
unified format AeroSync uses), with the operational `scrub` / `repair` /
`export-parity` surface. Details in [`AEROVAULT-V3-SPEC.md` section 11](../AEROVAULT-V3-SPEC.md#11-v4-evolution-note-t-aerovault-ecc-shipped).

## Where this is today

- **AeroVault v3 (Beta, opt-in):** packing, chunking, per-chunk zstd,
  per-chunk AES-256-GCM-SIV, BLAKE3 chunk id and cipher hash, the extension
  slot reserved for v4 Error Correction. The format stays Beta and is not the default tier
  until it has had a public spec review pass.
- **AeroSync:** the streaming surface inherits the wrappers progressively;
  chunk-first ordering is non-negotiable there because the whole product
  depends on "edit one byte, move one chunk".
- **Error correction:** v4, shipped. Reed-Solomon parity, embedded or in a
  detached self-healing `.aerocorrect` sidecar, with `scrub` / `repair` /
  `export-parity`.

The authoritative format specification is
[`AEROVAULT-V3-SPEC.md`](../AEROVAULT-V3-SPEC.md). This page is the intuition;
the spec is the contract.
