# rsync_native_proto

Native Rust implementation of rsync protocol 31/32 over SSH remote-shell.

## Notice & licensing

This is an **independent, clean-room Rust re-implementation** of the rsync
31/32 wire protocol. No rsync source code was copied into this tree. The
module depends only on permissively-licensed Rust crates (`russh`, `ssh2`,
`zstd`, `xxhash-rust`) for SSH transport, compression and hashing — it
neither links against librsync nor spawns the rsync binary.

The rsync project (rsync.samba.org) is GPL-3.0-or-later. AeroFTP is also
distributed under GPL-3.0-or-later (see the repo-level [`LICENSE`](../../../LICENSE)),
so licence compatibility is unconditional. The protocol itself (bytes
on wire, handshake sequence, file-list format) is not copyrightable —
interface specifications are idea/method, not expression (Sega v.
Accolade, Oracle v. Google).

## Status

> **Production** — with PR-T11 this module is the primary delta backend
> on Windows and the preferred path on Unix whenever the
> `proto_native_rsync` cargo feature is compiled in. The binary-rsync
> classic fallback stays available on Unix through `RsyncBinaryTransport`
> inside the same `DeltaTransport` trait surface. The `#![cfg(unix)]`
> file-level gates that used to block Windows were removed in
> `21d4448c` / `ecea5049`; `SftpProvider::delta_transport()` now
> dispatches cross-platform. The CI job
> `delta-sync-integration.yml::windows-native` builds and runs unit
> tests on `windows-latest` with and without the feature.
>
> Historical pre-S8 READMEs declared the module scaffold-only; this was
> accurate at that point in time and is preserved in the archive under
> [`docs/dev/roadmap/APPENDIX-C-Y-D/APPENDIX-Y/archive/strada-c/`](../../../docs/dev/roadmap/APPENDIX-C-Y-D/APPENDIX-Y/archive/strada-c/)
> (gitignored, developer-side).

## Scope del modulo

- **Protocol 31/32 wire format** — varint/varlong, preamble (client + server), file-list entries, signature phase (sum_head + sum_block), delta ops (literal + match + zstd-compressed literals), summary frame, xxh128 checksum trailer
- **Multiplex framing** bidirezionale attivato dopo il preamble (`MPLEX_BASE = 7`)
- **Remote-shell mode** via SSH (`SshRemoteShellTransport` con libssh2), host key pinning obbligatorio
- **Single-file transfer** (batch / session reuse è scope P3-T01 / EV-T03, fuori release corrente)
- **Explicit sender/receiver role split** nel driver state machine

## Gating

| Gate | Default | Effetto |
|---|---|---|
| Cargo feature `proto_native_rsync` | off | Se off, il modulo compila come stub; nessun dispatch runtime lo raggiunge |
| `settings::load_native_rsync_enabled()` (runtime TOML) | false | Se true e feature attiva, `SftpProvider::delta_transport()` ritorna `NativeRsyncDeltaTransport`; altrimenti `RsyncBinaryTransport` |
| `#[cfg(ci_lane3)]` su `driver_upload_live_lane_3_real_rsync_byte_identical` | spento | Attivato in CI con `RUSTFLAGS='--cfg ci_lane3'` su branch `strada-c-*` |
| `#![cfg(unix)]` su `delta_transport.rs` / `delta_sync_rsync.rs` | presente | **BLOCCA Windows** — target di rimozione in PR-T11 |

## Come esercitare

