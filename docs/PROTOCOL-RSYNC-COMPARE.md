# AeroRsync vs rsync: Reference Guide

> Public reference for technical questions of the form "what is AeroRsync compared to rsync?". No total-parity claim: the comparison is honest and mapped to actual code state (`src-tauri/src/aerorsync/`, module README, `aerorsync` 0.0.x crate on crates.io).
>
> Stable public reference for external communication on the relationship between rsync and AeroRsync. Tracks shipping behaviour, not internal roadmap identifiers.

---

## Official positioning (canonical tagline)

> **"Native rsync protocol 31 in pure Rust on SFTP. Cross-OS (Linux, macOS, Windows): no rsync binary required on the client, byte-identical against stock rsync 3.4.1."**

This is the official AeroFTP phrasing to use verbatim in README, product pages, mail and public answers. Decomposed:

| Fragment | Precise meaning |
|---|---|
| **Native rsync protocol 31** | Implements wire protocol 31 (encode/decode 31/32) speaking bytes-on-wire identical to rsync 3.2.x / 3.4.x. Not a separate protocol "inspired by rsync". |
| **in pure Rust** | Zero linking to librsync, zero `Command::new("rsync")`, zero MSYS2 / Cygwin / WSL. Only permissive Rust dependencies (russh, ssh2, zstd, xxhash-rust). |
| **on SFTP** | AeroRsync is wired as the `delta_transport()` of AeroFTP's **SFTP provider**, not a separately exposed protocol in the registry. It is the delta accelerator that, behind an SFTP/SSH server, talks to standard `rsync --server`. |
| **Cross-OS (Linux, macOS, Windows)** | The Cargo feature `aerorsync` compiles and runs on all three. On Windows it is the only delta-sync path possible for AeroFTP (`RsyncBinaryTransport` is `#[cfg(unix)]`). The runtime toggle is **ON by default** (`Auto` mode) since v3.8.0: the host-key algorithm negotiation asymmetry between the libssh2 leg and the russh leg was resolved in May 2026, after which the toggle was flipped. The handful of `#[cfg(unix)]` directives that remain inside `aerorsync/` are limited to Unix-only test gates and to POSIX file-mode preservation helpers with explicit non-Unix fallbacks: they do not block a Windows build. |
| **no rsync binary required on the client** | The user system does not need `rsync` installed. The *remote server* does, because AeroRsync speaks to standard `rsync --server`. |
| **byte-identical against stock rsync 3.4.1** | Verified by CI test `driver_upload_live_lane_3_real_rsync_byte_identical` (gated `RUSTFLAGS='--cfg ci_lane3'`), 1 MiB upload in Docker against `rsync 3.4.1`, output sha256 match, `phase == Complete`, `bytes_sent >= payload`. Plus 386 unit tests against rsync 3.2.7 frozen bytes. |

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
| **rsyncd mode (`rsync://`)** | Yes, full daemon + password auth | No | Out-of-scope initial for AeroRsync |
| **File-level checksum** | MD5 (modern default) / xxh3 / xxh128 / MD4 (legacy) | xxh128 only | Match in tests against rsync 3.4.1 lane 3 |
| **Block-level checksum (signature phase)** | Rolling Adler-32 + MD5/MD4 strong hash | Rolling Adler-32 + xxh128 strong hash (classic `sum_head` / `sum_block`) | Only the strong hash differs, rolling is identical |
| **Literal compression** | zlib (default), zstd (`--zc=zstd`), none (`-z` off) | zstd only, multi-chunk DEFLATED_DATA splitting >16 KiB | AeroRsync emits zstd tokens compatible with `token.c::send_zstd_token` |
| **Transfer granularity** | Recursive tree sync with a single invocation | **Single-file delta transfer per invocation** | For N files = N SSH sessions (see *Session reuse* below) |
| **Preserved metadata** | perms, owner, group, mtime, atime, symlinks, hardlinks, xattrs, ACL, sparse, devices | mtime, base perms via atomic finalize; **no symlink/hardlink/xattr/ACL/device** | Scope declared in *Known limits #4* of module README |
| **Filters (`--exclude` / `--include` / `--files-from`)** | Full suite, regex-like, patternfile | None (filter applied upstream by AeroFTP, not at wire level) | rsync has the filter in the protocol; AeroRsync does not |
| **Special modes (`--delete`, `--inplace`, `--append`, `--partial-dir`, `--sparse`, `--mkpath`)** | All supported | `--mkpath` (remote parent creation) and a `--sparse` analogue (opt-in hole-punched destination writes on the local delta path) are available; `--delete`, `--inplace`, `--append`, `--partial-dir` remain out of scope | The sparse analogue turns all-zero chunks into filesystem holes with the same atomic / kill-9 invariants as the dense write; output reads back byte-identical |
| **Destination atomic writes** | `.~tmp~<random>` with final rename (default), `--inplace` optional | `.aerotmp` with atomic rename via `StreamingAtomicWriter` (P3-T01 W2.3), kill-9 invariant guaranteed | Equivalent behavior, different temp names |
| **Streaming I/O (RSS bound)** | Always streaming, RSS bound `O(block_size)` | Streaming both upload (P3-T01 W1.2 `drive_upload_through_delta_streaming`) and download (P3-T01 W2.5 `apply_delta_streaming`). Signature phase still bulk-read upload-side | For strictly sub-128 MiB RSS, `build_signatures_streaming` adapter-side still needed (post-P3-T01) |
| **Session reuse** | Yes, one SSH session covers the entire dir | Available via `AerorsyncBatch` (W3, 2026-05-01): one SSH session for N file pairs | See `src-tauri/src/aerorsync/delta_transport_impl.rs:719` (`AerorsyncBatch`, impl `DeltaBatch` at line 814) |
| **Cross-platform** | Linux/macOS/BSD native. Windows = MSYS2 / Cygwin / WSL (no official native installer) | Linux / macOS / **Windows first-class** (the main reason for existing) | On Windows AeroRsync is the only delta-sync option for AeroFTP |
| **Distribution** | OS package, everywhere on Unix | In-process inside AeroFTP, **no runtime external dependency** | AeroFTP user does not install rsync, nor AeroRsync separately |
| **Tests against real rsync servers** | Community test suite + buildbot | 386 unit tests against **rsync 3.2.7 frozen bytes**; CI lane 3 `driver_upload_live_lane_3_real_rsync_byte_identical` byte-identical upload vs rsync 3.4.1 in Docker (gated `RUSTFLAGS='--cfg ci_lane3'`); 6 `#[ignore]` live tests on Docker fixtures | Blocco B closed 2026-04-26: production dispatch uses stock `rsync --server` (`RemoteCommandFlavor::WrapperParity`) |
| **Default enabled in AeroFTP** | N/A (AeroFTP does not spawn it in production) | **Yes, mode `auto` since v3.8.0**: native is attempted first and falls back to the classic `rsync` binary on construction failure or an unpinned host key (hard rejections do not fall back) | On Unix `auto` keeps a classic fallback; on Windows native is the only delta path. The cross-OS host-key asymmetry that previously kept the toggle off was resolved in May 2026. Forced `native` / `classic` modes remain available via Settings and `aeroftp-cli aerorsync mode` |
| **Daemon / server-side** | Yes (`rsync --daemon`, `rsyncd.conf`) | `aerorsync_serve` exists as a dev helper but is **for live tests only**; AeroFTP production talks to standard `rsync --server` | No intention to write an "AeroRsync server" daemon |
| **Identity in AeroFTP code** | Invoked by `rsync_over_ssh.rs` (1299 lines, `RsyncBinaryTransport`) | Module `src-tauri/src/aerorsync/` (26 files, ~30k LOC total, `AerorsyncDeltaTransport` impl `DeltaTransport`) | Both implement the same `DeltaTransport` trait, runtime choice |

