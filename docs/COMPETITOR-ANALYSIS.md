# AeroFTP Competitor Analysis

> Last Updated: 29 January 2026
> Version: v1.3.4-dev (Security Hardening + SFTP Host Key Verification)

---

## Market Overview

| Client | Platform | Price | Open Source | Stack | Downloads |
|--------|----------|-------|-------------|-------|-----------|
| **AeroFTP** | Linux, Windows, macOS | Free | GPL-3.0 | Rust + React | Growing |
| **FileZilla** | Linux, Windows, macOS | Free | GPL | C++ | 124M+ |
| **Cyberduck** | Windows, macOS | Free/$10 | GPL | Java | 30M+ |
| **WinSCP** | Windows | Free | GPL | C++ | 100M+ |
| **Transmit** | macOS | $45 | Proprietary | Swift | - |
| **ForkLift** | macOS | Free/$30 | Proprietary | Swift | - |

---

## Feature Comparison Matrix

### Protocol Support

| Protocol | AeroFTP | FileZilla | Cyberduck | WinSCP | Transmit | ForkLift |
|----------|---------|-----------|-----------|--------|----------|----------|
| FTP | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| FTPS (TLS) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| SFTP | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| WebDAV | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ |
| S3-compatible | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ |

### Cloud Storage Integration

| Provider | AeroFTP | FileZilla | Cyberduck | WinSCP | Transmit | ForkLift |
|----------|---------|-----------|-----------|--------|----------|----------|
| Google Drive | ✅ | ❌ | ✅ | ❌ | ✅ | ✅ |
| Dropbox | ✅ | ❌ | ✅ | ❌ | ✅ | ✅ |
| OneDrive | ✅ | ❌ | ✅ | ❌ | ✅ | ✅ |
| **MEGA.nz** | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Backblaze B2 | ✅ | ❌ | ✅ | ❌ | ✅ | ❌ |
| Azure Blob | 📋 | ❌ | ✅ | ❌ | ✅ | ❌ |
| OpenStack Swift | ❌ | ❌ | ✅ | ❌ | ❌ | ❌ |

### User Interface

| Feature | AeroFTP | FileZilla | Cyberduck | WinSCP | Transmit | ForkLift |
|---------|---------|-----------|-----------|--------|----------|----------|
| Dual-pane | ✅ | ✅ | ❌ | ✅ | ❌ | ✅ |
| Dark mode | ✅ | ❌ | ✅ | ❌ | ✅ | ✅ |
| Multi-tab | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Thumbnails | ✅ | ❌ | ❌ | ❌ | ✅ | ✅ |
| Grid/List view | ✅ | ❌ | ✅ | ❌ | ✅ | ✅ |
| Modern UI | ✅ | ❌ | ✅ | ❌ | ✅ | ✅ |

### Pro Features

| Feature | AeroFTP | FileZilla | Cyberduck | WinSCP | Transmit | ForkLift |
|---------|---------|-----------|-----------|--------|----------|----------|
| Code Editor | ✅ Monaco | ❌ | ❌ | ✅ Basic | ❌ | ❌ |
| Terminal | ✅ | ❌ | ❌ | ✅ PuTTY | ❌ | ❌ |
| AI Assistant | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Media Player | ✅ | ❌ | ❌ | ❌ | ❌ | Quick Look |
| Activity Log | ✅ | ✅ | ✅ | ✅ | ❌ | ❌ |

### Sync & Automation

| Feature | AeroFTP | FileZilla | Cyberduck | WinSCP | Transmit | ForkLift |
|---------|---------|-----------|-----------|--------|----------|----------|
| Personal Cloud | ✅ AeroCloud | ❌ | ❌ | ❌ | ❌ | ❌ |
| Background Sync | ✅ Tray | ❌ | ❌ | ❌ | ❌ | ❌ |
| Folder Sync | ✅ | ✅ | ❌ | ✅ | ✅ | ✅ |
| Scripting | 📋 | ❌ | ❌ | ✅ | ❌ | ❌ |
| Queue Management | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |

### Security

| Feature | AeroFTP | FileZilla | Cyberduck | WinSCP | Transmit | ForkLift |
|---------|---------|-----------|-----------|--------|----------|----------|
| Cryptomator | 📋 v1.7 | ❌ | ✅ | ❌ | ❌ | ❌ |
| Share Links | ✅ | ❌ | ✅ | ❌ | ❌ | ❌ |
| Keychain/Keyring | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Encrypted Vault (AES-256-GCM) | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Argon2id Key Derivation | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| SFTP Host Key Verification | ✅ TOFU | ✅ | ✅ | ✅ | ✅ | ✅ |
| OAuth2 PKCE Flow | ✅ | ❌ | ✅ | ❌ | ✅ | ❌ |
| Ephemeral OAuth Port | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| FTP Insecure Warning | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Memory Zeroization | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Config Permission Hardening | ✅ | ❌ | ❌ | ✅ | ✅ | ❌ |
| 7z AES-256 Archives | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |

### Distribution

| Feature | AeroFTP | FileZilla | Cyberduck | WinSCP | Transmit | ForkLift |
|---------|---------|-----------|-----------|--------|----------|----------|
| Snap | ✅ | ✅ | ❌ | ❌ | ❌ | ❌ |
| Flatpak | 🔄 | ✅ | ❌ | ❌ | ❌ | ❌ |
| AppImage | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Homebrew | 📋 | ✅ | ✅ | ❌ | ✅ | ✅ |
| Auto-Update | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| i18n Languages | **51** | 47 | ~10 | ~15 | ~5 | ~5 |

**Legend:** ✅ Available | 📋 Planned | 🔄 In Progress | ❌ Not Available

---

## AeroFTP Unique Selling Points (USP)

### Features No Competitor Has

| Feature | Description | Competitive Advantage |
|---------|-------------|----------------------|
| **AeroCloud** | Transform any FTP into personal cloud with bidirectional sync | Unique in market |
| **MEGA.nz Support** | Native integration with 20GB free encrypted storage | Only client with this |
| **Monaco Editor** | VS Code engine for remote file editing | Professional-grade |
| **AeroAgent AI** | AI assistant for commands and file analysis | Industry first |
| **Modern Stack** | Rust backend + React frontend | Performance + Security |
| **Tray Background Sync** | Continuous sync without main window | Not in any competitor |
| **OS Keyring Storage** | Native keyring on all platforms (gnome-keyring, macOS Keychain, Windows Credential Manager) | Zero plaintext credentials |
| **AES-256-GCM Vault** | Argon2id + AES-256-GCM encrypted vault when keyring unavailable | No competitor has fallback encryption |
| **SFTP TOFU + MITM Reject** | Host key verification with Trust On First Use, rejects on key change | Proactive MITM protection |
| **Ephemeral OAuth Ports** | OS-assigned random port for OAuth2 callback, prevents token interception | No competitor does this |
| **FTP Insecure Warning** | Visual badge + warning banner when selecting unencrypted FTP | Educates users on security |
| **Memory Zeroization** | Passwords and keys cleared from memory via zeroize/secrecy crates | Rust-only advantage |
| **Multi-Format Archives** | ZIP, 7z, TAR, GZ, XZ, BZ2 with context menu | Most complete in segment |

### Technology Advantages

| Aspect | AeroFTP | Legacy Competitors |
|--------|---------|-------------------|
| Backend | Rust (memory safe, fast) | C++/Java (legacy) |
| Frontend | React 18 + TypeScript | Qt/wxWidgets/Swing |
| Bundle Size | ~50MB | 100-200MB |
| Startup Time | <2s | 3-5s |
| Memory Usage | Low | High (especially Java) |

---

## Competitor Strengths (Gaps to Close)

| Competitor | Strength | Priority for AeroFTP |
|------------|----------|---------------------|
| **FileZilla** | SFTP native, 47 languages, stability | ✅ CLOSED: SFTP done, 51 langs, host key verification |
| **Cyberduck** | Cryptomator encryption, more clouds | MEDIUM: Cryptomator (v1.7.0) |
| **WinSCP** | Scripting/automation, PuTTY integration | MEDIUM: CLI/Scripting (v1.5.0) |
| **Transmit** | Raw speed, macOS polish | LOW: Already fast |
| **ForkLift** | Complete file manager | LOW: Different focus |

---

## Prioritized Roadmap Based on Analysis

### v1.2.7 - COMPLETED
1. ✅ **MEGA.nz Integration** - 20GB free encrypted storage (UNIQUE - no competitor has this)
2. ✅ **SFTP Support** - Password and SSH key authentication
3. ✅ **UI Consistency** - Protocol icons standardized across app

### v1.2.8 - RELEASED
1. ✅ **Overwrite Confirmation** - Like FileZilla/Cyberduck/WinSCP
2. ✅ **Properties/Checksum** - File metadata and MD5/SHA-256
3. ✅ **Compress/Archive** - ZIP creation and extraction
4. ✅ **51 Languages** - Surpassed FileZilla (47 languages)!
5. ✅ **Same-panel Drag & Drop** - Move files to folders
6. ✅ **Activity Log Move Tracking** - File operations fully logged

