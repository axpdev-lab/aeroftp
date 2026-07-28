# AeroRsync vs rsync: Reference Guide

> Public reference for technical questions of the form "what is AeroRsync compared to rsync?". No total-parity claim: the comparison is honest and mapped to actual code state (`src-tauri/src/aerorsync/`, module README, `aerorsync` 0.0.x crate on crates.io).
>
> Stable public reference for external communication on the relationship between rsync and AeroRsync. Tracks shipping behaviour, not internal roadmap identifiers.

---

## Official positioning (canonical tagline)

> **"Native rsync protocol 31 in pure Rust on SFTP. Cross-OS (Linux, macOS, Windows): no rsync binary required on the client, byte-identical against stock rsync 3.2.7."**

This is the official AeroFTP phrasing to use verbatim in README, product pages, mail and public answers. Decomposed:

| Fragment | Precise meaning |
|---|---|
| **Native rsync protocol 31** | Implements wire protocol 31 (encode/decode 31/32) speaking bytes-on-wire identical to rsync 3.2.x / 3.4.x. Not a separate protocol "inspired by rsync". |
| **in pure Rust** | Zero linking to librsync, zero `Command::new("rsync")`, zero MSYS2 / Cygwin / WSL. Only permissive Rust dependencies (russh, ssh2, zstd, xxhash-rust). |
| **on SFTP** | AeroRsync is wired as the `delta_transport()` of AeroFTP's **SFTP provider**, not a separately exposed protocol in the registry. It is the delta accelerator that, behind an SFTP/SSH server, talks to standard `rsync --server`. |
| **Cross-OS (Linux, macOS, Windows)** | The Cargo feature `aerorsync` compiles and runs on all three. On Windows it is the only delta-sync path possible for AeroFTP (`RsyncBinaryTransport` is `#[cfg(unix)]`). The runtime toggle is **ON by default** (`Auto` mode) since v3.8.0: the host-key algorithm negotiation asymmetry between the libssh2 leg and the russh leg was resolved in May 2026, after which the toggle was flipped. The handful of `#[cfg(unix)]` directives that remain inside `aerorsync/` are limited to Unix-only test gates and to POSIX file-mode preservation helpers with explicit non-Unix fallbacks: they do not block a Windows build. |
| **no rsync binary required on the client** | The user system does not need `rsync` installed. The *remote server* does, because AeroRsync speaks to standard `rsync --server`. |
| **byte-identical against stock rsync 3.2.7** | Verified by CI test `driver_upload_live_lane_3_real_rsync_byte_identical` (gated `RUSTFLAGS='--cfg ci_lane3'`), 1 MiB upload in Docker, output sha256 match, `phase == Complete`, `bytes_sent >= payload`. Plus 599 unit tests against rsync 3.2.7 frozen bytes. **The version matters and used to be stated wrong here:** lane 3 runs `aeroftp-rsync-real` on port 2224, a Debian bookworm image carrying **rsync 3.2.7**. The 3.4.1 number belongs to the *other* fixture, `aeroftp-delta-sync-fixture` on port 2222 (Alpine 3.19), which serves the `integration_delta_sync` product-path job. Both are real and both run in CI; attributing the neighbour's version to lane 3 was the error. |

Everything below expands this positioning into operational detail.

---

## TL;DR: one sentence per project

- **rsync** is the de-facto standard utility for Unix delta sync since 1996, written in C, with a public wire protocol (versions 27-32), a system binary and an optional daemon. Maintainer: rsync.samba.org, GPL-3.0-or-later.
- **AeroRsync** is a **clean-room Rust reimplementation** of rsync's *wire protocol 31* only, developed inside AeroFTP (module `src-tauri/src/aerorsync/`, historical code-name "Strada C"). Purpose: talk bytes-on-wire directly to a real remote `rsync --server`, **without depending on the rsync binary on the local machine**. The **public `aerorsync` crate on crates.io is not yet developed**: it is a **name reservation (0.0.x) on roadmap**, zero public API. Real development is happening inside AeroFTP; the crate will be extracted as an independent component only after closing the three promotion gates (stock-rsync interop green end-to-end, dependency direction inverted AeroFTP→aerorsync, separate clean-room commit history).

