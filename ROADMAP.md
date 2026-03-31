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

---

## In Progress

| Feature | Status | Description |
|---------|--------|-------------|
| **SourceForge** (21st provider) | Awaiting confirmation | SFTP preset for SourceForge File Release System. Backend ready, hidden until provider testing complete. |
| **Agent MCP Server** | In progress | `aeroftp-cli agent --mcp` for native integration with Claude Code, Cursor, and other MCP clients. |
| **Mobile App** | In progress | Android companion app with Capacitor 6 + React. FTP, SFTP, WebDAV protocols, AeroVault v2 import/export. |

---

## Planned

| Feature | Description |
|---------|-------------|
| **Universal File Versioning** | Unified versions panel across 12+ providers (Google Drive, Dropbox, OneDrive, Box, S3, Azure, Nextcloud, kDrive, Filen, pCloud, OpenDrive, MEGA). |
| **Universal Trash Restore** | List, restore, and empty trash across all providers with trash support. |
| **CLI Parallel Transfers** | `--parallel N` worker pool for concurrent transfers. Segmented parallel downloads for large files. Server-to-server copy for cloud-to-cloud migration. |
| **AeroCloud Selective Sync** | Folder-level exclusion with tree view, `.aeroignore` patterns, bandwidth throttling, conflict visualization. |
| **Agent Orchestration** | JSON-RPC programmatic mode, mutative remote operations (put, rm, mv, mkdir) with grant model, cross-server diff/sync. |
| **AeroVault v2 Enhancements** | Cross-platform migration, multi-device sync integration, key rotation. |
| **S3 Storage Class Management** | Set storage class on upload, change in-place, Glacier restore workflow, tier badges in UI. |
| **Azure Blob Tier Management** | Hot/Cool/Cold/Archive tier management with rehydration workflow. |

### Provider Pipeline

| Provider | Protocol | Status |
|----------|----------|--------|
| **SourceForge** | SFTP | Awaiting confirmation |
| **Blomp** | OpenStack Swift | Awaiting API access |
| **Nextcloud** (generic) | WebDAV + OCS | Planned (Felicloud paved the way) |

---

## Under Consideration

These features are being evaluated based on community interest:

| Feature | Description |
|---------|-------------|
| **IPFS / Web3 Storage** | Decentralized file storage integration (NLnet grant submitted) |
| **Tor Support** | Anonymous file transfers via Tor hidden services (NLnet grant submitted) |
| **Biometric Unlock** | Fingerprint/face unlock for the encrypted vault |
| **CLI TUI Explorer** | ncdu-style interactive disk usage explorer for remote servers |
| **CLI FUSE Mount** | Mount remote servers as local filesystem |
| **CLI Serve Mode** | Expose remote storage as local HTTP/WebDAV server |

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

*Last updated: March 31, 2026*