---

## Operational taxonomy: what one does, what the other does, what both do

**Only rsync, AeroRsync no**:
- Recursive directory sync with `--archive` and the whole metadata pack
- Hardlink, xattr, ACL, device files (symlink: wire codec implemented and proto-31 round-trip tested, but end-to-end source/receiver and byte-fixture validation against stock rsync are pending; sparse files: an opt-in analogue is available on the local delta path)
- Filters `--exclude` / `--include` / `--files-from` at protocol level (AeroFTP applies these one layer up, not at the rsync wire level)
- `--delete`, `--inplace`, `--append`, `--partial-dir`, `--backup` (`--mkpath` IS supported as remote parent creation)
- Daemon mode `rsync://` with rsyncd modules and `rsyncd.secrets`
- Batch mode (`--write-batch` / `--read-batch`), distinct from AeroFTP's SSH session reuse which is also called "batch"
- SSH agent forwarding, GSSAPI, custom PAM (SSH agent authentication itself IS supported on Unix via `SSH_AUTH_SOCK`)
- 30 years of compatibility surface to protocols 27-30
- Operation as a standalone CLI on any shell (AeroRsync is reachable from the shell via `aeroftp-cli --delta`, but not as an rsync-flag-compatible standalone binary)

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
| Recursive directory sync | One invocation, entire subtree | **Not supported** by module: AeroFTP enumerates files and calls AeroRsync N times (overhead) |
| Preserve ACL/xattr/symlink/owner | Does it with `-aHAX` | Does not, AeroFTP layer does not transport them |
| Daily backup with `--delete` and `--link-dest` | Classic rsync use case | Out of scope, delegated to AeroSync logic (in AeroFTP) or to rsync binary |
| Sync to `rsync://daemon.example.org/module` | rsyncd password auth | Not supported (no daemon mode) |
| SSH auth key-only, host key pinning UI | Works but pinning is user-side (`~/.ssh/known_hosts`) | Pinning is part of module flow, mandatory, visible in AeroFTP UI |
| Byte-level delta on very large single files | Does it, gold standard | Does it, verified byte-identical against rsync 3.4.1 in CI lane 3 |

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