**Relationship**: AeroRsync **does not replace** rsync, **it talks to** rsync. It is a native wire-31 client that lets AeroFTP do delta sync to a standard `rsync --server` even where the local `rsync` binary does not exist or is not desirable (Windows first).

---

## Full comparison table

| Dimension | rsync (upstream) | AeroRsync (AeroFTP module) | Notes |
|---|---|---|---|
| **Software type** | System binary + optional library | In-process Rust module inside AeroFTP (aerorsync 0.0.x crate = name only) | rsync is an executable; AeroRsync is code linked inside the app |
| **Language** | C | Rust | No unsafe in the module, permissive dependencies only (russh, ssh2, zstd, xxhash-rust) |
| **License** | GPL-3.0-or-later | GPL-3.0-only (aligned with AeroFTP and aerovault) | Compatible without conditions |
| **Code origin** | Tridgell et al., 1996 | Clean-room 2026, zero copy from rsync sources | Analogous precedent: openrsync (OpenBSD, 2019, BSD-licensed) |
| **Maturity** | ~30 years, production standard | Pre-1.0, runtime toggle ON by default since v3.8.0 | aerorsync crate stays `0.0.x` until stock-rsync interop is green end-to-end on all targets |
| **Wire protocols supported** | 27, 28, 29, 30, 31, 32 with negotiation | 31/32 only (subset of 31, encode/decode 31/32) | rsync must dialog with ancient installs; AeroRsync targets modern rsync (3.2.x / 3.4.x) |
| **Transport** | SSH remote-shell, native rsyncd (`rsync://`), local pipe, batch mode | SSH remote-shell only (`SshRemoteShellTransport` via libssh2, or `russh_session_transport`) | No `rsync://` daemon, no local, no batch |
| **SSH authentication** | SSH key, password, agent, GSSAPI, anything supported by `ssh(1)` | SSH key, password, and SSH agent (Unix `SSH_AUTH_SOCK`, via the russh leg); host-key pinning **mandatory** | Password-backed SFTP profiles now enter the native transport with cross-leg host-key pinning. SSH agent auth resolves identities from a running agent on Unix (Windows Pageant / named-pipe is a follow-up). GSSAPI and keyboard-interactive remain out of scope. |
| **rsyncd mode (`rsync://`)** | Yes, full daemon + password auth | No | **Out of scope by design**: a different transport, not a missing piece of the wire-31 client |
| **File-level checksum** | MD5 (modern default) / xxh3 / xxh128 / MD4 (legacy) | Negotiated: xxh128, xxh3, xxh64, md5, md4 (advertised in that order), plus sha1 reachable via `AEROFTP_RSYNC_CSUM_ALGOS` | The whole-file trailer is computed and **verified** for every implemented winner, in both directions; all six are covered live against a real peer. Only sha256 / sha512 / none remain no-op verifies, and only via explicit override. **sha1 depends on the peer's vintage**: rsync 3.2.7 advertises `xxh128 xxh3 xxh64 md5 md4 sha1 none`, rsync 3.4.1 advertises the same list **without sha1** - upstream dropped it. Forcing sha1 against 3.4.1 gets `requested action not supported (code 4) at compat.c(405)`, which is the peer refusing, not AeroRsync failing |
| **Block-level checksum (signature phase)** | Rolling Adler-32 + MD5/MD4 strong hash | Rolling Adler-32 + the negotiated strong hash (xxh128 / xxh3 / xxh64 / md5 / md4 / sha1), classic `sum_head` / `sum_block` | Rolling is identical. The block-strong seeding mirrors rsync 3.2.7 `checksum.c`: md4 appends the four-byte seed (builtin `get_checksum2`), sha1 prepends it (EVP path) |
| **Literal compression** | zlib (default), zstd (`--zc=zstd`), none (`-z` off) | zstd only, multi-chunk DEFLATED_DATA splitting >16 KiB | AeroRsync emits zstd tokens compatible with `token.c::send_zstd_token` |
| **Transfer granularity** | Recursive tree sync with a single invocation | **Single-file delta transfer per invocation** | For N files = N SSH sessions (see *Session reuse* below) |
| **Preserved metadata** | perms, owner, group, mtime, atime, symlinks, hardlinks, xattrs, ACL, sparse, devices | mtime, base perms via atomic finalize, **symlinks on unix** (single-file, both directions), and **`user.*` xattrs on Unix** (`-X`, both directions against stock rsync); **no ACL/owner-group/device/hardlink** | A source symlink is detected by `lstat` and never followed, emitted as an `S_IFLNK` file-list entry carrying the target; the receiver materialises it create-temp-then-rename. On non-unix a symlink entry fails closed with a typed error rather than silently writing a regular file. Unix production opts into `-X` via `SftpProvider::delta_transport` (Windows stays off: no direct `user.*` analogue). Remaining missing metadata is **not yet implemented** rather than out of scope: ACL is the next candidate, hardlink is the one structurally blocked on recursive scope. See the taxonomy below |
| **Filters (`--exclude` / `--include` / `--files-from`)** | Full suite, regex-like, patternfile | None at wire level | **Out of scope by design**: AeroFTP filters one layer up (`.aeroignore`, fail-closed since v4.1.6). The residual cost is session count, not correctness, and `AerorsyncBatch` absorbs most of it |
| **Special modes (`--delete`, `--inplace`, `--append`, `--partial-dir`, `--sparse`, `--mkpath`)** | All supported | A `--sparse` analogue (opt-in hole-punched destination writes on the local delta path) is available. `--delete`, `--inplace`, `--append`, `--partial-dir` are **out of scope by design**. `--mkpath` is **not implemented** - this document used to claim it was | Deletion and retention are owned by the sync layer with its own v4.1.6 gates; the destination write strategy is owned by `StreamingAtomicWriter`, whose kill-9 invariant `--inplace` would weaken. The sparse analogue turns all-zero chunks into filesystem holes with the same atomic / kill-9 invariants as the dense write; output reads back byte-identical |
| **Destination atomic writes** | `.~tmp~<random>` with final rename (default), `--inplace` optional | `.aerotmp` with atomic rename via `StreamingAtomicWriter` (P3-T01 W2.3), kill-9 invariant guaranteed | Equivalent behavior, different temp names |
| **Streaming I/O (RSS bound)** | Always streaming, RSS bound `O(block_size)` | Streaming both upload (P3-T01 W1.2 `drive_upload_through_delta_streaming`) and download (P3-T01 W2.5 `apply_delta_streaming`). Y-RSC.5 streaming signatures (`send_signature_phase_from_baseline`) | Peak RSS `O(block_size + writer_buffer)`; 4 GiB sparse RSS acceptance test |
| **Session reuse** | Yes, one SSH session covers the entire dir | Available via `AerorsyncBatch` (W3, 2026-05-01): one SSH session for N file pairs | See `src-tauri/src/aerorsync/delta_transport_impl.rs:719` (`AerorsyncBatch`, impl `DeltaBatch` at line 814) |
| **Cross-platform** | Linux/macOS/BSD native. Windows = MSYS2 / Cygwin / WSL (no official native installer) | Linux / macOS / **Windows first-class** (the main reason for existing) | On Windows AeroRsync is the only delta-sync option for AeroFTP |
| **Distribution** | OS package, everywhere on Unix | In-process inside AeroFTP, **no runtime external dependency** | AeroFTP user does not install rsync, nor AeroRsync separately |
| **Tests against real rsync servers** | Community test suite + buildbot | 599 unit tests against **rsync 3.2.7 frozen bytes**; 11 CI lane 3 live tests vs stock **rsync 3.2.7** in Docker (gated `RUSTFLAGS='--cfg ci_lane3'`), including the byte-identical upload; 8 live tests across the negotiated checksum matrix driving the production transports; 3 `#[ignore]` comparative benchmarks | Blocco B closed 2026-04-26: production dispatch uses stock `rsync --server` (`RemoteCommandFlavor::WrapperParity`) |
| **Default enabled in AeroFTP** | N/A (AeroFTP does not spawn it in production) | **Yes, mode `auto` since v3.8.0**: native is attempted first and falls back to the classic `rsync` binary on construction failure or an unpinned host key (hard rejections do not fall back) | On Unix `auto` keeps a classic fallback; on Windows native is the only delta path. The cross-OS host-key asymmetry that previously kept the toggle off was resolved in May 2026. Forced `native` / `classic` modes remain available via Settings and `aeroftp-cli aerorsync mode` |
| **Daemon / server-side** | Yes (`rsync --daemon`, `rsyncd.conf`) | **None.** The legacy RSNP server stack (`SessionDriver`, session, planner, server, frame codec, and the `aerorsync_serve` dev binary) was retired in v4.1.6; AeroFTP production talks only to standard `rsync --server` | No intention to write an "AeroRsync server" daemon. Retiring the parallel stack removed the only code path that was not exercised against stock rsync |
| **Identity in AeroFTP code** | Invoked by `rsync_over_ssh.rs` (1299 lines, `RsyncBinaryTransport`) | Module `src-tauri/src/aerorsync/` (23 files, ~35 800 lines total, `AerorsyncDeltaTransport` impl `DeltaTransport`) | Both implement the same `DeltaTransport` trait, runtime choice |

