# AeroFTP

<p align="center">
  <img src="https://github.com/axpnet/aeroftp/raw/main/icons/AeroFTP_simbol_color_512x512.png" alt="AeroFTP Logo" width="128" height="128">
</p>

<p align="center">
  <strong>Modern. Fast. Multi-protocol. AI-powered. Encrypted.</strong>
</p>

<p align="center">
  Cross-platform desktop client for FTP, FTPS, SFTP, WebDAV, S3-compatible storage, and cloud providers including Google Drive, Dropbox, OneDrive, MEGA, Box, pCloud, Azure Blob Storage, and Filen. 13 protocols, 30 presets, one app. AES-256 encrypted vaults, Cryptomator support, AI assistant with 24 tools.
</p>

<p align="center">
  <img src="https://img.shields.io/github/v/release/axpnet/aeroftp?style=for-the-badge" alt="Latest Release">
  <img src="https://img.shields.io/badge/Platform-Linux%20%7C%20Windows%20%7C%20macOS-green?style=for-the-badge" alt="Platform">
  <img src="https://img.shields.io/badge/Built%20with-Tauri%202%20%2B%20React%2018-purple?style=for-the-badge" alt="Built with">
  <img src="https://img.shields.io/badge/License-GPL--3.0-orange?style=for-the-badge" alt="License">
  <img src="https://img.shields.io/badge/Languages-51-blue?style=for-the-badge" alt="Languages">
  <img src="https://img.shields.io/badge/Protocols-13-teal?style=for-the-badge" alt="Protocols">
</p>

<p align="center">
  <a href="https://snapcraft.io/aeroftp">
    <img src="https://snapcraft.io/static/images/badges/en/snap-store-black.svg" alt="Get it from the Snap Store">
  </a>
</p>

---

## Protocol Support

| Protocol | Encryption | Features |
|----------|-----------|----------|
| **FTP** | None / Explicit TLS / Implicit TLS | MLSD/MLST (RFC 3659), resume transfers, TLS mode selection |
| **FTPS** | TLS/SSL (Explicit + Implicit) | Certificate verification options, self-signed cert support |
| **SFTP** | SSH | Key authentication, host key verification (TOFU), ed25519/RSA |
| **WebDAV** | HTTPS | Nextcloud, ownCloud, Koofr, Jianguoyun, InfiniCLOUD. File locking (RFC 4918) |
| **S3** | HTTPS | AWS S3, MinIO, Backblaze B2, Wasabi, Cloudflare R2, Alibaba OSS, Tencent COS. Multipart upload |
| **Google Drive** | OAuth2 PKCE | File versions, thumbnails, share permissions, workspace export |
| **Dropbox** | OAuth2 PKCE | File versions, thumbnails, native sharing |
| **OneDrive** | OAuth2 PKCE | Resumable upload, file versions, share permissions |
| **MEGA.nz** | Client-side AES | 20GB free, end-to-end encrypted, zero-knowledge |
| **Box** | OAuth2 PKCE | 10GB free, enterprise-grade, file versions, share links |
| **pCloud** | OAuth2 | 10GB free, US/EU regions, file versions, share links |
| **Azure Blob** | HMAC-SHA256 / SAS | Enterprise blob storage, container-based, XML API |
| **Filen** | Client-side AES-256-GCM | 10GB free, zero-knowledge E2E encryption, PBKDF2 |

---

## Key Features

### FTP-First Design
AeroFTP is an FTP client first. Full encryption support with configurable TLS modes (Explicit AUTH TLS, Implicit TLS, opportunistic TLS), certificate verification control, MLSD/MLST machine-readable listings (RFC 3659), and resume transfers (REST/APPE). More FTP options than FileZilla.

### AeroCloud - Your Personal Cloud
Turn **any FTP server** into a private personal cloud with bidirectional sync, tray background sync, share links, and per-project local folders. Sync index cache enables faster re-scans and true conflict detection across all 13 protocols.

### 51 Languages
More languages than any other FTP client. RTL support for Arabic, Hebrew, Persian, and Urdu. Automatic browser language detection.

