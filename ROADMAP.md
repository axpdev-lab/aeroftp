# AeroFTP Roadmap

> A transparent view of where AeroFTP has been, where it is today, and where it's headed.
> This roadmap is updated regularly. Feature requests and feedback are welcome via [GitHub Issues](https://github.com/axpdev-lab/aeroftp/issues).

---

## Legend

| Symbol | Meaning |
|--------|---------|
| **Shipped** | Released and available |
| **In progress** | Currently being worked on |
| **Planned** | Confirmed for a future release |
| **Considering** | Evaluating based on community interest |

---

## Recently Shipped

### v3.5.0 (April 2026)

| Feature | Description |
|---------|-------------|
| **FileZilla import/export bridge** | Import sites from FileZilla `sitemanager.xml` and export back. Supports FTP, SFTP, FTPS (implicit and explicit), and S3. Passwords decoded from base64 and upgraded to AES-256-GCM encrypted vault. GUI and CLI (`aeroftp import filezilla`). |
| **Unified Bridge hub** | Single "Bridge" section with app selector (rclone, WinSCP, FileZilla) replaces separate import/export sections. Three bridge tools, one interface. |
| **Nextcloud WebDAV auto-detection** | Connecting to a Nextcloud/ownCloud server without specifying the WebDAV path now auto-discovers `/remote.php/dav/files/{username}/`. No manual path configuration needed. |
| **Transfer engine hardening** | Timeout scales with file size (2s/MB + 30s base). "Skip if identical" works reliably. Retry queue preserved after batch completion. |

### v3.4.9 (April 2026)

| Feature | Description |
|---------|-------------|
| **WinSCP import/export bridge** | Import saved sessions from WinSCP configuration files. Supports SFTP, SCP, FTP, FTPS, WebDAV, and S3. Passwords decoded from WinSCP's XOR obfuscation and upgraded to AES-256-GCM vault. Export back to WinSCP.ini also available. GUI and CLI (`aeroftp import winscp`). |
| **Duplicate detection in import** | rclone and WinSCP import screens show an "Already exists" badge on matching profiles, with option to update credentials on re-import. |
| **macOS Quit fix** | Cmd+Q and menu bar Quit now exit correctly even when AeroCloud hide-to-tray is active. |
| **Import/export security hardening** | Path traversal rejection, symlink resolution, 10 MB size cap, credential redaction in JSON output, INI injection prevention. |

### v3.4.8 (April 2026)

| Feature | Description |
|---------|-------------|
| **Cross-profile transfer panel** | Dedicated toolbar button for cloud-to-cloud transfers. Floating panel with real-time queue, progress bars, and plan/execute/done transitions. |
| **CLI `transfer` command** | Cross-profile copy between two vault-backed profiles with dry-run, recursive mode, and `--skip-existing` for backup flows. |
| **CLI doctor workflows** | `sync-doctor` and `transfer-doctor` preflight commands with structured checks, risk summaries, and `suggested_next_command` for agent automation. |
| **Rate limit resilience** | Automatic retry with exponential backoff on 429/5xx for Zoho WorkDrive, GitLab, and Swift/Blomp. |

### v3.4.7 (April 2026)

| Feature | Description |
|---------|-------------|
| **rclone config import** | Import server profiles from rclone.conf files. Supports 17 rclone backend types (FTP, SFTP, S3, WebDAV, Google Drive, Dropbox, OneDrive, MEGA, Box, pCloud, Azure Blob, Swift, Yandex Disk, Koofr, Jottacloud, Backblaze B2, OpenDrive). Passwords de-obfuscated from rclone's reversible AES-256-CTR and stored in AES-256-GCM encrypted vault. |
| **rclone config export** | Export server profiles to rclone.conf format for full interoperability with rclone CLI. Passwords obfuscated using rclone's standard scheme. |
| **CLI `import rclone`** | New subcommand `aeroftp import rclone [path] [--json]` for headless config migration. |
| **MEGA default fix** | New MEGA profiles default to Native API instead of MEGAcmd. Existing profiles without explicit mode correctly labeled. |

### v3.3.0 (April 2026)

| Feature | Description |
|---------|-------------|
| **IntroHub redesign** | New tabbed interface replaces the 50/50 split layout. My Servers grid with favorites, Discover Services catalog (49 providers in 5 categories), Command Palette (Ctrl+K), and dynamic form tabs. |
| **SourceForge integration** | Native SFTP provider for SourceForge File Release System. Pre-configured connection with Project (Unixname) field and SSH key authentication. |
| **Custom Checkbox component** | All ~75 native HTML checkboxes replaced with animated SVG checkmark component. Focus-visible ring, aria-label support, keyboard navigation. |
| **SFTP upload fix (#73)** | Removed SSH2/SCP fallback that caused "host key changed" errors during upload. Uploads now use native russh_sftp through the same SSH session. |
| **Auto-update Trust UI** | Sigstore verification badges (green/amber/red). Linux restart reliability fix. Snap users redirected to store. Post-restart confirmation with actual verification status. |
| **Cloud provider descriptions** | All 17 cloud services in Discover show storage info and signup links. Info banners for all 5 categories translated in 47 languages. |
| **Collapsible SSH Auth** | SFTP SSH authentication fields collapse by default, saving form space. |
| **Badge accuracy** | Fixed kDrive, Yandex Disk, Koofr badges. Added OCS badge for Felicloud/Nextcloud, Swift for Blomp. |

### v3.2.6 (March 2026)

| Feature | Description |
|---------|-------------|
| **macOS crash fix** | Static liblzma linking eliminates DYLD crash at launch caused by dynamic Homebrew dependency. |
| **Security hardening** | Resolved HIGH vulnerability (russh 0.59), removed dangerouslySetInnerHTML XSS finding, 116 dependency updates. Aikido Security: **Top 5% benchmark**, 0 open issues. |
| **Felicloud direct access** | Clicking Felicloud in the connection screen now goes directly to the connection form instead of routing through the WebDAV preset list. |
| **Status bar consistency** | File count indicators (remote/local) now match the visual panel order when panels are swapped. |

### v3.2.5 (March 2026)

| Feature | Description |
|---------|-------------|
| **Linux localhost fix** | Fixed connection refused on startup when `tauri-plugin-localhost` resolved to IPv6. Plugin now explicitly binds to `127.0.0.1`. |

### v3.2.2 (March 2026)

| Feature | Description |
|---------|-------------|
| **Advanced Share Links** | Password protection, expiry dates, and granular permissions for share links across 13 providers. |
| **MEGA S4 Object Storage** | S3-compatible object storage via MEGA's S4 infrastructure. |
| **CLI link enhancements** | New `--password`, `--expires`, and `--permissions` flags for the `link` command. |
| **Security Hardening** | Additional security improvements across the application. |

### v3.2.0 (March 2026)

| Feature | Description |
|---------|-------------|
| **MEGA Native API** | Full native MEGA protocol - connect without MEGAcmd. Client-side AES-128-CTR encryption, RSA session auth, encrypted node tree, share links, trash management. Zero external dependencies. |
| **MEGA Dual-Backend** | Users choose Native API or MEGAcmd in connection form. Mode-specific badges, session persistence, backward compatibility with existing profiles. |
| **Windows MEGA Fixes** | Console flash eliminated (CREATE_NO_WINDOW), login via CLI arg for .bat wrappers. |
| **Trash Date Formatting** | All 11 trash managers now display human-readable dates. |

### v3.1.8 (March 2026)

| Feature | Description |
|---------|-------------|
| **Desktop Security Hardening** | Sigstore client-side update verification, backend AI tool approval with native OS dialogs, vault passphrase moved to OS keyring, plugin registry fail-closed. |
| **Agent Orchestration** | External AI agents (Claude Code, Codex, Cursor) can orchestrate AeroAgent via CLI with credential isolation. New `server_list_saved` and `server_exec` tools. New `ai-models` command for provider discovery. [Full documentation](https://docs.aeroftp.app/features/agent-orchestration). |
| **FileLu v2 Listing** | v2 `folder/list` as primary listing path with legacy hybrid fallback. New metadata fields: content_hash, direct_link, public status. |

### v3.1.7 (March 2026)

| Feature | Description |
|---------|-------------|
| **Glob Find Patterns** | 8 providers (WebDAV, SFTP, S3, Jottacloud, Yandex Disk, GitHub, Filen, pCloud) now support glob patterns in find. |
| **LargeIconsGrid Virtualization** | react-virtuoso for large directories. |
| **DOMPurify CVE Fix** | Override to 3.3.3 (CVE mutation-XSS via monaco-editor). |
| **Nextcloud Trash Scope** | Trash button restricted to Nextcloud/Felicloud WebDAV providers only. |

### v3.1.6 (March 2026)

| Feature | Description |
|---------|-------------|
| **Felicloud** (20th provider) | Nextcloud-based EU cloud storage with 10 GB free, GDPR compliant. Full OCS API integration with share links and trash management. |
| **Share Link Modal** | New dedicated modal for share links with visual confirmation, copy buttons, and automatic password display when required by the server. |
| **FileLu Performance** | Folder listing speed improved by 50% through parallel API calls. |
| **Activity Log Coverage** | All trash operations (restore, permanent delete, empty) now tracked in the activity log across 14 providers. Share link creation and 30+ provider-specific operations also logged. |
| **Share Link Bug Fixes** | Fixed missing share link support for Azure, kDrive, and Drime Cloud. Link expiration now properly passed to all providers that support it. |

### v3.1.5 (March 2026)

| Feature | Description |
|---------|-------------|
| **AeroAgent Hardening** | Improved AI agent reliability with prompt injection sanitization and memory management. |
| **CLI Evolution** | 30 subcommands, batch engine with 17 commands, parallel transfers, shell completions. |
| **Security Audit** | Full security review with backend-enforced command denylist and signed audit log. |

### v3.1.4 (March 2026)

| Feature | Description |
|---------|-------------|
| **FileLu v2 API** | Migrated to path-based API with hash-based sync support. |
| **AeroCloud Closure** | Production-grade personal cloud sync with conflict resolution. |

### v3.1.2 (February 2026)

| Feature | Description |
|---------|-------------|
| **Zoho WorkDrive** (16th provider) | Full OAuth2 integration with native document creation, labels, and share link management. |
| **Swap Panels** | One-click swap between local and remote panels. |

### v3.1.0 (February 2026)

| Feature | Description |
|---------|-------------|
| **Co-Author Address Book** | Manage and reuse co-authors for Git commits. |
| **Static CRT** | Windows builds with statically linked C runtime for broader compatibility. |

### v3.0.x Highlights (January - February 2026)

| Version | Feature |
|---------|---------|
| v3.0.9 | **GitHub Batch Operations** - bulk upload, delete, and commit across repositories |
| v3.0.7 | **GitHub Actions Browser** - monitor and trigger CI/CD workflows directly from AeroFTP |
| v3.0.5 | **GitHub App Authentication** - PEM vault storage, installation tokens, branch protection |
| v3.0.0 | **AeroFTP 3.0** - Tauri 2 migration, new UI, 15 AI providers, plugin system |

---

### Provider Timeline

Every new cloud provider integration is a milestone. Here's the full history:

| # | Provider | Version | Protocol |
|---|----------|---------|----------|
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

Plus the core protocols: **FTP**, **FTPS**, **SFTP**, **WebDAV**, **AeroCloud**

**Bridge interoperability** (v3.4.7-v3.5.0): Import/export profiles with **rclone** (17 backends), **WinSCP** (6 protocols), and **FileZilla** (4 protocols). Credentials decoded from each tool's obfuscation format and upgraded to AES-256-GCM vault.

---

## In Progress

| Feature | Status | Description |
|---------|--------|-------------|
| **InfiniCLOUD REST API** | Beta | Dual connector (WebDAV / REST API) with Muramasa API. Auto-discovery of user node server and real-time storage quota. Available for developer and beta testing. |
| **Agent MCP Server** | In progress | `aeroftp-cli agent --mcp` for native integration with Claude Code, Cursor, and other MCP clients. 10 phases, 8 new files, async stdio, connection pool. |
| **Mobile App** | In progress | Android companion app with Capacitor 6 + React. FTP, SFTP, WebDAV protocols, AeroVault v2 import/export. 17/19 tasks complete (89%). |

---

## Planned

### AeroFile Dual Panel (local)

Optional Total Commander-style dual local panel with unified tab bar, local-to-local drag-and-drop, and F5/F6 keyboard shortcuts.

### Provider Capabilities

| Feature | Description |
|---------|-------------|
| **Universal File Versioning** | Unified versions panel across 12+ providers (Google Drive, Dropbox, OneDrive, Box, S3, Azure, Nextcloud, kDrive, Filen, pCloud, OpenDrive, MEGA) |
| **Universal Trash Restore** | List, restore, and empty trash across all providers with trash support |
| **S3 Storage Class Management** | Set storage class on upload, change in-place, Glacier restore workflow, tier badges in UI |
| **Azure Blob Tier Management** | Hot/Cool/Cold/Archive tier management with rehydration workflow |

### CLI Advanced (already shipped, CLI-only)

These features are production-ready in the CLI. GUI integration is planned for future releases.

| Feature | CLI Status | GUI Planned |
| ------- | ---------- | ----------- |
| **FUSE Mount** | Shipped (v3.4.2) - Linux + macOS read-write, Windows WebDAV bridge. `aeroftp mount <profile>:<path> <mountpoint>` | Mount manager panel with mount/unmount, status indicators, auto-mount on startup |
| **Daemon & Job Queue** | Shipped (v3.4.2) - `aeroftp daemon start/stop/status`, HTTP RC API, persistent SQLite job queue, `jobs add/list/status/cancel` | Background daemon control in system tray, job queue viewer with pause/cancel/priority |
| **Serve HTTP/WebDAV/FTP/SFTP** | Shipped (v3.3.5-v3.4.2) - Expose any remote as local HTTP (range 206), WebDAV (r/w, 8 methods), FTP (`libunftp`), SFTP (`russh`) | Quick Share panel: one-click serve a folder with QR code and local URL |
| **Bisync** | Shipped (v3.4.2) - True bidirectional sync with snapshot, delta mtime, `--conflict-mode`, `--resync`, `--backup-dir` | Already partially in AeroSync GUI. Full conflict visualization planned |
| **NCdu Explorer** | Shipped (v3.4.2) - ratatui TUI with recursive scan, keyboard navigation, JSON export | Disk usage treemap already in GUI. Remote NCdu-style drill-down planned |
| **Crypt Overlay** | Shipped (v3.4.2) - AES-256-GCM content + AES-256-SIV filenames + Argon2id KDF. `crypt init/ls/put/get` | Transparent crypt layer in file browser, encrypt-on-upload toggle per profile |

### Sync & Transfer

| Feature | Description |
|---------|-------------|
| **AeroCloud Selective Sync** | Folder-level exclusion with tree view, `.aeroignore` patterns, bandwidth throttling, conflict visualization |
| **Streaming Scan Pipeline** | Producer-consumer architecture for immediate transfer start without full directory scan |
| **Rclone Crypt Read Compatibility** | Transparent read-only decryption of existing rclone-encrypted remotes (XSalsa20-Poly1305 content, EME filename encryption) |

### Agent & Orchestration

| Feature | Description |
|---------|-------------|
| **Agent Orchestration** | JSON-RPC programmatic mode, mutative remote operations (put, rm, mv, mkdir) with grant model, cross-server diff/sync |
| **AeroVault v2 Enhancements** | Cross-platform migration, multi-device sync integration, key rotation |

### Documentation & Distribution

| Feature | Description |
|---------|-------------|
| **Provider Landing Pages** | SEO landing pages on docs.aeroftp.app for 30+ providers with connection guides and feature matrices |
| **Auto-Update Trust UI** | Sigstore trust UI improvements, macOS DMG distribution |

### Provider Pipeline

| Provider | Protocol | Status |
|----------|----------|--------|
| **InfiniCLOUD** (REST API) | REST + WebDAV | Beta - dual connector with Muramasa API for auto-discovery and quota |
| **Blomp** | OpenStack Swift | Awaiting Blomp proxy fix (auth works, storage 403) |
| **GitLab** (completion) | REST API v4 | Tier 1 shipped (v3.3.2). Remaining: Tier 2-3 features |
| **Bitbucket** | REST 2.0 | Planned - Git forge Tier 1 |
| **Gitea / Forgejo** | REST v1 | Planned - Git forge Tier 1 (~90% GitHub reuse) |
| **ImageKit** | REST API | Planned - media CDN + storage |

**Already supported via presets:** Quotaless (S3 + WebDAV), PixelUnion/Immich (self-hosted), Hetzner Storage Box (WebDAV/SFTP), Nextcloud/ownCloud (WebDAV auto-detect since v3.5.0)

---

## Under Consideration

These features are being evaluated based on community interest and NLnet grant outcome:

| Feature | Description |
|---------|-------------|
| **Content-aware file intelligence** | What if your file manager understood what's inside your files, not just where they are? Cross-server awareness, smarter transfers, and a new way to think about files scattered across 40+ cloud services |
| **IPFS / Web3 Storage** | Decentralized file storage integration (NLnet grant submitted) |
| **Tor Support** | Anonymous file transfers via Tor hidden services (NLnet grant submitted) |
| **Biometric Unlock** | Fingerprint/face unlock for the encrypted vault |
| **Share Link Redesign** | Unified share experience with QR codes, link analytics, and team sharing |

---

## Supported Languages

AeroFTP is available in **47 languages**:

Bulgarian, Bengali, Catalan, Czech, Welsh, Danish, German, Greek, English, Spanish, Estonian, Basque, Finnish, French, Galician, Hindi, Croatian, Hungarian, Armenian, Indonesian, Icelandic, Italian, Japanese, Georgian, Khmer, Korean, Lithuanian, Latvian, Macedonian, Malay, Dutch, Norwegian, Polish, Portuguese, Romanian, Russian, Slovak, Slovenian, Serbian, Swedish, Swahili, Thai, Filipino, Turkish, Ukrainian, Vietnamese, Chinese

---

## How to Contribute

- **Star the repo** to show your support
- **Report bugs** via [GitHub Issues](https://github.com/axpdev-lab/aeroftp/issues)
- **Suggest features** by opening a discussion
- **Help translate** - we're always looking for native speakers to improve translations

---

*Last updated: April 14, 2026*