---

## Operational taxonomy: what one does, what the other does, what both do

**Only rsync, AeroRsync no.** This list is split in two on purpose, because the two halves do not mean the same thing. The first half is **not a backlog**: it is work AeroFTP deliberately does at another layer, or a different transport altogether, and re-implementing it at the rsync wire level would duplicate or actively weaken an authority that already exists. The second half is the real, incremental parity gap.

### First, name the frame

"Out of scope" is meaningless without saying *out of scope of what*, and for most of the first list the honest answer is "of **AeroFTP the product**". The sentence holding that list up is *"AeroFTP already does this one layer up"* - and that sentence is true only while AeroRsync lives inside AeroFTP. A library consumer has no AeroSync owning the tree, no `.aeroignore`, no `StreamingAtomicWriter`. For them `--delete` is not delegated to a better-placed authority; it simply does not exist.

That matters because extracting `aerorsync` as a standalone crate is a stated intention - **with no date and no public commitment**. The moment it is extracted, "another layer owns it" stops being an answer for part of this list, and only for part: some entries are architectural boundaries that hold in either frame.

| Capability | Inside AeroFTP (today) | In a standalone crate (if ever extracted) |
|---|---|---|
| Recursive tree sync | another layer owns it (AeroSync / AeroCloud) | **real backlog** - already tracked as capstone Y-RSC.7 |
| Wire-level filters | another layer owns it (`.aeroignore`) | **backlog**, follows recursive scope |
| `--delete` / `--backup` / `--link-dest` | another layer owns it, with v4.1.6 safety gates | **backlog**, follows recursive scope |
| `--inplace` / `--append` / `--partial-dir` | `StreamingAtomicWriter` owns the write strategy | **backlog** - a consumer picks its own strategy, so these become options |
| Daemon mode `rsync://` | different transport | **still out of scope** - an architectural boundary, not a layering one |
| Protocols 27-30 | deliberate protocol-31-only design | **still out of scope**, same reason |
| hardlink (`-H`) | structurally blocked without tree scope | unblocks by itself once recursive scope lands |