On 1 MiB single-file upload against rsync 3.4.1, lane 3 closes in ~330 ms with sha256 match. No published benchmark vs native rsync at parity hardware: the two transfer the same wire payload, so the difference is essentially in CPU encode/decode (Rust vs C) + SSH setup overhead. Don't expect miracles: the only big difference is "**it works even where rsync isn't**".

> **"Why xxh128 and not MD5 like modern rsync?"**

Architectural decision of the module. xxh128 is non-cryptographic (faster than MD5) and adequate as integrity checksum when not defending against active adversaries (host-key pinning and SSH transport cover the security part). For full interop with rsync requiring a specific strong hash in negotiation, the module selects the appropriate algorithm in handshake (see `real_wire.rs`).

> **"Is it production-ready?"**

For the use case *delta accelerator inside AeroFTP*: yes, runtime toggle is **on by default** (`Auto` mode) since v3.8.0, the cross-OS host-key asymmetry has been resolved, and the SFTP-key path is the production default. For the use case *standalone rsync client*: no, and the published crate explicitly declares this. The roadmap to v0.1.0 depends on the three promotion gates, not a hard ETA.

---

## What NOT to say externally (to avoid over-claiming)

- "AeroRsync replaces rsync": it is a wire-31 client, not a functional substitute for the binary
- "Supports all rsync options": supports wire protocol for single-file delta, not `-a`, `--delete`, `--inplace`, etc.
- "Works wherever rsync works": no daemon mode, no recursive tree, no advanced metadata, and protocol-31 only (an endpoint whose `rsync --server` negotiates protocol 27-30, or whose wrapper rejects the standard server flags, is out of scope: use the stock rsync binary there). Password auth and SSH agent auth (Unix) are wired into the native transport
- "The aerorsync crate is available on crates.io": the name is registered but the crate is 0.0.x without public API
- "AeroRsync is safer than rsync because Rust": it is memory-safe, but rsync has 30 years of hardening; the correct claim is "memory-safe by construction, different security model, mandatory host-key pinning in the flow"

---

## One line per audience

- **For Rust developers**: "Clean-room Rust impl of rsync wire protocol 31, single-file delta over SSH, in-process inside AeroFTP, GPL-3.0."
- **For AeroFTP end users**: "Lets AeroFTP do delta sync to standard rsync servers even on Windows, without installing the rsync binary."
- **For the rsync community**: "Not a fork, not a competitor, an independent wire-31 client that talks to real `rsync --server` and tests byte-identical against rsync 3.4.1 in CI."
- **For partners (Hetzner, rsync.net, Filen)**: "The native AeroRsync module speaks wire-31 byte-identical with rsync 3.4.1 and is in production single-file inside AeroFTP for cross-OS coverage, mode `auto` (native first, classic fallback) by default since v3.8.0. Key-based endpoints (rsync.net, Hetzner) work today through the SFTP provider's delta path, including SSH agent identities on Unix, and password-backed SFTP profiles enter the native transport directly. Endpoints that run an old or stripped `rsync --server` (protocol 27-30) fall outside the protocol-31 native engine and are served by the stock rsync binary instead."

---

*Last updated: 2026-06-22 (general docs audit); previously 2026-05-14, after: password-backed SFTP profiles entering the native transport, SSH agent auth on Unix, an opt-in sparse-write analogue on the local delta path, a proto-31 symlink wire codec (round-trip tested; end-to-end + byte-fixture validation pending), and a generic per-endpoint preamble-profile hook with env-tunable knobs. Updated when the AeroRsync module reaches new milestones (symlink end-to-end, recursive scope expansion).*
