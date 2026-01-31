# AeroFTP

<p align="center">
  <img src="https://github.com/axpnet/aeroftp/raw/main/icons/AeroFTP_simbol_color_512x512.png" alt="AeroFTP Logo" width="128" height="128">
</p>

<p align="center">
  <strong>Modern. Fast. Multi-protocol. AI-powered.</strong>
</p>

<p align="center">
  Cross-platform desktop client for FTP, FTPS, SFTP, WebDAV, S3-compatible storage, and cloud providers including Google Drive, Dropbox, OneDrive, MEGA, Box, pCloud, Azure Blob Storage, and Filen. 13 protocols in one app. Turn any FTP server into your personal cloud with AeroCloud.
</p>

<p align="center">
  <img src="https://img.shields.io/github/v/release/axpnet/aeroftp?style=for-the-badge" alt="Latest Release">
  <img src="https://img.shields.io/badge/Platform-Linux%20%7C%20Windows%20%7C%20macOS-green?style=for-the-badge" alt="Platform">
  <img src="https://img.shields.io/badge/Built%20with-Tauri%202%20%2B%20React%2018-purple?style=for-the-badge" alt="Built with">
  <img src="https://img.shields.io/badge/License-GPL--3.0-orange?style=for-the-badge" alt="License">
  <img src="https://img.shields.io/badge/Languages-51-blue?style=for-the-badge" alt="Languages">
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
| **WebDAV** | HTTPS | Nextcloud, ownCloud, Synology, DriveHQ. File locking (RFC 4918) |
| **S3** | HTTPS | AWS S3, MinIO, Backblaze B2, Wasabi, Cloudflare R2. Multipart upload |
| **Google Drive** | OAuth2 PKCE | File versions, thumbnails, share permissions |
| **Dropbox** | OAuth2 PKCE | File versions, thumbnails, native sharing |
| **OneDrive** | OAuth2 PKCE | Resumable upload, file versions, share permissions |
| **MEGA.nz** | Client-side AES | 20GB free, end-to-end encrypted, zero-knowledge |
| **Box** | OAuth2 PKCE | 10GB free, enterprise-grade, file versions, share links |
| **pCloud** | OAuth2 | 10GB free, US/EU regions, path-based API |
| **Azure Blob** | HMAC-SHA256 / SAS | Enterprise blob storage, container-based, XML API |
| **Filen** | Client-side AES-256-GCM | 10GB free, zero-knowledge E2E encryption, PBKDF2 |

---

## Key Features

### FTP-First Design
AeroFTP is an FTP client first. Full encryption support with configurable TLS modes (Explicit AUTH TLS, Implicit TLS, opportunistic TLS), certificate verification control, MLSD/MLST machine-readable listings (RFC 3659), and resume transfers (REST/APPE). More FTP options than FileZilla.

### AeroCloud - Your Personal Cloud
Turn **any FTP server** into a private personal cloud with bidirectional sync, tray background sync, share links, and per-project local folders.

### 51 Languages
More languages than any other FTP client. RTL support for Arabic, Hebrew, Persian, and Urdu. Automatic browser language detection.

### Cloud Storage Integration
13 protocols in one client. Native support for Google Drive, Dropbox, OneDrive, MEGA, Box, pCloud, Azure Blob Storage, and Filen alongside traditional FTP/SFTP/WebDAV/S3. Cross-provider features: remote search, storage quota, file versions, thumbnails, share permissions, and WebDAV locking. Filen provides zero-knowledge end-to-end AES-256-GCM encryption.

### Advanced File Management
- **Smart Overwrite Dialog**: File conflict resolution with comparison view
- **Properties Dialog**: Detailed metadata with checksum calculation
- **Archives**: ZIP, 7z with optional AES-256 encryption, TAR, TAR.GZ, TAR.XZ, TAR.BZ2, RAR extraction
- **Keyboard Shortcuts**: F2 rename, Delete, Ctrl+C/V, Ctrl+A
- **Drag and Drop**, **List/Grid view** with thumbnails, **media player**

### DevTools Panel
- **Monaco Editor** (VS Code engine) for remote file editing
- Integrated **terminal** with Tokyo Night theme
- **AeroAgent**: AI assistant for commands and file analysis

### Security
- **OS Keyring**: gnome-keyring, macOS Keychain, Windows Credential Manager
- **AI API keys in Keyring**: API keys for AI providers stored securely, never in localStorage
- **Encrypted vault fallback**: AES-256-GCM with Argon2id key derivation
- **SFTP host key verification**: TOFU with `~/.ssh/known_hosts`
- **Ephemeral OAuth2 port**: Random port for callbacks (no fixed port exposure)
- **FTP insecure warning**: Visual banner when using unencrypted FTP
- **Memory zeroization**: Credentials cleared via `secrecy` + `zeroize`

### Debug & Developer Tools
- **Debug Mode**: Toggle via File menu (Ctrl+Shift+F12)
- **Dependencies Panel**: Live crate version checking against crates.io
- **Debug Panel**: Connection, network, system, logs, and frontend diagnostics

---

## Competitor Comparison

| Feature | AeroFTP | FileZilla | Cyberduck | WinSCP |
|---------|---------|-----------|-----------|--------|
| Protocols | **13** | 3 | 6 | 4 |
| Cloud Providers | **8** (GDrive, Dropbox, OneDrive, MEGA, Box, pCloud, Azure, Filen) | 0 | 3 | 0 |
| Languages | **51** | 47 | ~10 | ~15 |
| FTPS TLS Modes | Explicit + Implicit + Auto | Explicit + Implicit | Implicit | Explicit + Implicit |
| Code Editor | Monaco (VS Code) | No | No | Basic |
| AI Assistant | Yes | No | No | No |
| Personal Cloud | AeroCloud | No | No | No |
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