The first four rows change column. The next two do not, and that is the useful distinction: daemon mode and old-protocol compatibility are design decisions that would survive extraction, while tree scope and everything hanging off it is deferred work that is currently *invisible* because AeroFTP happens to cover it elsewhere.

Read with a library author's eyes, the old flat list undersold the ambition: "out of scope by design" reads as "we will never do this", when for half the entries it means "the product does not need it from this layer". Conditional wording is deliberate throughout - nothing here is a roadmap commitment.

**Note on internal consistency**, since it was contradictory before: this document called recursive tree sync out of scope while `APPENDIX-AERORSYNC/TODO.md` carried it as a costed capstone. Both are right under the frame above - out of scope for the product today, on the roadmap for the crate - and neither is right on its own.

### Out of scope by design (owned at another layer, or a different transport)

- **Recursive directory sync with `--archive`.** AeroRsync is a single-file delta accelerator, wired as the `delta_transport()` of the SFTP provider: it transfers one file per invocation and does not own a tree. Enumeration, scheduling, progress and retention for a tree belong to AeroSync / AeroCloud and the DAG transfer engine.
- **`--delete`, `--backup`, `--link-dest`.** Deletion and retention authority lives at the sync layer, together with its safety gates - the scan-completeness gating and the archive-before-propagated-delete helper, both hardened in v4.1.6 (AUDIT-03). Implementing `--delete` at wire level would install a second, weaker deletion authority *underneath* the one that was just hardened. The accurate statement is "AeroFTP deletes at the sync layer, with its own gates", not "AeroRsync lacks `--delete`". (`--link-dest` additionally depends on hardlink support, item 5 below.)
- **`--inplace`, `--append`, `--partial-dir`.** The destination write strategy is owned by `StreamingAtomicWriter`: write to `.aerotmp`, then atomic rename, with a kill-9 invariant. `--inplace` would specifically *weaken* that invariant rather than add a capability.
- **Filters `--exclude` / `--include` / `--files-from` at wire level.** AeroFTP applies filters one layer up (`.aeroignore`, which v4.1.6 made fail-closed), so the behaviour is covered. What is genuinely lost is not correctness but session count: rsync sends one file list for a whole tree, AeroFTP enumerates and invokes per file. `AerorsyncBatch` (one SSH session for N file pairs) absorbs most of that cost. If wire-level filters ever become worth doing, they are worth doing as part of recursive tree scope, not on their own.
- **Daemon mode `rsync://`** with rsyncd modules and `rsyncd.secrets`. A different transport - daemon protocol, module configuration, its own authentication - not a missing piece of the wire-31 client. Nothing in the current design blocks it; whether AeroFTP should speak to public rsync mirrors is a product decision carrying its own security surface (daemon auth, module traversal), and it would arrive as a feature, not as a parity fix.
- **Batch mode (`--write-batch` / `--read-batch`)**, distinct from AeroFTP's SSH session reuse, which is also called "batch".
- **SSH agent forwarding, GSSAPI, custom PAM.** SSH agent *authentication* itself IS supported on Unix via `SSH_AUTH_SOCK`.
- **The compatibility surface to protocols 27-30.** AeroRsync is protocol-31-only by design; older or stripped endpoints are served by the stock rsync binary instead.
- **Operation as a standalone rsync-flag-compatible CLI.** AeroRsync is reachable from the shell via `aeroftp-cli --delta`, but it does not present an rsync command line.