```bash
# Compile check con feature on
cargo check  --features proto_native_rsync

# Clippy (D warnings)
cargo clippy --features proto_native_rsync --all-targets -- -D warnings

# Unit tests (contro frozen transcripts catturati da rsync 3.2.7 reale)
cargo test --features proto_native_rsync --lib rsync_native_proto

# Live greeting test contro rsync reale (richiede env vars al fixture Docker)
RSNP_TEST_REAL_SSH_KEY=.../ssh_key \
RSNP_TEST_REAL_HOST_FINGERPRINT=<sha256-hex> \
RSNP_TEST_REAL_REMOTE_UPLOAD_TARGET=/workdir/probe.bin \
cargo test --features proto_native_rsync \
  rsync_native_proto::live_tests::live_real_rsync_lane \
  -- --ignored --nocapture

# CI lane 3 full-upload byte-identical contro rsync 3.2.7 in Docker
RUSTFLAGS='--cfg ci_lane3' \
cargo test --features proto_native_rsync \
  driver_upload_live_lane_3_real_rsync_byte_identical
```

## Stato test

- **386 unit test passano** (contro byte reali di rsync 3.2.7 frozen): wire, protocol, compression, file-list, delta ops, summary frame, xxh128
- **6 live test** `#[ignore]` per fixture Docker (RSNP server proprietario + real-rsync lane)
- **1 CI test** `driver_upload_live_lane_3_real_rsync_byte_identical` gated `ci_lane3` — asserisce upload byte-identical contro rsync 3.2.7 reale, `phase == Complete`, `bytes_sent >= payload`

## Limiti noti (da chiudere)

1. **`#![cfg(unix)]` su `delta_transport.rs` + `delta_sync_rsync.rs` + `SftpProvider::delta_transport`** → nativi disponibili solo su Unix oggi. Rimozione chirurgica → PR-T11 scope.
2. **Cap in-memory 256 MiB** (`NATIVE_MAX_IN_MEMORY_BYTES`) → file sopra soglia ricadono al classic wrapper (Unix) o generano errore chiaro (Windows senza wrapper). Streaming chunked = scope P3-T01.
3. **Session reuse**: ogni file apre una nuova sessione SSH. Overhead visibile su batch di molti file piccoli. Scope P3-T01 / EV-T03.
4. **Writer path documentation stale nei commenti di `real_wire.rs`** che dicono "decode-only, writer paths deferred a S8c-bis/S8e" — **non più vero**, 131 funzioni `encode_*` implementate e testate. Commenti da pulire nel prossimo passaggio di refactor.

## File del modulo

- `mod.rs` — dichiarazione modulo + gating `proto_native_rsync`
- `real_wire.rs` (5 704 LOC) — wire format encode/decode rsync 31/32
- `native_driver.rs` (3 745 LOC) — state machine upload/download
- `tests.rs` (3 754 LOC) — unit tests contro frozen transcripts
- `delta_transport_impl.rs` (1 139 LOC) — `NativeRsyncDeltaTransport` (impl `DeltaTransport`)
- `events.rs`, `ssh_transport.rs`, `driver.rs`, `server.rs`, `live_tests.rs`, `rsync_event_bridge.rs` — supporto
- `mock.rs`, `fixtures.rs` — test scaffolding
- altri: `types.rs`, `protocol.rs`, `planner.rs`, `engine_adapter.rs`, `transport.rs`, `frame_io.rs`, `fallback_policy.rs`, `remote_command.rs`

Totale: 23 file, 20 604 LOC.

## Cross-reference

- **Assessment 22 apr 2026**: `docs/dev/roadmap/APPENDIX-C-Y-D/APPENDIX-Y/2026-04-22_Native_Rsync_Assessment.md`
- **Piano Windows promozione**: `docs/dev/roadmap/APPENDIX-C-Y-D/APPENDIX-Y/tasks/active/PR-T11_Native_Rsync_Cross_OS.md`
- **Roadmap Y produzione**: `docs/dev/roadmap/APPENDIX-C-Y-D/APPENDIX-Y/2026-04-22_P1-T03_ROADMAP_Produzione_Evoluzione.md`
- **Trait pubblico `DeltaTransport`**: `src-tauri/src/delta_transport.rs`
- **Adapter classico fallback**: `src-tauri/src/rsync_over_ssh.rs` (`RsyncBinaryTransport`)
- **Dispatcher produzione**: `src-tauri/src/providers/sftp.rs::delta_transport()` (linea ~231)
