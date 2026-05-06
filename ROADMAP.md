# AeroFTP Roadmap

> A transparent view of where AeroFTP has been, where it is today, and where it's headed.
> This roadmap is updated continuously. Feature requests and feedback are welcome via [GitHub Issues](https://github.com/axpdev-lab/aeroftp/issues).

> **This roadmap is indicative.** The order in which items are picked up may change based on technical evaluations made during development, dependencies between features, community feedback, and security findings. Items can move between lanes (or be deferred) without notice.

---

## At a Glance

A continuous flow rather than a calendar. Items move from right to left as they ship.

| 🟢 **Just Shipped** | 🟡 **In Flight** | 🔵 **Up Next** | ⚪ **On the Horizon** |
|---|---|---|---|
| Available in the latest release | Actively being worked on, ready to release soon | Confirmed for an upcoming release, design done | Planned but not yet started |

### 🟢 Just Shipped

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

### 🟡 In Flight

- **AeroFile Dual Panel**
  One surface for any pair of endpoints (local/local, local/remote, remote/local, remote/remote) with a FreeFileSync-style mirror, backup, and bisync workflow.
- **Local Transport for AeroRsync**
  Delta sync local-to-local, the same wire-protocol-compatible engine extended to local filesystem pairs.
- **Activity Log per-provider coverage**
  Beyond generic CRUD, surface provider-specific events such as share link rotated, version restored, label applied.
- **Bitbucket, Gitea, Forgejo native integrations**
  Git forge Tier 1 on top of the existing GitHub and GitLab providers (~90% reuse of the GitHub code path).

### 🔵 Up Next

- **Crypt as a dedicated profile type**
  Surface `crypt` in the `aeroftp-cli profiles` listing under the "Proto" column instead of hiding the encryption configuration inside `.sh` and `.ps1` automation scripts. Same on the GUI: Crypt becomes its own card on My Servers and Discover with a `256-bit 🔐` badge.
- **Compression wrapper profile**
  Symmetric to the Crypt overlay. A per-profile zstd compression layer with the safe ordering enforced by the engine (`Encrypt(Compress(Data))` only). The UI warns when a user tries to compress an already-encrypted overlay, which would defeat compression.
- **Streaming Scan Pipeline**
  Producer-consumer architecture for immediate transfer start without waiting for a full directory scan.
- **Share Link UX Redesign**
  Unified share experience with QR codes, link analytics, and team sharing on top of the 22 provider backends already shipped.
- **VS Code Remote Explorer extension**
  Browse, edit, and upload to remotes from inside VS Code, distinct from the existing MCP launcher extension.
- **Deploy Engine**
  One-click self-hosted server provisioning (S3, WebDAV, SFTP, FTP) on a NAS, VPS, or local Docker, with the resulting endpoint auto-saved as a connection profile.
- **Photo and Media Services expansion**
  Seven services beyond Immich and Google Photos.
- **Mobile-friendly window dimensions**
  Shrink the minimum width below the current bound so AeroFTP runs comfortably on Linux phones and half-screen splits.
- **Multiple AeroFTP users**
  Per-user profile partitioning with a dropdown selector in the titlebar, F2 rename, separate import/export per user, separate AeroSync settings. Vault schema migration is forward-only (the existing single-user install becomes the "default" user). New `--user <name>` flag on `aeroftp-cli`.
- **Universal File Versioning**
  Unified versions panel across 10 providers (Google Drive, Dropbox, OneDrive, Box, S3, Azure, Nextcloud, kDrive, Filen, pCloud).
- **AeroCloud Selective Sync**
  Folder-level exclusion with tree view, `.aeroignore` patterns, bandwidth throttling, conflict visualization.
- **Agent Orchestration v2**
  Mutative remote operations with grant model on top of the existing 39-tool MCP server.
- **AeroVault v2 Enhancements**
  Cross-platform migration, multi-device sync integration, key rotation.
- **S3 Storage Class Management**
  Set storage class on upload, change in-place, Glacier restore workflow, tier badges.
- **Azure Blob Tier Management**
  Hot, Cool, Cold, and Archive tiers with rehydration workflow.

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
- **AeroVault v3 with BLAKE3**
  Replace the SHA-2 fast-hashing parts of the AeroVault v2 stack with BLAKE3 while keeping Argon2id for KDF. Now is the right moment to make this kind of cryptographic decision before the install base grows.
- **ChaCha20 / XChaCha20 cipher family**
  Battery-efficient symmetric encryption option for mobile, exposed on desktop too for parity. Reference points: Kopia (ChaCha, zstd, BLAKE3) and Restic (zstd). Benchmark phase before pinning the default.
- **Biometric Unlock**
  Fingerprint and face unlock for the encrypted vault (Touch ID, Windows Hello).
- **Encryption-strength badges refresh**
  Replace the current `E2E` and `🔒` badges across My Servers, Discover Services, and ProtocolSelector with cipher-strength labels (`128-bit 🔐` / `256-bit 🔐`). Removes the misleading "End-to-End Encryption" framing on overlays where there is no destination decryption, and aligns OAuth providers with the same visual grammar as API providers.
- **Per-protocol comparison page in docs**
  Qualitative API vs WebDAV trade-offs, complementing Health Check and Speed Test.
- **Topbar nav restructure**
  Dedicated 3-cluster layout (page-nav, utility, window controls).
- **Custom favicon picker: manual reorder and sort toggle**
  User-uploaded library with explicit ordering and a sort toggle.
- **Icon size enlarge or Appearance slider**
  Bigger provider icons, or a user-adjustable size in Appearance.
- **Keyboard accessibility: Tab forward unstuck**
  Enter and Space activation already shipped; Tab traversal still pending.
- **AeroSync ↔ aeroftp-cli script export/import**
  `.ps1` and `.sh` with auto-detected shebang.
- **Top-right overlays: keep titlebar drag-region active**
  While modals are open, the titlebar should remain draggable so the window can be moved or split-screened.
- **Right-click "Open with default app"**
  `.aerovault`, `.aeroftp`, and `.aeroftp-keystore` open inside AeroFTP. `.ps1` and `.sh` open in AeroTools terminal. Everything else uses the OS default.

---

## Provider Pipeline

| Provider | Protocol | Status |
|----------|----------|--------|
| **InfiniCLOUD** (REST v2 + WebDAV) | Muramasa REST + WebDAV | 🟢 Just Shipped: dual-connector with auto-discovery and quota |
| **Immich** | REST API (self-hosted) | 🟢 Just Shipped |
| **Bitbucket** | REST 2.0 | 🟡 In Flight: Git forge Tier 1 |
| **Gitea / Forgejo** | REST v1 | 🟡 In Flight: Git forge Tier 1 (~90% GitHub reuse) |
| **Photo & Media services** | OAuth / REST | 🔵 Up Next: phased rollout, 7 services in queue |
| **GitLab Tier 2-3** | REST API v4 | 🔵 Up Next: Tier 1 already shipped |
| **ImageKit** | REST API | 🔵 Up Next: media CDN + storage |
| **Blomp** | OpenStack Swift | ⏸ Awaiting Blomp proxy fix (auth works, storage 403) |

**Already supported via presets**: Quotaless (S3 + WebDAV), PixelUnion (self-hosted), Hetzner Storage Box (WebDAV/SFTP), Nextcloud / ownCloud (WebDAV auto-detect).

---

## From the Community

A continuous stream of fixes and small features driven by GitHub Issues. From v3.7.2 onward the community input is split across two thread types:

- **Wishlist** (one per release cycle): small UX paper cuts, quick wins, provider polish, CLI flags. Closes when the corresponding release ships. The v3.7.2 wishlist closed with this release (#161); the next thread will open with the v3.7.3 cycle.
- **COMMUNITY ROADMAP** (permanent): big features that need multi-day or multi-week scope. Stays open across releases. Priority is shaped by comments (mentioning the codename), not by per-section voting prompts. Find it [here](https://github.com/axpdev-lab/aeroftp/issues).

Recent contributors include **[@EhudKirsh](https://github.com/EhudKirsh)**, whose detailed wishlists across multiple releases shaped the IntroHub polish, Activity Log filtering, OAuth Edit form parity, AeroFile auto-refresh, keyboard accessibility (Enter/Space activation, font-size shortcuts, terminal focus-aware Ctrl+- / Ctrl+= / Ctrl+0), the Choose Icon dialog, the detailed server cards with storage bar + Health Check radial, the GUI Mount Manager push that paid off in v3.7.1, and the v3.7.2 batch (per-column table alignment, sticky header, sentence-case headers, CLI profiles dynamic width, unified `--breakdown`, `--hide=fav` aliases, Esc-closes-Quick-Connect, grammatical Delete confirmation, modal X-click first time fix, T-TOPBAR-3-CLUSTER restructure, T-EDITOR-DRAG-RUN flow). **[@coolfocks](https://github.com/coolfocks)** raised the SFTP idle-reconnect issue (T-AUTO-RECONNECT-IDLE, #161) that ships in v3.7.2, and **[@legion1978](https://github.com/legion1978)** reported the Ctrl+T / Ctrl+S binding miss (#171) closed in the same release.

Carry-over community items still open after the v3.7.2 cut:

- `T-PROTOCOL-COMPARISON-DOCS`: per-protocol comparison page in the docs site (API vs WebDAV qualitative trade-offs). Requires real test runs against each backend before the matrix can be written; carries over to v3.7.3.
- `T-MANUAL-QUOTA`: optional `manualQuota` field per saved server for providers that do not expose `storage_info`. Filed for the v3.7.3 wishlist.

`T-EDITOR-DRAG-RUN` and `T-TOPBAR-3-CLUSTER` shipped in v3.7.2 (closed). Big-feature community items live in the COMMUNITY ROADMAP thread (`T-MULTI-USER`, `T-DUAL-PANEL-UNIFICATION`, `T-MOBILE-WINDOW`).

If you spot a bug, want a small feature, or want to nominate a provider for native integration, [open an issue](https://github.com/axpdev-lab/aeroftp/issues). Tier 1 quick wins are typically picked up within one or two releases.

---

## Detailed Release History

The lane view above is what most users want. The tables below are kept for users who want to see exactly which feature landed in which release.

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
| **MCP Server** | Native Model Context Protocol server via `aeroftp-cli mcp`. 16 curated tools across all 22 protocols (later expanded to 39 in v3.7.0), connection pooling, rate limiting, audit logging, 5 resources, 4 prompt templates. Works with Claude Desktop, Cursor, Windsurf, Claude Code via the [`axpdev-lab.aeroftp-mcp`](https://marketplace.visualstudio.com/items?itemName=axpdev-lab.aeroftp-mcp) extension. 2,800+ lines, async stdio, JSON-RPC 2.0 compliant. |
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
| 25 | **InfiniCLOUD** | v3.7.0 | REST v2 (Muramasa) + WebDAV |
| 24 | **Immich** | v3.4.4 | REST API (self-hosted) |
| 23 | **Google Photos** | v3.4.3 | OAuth2 (read-only, Google restricted scope 2025) |
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
