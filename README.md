# AeroFTP

<p align="center">
  <img src="https://github.com/axpnet/aeroftp/raw/main/icons/AeroFTP_simbol_color_512x512.png" alt="AeroFTP Logo" width="128" height="128">
</p>

<p align="center">
  <strong>Modern. Fast. Multi-protocol. AI-powered.</strong>
</p>

<p align="center">
  Cross-platform desktop client for FTP/FTPS, WebDAV, S3-compatible storage, and cloud providers including Google Drive, Dropbox, OneDrive, and MEGA. Turn any FTP server into your personal cloud with AeroCloud.
</p>

<p align="center">
  <img src="https://img.shields.io/github/v/release/axpnet/aeroftp?style=for-the-badge" alt="Latest Release">
  <img src="https://img.shields.io/badge/Platform-Linux%20%7C%20Windows%20%7C%20macOS-green?style=for-the-badge" alt="Platform">
  <img src="https://img.shields.io/badge/Built%20with-Tauri%202%20%2B%20React%2018-purple?style=for-the-badge" alt="Built with">
  <img src="https://img.shields.io/badge/License-GPL--3.0-orange?style=for-the-badge" alt="License">
</p>

<p align="center">
  <a href="https://snapcraft.io/aeroftp">
    <img src="https://snapcraft.io/static/images/badges/en/snap-store-black.svg" alt="Get it from the Snap Store">
  </a>
</p>

---

> **Security Update (v1.3.3):** If you are using AeroFTP v1.3.2 or earlier, please update immediately. This release fixes critical issues with the credential storage system. Previous versions may store credentials insecurely or fail to connect to saved servers after migration. Delete your saved servers and re-add them after updating.

---

## Key Features (v1.3.0)

### Global Multilingual Support - 51 Languages
AeroFTP now supports more languages than any other FTP client on the market:
- **51 languages** including all major world languages
- **RTL support** for Arabic, Hebrew, Persian, and Urdu
- Automatic UI direction switching for RTL languages
- Type-safe translation system with fallback to English

### AeroCloud - Your Personal Cloud on Any FTP Server
- Turn **any FTP server** into a private personal cloud
- **Bidirectional sync** (new files local to remote)
- Automatic interval sync with manual sync button (in app and tray menu)
- **Dedicated tray icon**: Close the app, AeroCloud keeps running in background
- **Share link** for remote folder (secure sharing)
- Per-project local folders with navigation sync

### Multi-protocol Support
- FTP / FTPS (TLS/SSL)
- **SFTP** (SSH File Transfer Protocol) - Native Rust implementation
- **WebDAV** (tested: Nextcloud, ownCloud, Synology, DriveHQ)
- **S3-compatible** (tested: Backblaze B2, Wasabi, MinIO)
- **MEGA.nz** - 20GB free, client-side encryption
- OAuth cloud integrations: **Google Drive**, **Dropbox**, **OneDrive**
- Custom connection for any WebDAV or S3-compatible server

### Advanced File Management
- **Smart Overwrite Dialog**: File conflict resolution with comparison view
  - Shows source vs destination (size, date, which is newer)
  - Actions: Overwrite, Skip, Rename, Cancel
  - "Apply to all" for batch operations
  - Configurable default behavior in Settings
- **Properties Dialog**: Detailed file/folder metadata with checksum calculation
- **Compress/Extract**: Create and extract ZIP and 7z archives (AES-256 decryption)
- **Drag and Drop**: Move files within panels
- **List/Grid view** with image thumbnails (local and remote)
- Built-in **media player** for audio/video
- Preview for images, text, and code files

### Activity Log
- Multiple themes: Tokyo Night, Cyber neon, Light
- Typewriter effect with badge counter
- Humanized messages in all supported languages
- Operation filtering (uploads, downloads, AeroCloud)

### DevTools Panel
- **Monaco Editor** (VS Code engine) for file editing
- Integrated **terminal** with Tokyo Night theme
- **AeroAgent**: AI assistant for commands, file analysis, and automation

### Smart Updater
- Auto-check on startup with manual check option
- Desktop notification with download badge
- Direct download link for your platform

### Additional Features
- Light and dark theme support
- Keyboard shortcuts for common operations
- Preserves saved connections and OAuth tokens across updates
- Cross-platform: Linux, Windows, macOS

**Roadmap**: End-to-end encryption, advanced multi-device AeroCloud sync, Cryptomator vault support

---

## Installation

### Linux Snap
```bash
sudo snap install aeroftp
```
> **Note:** Snap version has limited filesystem access due to strict confinement (`~/snap/aeroftp/` only). For full filesystem access, use .deb or .AppImage.

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