### Not yet implemented (the real parity gap, ordered by effort)

Metadata the wire protocol can carry and AeroRsync does not. This half *is* a backlog.

1. **ACL (`-A`)** - the next genuine candidate. Builds on xattr in rsync's own implementation, so it follows the shipped `-X` path rather than preceding it. POSIX ACL, Unix only. Flag string already measured (`A` before `X` in the compact bundle).
2. **owner / group (`-o` / `-g`)** - uid, gid and their resolved names are already emitted in the file-list entry because the wire format requires it, but nothing is applied on the receiving side: `StreamingAtomicWriter::finalize` takes only mode and mtime. Applying them needs a privileged receiver, which rarely matches AeroFTP's desktop deployment model.
3. **Device and special files** - Unix-only, needs a privileged create.
4. **hardlink (`-H`)** - the hardest, and the only one that is *structurally* blocked rather than merely unimplemented: it needs a device/inode map across the whole file list, which is a tree-scope concept. A single-file accelerator cannot detect that two paths share an inode. This one waits on recursive scope, not on effort.

Items that have already left this list: **symlinks** ship end-to-end on unix in both directions against stock rsync (Y-RSC.4, single-file scope); **sparse files** have an opt-in `--sparse` analogue on the local delta path; and **`user.*` xattrs (`-X`)** ship on Unix single-file against stock rsync (wire B1-B2, local read/apply B3, live acceptance + Unix production opt-in B4). Inline values (<=32 B), OOB digests, binary values with interior NUL, empty-session `-X`, and download apply-before-rename are covered by CI lane 3. Windows stays off (no direct `user.*` analogue). ENOTSUP on the destination is soft by default.

**Only AeroRsync, rsync no**:
- Delta sync **without installing the rsync binary on the user system** (zero runtime dep)
- Windows first-class without MSYS2/WSL/Cygwin
- Direct linking inside a Tauri app (in-process, no fork+exec)
- **Memory-safe** Rust implementation (no buffer-overflow class CVE from C)
- Atomic writes with `StreamingAtomicWriter` kill-9-safe invariant designed for desktop UI
- Native integration with AeroFTP's `DeltaTransport` trait (event sink, fallback policy, host key pinning as part of UI flow)

