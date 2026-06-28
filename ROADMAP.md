# AeroFTP Roadmap

> _Last updated: 2026-06-28_

> A transparent view of where AeroFTP has been, where it is today, and where it's headed.
> This roadmap is updated continuously. Feature requests and feedback are welcome via [GitHub Issues](https://github.com/axpdev-lab/aeroftp/issues).

> **This roadmap is indicative.** The order in which items are picked up may change based on technical evaluations made during development, dependencies between features, community feedback, and security findings. Items can move between lanes (or be deferred) without notice.

---

## At a Glance

A continuous flow rather than a calendar. Items move from right to left as they ship.

| 🟢 **Just Shipped** | 🟡 **In Flight** | 🔵 **Up Next** | ⚪ **On the Horizon** |
|---|---|---|---|
| Available in the latest release | Actively being worked on, ready to release soon | Confirmed for an upcoming release, design done | Planned but not yet started |

### Status index

Every roadmap item at a glance; the lanes below carry the full detail.

| Status | Item | Target |
|---|---|---|
| 🟢 Shipped | AeroShare P2P beta preview + AeroAgent coding loop foundation + per-user groups/favourites + benchmark/bridge hardening | v4.1.0 |
| 🟢 Shipped | AeroVault create redesign + AeroVault Zip plaintext lane + universal rclone export | v4.0.9 |
| 🟢 Shipped | AeroProgress transfer card + AeroCrypt first-class Crypt profile + Quick Connect polish | v4.0.8 |
| 🟢 Shipped | AeroVault dual blind security audit + error-correction crate convergence | v4.0.7 |
| 🟢 Shipped | AeroVault crate convergence (engine + ECC into the published crate) | v4.0.6 |
| 🟢 Shipped | AeroVault v4 error correction + AeroCrypt encrypted overlay | v4.0.5 |
| 🟢 Shipped | Reversible restricted-filename encoding + CLI polish | v4.0.4 |
| 🟢 Shipped | Community catalog + two-stage CLI security audit | v4.0.3 |
| 🟢 Shipped | Cross-machine keystore portability | v4.0.2 |
| 🟢 Shipped | S3 native AssumeRole + AeroVault audit hardening | v4.0.1 |
| 🟢 Shipped | Shaped Graph Transfer (DAG) + Multi-User Account Partition | v4.0.0 |
| 🟢 Shipped | AeroVault wrapper-stack + full CLI vault parity + AeroRsync streaming | v3.8.0 |
| 🟢 Shipped | AeroVault v3 (Archive tier) + AeroFile Dual Panel Slice A | v3.7.9 |
| 🟢 Shipped | AeroCrypt overlay first-class + ImageKit / Uploadcare + CLI audit | v3.7.2 |
| 🟢 Shipped | Persistent Mount Manager (GUI + CLI) | v3.7.1 |
| 🟢 Shipped (Beta) | P2P peer transfer (AeroShare) | v4.1.0 |
| 🟡 In Flight | Bitbucket / Gitea / Forgejo native integrations | next release |
| 🟡 In Flight | Selectable XChaCha20 vault cipher | next release |
| 🔵 Up Next | Share Link UX redesign (QR, analytics, team sharing) | planned |
| 🔵 Up Next | VS Code Remote Explorer extension | planned |
| 🔵 Up Next | Deploy Engine (one-click self-hosted server) | planned |
| 🔵 Up Next | Photo & media services expansion | planned |
| 🔵 Up Next | Agent Orchestration v2 (mutative remote ops) | planned |
| 🔵 Up Next | AeroVault v2 enhancements (migration, key rotation) | planned |
| 🔵 Up Next | Mobile-friendly window dimensions | planned |
| ⚪ Horizon | AeroIndex (content-aware file intelligence) | exploring |
| ⚪ Horizon | Mobile companion app (Android) | exploring |
| ⚪ Horizon | Flathub publish | exploring |
| ⚪ Horizon | IPFS / Web3 storage | exploring |
| ⚪ Horizon | Tor support | exploring |
| ⚪ Horizon | Biometric unlock | exploring |
| ⚪ Horizon | Per-protocol comparison page in docs | exploring |
| ⚪ Horizon | Keyboard accessibility: Tab traversal | exploring |

### 🟢 Just Shipped

- **AeroShare peer-to-peer transfer (Beta), AeroAgent coding loop and per-user groups** (v4.1.0)
  AeroShare arrives as a Beta preview: direct, end-to-end-encrypted device-to-device transfers with no server in the middle, built on iroh 1.0 with Mainline-DHT discovery and federated relays. It is always-on at launch (Discover tile with a 256-bit E2E badge, a titlebar **+friend** button, a draggable hub and a status-bar receiver indicator); adding a friend or sharing a folder auto-activates it, while the standing receive loop stays opt-in. AeroAgent gains a foundation coding loop (GUI-only, read-only or approval-gated): ripgrep workspace search, structured cargo/tsc/eslint diagnostics, git read plus stage and commit, a run-checks runner and an ordered verify, and safe patch with automatic checkpoints. Server groups and favourites move from a single global blob to each user's encrypted partition, and the interactive CLI grows a `New(N)` verb, `groups -i` member add/remove and a safe-first action bar ([#311](https://github.com/axpdev-lab/aeroftp/discussions/311), **Ehud Kirsh**). The benchmark gets real fixes (#368), the Filen Desktop bridges work headless, OS "Extract here / to folder" verbs land on Nautilus and Windows, Windows USB eject works, and the in-app sigstore update verification (sigstore 0.14) verifies for real again.
- **AeroVault create redesign, the AeroVault Zip plaintext lane and universal rclone export** (v4.0.9)
  AeroVault create is rebuilt Compressor-style: a named vault, mode cards with a conditional password, a result receipt instead of a jump into the browser, and a single tabbed shell (Home, Recent, Files) shared by the standalone modal and the browser ([#322](https://github.com/axpdev-lab/aeroftp/issues/322), **Ehud Kirsh**). A new AeroVault Zip plaintext lane adds a fast, honestly-unencrypted `.aerozip` archive format with optional recovery parity and a real measured size estimate, plus grid and list views and live progress bars across the vault and archive browsers. AeroMount gains a read-only mount and a one-shot Save-All for unlocked vaults (Ehud Kirsh's idea #1). On the bridge, rclone export now reaches Filen and every OAuth cloud provider, not just Jottacloud ([#128](https://github.com/axpdev-lab/aeroftp/issues/128)), the crypt overlay reads as a padlock at every site ([#272](https://github.com/axpdev-lab/aeroftp/issues/272)), and interactive `aeroftp groups` and `aeroftp users` join `profiles -i` on one engine ([#311](https://github.com/axpdev-lab/aeroftp/discussions/311)). Hardening: rclone config export strips CR/LF from every value (Backblaze B2 included) and the quinn-proto advisory RUSTSEC-2026-0185 is patched. A pre-release audit of all 86 changes closed 12 findings before tagging.
- **AeroProgress, Quick Connect polish and AeroCrypt as a first-class Crypt profile** (v4.0.8)
  AeroProgress brings back the floating transfer card with a lane per file, live speed, ETA and bytes, a collapsible speed graph and per-theme styling, with real progress across the Transfer Queue, vault creation and cross-profile transfers instead of a bare spinner or a frozen "Streaming" state. AeroCrypt is promoted to a first-class `Crypt` profile type with a navigate-out encrypted scope ([#272](https://github.com/axpdev-lab/aeroftp/issues/272), **Ehud Kirsh**), Quick Connect absorbs the v4.0.6 connection review with a per-mode credential isolation fix ([#215](https://github.com/axpdev-lab/aeroftp/issues/215)), Change Mode re-packs an open vault between the v2 and v3 formats, and AeroFTP's Cryptomator vaults are now byte-for-byte interoperable with the official Cryptomator in both directions ([#322](https://github.com/axpdev-lab/aeroftp/issues/322)). Bundles the `aerovault` crate 0.6.3 (streaming seal/extract, progress callbacks, per-shard health).
- **AeroVault dual blind security audit and error-correction crate convergence** (v4.0.7)
  AeroVault went through an independent dual blind security audit (Claude Opus 4.8 and Codex GPT-5) followed by full remediation and a two-round adversarial controaudit that closed every finding (1 High, 1 Medium, 3 Low, 4 Info, 0 Critical, grade A), verified on both the crate and the app. The error-correction engine moved into the published `aerovault` crate (0.6.2) as a single audited implementation shared by the desktop app, the CLI and any Rust consumer, with a cross-implementation golden keeping the bytes byte-for-byte identical. AeroVault extract now refuses to follow a Windows reparse-point or junction out of the destination, an interrupted seal leaves no leftover temp or lock, a forged extension directory is rejected at open, and `correct repair --expect-sha256` adds an authenticity anchor. The My Servers grid got container-aware column layout fixes, and DOMPurify was bumped to 3.4.11. The kill-cleanup pair was surfaced by **Ehud Kirsh**'s V3 Beta test pass.
- **AeroVault crate convergence** (v4.0.6)
  The AEROVAULT3 vault engine and its revision 4 Reed-Solomon error correction now live entirely in the published `aerovault` crate ([0.6.0 on crates.io](https://crates.io/crates/aerovault)): one audited implementation, shared byte-for-byte between the desktop app and the standalone CLI, with a cross-implementation fixture pinning the two to identical bytes so a vault made by either opens in the other. The app's vault commands became thin wrappers over the crate, around four thousand three hundred lines of duplicated cryptography removed. AEROVAULT3 design and the unified error-correction direction were driven by **Ehud Kirsh** ([#162](https://github.com/axpdev-lab/aeroftp/issues/162), [#276](https://github.com/axpdev-lab/aeroftp/discussions/276)). Also: cloud folder uploads now honor the skip and overwrite policy, and the benchmark profile picker marks selected rows with a checkmark ([#277](https://github.com/axpdev-lab/aeroftp/discussions/277), Ehud Kirsh).
- **AeroVault v4 error correction and the AeroCrypt encrypted overlay** (v4.0.5)
  AeroVault v4 adds a Reed-Solomon self-healing layer that scrubs a vault for damage and repairs it from embedded parity, leaving the vault byte-for-byte untouched when damage exceeds the recoverable budget (design anchor **Ehud Kirsh**, [#276](https://github.com/axpdev-lab/aeroftp/discussions/276)). AeroCrypt is a first-class native encrypted overlay (AES-256-GCM-SIV content, AES-256-SIV names, Argon2id) bound to a saved server profile, alongside a labelled rclone-crypt interop lane, opt-in with no default cipher and full GUI parity to the CLI. Plus CLI `compress`/`extract` for zip, 7z and tar with optional AES-256 passwords, the inline profiles action menu ([#311](https://github.com/axpdev-lab/aeroftp/issues/311), Ehud Kirsh), russh 0.61.2, and sync-cancel, master-password and Windows-portable fixes ([#332](https://github.com/axpdev-lab/aeroftp/issues/332), [#333](https://github.com/axpdev-lab/aeroftp/issues/333), [#334](https://github.com/axpdev-lab/aeroftp/issues/334), rockaut).
- **Reversible restricted-filename encoding, CLI polish and stability** (v4.0.4)
  Filenames containing characters a provider rejects now round-trip transparently on Box, Dropbox, Jottacloud and OpenDrive: control characters and each provider's reserved set are encoded with the rclone-compatible reversible scheme and decoded back on listing, so a name like `a:b` is preserved instead of failing silently ([#272](https://github.com/axpdev-lab/aeroftp/issues/272), [#266](https://github.com/axpdev-lab/aeroftp/issues/266), Ehud Kirsh). The interactive CLI absorbs the next wishlist wave ([#270](https://github.com/axpdev-lab/aeroftp/issues/270)): compact `u3`/`3u` user-switch and a visual diff on profile reorder. Stability: a tray badge update off the GTK main thread could corrupt the GLib heap on suspend/resume (fixed by marshalling onto the main thread), the macOS Tahoe main window now self-heals a poisoned zero size ([#290](https://github.com/axpdev-lab/aeroftp/issues/290), alexhorner), and an OAuth reconnect reuses the saved per-profile token instead of re-running the browser flow.
- **Cross-machine keystore portability and agent-facing polish** (v4.0.2)
  A keystore backup now carries a transport-wrapped key (Argon2id over the backup password) for each passphrase-less user partition and the import re-keys it to the local device, so an account that previously showed an empty My Servers after a cross-machine import populates correctly. The import is reversible (timestamped snapshot before overwrite) and legible (post-import summary), with a Repair multi-user data panel in Settings. Agent side: MCP `list_servers` returns lean identity fields by default (opt back in with `include_capabilities`), provider error messages stop leaking XML entities into JSON, and `--json` output is fully quiet.
- **Community roadmap, CLI security audit and transfer hardening** (v4.0.3)
  The Add Service page is rebuilt as a company-centric catalog with a list view, per-protocol categories, inline storage regions, a free/paid filter and a matching CLI `catalog` subcommand ([#224](https://github.com/axpdev-lab/aeroftp/issues/224), Ehud Kirsh); the Ehud wishlist lands across several waves ([#270](https://github.com/axpdev-lab/aeroftp/issues/270)); and the MEGAcmd WebDAV bridge auto-arms on connect ([#275](https://github.com/axpdev-lab/aeroftp/issues/275)). A two-stage independent CLI security audit (Codex plus Opus) hardens every destructive and agent-facing surface (roughly forty findings closed with new tests), the main window presents correctly on macOS Tahoe ([#290](https://github.com/axpdev-lab/aeroftp/issues/290)), and development builds are isolated from the released app's credentials ([#302](https://github.com/axpdev-lab/aeroftp/issues/302)).
- **S3 native AssumeRole, AeroVault audit hardening, settings consolidation** (v4.0.1)
  Connect to S3 by assuming an IAM role: a Role ARN (plus optional External ID, session name, duration and MFA) turns the access keys into base credentials that AeroFTP exchanges for temporary, role-scoped credentials via AWS STS at connect time, then re-assumes automatically before they expire. Built on a hand-rolled STS client (one SigV4-signed `AssumeRole` POST, no AWS SDK dependency) feeding the existing data-plane signer ([#301](https://github.com/axpdev-lab/aeroftp/issues/301), co-authored with the reporter). Plus the AeroVault dual-independent audit remediation (crate hardened to v3), the unified profile bridge (rclone / WinSCP / FileZilla through one dispatcher), the Servers settings tab folded into the Backup interoperability table ([#270](https://github.com/axpdev-lab/aeroftp/issues/270)), the first community wishlist batch ([#300](https://github.com/axpdev-lab/aeroftp/issues/300)), and a download-integrity fix for embedded rsync servers (WD MyCloud) that previously truncated delta downloads in silence.
- **Shaped Graph Transfer (DAG): single production path** (v4.0.0)
  The ready-frontier DAG transfer engine becomes the one production path for every transfer surface: single-file leaves, multi-file batches, sync sessions, intra-file segmented downloads, and cross-bucket copies. The three rollout flags and the hand-rolled JoinSet orchestrator are gone; a capability-aware shaped builder picks the right shape per call from each provider's `TransferCapabilities` (native multipart fan-out, server-side copy, segmented Range downloads), degrading honestly to a single stream when a backend advertises none of them. GUI, CLI, and MCP schedule through the same runners; the CLI exposes 25+ runtime knobs over the engine.
- **Multi-User Account Partition** (v4.0.0)
  The vault splits into per-user encrypted partitions (Argon2id-derived keys, AES partition encryption) while single-user installs stay fully compatible and migrate automatically. A boot-time Account Lock Screen lists the configured users, with per-user profiles and AeroSync settings, an opt-in admin role (last-admin guard, peer passphrase reset), and a CLI `--user` flag across profile and transfer commands.
- **AeroVault wrapper-stack hardening, telemetry, and full CLI vault parity** (v3.8.0)
  Real small-file packing exercised by the v3 write path, a behind-the-scenes technical receipt (`vault_telemetry::VaultReport`), and the `aeroftp-cli vault` subcommand extended to every format: `create/add/info/extract` for v1, v2 and v3 with header auto-detect, `--vault-version`, `--cascade` (v2 paranoid), `--receipt`. Sustained design contribution by **Ehud Kirsh** ([#162](https://github.com/axpdev-lab/aeroftp/issues/162)).
- **Storage quota override, recursive used-storage scan, and compression columns** (v3.8.0)
  Manual total-cap override per profile (a TRUE override beating an API total), an explicit user-triggered recursive `used` scan for no-quota FTP/FTPS/SFTP/S3/WebDAV (CLI `df --scan`/`--full`, GUI click-to-rescan with file count + Activity Log tracking, cached and incrementally updated on upload), and the optional default-hidden `Saved`/`Saved%` compression columns in `profiles` and the My Servers table. Closes the `T-MANUAL-QUOTA` carry-over. Ehud batch 2.
- **AeroRsync native streaming, default ON, local-to-local** (v3.8.0)
  256 MiB cap removed on both ends, batch SSH session reuse, host-key pinning symmetric across transports, native delta engine enabled by default on fresh installs, and a local-to-local `LocalDeltaTransport` with a dedicated AeroSync panel and CLI auto-detection.
- **AeroFile Dual Panel: Slice B + Slice C bridge** (v3.8.0)
  Unified panel controller with an endpoint selector and a transfer planner routing local/local, local/remote and remote/local through the correct engine; FreeFileSync-style compare panel (6-bucket classifier), sync presets, conflict policy with versioned backup, inline cross-profile transfer, terminal cwd follows the focused panel.
- **AeroFile Dual Panel - Slice A** (v3.7.9)
  Two local panels side by side in AeroFile mode, with full keyboard parity on the second panel (F2 / Delete / Enter / Backspace / clipboard / Quick Look / properties / arrows / Shift+arrow / Home / End all dispatch to the focused pane, Tab cycles between local and local2). Total-Commander shortcuts: F5 copy to other panel, F6 move to other panel, F7 new folder in the focused panel. Drag-and-drop between panes uses `rename_local_file` / `copy_local_file`; Ctrl+drag switches from move to copy. The separator is resizable from mouse and from keyboard (Arrow Left/Right ±10%, Home/End to extremes, Enter/Space to reset, `aria-valuenow` + `tabIndex=0`). Unified tab bar in the top strip with L/R markers and per-panel persistence. Slice B (each pane configurable as a local path or a saved remote profile) and Slice C (FreeFileSync-style mirror/backup/bisync workflows on top) follow in their own release windows.
- **AeroVault v4 ECC (T-AEROVAULT-ECC)** (shipped on feat/aerovault-v4-ecc, v4 track): Reed-Solomon 10+2 error-correction wrapper as 4th first-class layer (compression→chunk→crypt→ECC last, per Ehud Kirsh #272/#276). v2 fixed-grid payload (~20% real overhead proven live on incompressible data), per-shard BLAKE3 cksums for localized damage (incl. parity), all-or-nothing repair gate (re-verify cipher_hash or leave vault untouched). scrub/repair (GUI draggable modals + full CLI via --profile safety), P3-03 receipt telemetry (shards/bytes/overhead/repairs), i18n, help polish. "v3 + ECC = v4" forward-compat (non-critical ext). 22 tests + live CLI/GUI. Phase 4 docs + CHANGELOG close. See docs/dev/roadmap/APPENDIX-AEROVAULT-V4-ECC/.
- **AeroVault v3 (Experimental tier)** (v3.7.9)
  Draft container format that ships alongside v2. Pipeline: gear-CDC chunking → per-chunk zstd (fast `-3` / balanced `-9` / archive `-19`) → AES-256-GCM-SIV (RFC 8452, 96-bit random nonce + per-chunk AAD) → encrypted manifest. Chunks are content-addressed by BLAKE3-keyed-128 (chunk id, also the dedup key) and integrity-checked by BLAKE3-256 (cipher hash, pre-decryption check for the future ECC layer). Argon2id (128 MiB, t=4, p=4) derives two distinct KEKs via HKDF; both unwrap independent random 256-bit working keys through AES-KW. HMAC-SHA512 header tag verified before any unwrap. The 1024-byte header reserves an extension directory and an extension payload region so a future v4 reader can append Reed-Solomon / Parchive blocks without changing the header or manifest layout (`v3 + ECC = v4` forward-compat). v2 vaults remain the default; v3 is opt-in via the Experimental tier in the create dialog. No v2 → v3 migration in this release. Specification: [docs/AEROVAULT-V3-SPEC.md](docs/AEROVAULT-V3-SPEC.md). Tracked in [issue #162](https://github.com/axpdev-lab/aeroftp/issues/162) section 4 / T-AEROVAULT-ECC.
- **TOTP secret passthrough for Filen and MEGA** (v3.7.9)
  Persisted base32 2FA secret per profile; the backend derives the 6-digit code on every reconnect via `totp_helper::generate_totp_code`. Closes the TOTP passthrough point in [issue #128](https://github.com/axpdev-lab/aeroftp/issues/128).
- **AeroCrypt overlay first-class** (v3.7.2)
  rclone-crypt overlay promoted to a first-class encryption layer next to AeroVault. Folder transfers traverse encrypted directory trees end to end (BFS depth 64, per-level dirIV resolution), filename obfuscation via bucket-based ASCII + Latin-1, AEROCRYPT badge in the path bar. AeroCrypt toolbar button next to AeroVault.
- **ImageKit + Uploadcare native integrations** (v3.7.2)
  Two new image-CDN providers (23rd and 24th protocols). ImageKit on `api.imagekit.io` with private key auth and 20 GB free tier. Uploadcare on `api.uploadcare.com` with public + secret key auth, EU-based GDPR-friendly storage.
- **Codex CLI security audit (CLI-AUDIT-01..17)** (v3.7.2)
  External GPT 5.5 high audit closes 17 paired security and correctness fixes across the CLI / MCP / AI core dispatcher: GUI tool execution now enforces backend approval, MCP path validation, `server_exec` strictly read-only, MCP profile lookup requires exact match, atomic temp-file safety, SFTP packet bounds, daemon token mode 0600, sync direction validation, exit-code 130 cancellation. Direct `rsa` dependency dropped, `jsonwebtoken` switched to `aws-lc-rs`, `audit.toml` documents transitive RUSTSEC ignores with written threat-model justifications.
- **T-TOPBAR-3-CLUSTER + T-EDITOR-DRAG-RUN + T-AUTO-RECONNECT-IDLE** (v3.7.2)
  Custom titlebar restructured around three explicit clusters (page-nav / utility / window controls, fixes #129 click-shift drift). AeroFile to AeroTools Editor to Terminal drag-to-run flow (`.ps1` / `.sh` / `.py` with shell quoting and no auto-Enter). SFTP silent reconnect on idle session disconnect (#161, ConnectionLost classification, cwd restore, toast lifecycle).
- **Persistent Mount Manager (GUI + CLI)** (v3.7.1)
  File > Mount Manager dialog with cross-platform autostart (systemd-user units on Linux, Task Scheduler ONLOGON on Windows). Mount configs persist as plaintext sidecar JSON or encrypted vault entries, credentials always resolve through `aeroftp-cli --profile`. One-click "Open mount in file manager" auto-creates a default mount when none exists.
- **Filen Desktop local bridges**
  New presets for the local WebDAV (port 1900) and local S3 (port 1700) servers exposed by Filen Desktop, on top of a layered WebDAV scheme detection that unblocks every local HTTP-on-non-80 bridge.
- **AeroFile community polish**
  Multi-file Properties dialog (Windows-style aggregate view with mixed-state indicators), recursive `*` flatten search, smart Open with default app routing, PathBar empty-area edit and trailing chevron, configurable provider-icon size, drag-reorder custom icons, Server Health overlay dot on Discover cards.
- **AeroSync wrapper script export**
  Round-trip AeroSync configs as POSIX `.sh` or PowerShell `.ps1` scripts with an embedded `# AEROFTP-META` JSON line, defaulting to bash on Linux/macOS and pwsh on Windows.
- **My Servers unified table**
  Five-phase rework: storage Used / Total / % columns with configurable warning thresholds, semantic `<table>` with sticky thead/tfoot and click-to-sort, dedup-aware footer with per-protocol breakdown, CLI parity, drag-to-reorder + resize on three surfaces (My Servers, AeroFile remote, AeroFile local).
- **AeroRsync session-cached batch transport**
  One SSH session amortizes many consecutive delta transfers (`AerorsyncBatch` trait, per-file `delta_files[]`, `bytes_on_wire` counter).
- **AeroVault overlay session model**
  Open an `.aerovault` once, then route every list, upload, download, and rename through the encrypted overlay transparently.
- **rclone crypt full read/write**
  Beyond the existing read-only browse, AeroFTP now re-encrypts on the upload path with a transparent crypto overlay session.
- **Server Health Check engine**
  Real-time DNS, TCP, TLS, and HTTP probes per saved server in IntroHub Pro. Latency measurements, 0-100 score, capability matrix, SVG radial gauge.
- **MCP wave-5 cross-profile transfer**
  `aeroftp_transfer` and `aeroftp_transfer_tree` copy between two saved profiles in one batch.
- **MCP wave-6 ops tools**
  Six new tools (`aeroftp_touch`, `aeroftp_cleanup`, `aeroftp_speed`, `aeroftp_sync_doctor`, `aeroftp_dedupe`, `aeroftp_reconcile`) plus per-group caps on `aeroftp_check_tree`. MCP tool count: 27 → 39.
- **Box, Google Drive, Dropbox, OneDrive, Zoho deeper integrations**
  Labels, comments, file properties, tags, trash management, and versioning across the matrix.
- **InfiniCLOUD: REST v2 (Muramasa) + WebDAV**
  Dual-connector with auto-discovery and real-time quota.
- **Immich photo provider**
  Native REST API integration for self-hosted photo management.
- **Continuous bidirectional `sync --watch`**
  Native filesystem watcher (inotify, FSEvents, ReadDirectoryChangesW), anti-loop cooldown, NDJSON output.
- **MEGA Native crypto canonical layout**
  Interop fix so AeroFTP-uploaded files open correctly in MEGA Web, MEGA Mobile, and megajs.
- **Universal File Versioning**
  A single Versions dialog browses, downloads and restores prior file versions across the providers that expose them (Google Drive, Dropbox, OneDrive, Box, kDrive, pCloud, Drime, B2, S3, Koofr, Zoho WorkDrive, WebDAV and more), routed automatically through the StorageProvider trait.
- **S3 storage class management**
  Set the storage class on upload, change it in place via server-side copy, kick off a Glacier or Deep Archive restore, and read the tier back as a coloured badge in the file browser.
- **Azure Blob tier management**
  Set Hot, Cool, Cold or Archive on upload and rehydrate archived blobs.
- **AeroCloud selective sync**
  Folder-level exclusion through a checkbox tree, `.aeroignore` glob patterns, and per-direction bandwidth limits (KB/s, 0 = unlimited).
- **Cipher-strength badges**
  My Servers, Discover and the protocol selector show `128-bit`/`256-bit` lock badges instead of the old ambiguous `E2E`/`🔒` framing, aligning OAuth, API and overlay profiles on one visual grammar.
- **Streaming Scan Pipeline** (v3.3.5)
  Folder transfers no longer wait for a full recursive directory scan: the engine interleaves scan and transfer directory by directory (like an audio-player buffer), so the first file starts downloading after only the root directory is read, not after the whole tree is enumerated.

### 🟡 In Flight

- **Bitbucket, Gitea, Forgejo native integrations**
  Git forge Tier 1 on top of the existing GitHub and GitLab providers (~90% reuse of the GitHub code path).
- **Compression wrapper profile**
  Symmetric to the Crypt overlay. A per-profile zstd compression layer with the safe ordering enforced by the engine (`Encrypt(Compress(Data))` only), implemented as a provider wrapper that compresses on upload and decompresses on download. The UI warns when a user tries to compress an already-encrypted overlay, which would defeat compression.
- **Selectable XChaCha20 vault cipher**
  Promote ChaCha20 / XChaCha20-Poly1305 to a user-selectable primary content cipher for AeroVault v3 (battery-efficient on mobile and AES-NI-less ARM), as a new header-flagged mode that defaults to AES-256-GCM-SIV so existing vaults stay byte-compatible. Requires an `aerovault` crate format flag and release.

### 🔵 Up Next

- **Share Link UX Redesign**
  The unified share dialog already ships across 21-22 provider backends with expiry, password and permission controls plus a link-management tab. The remaining work is the presentation layer: QR codes for the generated links, link analytics, and team sharing.
- **VS Code Remote Explorer extension**
  Browse, edit, and upload to remotes from inside VS Code, distinct from the existing MCP launcher extension.
- **Deploy Engine**
  One-click self-hosted server provisioning (S3, WebDAV, SFTP, FTP) on a NAS, VPS, or local Docker, with the resulting endpoint auto-saved as a connection profile.
- **Photo and Media Services expansion**
  More photo and media-CDN services on top of the four already shipped (Immich, Cloudinary, ImageKit, Uploadcare).
- **Mobile-friendly window dimensions**
  Shrink the minimum width below the current bound so AeroFTP runs comfortably on Linux phones and half-screen splits.
- **Agent Orchestration v2**
  Mutative remote operations with grant model on top of the existing 35+ tool MCP server.
- **AeroVault v2 Enhancements**
  Cross-platform migration, multi-device sync integration, key rotation.

### ⚪ On the Horizon

- **AeroIndex**
  Content-aware file intelligence: cross-server deduplication, semantic tags, transactional preview, offline browsing, workspaces. A new way to think about files scattered across 40+ cloud services.
- **Mobile companion app**
  Android with Capacitor 6 and React. FTP, SFTP, and WebDAV protocols, plus AeroVault v2 import/export.
- **Flathub publish**
  Flatpak manifest done, `flathub-fork/` ready, awaiting acceptance into the Flathub remote.
- **IPFS / Web3 Storage**
  Decentralized storage integration (NLnet grant submitted).
- **Tor Support**
  Anonymous file transfers via Tor hidden services (NLnet grant submitted).
- **Biometric Unlock**
  Fingerprint and face unlock for the encrypted vault (Touch ID, Windows Hello).
- **Per-protocol comparison page in docs**
  Qualitative API vs WebDAV trade-offs, complementing Health Check and Speed Test.
- **Keyboard accessibility: Tab forward unstuck**
  Enter and Space activation already shipped; Tab traversal still pending.

---

## Provider Pipeline

| Provider | Protocol | Status |
|----------|----------|--------|
| **InfiniCLOUD** (REST v2 + WebDAV) | Muramasa REST + WebDAV | 🟢 Just Shipped: dual-connector with auto-discovery and quota |
| **Immich** | REST API (self-hosted) | 🟢 Just Shipped |
| **Bitbucket** | REST 2.0 | 🟡 In Flight: Git forge Tier 1 |
| **Gitea / Forgejo** | REST v1 | 🟡 In Flight: Git forge Tier 1 (~90% GitHub reuse) |
| **Photo & Media services** | OAuth / REST | 🔵 Up Next: phased rollout on top of the 4 shipped (Immich, Cloudinary, ImageKit, Uploadcare) |
| **ImageKit** | REST API | 🟢 Just Shipped (v3.7.2): media CDN + storage |
| **Uploadcare** | REST + Upload API | 🟢 Just Shipped (v3.7.2): media CDN, EU/GDPR |
| **GitLab Tier 2-3** | REST API v4 | 🔵 Up Next: Tier 1 already shipped |
| **Blomp** | OpenStack Swift | ⏸ Awaiting Blomp proxy fix (auth works, storage 403) |

**Already supported via presets**: Quotaless (S3 + WebDAV), PixelUnion (self-hosted), Hetzner Storage Box (WebDAV/SFTP), Nextcloud / ownCloud (WebDAV auto-detect), **Tab.digital** (Nextcloud-as-a-Service, EU / GDPR, v3.7.4), **Felicloud** (Nextcloud-as-a-Service, OCS API), **Seafile** (`seafdav` endpoint), **CloudMe** (Digest auth auto-detected), **Jianguoyun** (China-based WebDAV), **Filen Desktop S3 / WebDAV bridges** (local ports 1700 / 1900), **MEGA S4 Object Storage** (S3-compatible, 4 EU/CA regions), **Filen S5** (S3-compatible), **MinIO** (dedicated S3-compatible), **MEGAcmd** (anonymous WebDAV), **S3Drive** (path-style S3).

---

## From the Community

A continuous stream of fixes and small features driven by GitHub Issues. From v3.7.2 onward the community input is split across two thread types:

- **Wishlist** (one per release cycle): small UX paper cuts, quick wins, provider polish, CLI flags. Closes when the corresponding release ships. The v3.8.0 wishlist closed with this release ([#180](https://github.com/axpdev-lab/aeroftp/issues/180), [#194](https://github.com/axpdev-lab/aeroftp/issues/194), [#195](https://github.com/axpdev-lab/aeroftp/issues/195), [#196](https://github.com/axpdev-lab/aeroftp/issues/196)), together with the Ehud [#162](https://github.com/axpdev-lab/aeroftp/issues/162) batch 2 (storage quota override, CLI vault parity, MEGA speed-test fix, compression telemetry columns); the next thread opens with the following cycle.
- **COMMUNITY ROADMAP** (permanent): big features that need multi-day or multi-week scope. Stays open across releases. Priority is shaped by comments (mentioning the codename), not by per-section voting prompts. Find it [here](https://github.com/axpdev-lab/aeroftp/issues).

Recent contributors include **[@EhudKirsh](https://github.com/EhudKirsh)**, whose detailed wishlists across multiple releases shaped the IntroHub polish, Activity Log filtering, OAuth Edit form parity, AeroFile auto-refresh, keyboard accessibility (Enter/Space activation, font-size shortcuts, terminal focus-aware Ctrl+- / Ctrl+= / Ctrl+0), the Choose Icon dialog, the detailed server cards with storage bar + Health Check radial, the GUI Mount Manager push that paid off in v3.7.1, and the v3.7.2 batch (per-column table alignment, sticky header, sentence-case headers, CLI profiles dynamic width, unified `--breakdown`, `--hide=fav` aliases, Esc-closes-Quick-Connect, grammatical Delete confirmation, modal X-click first time fix, T-TOPBAR-3-CLUSTER restructure, T-EDITOR-DRAG-RUN flow). **[@coolfocks](https://github.com/coolfocks)** raised the SFTP idle-reconnect issue (T-AUTO-RECONNECT-IDLE, #161) that ships in v3.7.2, and **[@legion1978](https://github.com/legion1978)** reported the Ctrl+T / Ctrl+S binding miss (#171) closed in the same release.

Carry-over community items still open after the v3.8.0 cut:

- `T-PROTOCOL-COMPARISON-DOCS`: per-protocol comparison page in the docs site (API vs WebDAV qualitative trade-offs). Requires real test runs against each backend before the matrix can be written; carries over to v3.7.3.
- ~~`T-MANUAL-QUOTA`: optional manual total-storage cap per saved server for providers that do not expose `storage_info`.~~ Shipped in v3.8.0 as a TRUE override (`options.manualTotalBytes`, `--manual-total`) plus an explicit recursive used-storage scan.

`T-EDITOR-DRAG-RUN` and `T-TOPBAR-3-CLUSTER` shipped in v3.7.2 (closed). Big-feature community items live in the COMMUNITY ROADMAP thread (`T-MULTI-USER`, `T-DUAL-PANEL-UNIFICATION`, `T-MOBILE-WINDOW`).

If you spot a bug, want a small feature, or want to nominate a provider for native integration, [open an issue](https://github.com/axpdev-lab/aeroftp/issues). Tier 1 quick wins are typically picked up within one or two releases.

---

## Detailed Release History

The lane view above is what most users want. The tables below are kept for users who want to see exactly which feature landed in which release.

### v4.0.7

| Feature | Description |
|---------|-------------|
| **AeroVault dual blind security audit (grade A)** | An independent dual blind audit (Claude Opus 4.8 and Codex GPT-5) plus full remediation and a two-round adversarial controaudit closed 1 High, 1 Medium, 3 Low and 4 Info findings with zero Critical and zero open findings, verified on both the crate and the app. The kill-cleanup pair (M1 and M9) was surfaced by Ehud Kirsh's V3 Beta test pass. (@EhudKirsh) |
| **Error correction converged onto the `aerovault` crate (0.6.2)** | The app's forked standalone `.aerocorrect` and AeroSync error-correction engine (about 3,500 lines) is removed and replaced by a logic-free re-export of the crate, so the `.aerocorrect` format has a single audited implementation shared by the desktop app, the CLI and any Rust consumer. A cross-implementation golden keeps the bytes byte-for-byte identical (M7). The capability string now reflects the real Reed-Solomon engine instead of reporting a Phase 1 stub (M5). |
| **No leftover temp or lock after an interrupted seal** | Killing a vault operation mid-seal used to leave a `.aerovault.lock` that blocked the next writer and a plaintext temp beside the target; the container now writes through an auto-deleting temp on the error path, repair scrubs its temp on the persist-error branch (M1), and a lock orphaned by a crashed run is auto-reclaimed once its recorded owner PID is provably dead (M9). |
| **AeroVault extract blocks reparse-point escape** | Extracting a vault could follow a pre-planted Windows directory junction out of the destination; each path component is now created refusing to follow a pre-existing reparse point, and the canonical parent is checked to stay inside the destination root (M2). |
| **Authenticity anchor and forged-directory rejection** | `correct repair --expect-sha256` refuses a sidecar that declares a different hash before any byte is written, on the CLI, the library and the MCP tool (M3); a forged extension directory is now rejected at open before any recovery uses it, because the header MAC coverage was widened (M4). |
| **AI local tools resolve absolute POSIX paths on Windows** | A `/`-rooted path was treated as relative on Windows and re-based under the tool working directory; a leading `/` is now treated as absolute on every operating system. |
| **IntroHub My Servers grid layout** | The My Servers grid now uses container-aware column counts (3 to 9) with a full-height sidebar divider, symmetric grid gutters, and aligned toolbar and cards. |
| **DOMPurify 3.4.11** | Clears a Dependabot advisory (GHSA-cmwh-pvxp-8882). A precautionary transitive bump; the affected configuration path is not exercised by AeroFTP. |

### v4.0.6

| Feature | Description |
|---------|-------------|
| **AeroVault crate convergence** | The AEROVAULT3 vault engine and its revision 4 Reed-Solomon error correction move out of the app into the published `aerovault` crate (0.6.0 on crates.io): one audited implementation shared byte-for-byte between the desktop app and the standalone CLI, with a cross-implementation fixture pinning the two to identical bytes. The app's vault commands become thin wrappers over the crate (~4,300 lines of duplicated cryptography removed). AEROVAULT3 design and the unified `.aerocorrect` direction were driven by Ehud Kirsh (#162, #276). |
| **Folder upload honors skip/overwrite (audit)** | Folder uploads to cloud providers accepted a skip/overwrite policy but silently ignored it; re-uploading a tree now skips unchanged files as configured, with the skipped count reported. Found by an independent CLI audit. |
| **Benchmark picker checkmark (#277)** | The full-screen benchmark profile picker marks selected rows with a checkmark `[✓]` instead of `[x]`. (@EhudKirsh, #277) |

### v4.0.5

| Feature | Description |
|---------|-------------|
| **AeroVault v4 error correction (#276)** | A Reed-Solomon self-healing layer scrubs a vault for damage and repairs it from embedded or detached parity, leaving the vault byte-for-byte untouched when damage exceeds the recoverable budget. Embedded, detached `.aerocorrect` sidecar, or both; a plain rev. 3 reader still opens a rev. 4 vault. Design anchor @EhudKirsh, #276. |
| **AeroCrypt encrypted overlay** | A first-class native encrypted overlay (AES-256-GCM-SIV content, AES-256-SIV names, Argon2id) bound to a saved server profile, alongside a labelled rclone-crypt interop lane. Opt-in, no default cipher, full GUI parity to the CLI. |
| **CLI compress / extract** | `aeroftp-cli compress` and `extract` for zip, 7z and the tar family, with optional AES-256 passwords. |
| **Inline profiles action menu (#311)** | The interactive `profiles -i` selector gains an inline action menu. (@EhudKirsh, #311) |
| **SSH upgrade and stability fixes** | russh upgraded to 0.61.2 (clears the deferred advisories, byte-intact SFTP live test); sync cancel actually aborts the in-progress transfer (#332, @rockaut); master-password removal falls back to a permission-protected on-disk key when the OS keyring is unavailable (#333, @rockaut); the Windows portable build initializes its vault and persists profiles without the credential manager (#334, @rockaut). |

### v4.0.4

| Feature | Description |
|---------|-------------|
| **Reversible restricted-filename encoding (#272, #266)** | Box, Dropbox, Jottacloud and OpenDrive transparently encode filename characters the provider rejects (control characters plus each provider's reserved set) using the rclone-compatible reversible scheme (fullwidth and control-picture mappings, a quote-collision escape, position-dependent space and dot rules), then decode them back on listing. A name like `a:b` round-trips intact instead of failing silently. The encode/decode runs once at the provider boundary, covering the GUI, every CLI `cmd_*` handler, CLI `sync`/`benchmark`, the transfer engine and the session manager at once, property-tested `decode(encode(s)) == s` over tens of thousands of cases per provider. Providers whose only restriction is control characters keep the clear localized error from v4.0.3. (@EhudKirsh, #272/#266) |
| **CLI interactive shell polish (#270)** | The `profiles -i` user-switch now accepts the compact `u3` / `3u` tokens (previously the only action still requiring the spaced `u <N|name>` form), and a `# <selector> <N>` reorder reprints the table with a visual diff: a red struck-through ghost at the old slot, the live row at the new slot joined by a left-gutter arrow, and `old -> new` markers on every row whose index shifted. (@EhudKirsh, #270) |
| **Lightweight CI checks job** | A fast workflow runs `cargo fmt --check`, `cargo audit` (RustSec advisory scan) and the vitest React suite on every PR and push, outside the heavy Tauri build/release matrix, closing three gaps in CI coverage. |
| **Discover health-check toggle as icon button** | The Discover health-check toggle is now an icon-only button: the Activity icon is lit when health checks are on and dimmed when off, replacing the sliding switch for the same on/off affordance (`role=switch` and `aria-checked` kept). The manual Check button is unchanged. |
| **My Servers filters + PixelUnion catalog** | My Servers gains a "Local bridge" filter chip; the free/paid and HQ-country signals stay on Discover where they help before sign-up and were dropped from My Servers. PixelUnion catalog corrected to 16 GB free storage with a free API, regenerating the CLI catalog, README and docs/PROVIDERS.md from the single source of truth. |
| **macOS Tahoe zero-size window (#290)** | The main window could come up at a `0x0` content size on macOS 26 Tahoe, leaving only a Dock icon. The app now self-heals any restored inner size below the minimum (or zero) by resetting it to the computed initial size and re-centering, repairing already-poisoned window-state files without the user deleting anything. The size is also re-asserted after the window is on screen and re-checked on a short delay, with timestamped `[diag #290]` lines added to pinpoint a collapse. (@alexhorner, #290) |
| **Default account skipped welcome after tray Quit (#270)** | With more than one passphrase-free account the boot picker reappeared on every relaunch instead of entering the starred default, because a persisted password-free active user reported as already unlocked and bypassed the default-account fast path. The boot policy is now a pure, unit-tested decision that honors the default account on boot (default wins over last-active user); protected accounts still show their prompt and an explicit "switch account" still forces the picker. (@EhudKirsh, #270) |
| **OAuth reconnect re-authorized every time (#270)** | Reconnecting an OAuth profile re-ran the full browser authorization instead of reusing the saved per-profile token, because a snake_case `profile_id` argument never bound through Tauri v2's camelCase mapping. Passing `profileId` so it binds restores token reuse on reconnect. (@EhudKirsh, #270) |
| **Tray heap corruption on suspend/resume** | Tray badge updates mutated the StatusNotifierItem directly from caller threads, including the background sync worker on a tokio worker thread, racing the GLib main loop and corrupting the GLib heap (the recurring `malloc(): unaligned fastbin chunk detected` abort). The RGBA icon is now generated off-thread and every tray mutation is marshalled onto the GTK main thread via `run_on_main_thread`. |
| **CLI bootstrap hardening + rclone FTPS export** | `--help`, `--version`, `agent-info`, `profiles`, `catalog` and `completions` no longer depend prematurely on config / data-root / AIMD, the pre-clap parser no longer mistakes valued global options for subcommands, and a missing or unreadable default AIMD config no longer blocks metadata commands. rclone export now emits only `explicit_tls = true` for FTPS profiles; the previous `tls = true` meant implicit FTPS and produced an unusable remote. |

### v4.0.3

| Feature | Description |
|---------|-------------|
| **Add Service catalog overhaul (#224)** | The Add Service page becomes a company-centric catalog with a list view alongside the grid, per-protocol categories that split a company's products, available storage regions shown inline, a free/paid filter, in-grid search, and provider website links. A matching CLI `catalog` subcommand mirrors the same data from a single source of truth. (@EhudKirsh, #224) |
| **MEGAcmd WebDAV bridge auto-arm (#275, #264)** | Connecting a MEGAcmd profile auto-arms the local WebDAV bridge with a warmup notice; keep-alive reuse is disabled and transport errors are detailed, fixing single-file image preview. (@EhudKirsh, #275) |
| **Connection UX + B2 Range download** | Cancel an in-progress connection with Esc, plus a slow-connect modal while a connection is still establishing, and concurrent Range download for the native Backblaze B2 provider. |
| **CLI interactive additions** | Interactive `tree` depth control with a MEGAcmd warmup notice, a raw-mode arrow-key navigator in the interactive `profiles -i` shell, and `dedupe --force` / `--max-delete` for the destructive resolution modes. |
| **Wishlist items (#270)** | Tray restore from minimize, view-as-text in the preview pane, Yandex storage quota, image-preview transparency background, multi-user welcome polish, an offline-users note, and assorted copy fixes. (@EhudKirsh, #270) |
| **Server-side copy unified** | 14 native providers migrated from the legacy `server_copy` to `server_side_copy`; the multipart trait is documented as NotSupported-by-design on the remaining 8. Snap Store listing description refreshed. |
| **CLI security audit (Codex + Opus)** | Closed the merged release-gate findings (W0/W1) and a second-pass follow-up (W0.6) across every destructive and agent-facing surface: atomic download failure no longer deletes a pre-existing target; `sync --delete` refuses an incomplete or partial scan with a default delete cap; the remote-path resolver and `serve`/`speed`/`benchmark` reject `..` traversal and null bytes (exit 5); `rm -r`, `sync --delete` and `dedupe` destructive modes fail closed in non-TTY use without an explicit confirmation flag; MCP errors are scrubbed of keys and tokens; agent profile lookup is deterministic on duplicate names. Roughly forty findings closed with new unit tests and a live read-only matrix. |
| **macOS 26.5 Tahoe no window (#290)** | A borderless main window could not become key, leaving only a Dock icon after the splash; the window now presents via an overlay title bar. (@alexhorner, #290) |
| **Dev/release data isolation (#302)** | Debug builds use a sibling data root and `-dev` keyring accounts, with a release-only non-destructive migration, so a development run can no longer read or corrupt the released app's credentials. (@raelb, #302) |
| **DAG transfer audit + provider fixes** | Two patch sets correcting the multipart threshold, an AIMD deficit race, a multipart commit leak, parallel-part dispatch, Nextcloud parallel chunks and chunked-v2 threshold (256 MiB), the Azure threshold, and WebDAV download routing. S3 request logging routed to debug so `ls` and `tree` stay clean (#196, @EhudKirsh); archives written to a temp file and renamed on success; `profile-copy-user` / `profile-move-user` registered in the dispatcher allowlist; connection mode tabs persist across an in-edit protocol switch. |

### v4.0.2

| Feature | Description |
|---------|-------------|
| **Portable passphrase-less accounts** | A keystore backup now carries a transport-wrapped key (Argon2id over the backup password) for each passphrase-less user partition, and the import re-keys it to the local device. An account that previously showed an empty "My Servers" after a cross-machine import now populates correctly. |
| **Repair multi-user data** | A Settings panel proactively detects accounts whose data key is bound to another machine and rebuilds them from this device's saved servers, always taking a timestamped snapshot first. |
| **Reversible keystore import** | The import snapshots the existing `user_partitions.db` to a timestamped `.bak` before overwriting it, and a post-import summary modal surfaces the restart prompt, cross-machine re-key counts, and the snapshot path instead of a transient toast. |
| **Lean MCP `list_servers`** | `aeroftp_list_servers` returns lean identity fields by default; pass `include_capabilities: true` to embed the full per-profile transfer-capabilities block, so a vault of 80+ servers no longer overflows the agent tool-result cap on the default call. |
| **MCP error readability + quiet JSON** | S3, WebDAV, and Azure XML error messages no longer leak `&apos;` / `&amp;` / `&lt;` into JSON error fields; the final formatter emits raw UTF-8. The path-resolution `Note:` line is now silenced in `--json` mode, matching the profile banner and `Next:` hints. |
| **Settings backup table + i18n style** | The legacy "Other Apps" and "Import Any" rows collapse into a single "Bridge" row with a FILE FORMAT column listing the supported export extensions. Roughly 2024 em-dashes replaced with ASCII hyphens across all 47 locale files (punctuation only). Dependency patch updates: chrono 0.4.45, log 0.4.32, reqwest 0.13.4. |
| **Merged wishlist slice 2/3** | OpenDrive Native API header label, MEGAcmd real-quota storage handling, Manage Users avatar edit, and switching the active user from the interactive `profiles -i` loop. |

### v4.0.1

| Feature | Description |
|---------|-------------|
| **S3 native AssumeRole (#301)** | Connect to S3 by assuming an IAM role. Set a Role ARN (plus optional External ID, session name, duration and MFA) and the access keys become base credentials that AeroFTP exchanges for temporary, role-scoped credentials via AWS STS at connect time, re-assumed automatically before they expire so long sessions and large multipart uploads never fail with an expired token. Built on a hand-rolled STS client (a single SigV4-signed AssumeRole POST, no AWS SDK dependency). Also accepts an externally supplied session token, emitted as `x-amz-security-token` on signed requests and presigned URLs. Co-authored with the reporter (kennysliding). |
| **Import before password + editor open** | The .aeroftp import now loads the file before asking for the decryption password (KeePassXC pattern, #214/#300), and any plain-text file can be opened in the editor directly from the preview pane. |
| **CLI additions** | An `--access` privacy flag for `put`/`mkdir` (#252), a `#` reorder command in the interactive profiles shell, and a 2FA prompt on interactive master unlock. |
| **Profile bridge unified + Settings consolidation (#270)** | rclone, WinSCP and FileZilla import/export now run through the single generic dispatcher and panel with no loss of features; rclone remotes are listed in stable alphabetical order and Nextcloud/ownCloud DAV roots are appended correctly on export. The redundant "Servers" tab is folded into the "Backup" tab as an App / Format / Import / Export interoperability table; the Full Backup row reveals the keystore panel inline. macOS ships per-architecture DMGs built from a universal2 binary. |
| **AeroVault dual-audit remediation** | Closed the High-severity findings from the independent crypto/container audit (extract symlink write-through escape, reserved-key filter on credential read and delete, v1 format labeling) plus the remaining tranche-2 items. AeroVault crate hardened to v3 (0.4.x). |
| **Download integrity on embedded rsync servers** | Some embedded rsync firmwares (e.g. WD MyCloud) close the SSH channel before the trailing protocol marker, which the delta-sync path could accept as a clean end and commit a truncated file. The delta download now validates the reconstructed size against the remote file list and transparently falls back to the classic SFTP download on any shortfall, so a partial transfer can never overwrite the target with corrupt data. |
| **Profile duplicate keeps stored credentials** | Duplicating a saved server profile now copies its stored password or token regardless of the save-credentials flag, so the copy connects without re-entering the secret. |
| **TOTP throttle persistence + dependency hardening** | The vault 2FA lockout counter now survives restarts, with a replay guard and a bounded vault read. `tmp` bumped to 0.2.7 for the path traversal fix (CVE-2026-44705), codecov-action bumped for the template-injection fix, plus routine bumps. |
| **Legacy command and tab removal** | Dropped the legacy dedicated rclone/WinSCP/FileZilla Tauri commands and the duplicate Settings "Servers" tab, now superseded by the unified bridge and Backup table; the orphaned `protocol.servers` label string was removed from all 47 locales. |

### v4.0.0

| Feature | Description |
|---------|-------------|
| **DAG transfer engine promoted to single production path** | The ready-frontier DAG transfer engine introduced in v3.8.4 becomes the single path for every transfer surface. The three rollout flags are gone, the hand-rolled `JoinSet` batch orchestrator (130+ lines) is deleted, and the shaped builders are the single source of truth for single-file leaves, multi-file batches, sync sessions, intra-file segmented downloads, and cross-bucket copies. |
| **Capability-aware shape per transfer** | The shaped graph builder picks the transfer-core shape per call from a provider's capability snapshot: native multipart upload fan-out (S3, B2, Google Drive, Dropbox, OneDrive, Box), server-side copy on every backend that advertises it (S3 `x-amz-copy-source`, B2 `b2_copy_file`, WebDAV RFC 4918 `COPY`, ImageKit `copyFile`, plus 14 more), and intra-file segmented downloads through the shared `shaped_ranges` builder where a provider honours HTTP Range. Backends with none of these degrade honestly to the same single-transfer-core path that shipped pre-v4.0.0. |
| **Provider trait expansion** | Five new methods extend `StorageProvider`: `begin_multipart_upload`, `upload_part`, `complete_multipart_upload`, `abort_multipart_upload`, and `server_side_copy` (alongside the `supports_server_side_copy()` gate). Default implementations return `NotSupported`; nineteen native backends already implement them, with ImageKit / Internxt / MEGA / 4shared / FileLu documented as NotSupported-by-design where the protocol offers no real multipart surface. |
| **Power-user CLI knob expansion (PR #261)** | Twenty-five new flags expose the same DAG engine to scripted workflows: generic concurrency/checkers/tpslimit/order-by knobs, an S3 surface (`--s3-upload-concurrency`, `--s3-acl`, `--s3-storage-class`, and more), `Retry-After` parsing for S3/Azure/Filen, Drive/OneDrive/Azure-specific knobs, AIMD adaptive-concurrency overrides plus per-class TOML config, and a FUSE mount surface (`--cache-mode`, `--write-back-cache`, `--fuse-threads`; `fuser` 0.16 to 0.17). |
| **Multi-User Account Partition (PR #279)** | The vault splits into per-user partitions while keeping single-user installs fully backward-compatible: an encrypted partition foundation with Argon2id key derivation and AES partition encryption, a boot-time Account Lock Screen with honest crypto-stack labels, partition-aware vault wiring end-to-end, per-user AeroSync settings, an admin role with a self-or-admin gate and a last-admin guard, a CLI `--user` flag across profile and transfer commands, a cross-user dedup probe with HMAC keying, and account avatars with emoji/color customization. Migration from a v3.8.x single-user keystore is automatic, idempotent, and opt-in for the admin role. |
| **AeroCloud catch-up (PR #262)** | One-line factory dispatch fix routes Koofr to the background sync path (previously surfaced "provider not implemented" at first scheduled sync), and four providers already wired through the factory (ImageKit, Uploadcare, Cloudinary, Backblaze B2) are now exposed as selectable presets in the AeroCloud wizard. |
| **DAG engine fixes** | Per-provider chunk size honoured verbatim (fixes Google Drive 256-KiB and OneDrive 320-KiB alignment 503s); serial chain for `max_chunk_slots = 1` providers preserving Drive's monotonic `Content-Range`; Dropbox concurrent session explicit close before finish (no more HTTP 409); single-file lock race drained via `TransferOperationGuard` (#233); batch progress accounting on a transient acquire failure (#234); AIMD honours server-provided Retry-After on Drive/Dropbox/OneDrive/Box. |
| **Image preview clipped after zoom (#239)** | Scrolling the wheel on a previewed image silently switched the viewer out of Fit-to-screen mode. Wheel and toolbar zoom now act purely as a multiplier on top of the active Fit / Actual-size mode; the pan offset resets when zoom returns to fit. (reported by @EhudKirsh) |
| **Windows installer overwrote user PATH (#240)** | Critical regression affecting every Windows installer from v3.6.4 through v3.8.5: the NSIS `ReadRegStr` + `WriteRegExpandStr` pattern silently truncated user PATH values larger than `NSIS_MAX_STRLEN`, wiping previously registered toolchain entries. Replaced with the EnVar NSIS plugin (zlib licence) which talks to the Win32 registry directly, preserves the value type, and is idempotent. (reported by @miguelsotobaez) |
| **AeroFTP did not boot on macOS Intel (#241)** | A lookbehind regex from `mdast-util-gfm-autolink-literal` shipped untranspiled and threw a `SyntaxError` on JavaScriptCore <= 16.4 (Big Sur), leaving React unmounted. Patched via `patch-package` to substitute `\b` for the lookbehind, also clamping the initial window size and rebuilding the macOS `icon.icns`. The macOS Intel DMG (`x86_64-apple-darwin`) is restored alongside the Apple Silicon DMG. (reported by @reset131) |
| **OpenDrive privacy + visibility (#252)** | `folder/setaccess.json` now passes `with_child_files` so folder privacy propagates; a new `access_to_permissions` helper maps numeric levels (0 private, 1 public, 2 hidden) to canonical tokens and populates `RemoteEntry.permissions`, surfacing visibility in the context menu and Properties; a new `Privacy...` three-option chooser makes the `hidden` level reachable. (@EhudKirsh, PRs #280/#282) |
| **My Servers lag on Windows (#221)** | Five optimisations let `React.memo` skip re-rendering server cards whose data did not change: stable drag callbacks, `useMemo`'d id maps, a search-text cache, and pre-allocated context-menu icons. (reported by @raelb) |
| **MEGAcmd reliability + CLI Windows crash** | MEGAcmd non-WebDAV recursive delete timeout and Windows uploads fixed (#263, PR #265), MEGAcmd WebDAV single-file image preview fixed via a connect-time single-file probe (#264, PR #269), MEGA storage quota surfaced via `mega-df` with daemon warm-up (#253), and the `aeroftp-cli` Windows launch stack overflow fixed via `/STACK:8388608` (PR #267). (@EhudKirsh) |

### v3.8.5

| Feature | Description |
|---------|-------------|
| **Unified AeroSync panel** | The three previously disconnected sync surfaces (standalone Sync panel, AeroFile compare dialog, Sync Presets) collapse into one dialog with Compare / Plan / Sync tabs sharing state, covering local-to-local, local-to-remote and remote-to-remote pairs. Backed by a shared in-process runner with a per-file results table, journal-resume UI, Scheduler and Journal History launchers, canary mode with keep-both rename, and a bandwidth control row; opened from the toolbar and the F4 shortcut. The three legacy View-menu entries are removed. |
| **Provider-native recursive compare scan** | The Compare scan uses each provider's native recursive listing where one exists (S3 ListObjectsV2, Azure flat, Dropbox recursive, Filen tree, GDrive, OneDrive, B2, MEGA, pCloud, koofr/kdrive, OpenDrive, OAuth batch) and falls back to a depth-bounded BFS walk elsewhere. Compare results are exportable as JSON or CSV; deepest-first delete ordering prevents "directory not empty" failures. |
| **FTP transfer reliability** | The FTP clone-pool upload path dials before reading credentials (fixing "command not connected" on parallel uploads), the resume-download and in-memory `read_file` paths dial first (fixing zero-byte downloads and empty Properties previews on cold pools), AeroSync routes FTP through the provider API so the bandwidth limiter and retry policy apply, and a panicking local DAG-sync scan now surfaces as a structured `SyncError` instead of aborting. |
| **pCloud WebDAV preset** | A `pcloud-webdav` registry preset (email + password, port 443, US `webdav.pcloud.com` / EU `ewebdav.pcloud.com`) with data-region instructions, picked up automatically by Discover and translated in all 47 languages. The dedup layer recognises a pCloud WebDAV and pCloud OAuth profile sharing an email as one account. |
| **Filen: optional CLI API key, hardened master-keys ring** | A new optional "Filen CLI API Key" field lets `connect()` skip the `/v3/login` call so reconnects no longer wait on the 30-second TOTP window (the account password stays required for the master key). The API-key flow now fails `connect()` with a clear error when the encrypted master-keys ring cannot be assembled, instead of silently degrading (closes #229). |
| **WebDAV User-Agent pinned to major version** | The shared WebDAV client sends `AeroFTP/3` instead of the full `AeroFTP/3.8.x`. WebDAV servers including pCloud fingerprint the User-Agent as a device id, so the full version string forced a fresh email re-approval on every patch release; pinning to the major version keeps the device stable. |
| **Google OAuth refresh token always reissued** | The Google authorization URL now carries `prompt=consent`, so re-authentication always re-issues a refresh token. Previously a re-authorised account could return a short-lived access token with no refresh token, making `aeroftp-cli ls` / `tree` fail with `invalid_grant`. |
| **DAG transfer engine foundation (default off)** | A new shared execution engine runs single-file, batch and AeroSync transfers through a ready-frontier executor over a Directed Acyclic Graph, with AIMD backpressure and a session-probe cache. Off by default, enabled per surface via `AEROFTP_TRANSFER_ENGINE_DAG_SINGLE_FILE` / `_BATCH` / `_SYNC`; validated byte-identical on SFTP, S3 and FTP before merge. |
| **AeroRsync Z.4.3.f6 cross-platform closure** | Two orthogonal fixes close the last AeroRsync deadlock: the Linux leg coalesces `MSG_DATA` header and payload into a single send (fixing a framing-mismatch deadlock against WD My Cloud NAS), and the Windows leg synthesises `S_IFREG|0o644` on non-unix builds (fixing rsync exit 22). A Windows 100-file batch completes in 55.4 s in a single SSH session at 0.68% bytes on the wire. |
| **Smaller fixes** | File preview works on cloud providers (`ftp_read_file_base64` supports cloud-backed profiles), AeroAgent `local_*` path resolver expands `~`, the CLI exit code on `current_exe()` failure is remapped from 5 to 2 (#232), IntroHub inline rename via double-click, mount-config shell metachars rejected at the boundary, and `russh` bumped to 0.60.3 for GHSA-g9f8-wqj9-fjw5. |

### v3.8.4

| Feature | Description |
|---------|-------------|
| **A single DAG transfer executor** | A ready-frontier executor over a Directed Acyclic Graph progressively converges every transfer path (GUI segmented download, intra-file range downloads, cross-profile transfers, AeroSync, CLI `pget` and `sync`) onto one engine. Ships with prudent AIMD backpressure that adapts to the slowest link and an AppHandle-free observability sink so the CLI, agents and tests get the same progress signal the GUI does. The GUI grows a Settings knob for the download segment count. |
| **Intra-file range downloads on the converged engine** | Concurrent byte-range downloads for FTP and SFTP run on the shared engine, with a pooled SFTP range worker and pipelined reads on a single session; `provider_download_file` and the provider download executor both wire the segments. B2 large-file uploads also converge on the shared part engine. |
| **Transfer Queue with real staging** | The unified planner queue becomes a proper staging queue: items land as staged (with their pre-enumerated tree for FTP and SFTP), an explicit dispatcher moves them to pending, and an Auto-start setting decides whether they go automatically or wait for a manual Start. The transfer toast is demoted to a minimized indicator so the queue panel is the source of truth. |
| **CLI feature-complete at 69 subcommands** | `aeroftp-cli` reaches 69 top-level subcommands. New: `rmdirs` (recursive prune of empty directory trees, companion to `rmdir`), `agent peek` (read-only view of the staged + pending queue, with transfer count surfaced in `agent-info`), a CLI dispatcher binary routing deb / rpm / AppImage / Windows installs (with R10 / R11 / R13 CI gates), an `aero` alias toggle, and `--max-transfer` enforcement on the converged shared path. The whisper-rs / hound STT stack moves behind an opt-out `local-stt` cargo feature. |
| **AeroFile Sync (unified dialog)** | The three disconnected View-menu entries (Local Sync, Compare Panels, Sync Presets) collapse into a single AeroFile Sync dialog with Compare / Plan / Sync tabs that share context; the local-to-local engine is now named AeroRsync in the UI. New toolbar button on the AeroFile panel; F4 still opens the dialog on the Compare tab. |
| **Connection state on the home screen (#222)** | The per-server health dot (compact) and small radial (detailed) now pulse softly when the saved profile has at least one open session, a separate signal from the health-probe colour. The right-click menu gains a Disconnect entry that appears only when the profile is connected and closes every session from that profile. Co-authored with @raelb, with @EhudKirsh clarifying the existing probe semantics. |
| **Tier D server-side hashes (WebDAV and FTP)** | Server-side hashing now covers WebDAV (DAV `Mc-Checksum-*` headers) and FTP (`XCRC`, `XSHA256`, `MD5` extensions) on top of the existing coverage. The Properties dialog surfaces QuickXor (OneDrive) and Dropbox content hashes alongside MD5/SHA-1/SHA-256, and CLI `hashsum` prefers server-side hashes. The Sync GUI now honors the real per-provider transfer capabilities (concurrent ranges, segment count, server-side copy) instead of a one-size-fits-all default. |
| **Fixes** | Filen and MEGA TOTP 2FA wired into the Connect path and live login preview, AeroFile folder-tree roots loaded from drives on Windows, a double-clicked `.aeroftp` export routed to the import flow, SFTP `exec` drain past EOF so a late exit-status wins over the default 255, a cloned SFTP/FTP worker self-dialing on `read_range`, OpenSSL bumped 0.10.79 to 0.10.80, and GTC parity-harness bands widened to no-regression after live runs. |

### v3.8.3

| Feature | Description |
|---------|-------------|
| **Linux app fully non-functional on v3.8.2** | v3.8.2 shipped a Tauri 2.11.1 bump that reclassified the production webview origin as remote and made every Linux build reject all backend commands (no file listing, vault read, connect, mount or update check). Windows and macOS were unaffected. Pinned Tauri back to the known-good 2.11.0 so the production webview keeps backend access; every Linux v3.8.2 install was affected and upgrading is strongly recommended. |
| **Vault and edit-screen fixes** | The edit form now reads the stored password from the vault unconditionally (matching the connect path), the server list is rebuilt from the encrypted vault when the local cache is missing instead of showing zero servers, keystore backup/import key names are corrected and a double-clicked .aeroftp-keystore routes straight to the import screen, provider mode tabs are locked read-only during edit, and AeroFile starts on a single left panel and never lists with an empty path. |
| **Drag-and-drop profile import + parallel transfers** | A client profile or configuration file can be dragged directly onto its Profile Bridge import form. SFTP gains a connection pool for concurrent multi-file transfers and intra-file concurrent byte-range reads (also applied to WebDAV and Koofr), the GUI and CLI share one concurrent executor, and CLI recursive, glob and upload transfers run on it. |

### v3.8.2

| Feature | Description |
|---------|-------------|
| **Profile bridge: 12 new sources in the GUI** | The Export/Import dialog now lists all fifteen bridge sources. Beyond rclone, WinSCP and FileZilla you can import from and export to AWS CLI, MinIO Client, s3cmd, OpenSSH, PuTTY, MobaXterm, lftp, Cyberduck, Dreamweaver, Kopia, restic and Duplicacy. Each source auto-detects its conventional config path, recovers credentials into the vault where possible, and shows a clear per-source note when only metadata or partial secrets can be carried. |
| **Export filtered by protocol support** | When exporting to a target tool, profiles whose protocol that tool cannot carry are shown disabled with an explicit reason and excluded from the written file, so an export never produces an unusable entry. |
| **Server-side file hashing** | A Properties checksum tab for remote files exposes server-side hashes for S3, Backblaze B2, Google Drive, OneDrive (quickXor), Box, Dropbox and SFTP. CLI `hashsum` and `lsjson` prefer server-side hashes and avoid downloading, plus a new stable-JSON `lsjson` listing and additional commands (`size`, `lsd`, `lsl`, `lsf`, `purge`, `rmdir`). |
| **Interop fixes** | WebDAV reserved path characters are encoded and the auto-detected collection root applied (collection requests normalized to trailing-slash); SFTP entry type recovered via `stat` when a server omits attributes; rclone export conforms remote names and exports SFTP key material and bucket-relative S3 objects correctly; AeroCrypt overlay filename encoding aligned for cross-tool compatibility; CLI vault extract treats a trailing-slash destination as a directory; the MCP server keeps OAuth providers on a fresh token. |
| **Shared profile bridge core** | A single shared module now backs every importer (UUID, S3/WebDAV provider tables, INI/plist/XML scanners, atomic 0600 writes), with the per-source protocol filter and export format shared between the GUI and the CLI so the two never diverge. |

### v3.8.1

| Feature | Description |
|---------|-------------|
| **Ice theme** | A light icy white-blue palette for a calm, frosted look, wired through the full theme system (CSS palette, theme cycle and toggle, Monaco editor, xterm.js terminal, Settings selector, Activity Log, icon-theme defaults). |
| **Red Lava theme** | A brilliant crimson red on near-black: the titlebar and status bar render as glowing-red chrome (the status bar a touch darker for depth) while panels and cards stay dark, with every titlebar icon forced to white for contrast. The total number of themes goes from six to eight, with names and descriptions translated across all 47 languages. |
| **Toolbar legibility** | The AeroVault and AeroCrypt toolbar buttons now inherit the same bright foreground as the Sync and Refresh buttons instead of a dimmed grey, so they are clearly legible in their inactive state. |
| **Manual storage-quota auto-scan** | The manual quota auto-scan now runs on connect for profiles that opted in, even when only a manual total is present or a WebDAV/SFTP backend returns a partial quota; saved-server cards reflect an updated quota immediately without a manual reload. |
| **Provider form header width** | The provider form header subtitle is given more width so longer one-line descriptions (for example Backblaze B2) no longer wrap unnecessarily. |

### v3.8.0

| Feature | Description |
|---------|-------------|
| **AeroRsync native streaming, default ON** | The 256 MiB in-memory size cap is removed on both upload and download via iterator-style streaming delta plans (`send_delta_phase_streaming`, `apply_delta_streaming`), with a kill-9-safe `StreamingAtomicWriter`; validated against 4 GB cold and 4 GB modified files over real residential SSH. The native rsync engine now ships enabled by default on fresh installs (no config file); existing installs keep their stored choice. |
| **AeroRsync batch session reuse and robustness** | `AerorsyncBatch` shares one SSH session across a multi-file batch (`delta_session_count` / `delta_bytes_on_wire` surfaced in `SyncReport` and the AeroSync UI, translated in all 47 locales). Host-key pinning is symmetric across libssh2 and russh transports, and a wire-preamble truncation that was misclassified as a hard rejection now returns a recoverable `TruncatedBuffer`, dropping the 2-3% rejection rate on high-cardinality batches to zero. |
| **Local-to-local AeroSync** | A new `LocalDeltaTransport` runs the same delta engine against pure local paths, bypassing SSH (files >= 1 MiB go through the delta engine, smaller files fall back to plain copy). Exposed via CLI auto-detection of `aeroftp sync <SRC> <DST>` when both arguments are local and via a dedicated AeroSync panel (View > Local Sync...) with pickers, exclude patterns, a delta-transport toggle, dry-run, live progress and a savings report. |
| **AeroVault v3 wrapper-stack hardening** | The `packing: small-file-batching` wrapper is now actually exercised: sub-256-KiB files are concatenated into a pure-concatenation pack indexed by the manifest so the CDC chunker sees a wide stream and dedup stays chunk-aligned. Additive `pack_offset` with `#[serde(default)]`, no format bump, existing v3 vaults extract byte-identically. The wrapper-stack design is a sustained community contribution by Ehud Kirsh ([#162](https://github.com/axpdev-lab/aeroftp/issues/162)). |
| **AeroVault behind-the-scenes technical receipt** | A shared `vault_telemetry` module records the per-operation wrapper trail (packing, chunking, chunk-id, compression, crypt, cipher hash), plaintext vs compressed bytes, compression ratio, dedup count, chunk count and timing as a `VaultReport`, surfaced in an in-modal receipt panel (exportable as text) with an Activity Log entry per operation. Wired across v1/v2/v3 with inline academic attribution to Ehud Kirsh. |
| **CLI `vault` subcommand (v1, v2, v3)** | `aeroftp-cli vault {create,add,info,extract}` exposes the lifecycle for every AeroVault format, auto-detecting the on-disk version from the file header (`--vault-version` forces it; `create` defaults to v3, `--cascade` enables the v2 paranoid mode). `add --receipt` prints the technical receipt. Round-trip verified byte-identical on all three with wrong-password rejection. |
| **AeroFile dual-panel endpoint unification** | A unified panel controller with an endpoint selector and a transfer planner routes local/local, local/remote and remote/local through the right engine, plus a FreeFileSync-style 6-bucket compare panel, sync presets, conflict policy with versioned backup, inline cross-profile transfer from the planner, and the terminal cwd following the focused panel. Translated in all 47 locales. (issue #162 Slice B + Slice C bridge) |
| **Provider modes and AeroCrypt** | OpenDrive joins the generalized `ProviderModeTabs` with a Native API plus WebDAV selector, a Filen group is added and legacy-profile edit/connect is fixed, FileLu Rsync R2 lands as a dedicated provider behind the engine-awareness gate, and AeroCrypt gains encrypted-name path operations (path-overlay decode plus mkdir and rename, [#179](https://github.com/axpdev-lab/aeroftp/issues/179)). |
| **Storage quota: manual override and explicit scan (Ehud)** | An optional per-profile manual total is a TRUE override that wins even over an API-reported total (SFTP `statfs` reports whole-disk, not the user's allotment), and a user-triggered recursive scan supplies `used` for no-quota FTP/FTPS/SFTP/S3/WebDAV backends (never automatic on connect, cached on the profile, reconciled against the live connection identity). Shared by the GUI and the CLI (`df --scan`/`--full`). |
| **CLI profile management (#180, #194, #195, #196)** | A `profiles -i` interactive-shell refactor (multi-target selectors, `0`/`-1`/`q` quit, tombstone reprint, `f`/`r`/`c`/`e` actions), scriptable `profile-add` / `profile-duplicate`, vault-first reconcile so CLI deletes propagate to the GUI (#194), favourites moved into the vault so the `Fav` column renders (#195), the real OAuth error surfaced before browser re-auth (#196), the Activity Log `Errors` filter matching `status=error` rows (#180), and `--show` as an exclusive allowlist with `*`/`all` (#180). |
| **Quota and provider fixes** | Nextcloud `quota-available-bytes` of `-3` mapped to "unlimited", the MEGA speed test regenerating its payload per iteration so server-side dedup no longer inflates throughput (Ehud), a no-quota WebDAV reconnect no longer wiping a cached scan with a meaningless zero, and MEGAcmd-mode download now removing a pre-existing target before `mega-get` so callers no longer read stale bytes ([#128](https://github.com/axpdev-lab/aeroftp/issues/128)). |
| **Three-layer pre-release security audit** | The release was gated by a self-hosted `/security-review` (no new HIGH/MEDIUM), the npm regression suite and vulnerability report, and an independent five-area multi-agent expert audit. Notable hardening: a recursive-scan baseline can no longer be clobbered by an API quota read on whole-disk-statfs backends, the used-storage scan skips symlink cycles and caps server-controlled input on every path, and the AeroVault v3 master and MAC keys are zeroized on drop. |

### v3.7.9

| Feature | Description |
|---------|-------------|
| **AeroFile Dual Panel - Slice A** | Two side-by-side local panes with full keyboard parity (F2 / Delete / Enter / Backspace / Ctrl+A/C/X/V/R/F / Space / Alt+Enter / arrows / Home / End route to the focused panel, Tab cycles the two panes), Total-Commander F5 copy / F6 move / F7 new-folder shortcuts, a unified tab bar with L/R markers, drag-to-copy/move between panes (Ctrl+drag switches move to copy), a keyboard-operable resize separator, and a persisted split ratio. Toggle via the Columns icon or `Ctrl+Shift+D`. (issue [#162](https://github.com/axpdev-lab/aeroftp/issues/162) section 2) |
| **AeroVault v3 (Experimental)** | A draft container format alongside v2 using gear-CDC chunking, per-chunk zstd at fast/balanced/archive profiles (-3 / -9 / -19), AES-256-GCM-SIV per chunk with per-chunk AAD, BLAKE3-128 chunk id + BLAKE3-256 cipher hash, Argon2id (m=128 MiB, t=4, p=4) deriving distinct encryption and MAC KEKs via HKDF + AES-KW, an HMAC-SHA512 header tag, and a reserved extension directory implementing the `v3 + ECC = v4` forward-compat contract. v2 remains the default; v3 is opt-in. Full spec in `docs/AEROVAULT-V3-SPEC.md`. |
| **TOTP secret passthrough (Filen + MEGA)** | A base32 2FA secret can be persisted once per profile; the backend derives the current 6-digit code on every connect via `totp_helper::generate_totp_code` (single-use codes still accepted as fallback), removing the manual prompt. Closes the TOTP passthrough point in [#128](https://github.com/axpdev-lab/aeroftp/issues/128). |
| **MEGA HTTP 402 response-body surface** | The MEGA native client now reads the response body before classifying HTTP failures and embeds a 200-byte preview in the tracing log and the surfaced error, making the chronic "session expired or invalid" path actionable. Diagnostic-only. |
| **DebugPanel diagnostic surface** | The DebugPanel is elevated to a real diagnostic surface with backend log streaming via `log://log`, console serializer hardening, redaction of API keys / Bearer tokens / JWT / inline passwords / emails / non-loopback IPv4 / home paths / high-entropy hex, a Tests tab driving 6 backend probes and 2 frontend benchmarks, multi-format export, and a ZIP diagnostic bundle. Three new MCP tools (`aeroftp_debug_snapshot`, `aeroftp_debug_run_test`, `aeroftp_benchmark`) expose the same surface to agents. |
| **Fixes** | The IntroHub no longer flashes the empty "Get started" state on a connect/disconnect cycle (`MyServersPanel` initialises synchronously from localStorage then reconciles against the vault), AeroFile copy/move-to-other-panel reads the correct source panel via an explicit `sourceOverride`, inline rename refreshes the right panel correctly, the local-vault footer Save button becomes a confirm-and-close instead of a no-op, and the `tauri_plugin_log` global level is back to Info with Trace scoped to the aeroftp crates to stop a startup stall. |

### v3.7.8

| Feature | Description |
|---------|-------------|
| **Full keystore backup v2 (#178)** | The `.aeroftp-keystore` format becomes a complete single-file snapshot (vault entries, 5 managed SQLite DBs, the `plugins/` and `sync_snapshots/` trees, and a 21-key localStorage whitelist), compress-then-encrypted with zstd level 19 over the same Argon2id / AES-256-GCM envelope. A typical power-user backup drops from ~20-25 MB to ~5-7 MB; v1 vault-only backups still import unchanged. A two-tier export contract (`full` vs `vault-only`) is exposed in Settings > Backup and via a CLI `--mode` flag. |
| **`aeroftp-cli keystore export\|import\|info`** | CLI parity with the GUI Backup surface: `--json` output, `--password-stdin` and `AEROFTP_KEYSTORE_PASSWORD` to keep passwords out of `ps`, per-section `--skip-*` flags, a `--config-dir` override, and exit codes (0 / 2 / 6 / 7 / 11 / 99). A CLI-produced export carries 0 localStorage keys by design (no WebView2). |
| **Two-pass review hardening** | The format went through two independent LLM reviewers (Claude Opus 4.7 and GPT-5.5) with every finding addressed: authenticated metadata embedded in the encrypted payload and verified before any disk write, binary fields emitted as base64 (eliminating a 3.6x pre-zstd memory bloat), bounded zstd decompression with a 2 GiB ceiling, a 2 GiB cap on the backup file before it touches RAM, atomic writes through O_EXCL tempfiles to close a symlink race, AES-GCM nonce and salt validated before the cipher, and SQLite snapshots via `VACUUM INTO` from a read-only connection. |
| **Restart-required import flag** | A Linux/macOS rename underneath a live `app.manage(Mutex<Connection>)` strands imported state on an unlinked inode and a Windows rename fails with a sharing violation, both of which looked like success. Import now returns a `requires_restart` flag and a dedicated event, surfaced as an amber banner suggesting a restart before reopening AeroAgent or AeroFile. Argon2id and zstd run on a blocking worker so the runtime stays responsive. |
| **Portable Windows WebView isolation (#178)** | Portable builds use a `<exe-dir>/data/webview/` per-folder data dir so two co-installed portable folders never share localStorage / IndexedDB / cookies / cache, with a first-run banner pointing at the resolved folder. Saved server profiles become vault-only (legacy localStorage imported once then removed), and the NSIS uninstaller is install-format aware. |
| **Plugin executable bit fix** | The previous restore path checked a `plugins/` prefix that had already been stripped, so every restored script came back as `0o600` and the loader silently skipped them, hard-breaking the plugin system after a full backup/restore. The caller now passes `make_scripts_executable` explicitly with a whitelist covering `.sh`, `.bash`, `.zsh`, `.py`, `.js`, `.mjs`, `.rb`, `.pl`, `.ts`. |
| **S3 copy-source encoding + Filen quota redirect (#128)** | `server_copy` was concatenating the raw source key into the `x-amz-copy-source` header, producing a SigV4 mismatch on strict bridges (Filen) when the key contained spaces, emoji or reserved characters; the header now reuses the same per-segment `encode_s3_key_path` encoder the destination URL uses, and Filen S3 quota reads follow the bridge's HTTP redirect. |
| **Smaller items** | A `version == 0` backup is now rejected up-front, malformed-envelope salt/nonce/payload lengths are validated before crypto runs, restored files are fsynced, a View-menu toggle for Detailed Server Cards (`Ctrl+Shift+V`) mirrors the Settings preference, and the version bump was propagated to all identity sources so `aeroftp-cli --version` reports 3.7.8 consistently. |

### v3.7.7

| Feature | Description |
|---------|-------------|
| **MIME-style icons for AeroFTP file types** | `.aerovault`, `.aeroftp` and `.aeroftp-keystore` render with their own document icons in the file list and the host file manager. The app version is also shown in the window title and the Help menu. |
| **AeroFile UX polish (#178)** | A debounced soft overlay spinner shows on both Remote and Local panels while a directory listing is in flight (opacity 0 for the first ~250 ms so fast listings never reveal it, honouring `prefers-reduced-motion`), Up moves from the global toolbar into each panel's path bar, and the parent `..` row now navigates on a single click instead of requiring a double-click. |
| **No-trace portable update staging (#176)** | Self-extracting auto-update artifacts (portable `.zip`, `.AppImage`) stage into a private cache directory rather than `~/Downloads/` (XDG cache on Linux, Library/Caches on macOS, LOCALAPPDATA on Windows); installer formats keep `~/Downloads/`. The Windows portable cmd helper wipes the staged ZIP, sigstore sidecar and temp extraction dir after the swap. |
| **Provider and connection fixes** | WebDAV reads tolerate a missing TLS `close_notify` once Content-Length is satisfied, trash-purge corrections for MEGA / Internxt / pCloud after live tests, Filen S3 and WebDAV wrapper triage, a corrected Filen email placeholder and Tab.digital `tabdigital.cloud` shard recognition, and 2FA detection ordered before the failure log on the saved-server connect path so a 2FA challenge is no longer mislabelled as a bad password ([#128](https://github.com/axpdev-lab/aeroftp/issues/128) follow-up). |
| **Windows install fixes (#176)** | Portable AeroFTP uses a separate `vault-passphrase-portable` keyring slot so it cannot lock an installed app's vault, file associations register under HKCU for per-user installs (HKLM writes were being dropped by registry virtualisation) with an Explorer icon-cache flush, and the auto-updater no longer flashes a `cmd.exe` console window. |
| **Benchmark trash purge** | A new `StorageProvider::delete_permanent` trait method (defaulting to a no-op for trash-less protocols) lets `aeroftp-cli speed` and `benchmark` hard-delete test artefacts that would otherwise fill provider recycle bins, with `trash_purged` / `trash_purge_error` reporting. Overrides land for Google Drive, Dropbox, MEGA (cmd + native), Box, OpenDrive, kDrive, Zoho WorkDrive, pCloud (folder case), FileLu (single-file case), Internxt, Backblaze B2 versions, and OneDrive (best-effort). |

### v3.7.6

| Feature | Description |
|---------|-------------|
| **IPC restored on Linux production builds** | v3.7.5 shipped a critical regression that broke every IPC call on Linux with "Command X not allowed by ACL" after tauri 2.11.1's new `is_local_url()` check classified the `tauri-plugin-localhost` loopback origin (`http://127.0.0.1:14321`) as remote and rejected every custom command. Pinned `tauri = "=2.11.0"` to restore the backend surface while preserving every WebKit origin-scoped value on upgrade. The broken v3.7.5 release, tag and Snap channel were rolled back; no user data was lost. |
| **Programmatic window creation** | Main window creation moves from `tauri.conf.json` to a programmatic `WebviewWindowBuilder` in `setup()`, setting the URL up-front per platform with no post-creation `navigate()`. Linux production loads `http://127.0.0.1:14321/index.html` directly; macOS and Windows keep the bundled-protocol default. |
| **GHSA-7gmj-67g7-phm9 accepted as not applicable** | The Tauri Origin Confusion CVE (CVE-2026-42184, MEDIUM 6.1) requires loading remote/untrusted content into a webview; AeroFTP loads only its own bundled assets with no remote iframe or external navigation surface, so the vector cannot be triggered. The `=2.11.0` pin is documented in `Cargo.toml` and `audit.toml`, with a full ACL-manifest migration tracked for a future release. |

### v3.7.5

| Feature | Description |
|---------|-------------|
| **Self-hosted vulnerability audit pipeline** | `npm run security:report` aggregates `cargo audit` (RustSec), `npm audit` and `osv-scanner` (Google OSV) into a single self-contained HTML report under `docs/security/`, splitting findings into "open" (require action) and "suppressed" (require written rationale in audit.toml). Latest run: 0 open / 25 suppressed. A continuous audit results table is published in README and SECURITY so readers do not need proprietary PDFs. |
| **Four GHSA advisories closed** | CVE-2026-42184 / GHSA-7gmj-67g7-phm9 (Tauri Origin Confusion, MEDIUM 6.1) via `tauri 2.11.0 -> 2.11.1`; GHSA-xp3w-r5p5-63rr (openssl HIGH 8.7) and GHSA-xv59-967r-8726 (openssl MEDIUM 5.1) via `openssl 0.10.78 -> 0.10.79`; GHSA-2p6r-x3vv-xqm2 (rpassword partial password reveal, LOW 3.8) via `rpassword 7.4.0 -> 7.5.2` (7.5.1/7.5.2 hotfixes drop a glibc-only errno regression). |
| **CLI Section 7 benchmark closures** | Tooling fixes that close the five known open bugs from the v3.7.3 community benchmark report: a per-profile timeout flag for slow storage (idrive S3, InfiniCloud jp), a sub-path benchmark variant for providers that refuse `/` operations (kDrive, SeaFile WebDAV), strict-provider delete-between-runs handling for benchmarks assuming overwrite-on-PUT (4shared), and improved retry resilience for provider-side intermittent 5xx (FileLu native delete, Yandex Disk). |
| **CLI profile export to rclone, WinSCP and FileZilla** | `aeroftp-cli export rclone\|winscp\|filezilla` exports saved profiles into the native config formats (`rclone.conf` ini, WinSCP sessions ini, FileZilla `sitemanager.xml`) so users can migrate without retyping credentials, with S3 percent-encoding, Azure access-key mapping and reconstructed WebDAV URLs. Round-trip verified against rclone 1.69 and WinSCP 6.x. |
| **Transfer fixes** | Yandex Disk chunked Content-Range upload (Y3 fix) so large payloads no longer fail on the body-decode path, FileLu native delete retry with exponential backoff on body-level 5xx, WebDAV `{username}` placeholder substitution in `initial_path`, and the S3 multipart cutoff bumped from 5 MiB to 200 MiB for rclone parity and faster small-file uploads. |

### v3.7.4

| Feature | Description |
|---------|-------------|
| **Filen v3 upload reliability** | Uploads now use the required 1 MiB AES-GCM chunking model, retry individual chunks when response-body decoding flakes, and open the dedicated 2FA prompt on Filen TOTP challenges. |
| **Media providers first-class** | ImageKit, Uploadcare, Cloudinary and related integrations are surfaced consistently in provider discovery and navigation, with Cloudinary joining the media-provider set; Filen Desktop local WebDAV and S3 bridge profiles are treated as first-class local integrations. |
| **Tab.digital provider preset** | Tab.digital is available as a Nextcloud-as-a-service preset with EU / GDPR positioning, first-run health metadata and provider-card polish; stale `basePath` / `contactVerified` defaults are removed and the IntroHub card detects its WebDAV variant correctly. |
| **Backblaze B2 usability upgrades** | Bucket-level quota, a share-link UI and a clearer hide-vs-permanent-delete model let users recover soft-deleted files instead of treating every delete as final. |
| **Provider navigation and timeout fixes** | Provider HTTP transfers share a 30-minute read timeout to reduce false failures on large downloads, OneDrive nested mkdir and Drime listing avoid surprising side effects, Nextcloud / ownCloud WebDAV drill-down and URL-bar population are fixed, CLI/MCP map saved `webdavScheme` values to `tls_mode` to match the GUI, and `aeroftp-cli benchmark` sanitizes PII and ignores SIGPIPE. |
| **Portable Windows ZIP becomes a complete build (#176)** | The portable artifact ships `portable.marker`, `README.txt` and `LICENSE.txt` alongside `AeroFTP.exe`; when the marker is present all per-app data is written to `<exe-dir>\data\` so the folder is fully portable. Standard MSI / NSIS installs keep `%APPDATA%` with no migration. |
| **Windows Auto-Update parity** | The updater now installs silently and restarts automatically across all three formats via a transient `.cmd` helper: MSI (`msiexec /qb /norestart`), NSIS (`setup.exe /S`), and Portable (extract to `%TEMP%`, rename the running exe to `*.old`, swap in the new exe, relaunch with `--post-update-cleanup`). Install-format detection is now deterministic via a marker / registry / path cascade, fixing portable users being pointed at the NSIS installer ([#176](https://github.com/axpdev-lab/aeroftp/issues/176)). |

### v3.7.3

| Feature | Description |
|---------|-------------|
| **`aeroftp-cli benchmark` CLI command** | A schema-v1 conforming, sanitization-enforced community benchmark across upload, download, list, stat and delete operations, with four levels (`quick`, `standard`, `deep`, `custom`). Output is anonymized: no hostnames, paths, credentials, usernames or bucket names are written to the report. |
| **Community Benchmark guide and template** | `docs/COMMUNITY-BENCHMARK.md` explains why the dataset exists, how to run each level, what the JSON report contains, what is never collected, and how the 2-month Phase 2 decision gate works, paired with a `.github/ISSUE_TEMPLATE/benchmark-report.yml` that accepts sanitized JSON reports with coarse region and connection-type metadata. |
| **ImageKit listing fixes** | The provider was sending `type=file-and-folder`, which is not in ImageKit's accepted set, so every list call after authentication failed; switched to `type=all`. A follow-on decode error on folder rows (ImageKit emits explicit JSON `null` for file-only fields) is fixed with a `null_to_default` deserializer applied to every non-Option field the API can return as `null`. |
| **Activity log credential masking** | `maskCredential()` was masking public provider account identifiers that happen to be URLs (ImageKit's URL Endpoint ID, Uploadcare's, self-hosted WebDAV / Immich) as an unhelpful three-char prefix; it now detects `https://...` and returns host + pathname unmasked, so the log reads `Authenticated as ik.imagekit.io/aeroftp`. |
| **Benchmark profile path + Windows titlebar** | The `benchmark` CLI now anchors its working directory under the resolved profile base instead of the remote root, so it works on read-only roots, and warm-up upload errors are fatal instead of swallowed into a "0 runs" report. On Windows, modal close (X) buttons within the upper 36 px are no longer blocked by an overly wide drag region. |
| **IntroHub redundant `+ New` button merged (#171)** | The standalone "+ New" button and the "Discover Services" tab opened the same destination, confusing onboarding (reported by @legion1978). The button is removed; the tab is relabeled `Add Service` (translated in 47 languages) with a circled-plus icon, while Ctrl+N and the empty-state quick-connect still route there. |
| **CI: Snap build hardening** | The Tauri release binary is built inside the `build-snap` job before `snapcore/action-build` (pulling a `libnghttp2-14` patched against USN-8233-1), and `continue-on-error` is dropped so a snap regression now blocks the release like deb / rpm / AppImage do (v3.7.2 had masked a real failure as success). |

### v3.7.2

| Feature | Description |
|---------|-------------|
| **AeroCrypt overlay first-class** | rclone-crypt overlay promoted to a first-class encryption layer next to AeroVault. Folder transfers traverse encrypted directory trees end to end (BFS depth 64, per-level dirIV resolution). Filename obfuscation via bucket-based ASCII + Latin-1 (`obfuscate_name` / `deobfuscate_name`). New `rclone_crypt_provider_create_remote` initialises the dirIV in an optional subpath. AeroCrypt toolbar button next to AeroVault, AEROCRYPT badge in the path bar when overlay is active, post-connect banner auto-detects `rcloneCryptEnabled`. 15 new tests on obfuscate roundtrip + end-to-end smoke. |
| **ImageKit (23rd protocol)** | Native REST API integration. Auth via private key (HTTP Basic), endpoint `api.imagekit.io`, full StorageProvider trait surface plus media-CDN transformation passthrough. 20 GB media + 20 GB bandwidth/month free tier. |
| **Uploadcare (24th protocol)** | Native REST + Upload API integration. Auth via public + secret key, endpoints `api.uploadcare.com` and `upload.uploadcare.com`. Cursor-based listing, store-once semantics mapped to AeroFTP's directory model. EU-based, GDPR-friendly. |
| **Codex CLI security audit (CLI-AUDIT-01..17)** | External GPT 5.5 high audit on 2026-05-06 with 17 paired security fixes across the CLI / MCP / AI core dispatcher. Highlights: GUI tool execution now enforces backend approval, MCP / AI core remote dispatcher path validation, `server_exec` strictly read-only, MCP profile lookup requires exact id/name or unique substring, `local_copy_files` and `local_stat_batch` validate every path including symlinks, SFTP packet parser bounds-checked end to end, `.aerotmp` writes use `create_new` and refuse symlinked temp paths, daemon auth token created with `O_NOFOLLOW` + mode 0600, `sync --direction <invalid>` fails before connecting (exit 5), `sync-doctor` resolves remote paths the same way `sync` does, `transfer` checks cancellation between plan and execution (exit 130), CLI help footer documents the extended exit-code contract. Direct `rsa = "0.9"` dependency dropped, `jsonwebtoken` switched to `aws-lc-rs`. Full report under `docs/security-evidence/AEROFTP-CLI-AUDIT-2026-05-06.md`. |
| **T-TOPBAR-3-CLUSTER** | Custom titlebar restructured around three explicit clusters (page-nav / utility / window controls), Cluster 1 reserves a fixed minimum width so the utility icons (AeroVault, Lock, Settings) no longer shift between Connect / Disconnect states. Closes #129 click-shift drift. |
| **T-EDITOR-DRAG-RUN** | Drag a `.ps1` / `.sh` / `.py` from AeroFile into AeroTools Editor to open and edit, then drag from the Editor header into the Terminal area to stage the run command. Extension mapping is automatic (`pwsh` / `bash` / `python` with shell quoting), no auto-Enter so the user can review. Visual drop-target highlight + inline feedback. |
| **T-AUTO-RECONNECT-IDLE** | SFTP silent reconnect on idle session disconnect (Tom, #161). russh `session closed` errors are now classified as `ConnectionLost` (not `NotFound`), `provider_change_dir` / `provider_go_up` / `provider_list_files` retry once after a silent reconnect that reuses the in-memory `SftpConfig`, best-effort restores the previous cwd, and replays the failed operation. Toast lifecycle "Session expired, reconnecting..." then "Reconnected to server". |
| **Ctrl+T cycles theme + Ctrl+S saves Monaco editor** | Both shortcuts had been advertised in menu labels and tooltips for a while but never actually bound (#171, reported by @legion1978). Ctrl+T cycles `light` -> `dark` -> `tokyo` -> `cyber` -> `auto` everywhere outside text inputs / Monaco / xterm. Ctrl+S saves through the Monaco `editor.addAction` path so it does not collide with the global keyboard hook. |
| **Ehud table polish (#161)** | Per-column alignment with `L` / `C` / `R` toggles in the column manager popover (default sentence-aware), sticky header during vertical scroll, sentence-case headers (Host / Name / Health / ...), redundant "Detailed server cards" toolbar toggle removed, sticky-header `<thead>` no longer drifts with the rows. |
| **CLI profiles polish (Ehud, #161)** | Output now respects the current terminal width (shrinkable columns share whatever is left after fixed columns, dynamic per-column cap from `crossterm::terminal::size()`, 8-char floor on narrow terminals). `--breakdown` is a single unified table with TOTAL folded as the last row. `--hide=fav` / `favorite` / `favourite` / `favs` all accepted. |
| **AeroFile UX polish (Ehud, #161)** | Esc closes the active Quick Connect form tab. Delete confirmation built from the actual selection (single file shows its name, single folder labelled, mixed batches show separate counts) translated in 47 languages. Selection cleared when leaving AeroFile for the connection screen. Backspace no-op on connection screen. Draggable modals (AeroVault, Settings, Master Password, Mount Manager, Health Check, Speed Test, Dependencies, Shortcuts, MCP) close on the first X click (instanceof Element fix for SVG icons under WebKit). |
| **About > Library version check** | New "Check Updates" button in the Linked Libraries section queries crates.io for each of the 12 tracked libraries (russh, russh-sftp, suppaftp, reqwest, keyring, aerovault, aes-gcm, argon2, zip, sevenz-rust, quick-xml, oauth2). Color-coded status badges (green / yellow up-arrow / red triangle for major bumps). Reuses the existing `check_crate_versions` Tauri command. |
| **Support Reviews section** | New "Leave a Review" block in the heart-icon Support modal. Two side-by-side buttons: SourceForge review link (relocated from the About > Support tab) and AeroFTP MCP listing on Visual Studio Marketplace. Both render with their official brand SVG inline. Translated in 47 locales. |
| **Bug fixes** | S3Drive preset switched to path-style addressing (kapsa.io does not resolve bucket-as-subdomain in DNS), `@` toolbar toggle now honoured on every branch (Cloud OAuth, S3, opaque-token API providers), S3 access keys (Tencent / Mega S3 / Quotaless / Cloudflare R2) no longer hidden by the opaque-token heuristic, kDrive / Jottacloud / FileLu / Drime / Yandex Disk cards no longer blank when the username field stores an API key or OAuth Client ID, Yandex Disk gets a paired backend write that persists `credentials.clientId` into `server.username` on first connect, drag-to-reorder unlocked in grid view (no longer gated by stale list-view sort) and works on list view despite a WebKitGTK `dragstart`-on-`<tr>` quirk (handler relocated to the index `<td>`), Cross-Profile selection badge gets `z-10`, view-mode toggle simplified to a single button, search input padding regression on smaller font sizes, StatusBar storage quota palette aligned with the `getStorageTone` helper. |
| **CI hardening** | Windows build whisper-rs-sys cache fix for the Visual Studio 17 to Visual Studio 18 image rollover (rust-cache `prefix-key: 'v2-whisper-vs18'` + complete `whisper-rs-sys-*` directory purge). Delta-sync password-only fixture timeout raised from 15 to 25 minutes for cold-cache deps-bump PRs. Documented `audit.toml` ignores so `cargo audit` exits 0 with written threat-model justifications. Tauri ecosystem 2.10 to 2.11 (#168), Rust deps batch (#169). |

### v3.7.1

| Feature | Description |
|---------|-------------|
| **Mount Manager** | Persistent FUSE / WebDAV mount manager reachable from File > Mount Manager, the My Servers toolbar, and the connected remote address bar. Sidecar JSON or vault-backed storage, per-mount autostart (systemd-user / Task Scheduler ONLOGON), Pick free drive letter helper on Windows, "Open mount in file manager" auto-creates a default mount when none exists. Mount configs never carry secrets. |
| **Filen Desktop local bridges** | Local WebDAV (port 1900) and local S3 (port 1700) presets connect AeroFTP to a logged-in Filen Desktop instance. Inline 5-step setup banner. WebDAV scheme detection rewritten so HTTP-on-non-80 bridges work universally (explicit scheme on host wins, then `tls_mode` extra, then auto maps localhost / RFC 1918 / `*.local` to HTTP on any port). |
| **AeroFile multi-file Properties** | Right-click on two or more files now opens an aggregate Properties dialog with kind breakdown, total bytes, common parent path, modified-date range, and Mixed indicators on permissions / read-only / hidden. |
| **AeroFile recursive search** | Typing `*` or `**` flattens the subtree under the current directory, BFS-bounded at 32 levels and 5,000 entries. Optional residual filter narrows by relative-path substring. |
| **AeroFile right-click "Open with default app"** | `.aerovault` / `.aeroftp` open in AeroFTP, scripts (`.ps1` / `.sh`) drop into AeroTools Terminal with the right shell prefix and POSIX-quoted path, anything else goes through the OS default. |
| **AeroSync wrapper script export** | Templates dialog now exports the active sync configuration as POSIX `.sh` or PowerShell `.ps1` with an embedded `# AEROFTP-META` JSON line for round-trip import. |
| **Custom Icons Manager + drag-reorder + sort** | Settings > Appearance > Icons hosts a standalone gallery (upload, sort, drag-reorder, rename, delete). IconPickerDialog Custom tab gains drag-reorder, Shipped tab gets a Popular / A-Z sort toggle. |
| **Configurable provider icon size** | Settings > Appearance > Interface exposes an 18-32 px slider, My Servers and Discover cards size from the shared preference. Default bumped to 24 px. |
| **PathBar empty-area edit + trailing chevron** | Click the empty area to enter edit mode (Enter commits, Escape cancels), trailing `>` chevron lists first-generation subdirectories. |
| **Settings keyboard navigation** | Settings is now a proper modal: Tab focus trap, Escape close, sidebar `tablist` with Arrow / Home / End, horizontal Appearance subtabs follow the same model. |
| **My Servers unified table cluster** | 5 phases: storage Used / Total / % columns + warning thresholds, semantic `<table>` with sticky thead/tfoot, click-to-sort, dedup-aware footer with per-protocol breakdown, CLI parity, drag-to-reorder + resize on three surfaces (My Servers, AeroFile remote, AeroFile local). |
| **Server Health overlay dot on Discover** | Each `ServiceCard` now renders the same overlay-dot pattern as the compact `ServerCard`, gated on `healthStatus !== 'unknown'`. |
| **CLI `aeroftp-cli profiles -i`** | Interactive prompt loop with compact `1l` / `2t` / `3d` / `q` tokens, delete gated by typed-name confirmation. |
| **Filen v3 Argon2id** | New Filen accounts using `authVersion >= 3` can now log in. v1 (SHA-512) and v2 (PBKDF2-SHA512) continue unchanged. New `v1` / `v2` / `v3` Auth version badge on saved cards. |
| **Provider polish** | S3Drive icon + 5-step setup-with-rclone banner, Filen Desktop S3 / WebDAV presets pick up the official Filen logo, MEGAcmd anonymous WebDAV, Backblaze B2 native Quick Connect form. |
| **Bug fixes** | Storage quota persistence on OAuth providers (Dropbox, Google Drive, pCloud), Koofr WebDAV quota fallback to native API, terminal black-on-tab-switch (Linux WebKitGTK + Windows WebView2), Ctrl+- / Ctrl+= / Ctrl+0 on focused terminal in real time, F2 inline rename in Large Icons view, Forward / Back mouse buttons (X1 / X2), choose-icon dialog regressions for PNG-backed logos. |

### v3.7.0

| Feature | Description |
|---------|-------------|
| **AeroRsync session-cached batch transport** | New `AerorsyncBatch` trait amortizes a single SSH session across many consecutive delta transfers. `SyncReport` exposes `delta_files[]` (per-file breakdown) and `bytes_on_wire` (cumulative wire savings) surfaced in SyncPanel. |
| **AeroVault overlay session model** | Open an `.aerovault` once and route every list/upload/download/rename through the encrypted overlay transparently. Provider sees only opaque vault chunks; UI shows plaintext entries. Header status badge marks when overlay is active. |
| **rclone crypt full read/write** | Beyond the existing read-only browse, AeroFTP now re-encrypts on the upload path with a transparent crypto overlay session. Filename obfuscation is deterministic; provider sees only encrypted blobs. |
| **Server Health Check** | Real-time DNS/TCP/TLS/HTTP probes per saved server in IntroHub Pro. Latency measurements, 0-100 health scoring, capability matrix per protocol, SVG radial gauge, parallel batch refresh. |
| **MCP wave-5 cross-profile transfer** | `aeroftp_transfer` and `aeroftp_transfer_tree` copy files between two saved profiles in one batch. Source and destination provider opened once and reused; path validation, audit log, throttled progress streaming. |
| **MCP wave-6 ops tools** | Six new tools (`aeroftp_touch`, `aeroftp_cleanup`, `aeroftp_speed`, `aeroftp_sync_doctor`, `aeroftp_dedupe`, `aeroftp_reconcile`) plus per-group caps (`max_match`, `max_differ`, `max_missing_local`, `max_missing_remote`) and `omit_match` switch on `aeroftp_check_tree`. MCP tool count: 27 → 39. |
| **`aerovault` crate 0.3.4** | New overlay-session API and KEK-derivation polish in the standalone Rust crate. New `rename_entry` / `move_entry` / `copy_entry` public API on `Vault`, mirrored by `aerovault rename / move / copy` CLI subcommands. |
| **MEGA Native crypto polish** | Non-regressive cleanup on top of the v3.6.10 canonical-layout fix (less log noise, nonce/key edge cases, listing pagination). |
| **B2 native v4 hardening** | Auth/list/upload/download retry surface aligned with provider-trait expectations. |

### v3.5.0

| Feature | Description |
|---------|-------------|
| **FileZilla import/export bridge** | Import sites from FileZilla `sitemanager.xml` and export back. Supports FTP, SFTP, FTPS (implicit and explicit), and S3. Passwords decoded from base64 and upgraded to AES-256-GCM encrypted vault. GUI and CLI (`aeroftp import filezilla`). |
| **Unified Bridge hub** | Single "Bridge" section with app selector (rclone, WinSCP, FileZilla) replaces separate import/export sections. Three bridge tools, one interface. |
| **Nextcloud WebDAV auto-detection** | Connecting to a Nextcloud/ownCloud server without specifying the WebDAV path now auto-discovers `/remote.php/dav/files/{username}/`. No manual path configuration needed. |
| **Transfer engine hardening** | Timeout scales with file size (2s/MB + 30s base). "Skip if identical" works reliably. Retry queue preserved after batch completion. |

### v3.4.9

| Feature | Description |
|---------|-------------|
| **WinSCP import/export bridge** | Import saved sessions from WinSCP configuration files. Supports SFTP, SCP, FTP, FTPS, WebDAV, and S3. Passwords decoded from WinSCP's XOR obfuscation and upgraded to AES-256-GCM vault. Export back to WinSCP.ini also available. GUI and CLI (`aeroftp import winscp`). |
| **Duplicate detection in import** | rclone and WinSCP import screens show an "Already exists" badge on matching profiles, with option to update credentials on re-import. |
| **macOS Quit fix** | Cmd+Q and menu bar Quit now exit correctly even when AeroCloud hide-to-tray is active. |
| **Import/export security hardening** | Path traversal rejection, symlink resolution, 10 MB size cap, credential redaction in JSON output, INI injection prevention. |

### v3.4.8

| Feature | Description |
|---------|-------------|
| **MCP Server** | Native Model Context Protocol server via `aeroftp-cli mcp`. 16 curated tools across all 22 protocols (later expanded to 35+ canonical tools), connection pooling, rate limiting, audit logging, 5 resources, 4 prompt templates. Works with Claude Desktop, Cursor, Windsurf, Claude Code via the [`axpdev-lab.aeroftp-mcp`](https://marketplace.visualstudio.com/items?itemName=axpdev-lab.aeroftp-mcp) extension. 2,800+ lines, async stdio, JSON-RPC 2.0 compliant. |
| **Cross-profile transfer panel** | Dedicated toolbar button for cloud-to-cloud transfers. Floating panel with real-time queue, progress bars, and plan/execute/done transitions. |
| **CLI `transfer` command** | Cross-profile copy between two vault-backed profiles with dry-run, recursive mode, and `--skip-existing` for backup flows. |
| **CLI doctor workflows** | `sync-doctor` and `transfer-doctor` preflight commands with structured checks, risk summaries, and `suggested_next_command` for agent automation. |
| **Rate limit resilience** | Automatic retry with exponential backoff on 429/5xx for Zoho WorkDrive, GitLab, and Swift/Blomp. |

### v3.4.7

| Feature | Description |
|---------|-------------|
| **rclone config import** | Import server profiles from rclone.conf files. Supports 17 rclone backend types (FTP, SFTP, S3, WebDAV, Google Drive, Dropbox, OneDrive, MEGA, Box, pCloud, Azure Blob, Swift, Yandex Disk, Koofr, Jottacloud, Backblaze B2, OpenDrive). Passwords de-obfuscated from rclone's reversible AES-256-CTR and stored in AES-256-GCM encrypted vault. |
| **rclone config export** | Export server profiles to rclone.conf format for full interoperability with rclone CLI. Passwords obfuscated using rclone's standard scheme. |
| **CLI `import rclone`** | New subcommand `aeroftp import rclone [path] [--json]` for headless config migration. |
| **MEGA default fix** | New MEGA profiles default to Native API instead of MEGAcmd. Existing profiles without explicit mode correctly labeled. |

### v3.3.0

| Feature | Description |
|---------|-------------|
| **IntroHub redesign** | New tabbed interface replaces the 50/50 split layout. My Servers grid with favorites, Discover Services catalog, Command Palette (Ctrl+K), and dynamic form tabs. |
| **SourceForge integration** | Native SFTP provider for SourceForge File Release System. Pre-configured connection with Project (Unixname) field and SSH key authentication. |
| **Custom Checkbox component** | All native HTML checkboxes replaced with animated SVG checkmark component. Focus-visible ring, aria-label support, keyboard navigation. |
| **SFTP upload fix (#73)** | Removed SSH2/SCP fallback that caused "host key changed" errors during upload. Uploads now use native russh_sftp through the same SSH session. |
| **Auto-update Trust UI** | Sigstore verification badges (green/amber/red). Linux restart reliability fix. Snap users redirected to store. Post-restart confirmation with actual verification status. |
| **Cloud provider descriptions** | All cloud services in Discover show storage info and signup links. Info banners for all 5 categories translated in 47 languages. |
| **Collapsible SSH Auth** | SFTP SSH authentication fields collapse by default, saving form space. |
| **Badge accuracy** | Fixed kDrive, Yandex Disk, Koofr badges. Added OCS badge for Felicloud/Nextcloud, Swift for Blomp. |

### v3.2.x

| Version | Feature |
|---------|---------|
| v3.2.6 | macOS crash fix (static liblzma), security hardening (russh 0.59 HIGH, Aikido Top 5%), Felicloud direct access, status bar consistency |
| v3.2.5 | Linux localhost fix (`tauri-plugin-localhost` IPv6 → `127.0.0.1`) |
| v3.2.2 | Advanced share links (password / expiry / permissions across 21 providers), MEGA S4 Object Storage, CLI link enhancements |
| v3.2.0 | **MEGA Native API** (full native protocol, AES-128-CTR, RSA session, encrypted node tree), MEGA dual-backend, Windows MEGA fixes, trash date formatting |

### v3.1.x

| Version | Feature |
|---------|---------|
| v3.1.8 | Desktop security hardening (Sigstore, native OS approval dialogs, OS keyring), **Agent Orchestration** (CLI `agent` mode, `server_list_saved`, `server_exec`), FileLu v2 listing |
| v3.1.7 | Glob find patterns (8 providers), LargeIconsGrid virtualization, DOMPurify CVE fix, Nextcloud trash scope |
| v3.1.6 | **Felicloud** integration (Nextcloud-based EU cloud, OCS API, share links, trash), share link modal redesign, FileLu listing perf, activity log coverage |
| v3.1.5 | AeroAgent hardening (prompt injection sanitization, memory management), CLI evolution (38 subcommands, batch engine), security audit (signed log, command denylist) |
| v3.1.4 | FileLu v2 path-based API, AeroCloud production closure |
| v3.1.2 | **Zoho WorkDrive** OAuth2, swap panels |
| v3.1.0 | Co-Author address book, Windows static CRT |

### v3.0.x

| Version | Feature |
|---------|---------|
| v3.0.9 | GitHub batch operations (bulk upload, delete, commit) |
| v3.0.7 | GitHub Actions browser (CI/CD monitor and trigger) |
| v3.0.5 | GitHub App authentication (PEM vault storage, installation tokens, branch protection) |
| v3.0.0 | **AeroFTP 3.0**: Tauri 2 migration, new UI, plugin system |

---

### Provider Timeline

Every native cloud provider integration is a milestone. Here's the full history:

| # | Provider | Version | Protocol |
|---|----------|---------|----------|
| 28 | **Cloudinary** | v3.7.4 | REST API (image / video CDN + media services) |
| 27 | **Uploadcare** | v3.7.2 | REST + Upload API (EU / GDPR media storage) |
| 26 | **ImageKit** | v3.7.2 | REST API (media CDN + storage) |
| 25 | **InfiniCLOUD** | v3.7.0 | REST v2 (Muramasa) + WebDAV |
| 24 | **Immich** | v3.4.4 | REST API (self-hosted) |
| 23 | **Google Photos** | dev only | OAuth2 (read-only); kept in development due to a Photos API problem, hidden in release builds |
| 22 | **GitLab** | v3.3.2 | REST API v4 |
| 21 | **SourceForge** | v3.3.0 | SFTP |
| 20 | **Felicloud** | v3.1.6 | WebDAV + OCS API |
| 19 | **FileLu** | v2.7.0 | REST API |
| 18 | **Zoho WorkDrive** | v3.1.2 | OAuth2 |
| 17 | **Yandex Disk** | v2.9.0 | OAuth2 |
| 16 | **OpenDrive** | v2.8.0 | REST API |
| 15 | **Koofr** | v2.8.0 | REST API |
| 14 | **Jottacloud** | v2.8.0 | REST API |
| 13 | **kDrive** | v2.8.0 | REST API |
| 12 | **Drime Cloud** | v2.8.0 | REST API |
| 11 | **Internxt** | v2.6.0 | E2E Encrypted |
| 10 | **Filen** | v2.6.0 | E2E Encrypted |
| 9 | **4shared** | v2.6.0 | OAuth 1.0 |
| 8 | **GitHub** | v2.6.0 | REST API |
| 7 | **pCloud** | v2.3.0 | OAuth2 |
| 6 | **Box** | v2.3.0 | OAuth2 |
| 5 | **MEGA** | v2.2.0 | E2E Encrypted |
| 4 | **Dropbox** | v2.1.0 | OAuth2 |
| 3 | **OneDrive** | v2.1.0 | OAuth2 |
| 2 | **Google Drive** | v2.0.0 | OAuth2 |
| 1 | **Azure Blob + S3** | v1.5.0 | HMAC |

Plus the core protocols: **FTP**, **FTPS**, **SFTP**, **WebDAV**, **AeroCloud**.

**Bridge interoperability** (v3.4.7-v3.5.0): Import/export profiles with **rclone** (17 backends), **WinSCP** (6 protocols), and **FileZilla** (4 protocols). Credentials decoded from each tool's obfuscation format and upgraded to AES-256-GCM vault.

---

## Supported Languages

AeroFTP is available in **47 languages**:

Bulgarian, Bengali, Catalan, Czech, Welsh, Danish, German, Greek, English, Spanish, Estonian, Basque, Finnish, French, Galician, Hindi, Croatian, Hungarian, Armenian, Indonesian, Icelandic, Italian, Japanese, Georgian, Khmer, Korean, Lithuanian, Latvian, Macedonian, Malay, Dutch, Norwegian, Polish, Portuguese, Romanian, Russian, Slovak, Slovenian, Serbian, Swedish, Swahili, Thai, Filipino, Turkish, Ukrainian, Vietnamese, Chinese.

---

## How to Contribute

- **Star the repo** to show your support
- **Report bugs** via [GitHub Issues](https://github.com/axpdev-lab/aeroftp/issues)
- **Suggest features** by opening a discussion or commenting on an existing wishlist thread
- **Help translate**: we're always looking for native speakers to improve translations
- **Run a storage service?** See the [Provider Integration Guide](docs/PROVIDER-INTEGRATION-GUIDE.md) for a native integration in AeroFTP. We collaborate directly with providers on the API mapping.