### v1.2.9 - RELEASED
1. ✅ **Privacy & Security Settings** - Analytics opt-in, data protection info
2. ✅ **51 Languages at 100%** - All 434 keys translated in all languages
3. ✅ **Back Button Fix** - Proper session restoration

### v1.3.0 - RELEASED
1. ✅ **7z Archive Support** - LZMA2 compression + AES-256 encrypted extraction
2. ✅ **Anonymous Analytics** - Aptabase integration (opt-in, privacy-first)

### v1.3.1 - RELEASED
1. ✅ **Multi-Format Archives** - TAR, GZ, XZ, BZ2 compress/extract
2. ✅ **Keyboard Shortcuts** - F2 rename, Del delete, Ctrl+C/V copy-paste
3. ✅ **Archive Context Submenu** - Compress As (ZIP/7z/TAR/GZ/XZ/BZ2), Extract Here/To Folder
4. ✅ **UX Overhaul** - Context menus, drag & drop improvements

### v1.3.2 - RELEASED (Security Hotfix)
1. ✅ **Secure Credential Storage** - OS Keyring (macOS/Windows/Linux) + AES-256-GCM vault fallback
2. ✅ **Argon2id Key Derivation** - 64MB/3iter/4lanes for master password vault
3. ✅ **Permission Hardening** - 0o600 files, 0o700 directories

### v1.3.3 - RELEASED (Critical Security Fix)
1. ✅ **OS Keyring Fix** - Added `linux-native` backend to keyring crate (was silently no-op)
2. ✅ **Removed Migration System** - Broken migration deleted passwords before confirming keyring; removed entirely
3. ✅ **Session Tabs Fix** - FTP/FTPS Quick Connect now creates session tabs correctly
4. ✅ **Update Notification Fix** - Toast dismiss independent from status bar badge, removed pulse animation
5. ✅ **AeroCloud Sync Notifications** - Sync results now logged in Activity Log
6. ✅ **Credential Store Only** - No plaintext password fallback in localStorage

### v1.3.4 - Security Hardening (In Development)
1. ✅ **SFTP Host Key Verification** - TOFU with `~/.ssh/known_hosts`, rejects on key mismatch (MITM protection)
2. ✅ **OAuth2 Ephemeral Port** - OS-assigned random port for callback server, prevents local token interception
3. ✅ **FTP Insecure Warning** - Red "Insecure" badge + warning banner recommending FTPS/SFTP
4. ✅ **SECURITY.md Overhaul** - Full security architecture documentation
5. ✅ **CI Release Notes from CHANGELOG** - Automated extraction for GitHub Releases
6. ✅ **i18n Sync** - 51 languages synchronized, Italian contextMenu translated

### v1.4.0 - Next Major Release
1. **7z AES-256 Write** - Password-protected 7z creation via p7zip sidecar
2. **RAR Extraction** - Via p7zip CLI (GPL-safe, no libunrar)
3. **Bandwidth Throttling** - Like FileZilla

### v1.5.0 - AeroVault + Scripting
1. **AeroVault** - Virtual encrypted location (Argon2id + AES-256-GCM per file)
2. **CLI/Scripting** - Like WinSCP
3. **Azure Blob Storage** - Already in Cyberduck

### v1.7.0 - Cryptomator
1. **Cryptomator Import/Export** - Cyberduck's premium feature

---

## Market Positioning

```
                    CLOUD INTEGRATION
                          ▲
                          │
         Cyberduck ●      │      ● AeroFTP
                          │        (Future)
    ──────────────────────┼──────────────────────► PRO FEATURES
         FileZilla ●      │      ● AeroFTP
                          │        (Current)
              WinSCP ●    │
                          │
                    TRADITIONAL FTP
```

**AeroFTP Target Position:** Upper-right quadrant
- Maximum cloud integration (like Cyberduck)
- Maximum pro features (editor, terminal, AI)
- Modern UX with legacy protocol support

---

## Sources

- [FileZilla Features](https://filezilla-project.org/client_features.php)
- [Cyberduck Official](https://cyberduck.io/)
- [WinSCP Features](https://winscp.net/eng/docs/features)
- [Top FTP Clients 2025](https://www.cotocus.com/blog/top-10-ftp-clients-tools-in-2025-features-pros-cons-comparison/)
- [Best SFTP Clients 2025](https://sftptogo.com/blog/best-sftp-clients-of-2025-secure-fast-file-transfers/)
- [Mountain Duck 5 Announcement](https://blog.cyberduck.io/2025/08/19/mountain-duck-5/)

---

*This document is maintained as part of AeroFTP strategic planning.*
