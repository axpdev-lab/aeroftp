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
`zstd`, `xxhash-rust`) for SSH transport, compression and hashing: it
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
> `native_rsync_enabled` **defaults ON since `Z.1.5` (2026-05-12)**
> for fresh installs after the `AERORSYNC_HOST_KEY_ALGS` + russh
> `Preferred.key` follow-up closed the host-key algorithm negotiation
> asymmetry that had motivated the temporary OFF default in `aca4577c`.
> Existing installs keep the user-persisted value. The config key
> retains its historical name for backward compatibility of persisted
> settings. The module ships with wire-protocol 31 parity for the
> single-file delta path on Unix; on Windows the Tauri / sync_tree
> call surface compiles since Patch v2 (2026-05-21), but the runtime
> batch path is currently tracked under `Z.4.3.f6` (channel-multiplex
> deadlock on session reuse).
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

- **Protocol 31/32 wire format**: varint/varlong, preamble (client + server), file-list entries, signature phase (sum_head + sum_block), delta ops (literal + match + literali compressi nel compressore negoziato: zstd, oppure `zlibx` = raw deflate con `Z_SYNC_FLUSH` per record), summary frame, whole-file checksum trailer nel winner negoziato
- **Advertisement dei compressori = promessa, non wish list**: annunciamo `zstd zlibx none`, non la lista di stock rsync `zstd lz4 zlibx zlib none`. Il vincitore e' il primo nome della **nostra** lista che il peer ha anche lui (`compat.c::parse_negotiate_str`), quindi annunciare un codec che non sappiamo guidare significa perdere contro noi stessi: con la lista di stock, un peer compilato con lz4 ma senza zstd ci faceva scegliere `lz4`, per cui non esiste nessun codec nel modulo, mentre `zlibx` stava una posizione sotto, implementato e misurato. `none` in coda garantisce che le due liste si intersechino sempre, cosi' un peer che offre solo codec che rifiutiamo ottiene literal non compressi (delta funzionante) invece di un'intersezione vuota. `zlib` semplice e' rifiutato per motivi strutturali: accoppia i matched block nella history del compressore (`token.c::see_deflate_token`) e per decodificarlo serve `inflateIncomp`, funzione che rsync patcha nella zlib che si porta dentro e che non esiste ne' nella zlib di sistema ne' in alcun crate Rust. E' esattamente il motivo per cui upstream ha creato `zlibx`
- **Checksum negoziati first-class (Y-RSC.3)**: xxh128, xxh3, xxh64, md5, md4 e sha1 sono implementati in entrambi i ruoli e in entrambe le direzioni (block-strong signatures seeded + whole-file trailer/flist digest unseeded, semantica derivata da rsync 3.2.7 `checksum.c`). L'advertisement di default resta `xxh128 xxh3 xxh64 md5 md4` (byte-pinned, md4 last-resort); sha1 non è pubblicizzato di default e diventa negoziabile via `AEROFTP_RSYNC_CSUM_ALGOS=sha1`. La verify whole-file lato download è un no-op onesto solo per i winner non implementati raggiungibili via override (sha256/sha512/none)
- **Multiplex framing** bidirezionale attivato dopo il preamble (`MPLEX_BASE = 7`)
- **Remote-shell mode** via SSH (`SshRemoteShellTransport` con libssh2), host key pinning obbligatorio
- **Single-file transfer** (batch / session reuse chiuso con `AerorsyncBatch`, P3-T01 W3)
- **Explicit sender/receiver role split** nel driver state machine

## Gating

| Gate | Default | Effetto |
|---|---|---|
| Cargo feature `aerorsync` | on | Compila il backend nativo e i test del modulo; si puo disattivare con `--no-default-features` per build/debug lean |
| `settings::load_native_rsync_mode()` (runtime TOML) | `auto` (ON da v3.8.0) | In `auto`/`native` e feature attiva, `SftpProvider::delta_transport()` ritorna `AerorsyncDeltaTransport` (con fallback classic su soft refusal); in `classic` forza `RsyncBinaryTransport` su Unix o SFTP pieno su Windows |
| `#[cfg(ci_lane3)]` su `driver_upload_live_lane_3_real_rsync_byte_identical` (+ twin streaming) | spento | Attivato in CI con `RUSTFLAGS='--cfg ci_lane3'` su branch `strada-c-*` |

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

