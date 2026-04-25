# aerorsync

**aerorsync** is AeroFTP's native Rust implementation of the rsync wire
protocol 31, an independent clean-room component of the Aero family.
Historical code name: Strada C / `rsync_native_proto`.

## Mission & scope

Speak rsync protocol 31 on the wire from pure Rust, so AeroFTP can deliver
byte-level delta sync on platforms where the stock `rsync` binary is not
readily available (Windows first-class) and as an opt-in accelerator on
Unix. Full rsync parity is the north-star of the roadmap, not a shipped
claim: the current scope is single-file transfers over SSH with the subset
documented in *Limiti noti* below.

`aerorsync` does **not** bundle or replace the `rsync` binary. Users with
`rsync` installed keep the classic `RsyncBinaryTransport` path (Unix only)
available inside the same `DeltaTransport` trait surface. `aerorsync`
complements that path; it does not supplant it.

## Notice & licensing

This is an **independent, clean-room Rust re-implementation** of the rsync
wire protocol. No rsync source code was copied into this tree. The module
depends only on permissively-licensed Rust crates (`russh`, `ssh2`,
`zstd`, `xxhash-rust`) for SSH transport, compression and hashing — it
neither links against librsync nor spawns the rsync binary.

The rsync project (rsync.samba.org) is GPL-3.0-or-later. AeroFTP is also
distributed under GPL-3.0-or-later (see the repo-level [`LICENSE`](../../../LICENSE)),
so licence compatibility is unconditional. The wire protocol itself
(bytes on wire, handshake sequence, file-list format) is not copyrightable:
interface specifications are idea/method, not expression (Sega v. Accolade,
Oracle v. Google). Precedent of an rsync-named clean-room reimplementation
in a different language: OpenBSD's `openrsync` (2019→, BSD-licensed,
shipped as default on OpenBSD).

## Status

> Cargo feature `aerorsync` is compiled by default. Runtime toggle
> `native_rsync_enabled` defaults OFF since `aca4577c` pending the
> host-key algorithm negotiation asymmetry fix (the config key retains
> its historical name for backward compatibility of persisted user
> settings). The module ships with wire-protocol 31 parity for the
> single-file delta path.
>
> **Production dispatch (Blocco B closed 2026-04-26)**: `AerorsyncDeltaTransport`
> invokes stock `rsync --server` via `RemoteCommandFlavor::WrapperParity`
> only. The probe runs `rsync --version` and a missing binary maps to
> `RsyncError::RemoteNotAvailable` (soft classic fallback). The pin
> tests `remote_command::tests::{upload,download}_spec_is_always_wrapper_parity_for_production`
> guard the constructors from regressing to the dev helper. The
> `AerorsyncServe` flavor (and the `RemoteCommandSpec::aerorsync_upload` /
> `aerorsync_download` constructors) are kept alive exclusively for
> in-process mock tests and the `live_tests.rs` lane that runs the
> dev-only `/opt/aerorsync/bin/aerorsync_serve` binary under
> `#[cfg(all(test, feature = "aerorsync"))]`. Closure evidence in
> [`2026-04-26_Aerorsync_B2_Step5_Closure.md`](../../../docs/dev/roadmap/APPENDIX-C-Y-D/APPENDIX-Y/archive/aerorsync-saga-2026-04/2026-04-26_Aerorsync_B2_Step5_Closure.md).
>
> The binary-rsync classic fallback stays available on Unix through
> `RsyncBinaryTransport` inside the same `DeltaTransport` trait surface.
> `SftpProvider::delta_transport()` dispatches cross-platform when the
> feature is compiled and the runtime toggle is enabled.

## Scope del modulo

- **Protocol 31/32 wire format** — varint/varlong, preamble (client + server), file-list entries, signature phase (sum_head + sum_block), delta ops (literal + match + zstd-compressed literals), summary frame, xxh128 checksum trailer
- **Multiplex framing** bidirezionale attivato dopo il preamble (`MPLEX_BASE = 7`)
- **Remote-shell mode** via SSH (`SshRemoteShellTransport` con libssh2), host key pinning obbligatorio
- **Single-file transfer** (batch / session reuse è scope P3-T01 / EV-T03, fuori release corrente)
- **Explicit sender/receiver role split** nel driver state machine

## Gating