**Both**:
- Speak bytes-on-wire **identical** to rsync wire protocol 31 (AeroRsync verifies this byte-identical in CI lane 3)
- Use rolling Adler-32 in signature phase
- Transport via SSH (remote-shell mode)
- Compress literals (rsync = zlib/zstd, AeroRsync = zstd only)
- Can interoperate: AeroRsync (client) ↔ `rsync --server` (server). This is exactly what AeroFTP does today in production on Unix when the toggle is active
- Are GPL: GPL-3.0-or-later vs GPL-3.0-only, compatible

**Same operational field, who does what compared to the other**:

| Scenario | rsync | AeroRsync |
|---|---|---|
| Upload of modified file to Unix SFTP/SSH server | Does it via `rsync -e ssh` on system binary | Does it via native wire-31, talking to remote `rsync --server` |
| Same scenario, but from Windows without MSYS2 | Does not do it (binary not available) | Does it natively |
| Recursive directory sync | One invocation, entire subtree | **Out of scope by design**: AeroSync / AeroCloud own the tree; they enumerate and call AeroRsync per file, with `AerorsyncBatch` reusing one SSH session for N pairs |
| Preserve ACL/xattr/owner | Does it with `-aHAX` | **xattr (`user.*`, `-X`) on Unix**: yes, single-file both directions (B4). **ACL**: not yet, next candidate. **owner/group**: not applied (needs a privileged receiver) |
| Transfer a symlink | Does it with `-l` / `-a` | Does it on unix, single-file, both directions (target carried in the file-list entry, never followed) |
| Daily backup with `--delete` and `--link-dest` | Classic rsync use case | Out of scope, delegated to AeroSync logic (in AeroFTP) or to rsync binary |
| Sync to `rsync://daemon.example.org/module` | rsyncd password auth | Not supported (no daemon mode) |
| SSH auth key-only, host key pinning UI | Works but pinning is user-side (`~/.ssh/known_hosts`) | Pinning is part of module flow, mandatory, visible in AeroFTP UI |
| Byte-level delta on very large single files | Does it, gold standard | Does it, verified byte-identical against rsync 3.2.7 in CI lane 3 |

---

## Typical FAQ with correct answers

> **"Is AeroRsync a fork of rsync?"**

No. It is a **clean-room reimplementation in Rust** of the public wire protocol (version 31/32). No line of rsync code has been copied. Same pattern as `openrsync` from OpenBSD (BSD-licensed, from 2019, default on OpenBSD). Wire protocol as interface specification is not copyrightable (Sega v. Accolade, Oracle v. Google).

> **"So does AeroFTP replace rsync?"**

No. AeroRsync is a wire-31 **client**. On the remote server there is still standard `rsync --server` (version 3.2.x / 3.4.x). AeroRsync replaces only the dependency on the rsync binary **on the user's local machine**, and is useful on systems (Windows) or contexts (Tauri app in-process) where spawning the binary is not desirable.

> **"Can I use it as a standalone library?"**

Not yet. The published `aerorsync` crate on crates.io (version `0.0.x`) is a **reserved namespace placeholder** that exports no public API. The code lives in `aeroftp/src-tauri/src/aerorsync/`. Promotion to independent crate (`0.1.0`) requires: (a) stock-rsync interop green end-to-end (done April 2026), (b) dependency direction inverted AeroFTP→aerorsync, (c) separate clean-room commit history.

> **"Does it work against rsync.net / Hetzner Storage Box?"**

The wire layer speaks rsync protocol 31, so yes to a *modern* `rsync --server` (3.2.x / 3.4.x). End-to-end practical status by endpoint class:

1. **Key-based endpoints** (rsync.net, Hetzner Storage Box, generic SFTP+rsync): supported today; the SFTP provider's `delta_transport()` lifts to `AerorsyncDeltaTransport` whenever the parent SSH session has captured a host-key fingerprint. SSH agent identities are also accepted on Unix.
2. **Password-based endpoints**: password-backed SFTP profiles enter the native transport directly, with cross-leg host-key pinning.
3. **Old or stripped-rsync endpoints**: an endpoint whose `rsync --server` negotiates an old wire protocol (27-30) or whose wrapper rejects the standard server flag string is **out of scope** for the native engine, which is protocol-31-only by design. Such endpoints are served by the stock `rsync` binary, not by AeroRsync.

> **"What performance does it have vs native rsync?"**

