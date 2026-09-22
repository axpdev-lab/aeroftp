# Dependency refresh — 2026-09-23

## Patch batch

Base: origin/main at `52d55cf19`. This base already includes clap 4.6.7,
clap_complete 4.6.11, and trash 5.2.9. Three patch updates remain.

Update the application lockfile only:

| Dependency | Before | After |
| --- | --- | --- |
| rand (0.10 lineage only) | 0.10.2 | 0.10.3 |
| tauri-plugin-log | 2.9.1 | 2.9.2 |
| tauri-plugin-single-instance | 2.4.4 | 2.4.5 |

No manifest requirements or exact pins change. The standalone peer-l0 lockfile
is outside this app batch. Main already uses aerovault 0.6.6 and
chacha20poly1305 0.11.0.

Notable upstream changes: single-instance allows Windows foreground activation
by the first instance; log changes the webview location separator to `::`.

## Pin exit checks

These conclusions use the published crate sources, not just release labels.

| Pin / constraint | Evidence | Decision |
| --- | --- | --- |
| suppaftp 10.0.2 fork | 12.0.1 still keeps the reply BufReader private. ControlSocket exposes the TCP socket, not buffered reply bytes. TransferStream reads completion replies in finish(); poll_read delegates to the data stream. | Keep. The coalesced preliminary/failure reply with a stalled data channel is not addressed by these APIs. |
| tauri 2.11.0 | 2.11.6 still enforces the IPC gate when `!is_local`; the Linux localhost-origin constraint remains relevant. | Keep; no runtime migration attempted. |
| aes-gcm 0.10.3 | The resolved noq-proto 1.3.0 still requires `aes-gcm ^0.10.3`. | Keep the existing cohort decision. |
| aes 0.8.4 / cbc 0.1.2 | Coupled cipher-generation migration; the three remaining patch updates do not change these constraints. | Keep; assess with the crypto batch. |
| n0-mainline git patch | iroh-mainline-address-lookup 0.5.0 requires n0-mainline 0.6. Its ed25519-dalek requirement is `>=3.0.0-rc.0, <4.0.0`, admitting stable 3.0.0. | Candidate for removal in a dedicated P2P batch. |

A scratch copy of the app manifests and lockfile, with both discovery crates
set to 0.5 and **only** the n0-mainline patch removed, resolves successfully:
both discovery crates become 0.5.0 and registry n0-mainline 0.6.0 replaces the
git fork. The suppaftp patch remains intact. This is resolution evidence only;
P2P compilation and DHT/LAN/identity regression tests remain necessary.

Suppaftp 11 fixes graceful FTPS retrieval shutdown; 12 introduces TransferStream
and cancellation-safe finalization. Those improvements warrant a separate
migration, but do not establish that our buffer-readiness requirement is met.
Migration must cover coalesced replies, stalled data channels, cancellation,
FTPS close_notify, and control-channel reuse.

The existing n0-mainline watcher removes the entire patch section in its
scratch probe. Before using it as proof or retiring it, account for the
independent suppaftp patch; do not remove both patches from the real manifest.

## Sources

- https://docs.rs/crate/suppaftp/12.0.1/source/src/async_ftp/tokio_ftp/control.rs
- https://docs.rs/crate/suppaftp/12.0.1/source/src/async_ftp/tokio_ftp/transfer_stream.rs
- https://github.com/veeso/suppaftp/blob/main/CHANGELOG.md
- https://docs.rs/crate/tauri/2.11.6/source/src/webview/mod.rs
- https://docs.rs/crate/iroh-mainline-address-lookup/0.5.0/source/Cargo.toml
- https://index.crates.io/n0/-m/n0-mainline
- https://index.crates.io/no/q-/noq-proto

## Validation

On the synchronized base with the three updates:

- `cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check`: passed.
- `cargo test --manifest-path src-tauri/Cargo.toml --locked --lib aerorsync::russh_session_transport::tests`: 18 passed, including generated-key pinning tests.
- `cargo test --manifest-path src-tauri/Cargo.toml --locked --lib dependency_index::tests`: 11 passed.
- `git diff --check`: passed.

Multi-platform PR CI remains the full gate. No live FTP/P2P migration or GUI
behavior is claimed by this patch batch.