| Gate | Default | Effetto |
|---|---|---|
| Cargo feature `aerorsync` | on | Compila il backend nativo e i test del modulo; si puo disattivare con `--no-default-features` per build/debug lean |
| `settings::load_native_rsync_enabled()` (runtime TOML) | false | Se true e feature attiva, `SftpProvider::delta_transport()` ritorna `AerorsyncDeltaTransport`; altrimenti `RsyncBinaryTransport` su Unix o SFTP pieno su Windows |
| `#[cfg(ci_lane3)]` su `driver_upload_live_lane_3_real_rsync_byte_identical` | spento | Attivato in CI con `RUSTFLAGS='--cfg ci_lane3'` su branch `strada-c-*` |

## Come esercitare

```bash
# Compile check con feature on
cargo check  --features aerorsync

# Clippy (D warnings)
cargo clippy --features aerorsync --all-targets -- -D warnings

# Unit tests (contro frozen transcripts catturati da rsync 3.2.7 reale)
cargo test --features aerorsync --lib aerorsync

# Live greeting test contro rsync reale (richiede env vars al fixture Docker)
RSNP_TEST_REAL_SSH_KEY=.../ssh_key \
RSNP_TEST_REAL_HOST_FINGERPRINT=<sha256-hex> \
RSNP_TEST_REAL_REMOTE_UPLOAD_TARGET=/workdir/probe.bin \
cargo test --features aerorsync \
  aerorsync::live_tests::live_real_rsync_lane \
  -- --ignored --nocapture

# CI lane 3 full-upload byte-identical contro rsync 3.2.7 in Docker
RUSTFLAGS='--cfg ci_lane3' \
cargo test --features aerorsync \
  driver_upload_live_lane_3_real_rsync_byte_identical
```

## Stato test

- **386 unit test passano** (contro byte reali di rsync 3.2.7 frozen): wire, protocol, compression, file-list, delta ops, summary frame, xxh128
- **6 live test** `#[ignore]` per fixture Docker (RSNP server proprietario + real-rsync lane)
- **1 CI test** `driver_upload_live_lane_3_real_rsync_byte_identical` gated `ci_lane3` — asserisce upload byte-identical contro rsync 3.2.7 reale, `phase == Complete`, `bytes_sent >= payload`

## Limiti noti (da chiudere)

1. ~~**Stock rsync interop**: production dispatch still uses `aerorsync_serve`~~ Done — Blocco B chiuso il 2026-04-26. Production dispatch usa stock `rsync --server` (WrapperParity); pin test in `remote_command::tests`. Live gate verde con sha256 match contro rsync 3.4.1.
1a. ~~**Multi-chunk DEFLATED_DATA splitting (S8j)**: cap 16 KiB per literal~~ Done (2026-04-26) — `send_delta_phase_single_file` splitta i blob zstd oltre `MAX_DELTA_LITERAL_LEN` in N DEFLATED_DATA consecutivi (mirror di `token.c::send_zstd_token`). Live upload 1 MiB contro rsync 3.4.1 passa con sha256 match in ~330 ms.
2. **Cap in-memory 256 MiB** (`AERORSYNC_MAX_IN_MEMORY_BYTES`) → file sopra soglia ricadono al classic wrapper (Unix) o generano errore chiaro (Windows senza wrapper). Streaming chunked = scope P3-T01.
3. **Session reuse**: ogni file apre una nuova sessione SSH. Overhead visibile su batch di molti file piccoli. Scope P3-T01 / EV-T03.
4. **Scope funzionale**: single-file delta accelerator, non sostituto completo di rsync. Fuori scope: recursive tree sync, symlink/hardlink, xattrs, ACL, `--inplace`, `--append`, `--delete*`, `--mkpath`, `--partial-dir`, `--sparse`, streaming multi-GB e session reuse cross-file.

## File del modulo

- `mod.rs` — dichiarazione modulo + gating `aerorsync`
- `real_wire.rs` (5 704 LOC) — wire format encode/decode rsync 31/32
- `native_driver.rs` (3 745 LOC) — state machine upload/download
- `tests.rs` (3 754 LOC) — unit tests contro frozen transcripts
- `delta_transport_impl.rs` (1 139 LOC) — `AerorsyncDeltaTransport` (impl `DeltaTransport`)
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
