# AeroFTP Roadmap

> A transparent view of where AeroFTP has been, where it is today, and where it's headed.
> This roadmap is updated continuously. Feature requests and feedback are welcome via [GitHub Issues](https://github.com/axpdev-lab/aeroftp/issues).

---

## At a Glance

A continuous flow rather than a calendar. Items move from right to left as they ship.

| 🟢 **Just Shipped** | 🟡 **In Flight** | 🔵 **Up Next** | ⚪ **On the Horizon** |
|---|---|---|---|
| Available in the latest release | Actively being worked on, ready to release soon | Confirmed for an upcoming release, design done | Planned but not yet started |

### 🟢 Just Shipped

- **AeroRsync session-cached batch transport** — one SSH session amortizes many consecutive delta transfers (`AerorsyncBatch` trait, per-file `delta_files[]`, `bytes_on_wire` counter)
- **AeroVault overlay session model** — open an `.aerovault` once, then route every list/upload/download/rename through the encrypted overlay transparently
- **rclone crypt — full read/write** — beyond the existing read-only browse, AeroFTP now re-encrypts on the upload path with a transparent crypto overlay session
- **Server Health Check engine** — real-time DNS/TCP/TLS/HTTP probes per saved server in IntroHub Pro (latency, 0-100 score, capability matrix, SVG radial gauge)
- **MCP wave-5 cross-profile transfer** — `aeroftp_transfer` / `aeroftp_transfer_tree` between two saved profiles in one batch
- **MCP wave-6 ops tools** — `aeroftp_touch` / `aeroftp_cleanup` / `aeroftp_speed` / `aeroftp_sync_doctor` / `aeroftp_dedupe` / `aeroftp_reconcile` plus per-group caps on `aeroftp_check_tree` (MCP tool count: 27 → 39)
- **Box / Google Drive / Dropbox / OneDrive / Zoho deeper integrations** — labels, comments, file properties, tags, trash management, versioning across the matrix
- **InfiniCLOUD — REST v2 (Muramasa) + WebDAV** — dual-connector with auto-discovery and real-time quota
- **Immich photo provider** — native REST API integration, self-hosted photo management
- **Continuous bidirectional `sync --watch`** — native filesystem watcher (inotify / FSEvents / ReadDirectoryChangesW), anti-loop cooldown, NDJSON output
- **MEGA Native crypto canonical layout** — interop fix so AeroFTP-uploaded files open correctly in MEGA Web / MEGA Mobile / megajs

### 🟡 In Flight

- **AeroFile Dual Panel** — one surface for any pair of endpoints (local/local, local/remote, remote/local, remote/remote) with FreeFileSync-style mirror / backup / bisync workflow
- **Local Transport for AeroRsync** — delta sync local-to-local, same wire-protocol-compatible engine extended to local filesystem pairs
- **Activity Log per-provider coverage** — beyond generic CRUD, surface provider-specific events (share link rotated, version restored, label applied)
- **Flathub publish** — flatpak manifest done, `flathub-fork/` ready, awaiting acceptance into the Flathub remote
- **Bitbucket / Gitea / Forgejo native integrations** — Git forge Tier 1 on top of the existing GitHub + GitLab providers (~90% reuse of the GitHub code path)
- **Mobile companion app** — Android with Capacitor 6 + React, FTP / SFTP / WebDAV protocols and AeroVault v2 import/export

### 🔵 Up Next

- **Persistent Mount Manager** (GUI + CLI) — pick a free drive letter on Windows or a mount path on Linux, persist across reboots; `Open Mount` button in the dual panel
- **Streaming Scan Pipeline** — producer-consumer architecture for immediate transfer start without waiting for a full directory scan
- **Share Link UX Redesign** — unified share experience with QR codes, link analytics, and team sharing on top of the 22 provider backends already shipped
- **VS Code Remote Explorer extension** — browse, edit, and upload to remotes from inside VS Code (distinct from the existing MCP launcher extension)
- **Deploy Engine** — one-click self-hosted server provisioning (S3 / WebDAV / SFTP / FTP) on a NAS, VPS, or local Docker, with the resulting endpoint auto-saved as a connection profile
- **Photo & Media Services expansion** — 7 services beyond Immich and Google Photos
- **Mobile-friendly window dimensions** — shrink the minimum width below current bound so AeroFTP runs comfortably on Linux phones / half-screen splits
- **Universal File Versioning** — unified versions panel across 10 providers (Google Drive, Dropbox, OneDrive, Box, S3, Azure, Nextcloud, kDrive, Filen, pCloud)
- **AeroCloud Selective Sync** — folder-level exclusion with tree view, `.aeroignore` patterns, bandwidth throttling, conflict visualization
- **Agent Orchestration v2** — mutative remote operations with grant model on top of the existing 39-tool MCP server
- **AeroVault v2 Enhancements** — cross-platform migration, multi-device sync integration, key rotation
- **S3 Storage Class Management** — set storage class on upload, change in-place, Glacier restore workflow, tier badges
- **Azure Blob Tier Management** — Hot / Cool / Cold / Archive with rehydration workflow

### ⚪ On the Horizon

- **AeroIndex** — content-aware file intelligence: cross-server deduplication, semantic tags, transactional preview, offline browsing, workspaces. A new way to think about files scattered across 40+ cloud services.
- **IPFS / Web3 Storage** — decentralized storage integration (NLnet grant submitted)
- **Tor Support** — anonymous file transfers via Tor hidden services (NLnet grant submitted)
- **Biometric Unlock** — fingerprint / face unlock for the encrypted vault (Touch ID, Windows Hello)
- **Per-protocol comparison page in docs** — qualitative API vs WebDAV trade-offs, complementing Health Check + Speed Test
- **Topbar nav restructure** — dedicated 3-cluster layout (page-nav / utility / window controls)
- **Custom favicon picker — manual reorder + sort toggle**
- **Icon size enlarge / Appearance slider** — bigger provider icons or user-adjustable size
- **Keyboard accessibility — Tab forward unstuck** — Enter / Space activation already shipped; Tab traversal still pending
- **AeroSync ↔ aeroftp-cli script export/import** — `.ps1` / `.sh` with auto-detected shebang
- **Top-right overlays — keep titlebar drag-region active** while modals are open
- **Right-click "Open with default app"** — `.aerovault` / `.aeroftp` / `.aeroftp-keystore` open inside AeroFTP, `.ps1` / `.sh` open in AeroTools terminal

---

## Provider Pipeline

| Provider | Protocol | Status |
|----------|----------|--------|
| **InfiniCLOUD** (REST v2 + WebDAV) | Muramasa REST + WebDAV | 🟢 Just Shipped — dual-connector with auto-discovery and quota |
| **Immich** | REST API (self-hosted) | 🟢 Just Shipped |
| **Bitbucket** | REST 2.0 | 🟡 In Flight — Git forge Tier 1 |
| **Gitea / Forgejo** | REST v1 | 🟡 In Flight — Git forge Tier 1 (~90% GitHub reuse) |
| **Photo & Media services** | OAuth / REST | 🔵 Up Next — phased rollout, 7 services in queue |
| **GitLab Tier 2-3** | REST API v4 | 🔵 Up Next — Tier 1 already shipped |
| **ImageKit** | REST API | 🔵 Up Next — media CDN + storage |
| **Blomp** | OpenStack Swift | ⏸ Awaiting Blomp proxy fix (auth works, storage 403) |

**Already supported via presets**: Quotaless (S3 + WebDAV), PixelUnion (self-hosted), Hetzner Storage Box (WebDAV/SFTP), Nextcloud / ownCloud (WebDAV auto-detect).

---

## From the Community

A continuous stream of fixes and small features driven by GitHub Issues. We treat the wishlist threads as a single rolling backlog: items get tagged, sorted by effort, and merged into the lanes above as they're picked up.

Recent contributors include **[@EhudKirsh](https://github.com/EhudKirsh)**, whose detailed wishlists across multiple releases shaped the IntroHub polish, Activity Log filtering, OAuth Edit form parity, AeroFile auto-refresh, keyboard accessibility (Enter/Space activation, font-size shortcuts), the Choose Icon dialog, and the detailed server cards with storage bar + Health Check radial.

Open community items currently in our triage:

- AeroSync ↔ aeroftp-cli script export/import (`.ps1` / `.sh`, OS-aware shebang)
- Top-right overlays — keep titlebar drag-region active while modal is open
- AeroFile right-click "Open with default app" (`.aerovault` / `.aeroftp` in-app, `.ps1` / `.sh` in AeroTools)
- Persistent Mount Manager + Open Mount button
- Mobile-friendly window dimensions for Linux phones
- Per-protocol comparison page in docs

If you spot a bug, want a small feature, or want to nominate a provider for native integration, [open an issue](https://github.com/axpdev-lab/aeroftp/issues). Tier 1 quick wins are typically picked up within one or two releases.

---

## Detailed Release History

The lane view above is what most users want. The tables below are kept for users who want to see exactly which feature landed in which release.

### v3.7.0

| Feature | Description |
|---------|-------------|
| **AeroRsync session-cached batch transport** | New `AerorsyncBatch` trait amortizes a single SSH session across many consecutive delta transfers. `SyncReport` exposes `delta_files[]` (per-file breakdown) and `bytes_on_wire` (cumulative wire savings) surfaced in SyncPanel. |
| **AeroVault overlay session model** | Open an `.aerovault` once and route every list/upload/download/rename through the encrypted overlay transparently. Provider sees only opaque vault chunks; UI shows plaintext entries. Header status badge marks when overlay is active. |
| **rclone crypt full read/write** | Beyond the existing read-only browse, AeroFTP now re-encrypts on the upload path with a transparent crypto overlay session. Filename obfuscation is deterministic; provider sees only encrypted blobs. |
| **Server Health Check** | Real-time DNS/TCP/TLS/HTTP probes per saved server in IntroHub Pro. Latency measurements, 0-100 health scoring, capability matrix per protocol, SVG radial gauge, parallel batch refresh. |
| **MCP wave-5 cross-profile transfer** | `aeroftp_transfer` and `aeroftp_transfer_tree` copy files between two saved profiles in one batch. Source and destination provider opened once and reused; path validation, audit log, throttled progress streaming. |
| **MCP wave-6 ops tools** | Six new tools — `aeroftp_touch`, `aeroftp_cleanup`, `aeroftp_speed`, `aeroftp_sync_doctor`, `aeroftp_dedupe`, `aeroftp_reconcile` — plus per-group caps (`max_match`, `max_differ`, `max_missing_local`, `max_missing_remote`) and `omit_match` switch on `aeroftp_check_tree`. MCP tool count: 27 → 39. |
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
| v3.0.0 | **AeroFTP 3.0** — Tauri 2 migration, new UI, plugin system |

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
- **Help translate** — we're always looking for native speakers to improve translations
- **Run a storage service?** See the [Provider Integration Guide](docs/PROVIDER-INTEGRATION-GUIDE.md) for a native integration in AeroFTP. We collaborate directly with providers on the API mapping.
