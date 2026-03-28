# AeroFTP Roadmap

> A transparent view of where AeroFTP has been, where it is today, and where it's headed.
> This roadmap is updated regularly. Feature requests and feedback are welcome via [GitHub Issues](https://github.com/axpnet/aeroftp/issues).

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

### v3.1.6 (March 2026)

| Feature | Description |
|---------|-------------|
| **FeliCloud** (20th provider) | Nextcloud-based EU cloud storage with 10 GB free, GDPR compliant. Full OCS API integration with share links and trash management. |
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
| 20 | **FeliCloud** | v3.1.6 | WebDAV + OCS API |
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

### v3.2.0 (Q2 2026)

| Feature | Status | Description |
|---------|--------|-------------|
| **Advanced Share Links** | Design complete | Expiration date picker, password protection, permission controls. 9 providers support expiration, 6 support configurable permissions. |
| **Blomp** (21st provider) | Awaiting API access | 40 GB free cloud storage via OpenStack Swift. Backend ready, waiting for storage proxy access. |
| **Full Activity Logging** | Phase 1 done | Extending activity log to cover all provider-specific operations. |

---

## Planned

| Feature | Description |
|---------|-------------|
| **Share Link Management** | View, revoke, and manage existing share links across 10+ providers that support it. |
| **Voice Commands** | Whisper-based speech-to-text for AeroAgent, with on-device transcription. |
| **AeroVault v2** | Next-generation encrypted vault with directory support and AES-256-GCM-SIV chunked encryption. |
| **AeroCloud v2** | Enhanced personal cloud sync with selective sync, bandwidth throttling, and conflict visualization. |

### Provider Pipeline

| Provider | Protocol | Status |
|----------|----------|--------|
| **Blomp** | OpenStack Swift | Awaiting API access |
| **Nextcloud** (generic) | WebDAV + OCS | Planned (FeliCloud paved the way) |

---

## Under Consideration

These features are being evaluated based on community interest:

| Feature | Description |
|---------|-------------|
| **IPFS / Web3 Storage** | Decentralized file storage integration |
| **Tor Support** | Anonymous file transfers via Tor hidden services |
| **Biometric Unlock** | Fingerprint/face unlock for the encrypted vault |
| **Code Signing** | Signed builds for Windows, macOS, and Linux |
| **Mobile App** | Android companion app |

---

## Supported Languages

AeroFTP is available in **47 languages**:

Bulgarian, Bengali, Catalan, Czech, Welsh, Danish, German, Greek, English, Spanish, Estonian, Basque, Finnish, French, Galician, Hindi, Croatian, Hungarian, Armenian, Indonesian, Icelandic, Italian, Japanese, Georgian, Khmer, Korean, Lithuanian, Latvian, Macedonian, Malay, Dutch, Norwegian, Polish, Portuguese, Romanian, Russian, Slovak, Slovenian, Serbian, Swedish, Swahili, Thai, Filipino, Turkish, Ukrainian, Vietnamese, Chinese

---

## How to Contribute

- **Star the repo** to show your support
- **Report bugs** via [GitHub Issues](https://github.com/axpnet/aeroftp/issues)
- **Suggest features** by opening a discussion
- **Help translate** - we're always looking for native speakers to improve translations

---

*Last updated: March 28, 2026*