There is now a measured answer, and it is not one number. Run 2026-07-28 on an idle 24-core machine, both engines back-to-back against the same container over SSH loopback.

**Fairness anchor.** The rsync side ran `-logDtprcz`, which is the client flag set that produces `--server -logDtprcze.iLsfxCIvu` - byte-for-byte the argument string AeroRsync sends. That was not assumed: a wrapper on the container logged the real server command line for each candidate flag set. The everyday `-az` set was deliberately *not* used for the published numbers, because with it the delta scenarios measure nothing at all - the two 50 MB fixtures share a size and an mtime, so rsync's quick check declares them identical and skips. The `c` (`--checksum`) in the strict set is what forces a real comparison.

| Scenario | AeroRsync | stock rsync 3.2.7 | Wire bytes (AeroRsync / rsync) |
|---|---|---|---|
| A1 cold upload, 50 MB incompressible | **1.068 s** | 1.367 s | 52 436 493 / 52 440 795 |
| A2 delta upload, 640 × 4 KiB changed | 2.000 s | **1.358 s** | 3 748 322 / 3 743 589 |
| A3 delta download, same change set | **1.310 s** | 1.344 s | sent 43 481 / 50 737 |
| A5 redundant upload (nothing to do) | **0.461 s** | 1.260 s | 49 B / 82 B |
| A4 20 × 256 KiB, one session per file | **4.981 s** | 25.249 s | - |
| A4' same tree, one recursive `rsync` call | *no recursive scope* | **1.308 s** | - |

Three distinct results, and the honest reading matters more than the winner count:

1. **AeroRsync wins wherever a process would have to be spawned.** Cold upload, no-op, and twenty small files one at a time. `rsync` pays a fresh `ssh` plus `rsync` fork per file (~1.25 s each); AeroRsync opens an in-process session. That is the 5.1× on A4 and the 2.7× on A5.
2. **AeroRsync loses the delta upload by about 30%, doing identical work on the wire.** 3 748 322 bytes against 3 743 589 is a 0.13% difference, so the protocol decisions match; the gap is CPU in the Rust encode path against 30 years of tuned C.
3. **The real gap is not in this table.** rsync does the whole 20-file tree in *one* invocation in 1.308 s, 3.8× better than AeroRsync's per-file path. That is recursive scope (Y-RSC.7), and `AerorsyncBatch` - one SSH session for N files - is what closes it at the transport layer. This benchmark deliberately exercises the per-file path, so it does not measure the batch improvement.

**Dataset caveat, stated because it flatters the delta rows.** `R_rand.bin` is incompressible (zstd-3 ratio 1.0:1), so A1 is a clean transport measurement. `A_base.bin` compresses 9.1:1, and its 640 mutated 4 KiB regions are largely reconstructible from blocks elsewhere in the basis - rsync reports only 119 800 bytes of literal data for the download. Real-world delta ratios will be worse on incompressible payloads. The *comparison* stays valid, since both engines get the same bytes.

The old answer to this question - "no published benchmark, don't expect miracles, the only big difference is that it works where rsync isn't" - understated the case. Process-spawn-bound workloads are exactly what a desktop file manager does, and there AeroRsync is ahead.

> **"Why xxh128 and not MD5 like modern rsync?"**

Architectural decision of the module. xxh128 is non-cryptographic (faster than MD5) and adequate as integrity checksum when not defending against active adversaries (host-key pinning and SSH transport cover the security part). For full interop with rsync requiring a specific strong hash in negotiation, the module selects the appropriate algorithm in handshake (see `real_wire.rs`).

> **"What is actually still missing compared to rsync?"**

Two different things, and conflating them undersells the design. Most of what rsync does and AeroRsync does not is **deliberately handled at another layer**: deletion and retention belong to AeroSync / AeroCloud, which own the tree and carry their own safety gates; filters are applied one layer up by `.aeroignore`; the destination write strategy is owned by `StreamingAtomicWriter`, whose atomic-rename invariant `--inplace` would weaken; and `rsync://` daemon mode is a different transport rather than a missing piece of the wire-31 client. Re-implementing any of those at wire level would duplicate or weaken something that already exists.