### Cloud Storage Integration
13 protocols, 30 presets in one client. Native support for Google Drive, Dropbox, OneDrive, MEGA, Box, pCloud, Azure Blob Storage, and Filen alongside traditional FTP/SFTP/WebDAV/S3. Cross-provider features: remote search, storage quota display in status bar, file versions, thumbnails, share permissions, and WebDAV locking.

### Encryption and Vaults (v1.8.0)
- **AeroVault v2**: Military-grade encrypted containers (.aerovault) with advanced security stack:
  - *AES-256-GCM-SIV* (RFC 8452): Nonce misuse-resistant content encryption — even nonce reuse doesn't compromise security
  - *AES-256-KW* (RFC 3394): Key wrapping for master key protection with built-in integrity
  - *AES-256-SIV*: Deterministic filename encryption — file names are hidden, not just content
  - *Argon2id KDF*: 128 MiB memory / 4 iterations / 4 parallelism — exceeds OWASP 2024 high-security recommendations
  - *HMAC-SHA512*: Header integrity verification — detects tampering before decryption
  - *ChaCha20-Poly1305*: Optional cascade mode for defense-in-depth (double encryption)
  - *64 KB chunks*: Optimal streaming performance with per-chunk authentication
- **Cryptomator**: Open and browse Cryptomator format 8 vaults (legacy support via context menu). Decrypt and encrypt files with scrypt + AES-SIV + AES-GCM
- **Archive Browser**: Browse contents of ZIP, 7z, TAR, and RAR archives in-app without extracting. Selective single-file extraction
- **Archive Encryption**: ZIP and 7z with AES-256 password protection. Compression levels (Store/Fast/Normal/Maximum)

### AeroAgent AI (v1.7.0)
AI-powered assistant with **24 provider-agnostic tools** that work across all 13 protocols:
- **7 AI providers**: OpenAI, Anthropic, Google Gemini, xAI Grok, OpenRouter, Ollama, Custom
- **Native function calling**: OpenAI tools[], Anthropic tool_use, Gemini functionDeclarations
- **Streaming responses**: Real-time incremental rendering for all providers
- **File operations**: List, read, search, create, edit, rename, delete (local + remote)
- **Batch transfers**: Multi-file upload/download, sync preview
- **Find and replace**: Edit text in local and remote files directly from chat
- **Context-aware**: Knows your connected server, current paths, selected files, and protocol
- **Chat history**: Persistent conversations with cost tracking per message

### Advanced File Management
- **AeroFile Mode**: Local-only file manager with resizable preview panel showing image thumbnails, file info, and dimensions
- **CompressDialog**: Unified compression UI with format selection, compression levels, password, and file info
- **Smart Sync** (v1.8.0): Intelligent conflict resolution — overwrite if newer, overwrite if different, skip if identical (timestamp tolerance 1s)
- **Batch Rename** (v1.8.0): Rename multiple files with Find/Replace, Add Prefix, Add Suffix, or Sequential numbering — live preview with conflict detection
- **Inline Rename** (v1.8.0): Click on filename or press F2 to rename directly in file list
- **Smart Overwrite Dialog**: File conflict resolution with comparison view
- **Properties Dialog**: Detailed metadata with checksum calculation (MD5/SHA-256)
- **Keyboard Shortcuts**: 12 shortcuts including F2 inline rename, Delete, Ctrl+C/V, Tab panel switch
- **Drag and Drop**: Cross-panel drag for upload/download, intra-panel drag for move
- **List/Grid view** with thumbnails, sortable columns (name, size, type, date), **media player**

### DevTools Panel
- **Monaco Editor** (VS Code engine) for remote file editing with syntax highlighting
- Integrated **terminal** with 8 themes, multiple tabs, SSH remote shell, font size control
- **AeroAgent** AI assistant panel with tool approval workflow