- **605 unit/integration test passano, 0 failed** (contro byte reali di rsync 3.2.7 frozen e contro l'oracolo di byte catturati da rsync 3.1.3): wire, protocol, compression zstd e deflate, file-list, delta ops, summary frame, xxh128 (snapshot 2026-07-28)
- **8 live test** `#[ignore]` sulla matrice dei checksum negoziati (xxh128, xxh3, xxh64, md5, md4, sha1) che pilotano i transport di upload e download di produzione, piu' i live test per fixture Docker + **3 benchmark live** `#[ignore]` (`aerorsync_bench_*`, confronto diretto con rsync nativo sullo stesso harness)
- **11 CI test** `ci_lane3` contro stock rsync 3.2.7 reale in Docker: upload byte-identical (sha256 match), upload streaming, symlink in entrambe le direzioni, `user.*` xattr inline / out-of-band / binario con NUL / vuoto, il path batch su una sola sessione, e un symlink che prova di non ereditare gli attributi del target. 2026-07-21: fix del file-list entry shape dei due test bulk/streaming (vedi report audit in `docs/dev/roadmap/APPENDIX-C-Y-D/APPENDIX-Y/reports/`)
- **1 lane deflate dedicata** in `.github/workflows/aerorsync-protocol.yml`: fixture Docker con client e server entrambi pinnati a rsync 3.1.3 protocollo 31, che e' il vintage senza zstd, piu' l'oracolo di byte catturati che pinna il framing dei token `zlibx`

## Limiti noti (da chiudere)

1. ~~**Stock rsync interop**: production dispatch still uses `aerorsync_serve`~~ Done: Blocco B chiuso il 2026-04-26. Production dispatch usa stock `rsync --server` (WrapperParity); pin test in `remote_command::tests`. Live gate verde con sha256 match contro rsync 3.4.1.
1a. ~~**Multi-chunk DEFLATED_DATA splitting (S8j)**: cap 16 KiB per literal~~ Done (2026-04-26): `send_delta_phase_single_file` splitta i blob zstd oltre `MAX_DELTA_LITERAL_LEN` in N DEFLATED_DATA consecutivi (mirror di `token.c::send_zstd_token`). Live upload 1 MiB contro rsync 3.4.1 passa con sha256 match in ~330 ms.
2a. ~~**Cap in-memory 256 MiB upload-side** (`AERORSYNC_MAX_IN_MEMORY_BYTES`)~~ Done (P3-T01 W1.3): `upload_inner` apre la sorgente come `tokio::fs::File` e la fa scorrere via `drive_upload_through_delta_streaming` (W1.2). Sources di qualsiasi dimensione passano per la streaming path; il cap upload-side è rimosso. RSS proporzionale a `source_len` per il caso `block_size == 0` finché lo zstd encoder + wire emission non saranno streaming-aware (post-P3-T01).
2b. ~~**Cap in-memory 256 MiB download-side**~~ Done (P3-T01 W2.5): `download_inner` apre il baseline locale come `FileBaseline` per il `CopyBlock` dispatch e i bytes ricostruiti scorrono attraverso `StreamingAtomicWriter` (`<target>.aerotmp` → `finalize` con rename atomico). Il cap `AERORSYNC_MAX_IN_MEMORY_BYTES` è eliminato. RSS scala con `O(baseline + writer_buffer)` invece di `O(baseline + reconstructed)`. Y-RSC.5: signature phase streams via `send_signature_phase_from_baseline` (no bulk `tokio::fs::read`); peak RSS `O(block_size + writer_buffer)`. **W2.1** (additivo): `BaselineSource` trait + `FileBaseline` + `MemoryBaseline`. **W2.2** (additivo): `apply_delta_streaming(baseline, ops, block_size, writer) -> io::Result<u64>` con pin parity bit-for-bit contro `delta_sync::apply_delta`. **W2.3** (additivo): `StreamingAtomicWriter` in `streaming_writer.rs`, kill-9 invariant: drop senza finalize lascia il temp orfano e il `target` originale intatto. **W2.4+W2.5** (refactor): `drive_download_through_delta_streaming(spec, baseline, writer, adapter, bridge)` accetta il writer come `&mut (dyn AsyncWrite + Send + Unpin)` parametro. Il caller mantiene full ownership del `StreamingAtomicWriter` per chiamare `finalize(mode, mtime)` dopo che il driver ritorna. I 3 test mock download esistenti (`driver_download_delta_*`) restano la non-regression del path bulk.
3. ~~**Session reuse**: ogni file apre una nuova sessione SSH~~ Done (P3-T01 W3, `AerorsyncBatch` in `delta_transport_impl.rs`): una sessione SSH racchiude N file, usata da sync-tree core, DAG serial lane e CLI sync. Validata live (Z.1.1, smoke KPI).
4. **Scope funzionale**: single-file delta accelerator, non sostituto completo di rsync. Le due categorie qui sotto non sono la stessa cosa e non vanno accorpate in un unico backlog (vedi `docs/PROTOCOL-RSYNC-COMPARE.md`, tassonomia operativa). **(a) Fuori scope per design**, perche' l'autorita' vive a un altro layer o e' un altro transport: recursive tree sync, `--delete*`, `--backup`, `--link-dest` (cancellazione e retention sono di AeroSync/AeroCloud, con i gate v4.1.6 AUDIT-03: se implementati a livello wire creerebbero una seconda autorita' di cancellazione piu' debole sotto quella appena irrobustita), `--inplace` / `--append` / `--partial-dir` (la strategia di scrittura e' di `StreamingAtomicWriter`, e `--inplace` ne indebolirebbe l'invariante kill-9), filtri `--exclude` / `--include` / `--files-from` a livello wire (AeroFTP filtra un layer sopra con `.aeroignore`, fail-closed da v4.1.6; il costo residuo e' il numero di sessioni, assorbito in gran parte da `AerorsyncBatch`), daemon mode `rsync://`. **(b) Non ancora implementato**, gap di parita' reale, in ordine di sforzo: ACL (`-A`, prossimo candidato: poggia su xattr anche in rsync, POSIX-only), owner/group (`-o`/`-g`: uid/gid e nomi sono gia' emessi nella file-list entry perche' il formato wire lo richiede, ma `StreamingAtomicWriter::finalize` applica solo mode e mtime), device/special files, hardlink (`-H`: bloccato strutturalmente, non per sforzo, perche' richiede una mappa device/inode sull'intera file-list, che e' un concetto tree-scope). **Supportati**: symlink end-to-end su Unix (Y-RSC.4: lstat della sorgente con `S_IFLNK` + target, ricreazione atomica lato receiver, `-l` preserve-links gia' presente nella flag string catturata, fail-closed su target non-Unix; il target ricevuto dal peer e' sanificato safe-links di default, un target assoluto o che risale sopra la directory di download e' rifiutato per non fidarsi di un server ostile, audit S1); **`user.*` xattr (`-X`) su Unix** (B1-B4: flag string, blob inline + sezione OOB, read/apply locale prima del rename, live lane 3 vs stock rsync, opt-in produzione via `SftpProvider::delta_transport` su Unix; Windows resta off, nessun analogo `user.*`; ENOTSUP soft di default); un analogo `--sparse` opt-in sul local delta path (hole-punched writes, output byte-identical). **`--mkpath` NON e' implementato**: nessuna creazione del parent remoto esiste nel modulo, verificato 2026-07-28 con un grep che non trova nulla ne' in `aerorsync/` ne' nel path delta del provider. Questo documento lo dichiarava supportato.

## File del modulo

- `mod.rs`: dichiarazione modulo + gating `aerorsync`
- `real_wire.rs` (~6 200 LOC): wire format encode/decode rsync 31/32
- `native_driver.rs` (~8 300 LOC): state machine upload/download
- `tests.rs` (~3 800 LOC): unit tests contro frozen transcripts
- `delta_transport_impl.rs` (~3 300 LOC): `AerorsyncDeltaTransport` (impl `DeltaTransport`) + `AerorsyncBatch` session reuse
- `events.rs`, `ssh_transport.rs`, `driver.rs`, `server.rs`, `live_tests.rs`, `rsync_event_bridge.rs`: supporto
- `mock.rs`, `fixtures.rs`: test scaffolding
- `streaming_writer.rs` (W2.3): `StreamingAtomicWriter`, counterpart streaming di `delta_transport_impl::write_atomic_chunked` (`AsyncWrite` + `finalize` rename-last)
- altri: `types.rs`, `protocol.rs`, `planner.rs`, `engine_adapter.rs`, `transport.rs`, `frame_io.rs`, `fallback_policy.rs`, `remote_command.rs`

Totale: 26 file `.rs` + harness `capture/` (snapshot 2026-07-21).

## Cross-reference

- **Assessment 22 apr 2026**: `docs/dev/roadmap/APPENDIX-C-Y-D/APPENDIX-Y/2026-04-22_Native_Rsync_Assessment.md`
- **Piano Windows promozione**: `docs/dev/roadmap/APPENDIX-C-Y-D/APPENDIX-Y/tasks/active/PR-T11_Native_Rsync_Cross_OS.md`
- **Roadmap Y produzione**: `docs/dev/roadmap/APPENDIX-C-Y-D/APPENDIX-Y/2026-04-22_P1-T03_ROADMAP_Produzione_Evoluzione.md`
- **Trait pubblico `DeltaTransport`**: `src-tauri/src/delta_transport.rs`
- **Adapter classico fallback**: `src-tauri/src/rsync_over_ssh.rs` (`RsyncBinaryTransport`)
- **Dispatcher produzione**: `src-tauri/src/providers/sftp.rs::delta_transport()` (linea ~231)