The genuine, incremental gap is **metadata**: ACL, owner/group and device files are not implemented, and hardlink is structurally blocked because detecting that two paths share an inode requires tree scope that a single-file accelerator does not have. **`user.*` xattrs (`-X`) ship on Unix** (B4); **ACL is the next real candidate**. Symlinks (v4.1.6) and xattrs (B1-B4) are the worked examples of how a metadata type gets added end-to-end here.

> **"Is it production-ready?"**

For the use case *delta accelerator inside AeroFTP*: yes, runtime toggle is **on by default** (`Auto` mode) since v3.8.0, the cross-OS host-key asymmetry has been resolved, and the SFTP-key path is the production default. For the use case *standalone rsync client*: no, and the published crate explicitly declares this. The roadmap to v0.1.0 depends on the three promotion gates, not a hard ETA.

---

## What NOT to say externally (to avoid over-claiming)

- "AeroRsync replaces rsync": it is a wire-31 client, not a functional substitute for the binary
- "Supports all rsync options": supports wire protocol for single-file delta, not `-a`, `--delete`, `--inplace`, etc.
- "Works wherever rsync works": no daemon mode, no recursive tree, no ACL/owner/device/hardlink metadata, and protocol-31 only (an endpoint whose `rsync --server` negotiates protocol 27-30, or whose wrapper rejects the standard server flags, is out of scope: use the stock rsync binary there). Unix does preserve `user.*` xattrs via `-X`. Password auth and SSH agent auth (Unix) are wired into the native transport
- "The aerorsync crate is available on crates.io": the name is registered but the crate is 0.0.x without public API
- "AeroRsync is safer than rsync because Rust": it is memory-safe, but rsync has 30 years of hardening; the correct claim is "memory-safe by construction, different security model, mandatory host-key pinning in the flow"
- "AeroRsync lacks `--delete`" (and the same for filters, `--inplace`, daemon mode): true as a sentence, misleading as a gap. These are handled at another layer on purpose, and saying it the other way invites the reader to score a design decision as a missing feature. Say what AeroFTP *does*: "deletion happens at the sync layer, with its own safety gates". Reserve the word "missing" for the remaining metadata list - ACL, owner/group, devices, hardlink - which really is a backlog (`user.*` xattr on Unix has left that list)

---

## One line per audience

- **For Rust developers**: "Clean-room Rust impl of rsync wire protocol 31, single-file delta over SSH, in-process inside AeroFTP, GPL-3.0."
- **For AeroFTP end users**: "Lets AeroFTP do delta sync to standard rsync servers even on Windows, without installing the rsync binary."
- **For the rsync community**: "Not a fork, not a competitor, an independent wire-31 client that talks to real `rsync --server` and tests byte-identical against rsync 3.2.7 in CI."
- **For partners (Hetzner, rsync.net, Filen)**: "The native AeroRsync module speaks wire-31 byte-identical with rsync 3.2.7 and is in production single-file inside AeroFTP for cross-OS coverage, mode `auto` (native first, classic fallback) by default since v3.8.0. Key-based endpoints (rsync.net, Hetzner) work today through the SFTP provider's delta path, including SSH agent identities on Unix, and password-backed SFTP profiles enter the native transport directly. Endpoints that run an old or stripped `rsync --server` (protocol 27-30) fall outside the protocol-31 native engine and are served by the stock rsync binary instead."

---

*Last updated: 2026-07-27 (B4 xattr closeout): `user.*` xattrs (`-X`) ship on Unix single-file against stock rsync (live lane 3 + CLI smoke); removed from "not yet implemented"; ACL promoted to the next candidate; Unix production opts into `-X` via `SftpProvider::delta_transport`. Previously 2026-07-25 (post-v4.1.6), splitting the "Only rsync, AeroRsync no" taxonomy into "out of scope by design (owned at another layer)" and "not yet implemented"; xattr had been the first real parity candidate. Same day, after the Y-RSC wave: md4 and sha1 as first-class negotiated checksums in both roles with the whole-file reconstruction now genuinely verified (Y-RSC.3), symlink transfer end-to-end on unix (Y-RSC.4), streaming download signatures that drop the O(file) baseline read (Y-RSC.5), and retirement of the legacy RSNP server stack so production has a single path against stock `rsync --server` (Y-RSC.8). Previously 2026-06-22 (general docs audit) and 2026-05-14. Updated when the AeroRsync module reaches new milestones (recursive scope expansion, filters at wire level).*