### Security
- **AeroVault v2**: Military-grade containers with AES-256-GCM-SIV (nonce misuse-resistant), AES-256-KW key wrapping, AES-SIV filename encryption, Argon2id (128 MiB), HMAC-SHA512 header integrity, optional ChaCha20 cascade
- **OS Keyring**: gnome-keyring, macOS Keychain, Windows Credential Manager
- **Cryptomator vaults**: Format 8 compatibility (scrypt + AES-SIV + AES-GCM) via context menu
- **AI API keys in Keyring**: API keys for AI providers stored securely, never in localStorage
- **Encrypted vault fallback**: AES-256-GCM with Argon2id when keyring unavailable
- **SFTP host key verification**: TOFU with `~/.ssh/known_hosts`
- **Ephemeral OAuth2 port**: Random port for callbacks (no fixed port exposure)
- **FTP insecure warning**: Visual banner when using unencrypted FTP
- **Memory zeroization**: Credentials cleared via `secrecy` + `zeroize`
- **Archive passwords**: ZIP/7z passwords wrapped in SecretString, zeroed on drop

### Auto-Updater
- In-app update download with progress bar showing speed and ETA
- AppImage auto-install with "Install & Restart" button
- Periodic background check every 24 hours + tray menu manual check

---

## Competitor Comparison

| Feature | AeroFTP | FileZilla | Cyberduck | WinSCP |
|---------|---------|-----------|-----------|--------|
| Protocols | **13** | 3 | 6 | 4 |
| Cloud Providers | **8** (GDrive, Dropbox, OneDrive, MEGA, Box, pCloud, Azure, Filen) | 0 | 3 | 0 |
| Languages | **51** | 47 | ~10 | ~15 |
| FTPS TLS Modes | Explicit + Implicit + Auto | Explicit + Implicit | Implicit | Explicit + Implicit |
| Code Editor | Monaco (VS Code) | No | No | Basic |
| AI Assistant | **24 tools, 7 providers** | No | No | No |
| Cryptomator | **Yes (format 8)** | No | Yes | No |
| Encrypted Vaults | **AeroVault (AES-256-GCM)** | No | No | No |
| Archive Browser | **ZIP/7z/TAR/RAR** | No | No | No |
| Personal Cloud | AeroCloud | No | No | No |
| Storage Quota | 9 providers | No | Yes | No |
| Sync Index Cache | Yes | No | No | No |
| Dark Mode | Yes | No | Yes | No |
| Archive Encryption | ZIP AES-256, 7z AES-256 | No | No | No |
| Memory Zeroization | Yes (Rust) | No | No | No |

---

## Installation

### Linux Snap
```bash
sudo snap install aeroftp
```
> **Note:** Snap version has limited filesystem access due to strict confinement. For full filesystem access, use .deb or .AppImage.

### Other Formats
Download from [GitHub Releases](https://github.com/axpnet/aeroftp/releases/latest):
- **Linux:** .deb, .rpm, .AppImage
- **Windows:** .exe, .msi
- **macOS:** .dmg

---

## Support the Project

AeroFTP is free and open source software. If you find it useful, please consider supporting its development:

### Donate

- **GitHub Sponsors**: [github.com/sponsors/axpnet](https://github.com/sponsors/axpnet)
- **Buy Me a Coffee**: [buymeacoffee.com/axpnet](https://buymeacoffee.com/axpnet)

### Cryptocurrency

- **Bitcoin (BTC)**: `bc1qdxur90s5j4s55rwe9rc9n95fau4rg3tfatfhkn`
- **Ethereum (ETH/EVM)**: `0x08F9D9C41E833539Fd733e19119A89f0664c3AeE`
- **Solana (SOL)**: `25A8sBNqzbR9rvrd3qyYwBkwirEh1pUiegUG6CrswHrd`
- **Litecoin (LTC)**: `LTk8iRvUqAtYyer8SPAkEAakpPXxfFY1D1`

### Contributing

Contributions are welcome. Please open an issue to discuss proposed changes before submitting a pull request.

---

## License

GPL-3.0 - See [LICENSE](LICENSE) for details.

---

*Built with Rust (Tauri 2) + React 18 + TypeScript*
