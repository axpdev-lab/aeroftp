# AeroFTP Competitor Analysis

> Last Updated: 4 February 2026
> Version: v1.8.0

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
| FTP | Yes | Yes | Yes | Yes | Yes | Yes |
| FTPS (TLS) | Yes | Yes | Yes | Yes | Yes | Yes |
| SFTP | Yes | Yes | Yes | Yes | Yes | Yes |
| WebDAV | Yes | No | Yes | Yes | Yes | Yes |
| S3-compatible | Yes | No | Yes | Yes | Yes | Yes |

### Cloud Storage Integration

| Provider | AeroFTP | FileZilla | Cyberduck | WinSCP | Transmit | ForkLift |
|----------|---------|-----------|-----------|--------|----------|----------|
| Google Drive | Yes | No | Yes | No | Yes | Yes |
| Dropbox | Yes | No | Yes | No | Yes | Yes |
| OneDrive | Yes | No | Yes | No | Yes | Yes |
| **MEGA.nz** | **Yes** | No | No | No | No | No |
| Backblaze B2 | Yes (S3 preset) | No | Yes | No | Yes | No |
| Cloudflare R2 | Yes (S3 preset) | No | No | No | No | No |
| Storj | Yes (S3 preset) | No | No | No | No | No |
| **Box** | **Yes (Beta)** | No | Yes | No | No | No |
| **pCloud** | **Yes (Beta)** | No | No | No | No | No |
| **Filen** | **Yes (Beta)** | No | No | No | No | No |
| Azure Blob | **Yes (Beta)** | No | Yes | No | Yes | No |

### User Interface

| Feature | AeroFTP | FileZilla | Cyberduck | WinSCP | Transmit | ForkLift |
|---------|---------|-----------|-----------|--------|----------|----------|
| Dual-pane | Yes | Yes | No | Yes | No | Yes |
| Dark mode | Yes | No | Yes | No | Yes | Yes |
| Multi-tab | Yes | Yes | Yes | Yes | Yes | Yes |
| Thumbnails | Yes | No | No | No | Yes | Yes |
| Grid/List view | Yes | No | Yes | No | Yes | Yes |
| Modern UI | Yes | No | Yes | No | Yes | Yes |

### Pro Features

| Feature | AeroFTP | FileZilla | Cyberduck | WinSCP | Transmit | ForkLift |
|---------|---------|-----------|-----------|--------|----------|----------|
| Code Editor | Yes (Monaco) | No | No | Yes (Basic) | No | No |
| Terminal | Yes | No | No | Yes (PuTTY) | No | No |
| AI Assistant | **Yes (Pro)** | No | No | No | No | No |
| Media Player | Yes | No | No | No | No | Quick Look |
| Activity Log | Yes | Yes | Yes | Yes | No | No |
| Remote Search | Yes (all 13) | No | Yes | No | No | No |
| File Versions | Yes (5 providers) | No | Yes | No | No | No |
| File Locking | Yes (WebDAV) | No | Yes | No | No | No |
| Batch Rename | **Yes (v1.8.0)** | No | No | Yes | Yes | Yes |
| Inline Rename (F2) | **Yes (v1.8.0)** | Yes | Yes | Yes | Yes | Yes |

### Sync & Automation

| Feature | AeroFTP | FileZilla | Cyberduck | WinSCP | Transmit | ForkLift |
|---------|---------|-----------|-----------|--------|----------|----------|
| Personal Cloud | Yes (AeroCloud) | No | No | No | No | No |
| Background Sync | Yes (Tray) | No | No | No | No | No |
| Folder Sync | Yes (all 13 protocols) | Yes | No | Yes | Yes | Yes |
| Smart Sync | **Yes (v1.8.0)** | No | No | Yes | Yes | No |
| Sync Index Cache | Yes | No | No | No | No | No |
| Storage Quota | Yes (9 providers) | No | Yes | No | Yes | No |
| Scripting | Planned | No | No | Yes | No | No |
| Queue Management | Yes | Yes | Yes | Yes | Yes | Yes |

### Security

| Feature | AeroFTP | FileZilla | Cyberduck | WinSCP | Transmit | ForkLift |
|---------|---------|-----------|-----------|--------|----------|----------|
| Cryptomator | **Yes (v1.7.0)** | No | Yes | No | No | No |
| Share Links | Yes | No | Yes | No | No | No |
| Keychain/Keyring | Yes | Yes | Yes | Yes | Yes | Yes |
| Encrypted Vault (AES-256-GCM) | Yes | No | No | No | No | No |
| Argon2id Key Derivation | Yes | No | No | No | No | No |
| SFTP Host Key Verification | Yes (TOFU) | Yes | Yes | Yes | Yes | Yes |
| OAuth2 PKCE Flow | Yes | No | Yes | No | Yes | No |
| Ephemeral OAuth Port | Yes | No | No | No | No | No |
| FTP Insecure Warning | Yes | No | No | No | No | No |
| Memory Zeroization | Yes | No | No | No | No | No |
| AI Keys in OS Keyring | Yes | N/A | N/A | N/A | N/A | N/A |
| 7z AES-256 Archives | Yes | No | No | No | No | No |
| ZIP AES-256 Archives | Yes | No | No | No | No | No |
| RAR Extraction | Yes | No | No | No | No | No |
| Archive Browser (in-app) | **Yes (v1.7.0)** | No | No | No | No | No |
| Selective Archive Extract | **Yes (v1.7.0)** | No | No | No | No | No |

### Advanced Protocol Features (v1.4.0)

| Feature | AeroFTP | FileZilla | Cyberduck | WinSCP | Transmit | ForkLift |
|---------|---------|-----------|-----------|--------|----------|----------|
| FTPS Explicit TLS | Yes | Yes | Yes | Yes | Yes | Yes |
| FTPS Implicit TLS | Yes | Yes | Yes | Yes | Yes | Yes |
| FTPS Cert Options | Yes | Yes | Yes | Yes | No | No |
| FTP MLSD/MLST | Yes | Yes | No | Yes | No | No |
| FTP Resume (REST) | Yes | Yes | No | Yes | Yes | No |
| S3 Multipart Upload | Yes | No | Yes | No | Yes | No |
| WebDAV Locking | Yes | No | Yes | No | No | No |
| Storage Quota Display | Yes | No | Yes | No | No | No |
| OneDrive Resumable | Yes | No | Yes | No | Yes | No |

### Distribution

| Feature | AeroFTP | FileZilla | Cyberduck | WinSCP | Transmit | ForkLift |
|---------|---------|-----------|-----------|--------|----------|----------|
| Snap | Yes | Yes | No | No | No | No |
| AppImage | Yes | No | No | No | No | No |
| Auto-Update | Yes | Yes | Yes | Yes | Yes | Yes |
| i18n Languages | **51** | 47 | ~10 | ~15 | ~5 | ~5 |

---

## AeroFTP Unique Selling Points

| Feature | Description |
|---------|-------------|
| **AeroCloud** | Transform any FTP into personal cloud with bidirectional sync |
| **13 Native Protocols + 30 Presets** | FTP, FTPS, SFTP, WebDAV, S3, Google Drive, Dropbox, OneDrive, MEGA, Box, pCloud, Azure Blob, Filen |
| **Filen E2E Support** | Only client besides Filen web app with native Filen E2E encryption support |
| **Monaco Editor** | VS Code engine for remote file editing |
| **AeroVault** | AES-256-GCM encrypted containers with Argon2id KDF for secure file storage |
| **Cryptomator Format 8** | Open, browse, decrypt, encrypt files in Cryptomator vaults (scrypt + AES-SIV + AES-GCM) |
| **AeroAgent AI Pro** | AI assistant with native function calling, streaming, 24 tools (local+remote edit, batch transfers), protocol expertise, 7 AI providers |
| **Modern Stack** | Rust backend + React frontend (performance + security) |
| **Tray Background Sync** | Continuous sync without main window |
| **Sync Index Cache** | Persistent cache for faster re-scans and true conflict detection |
| **Storage Quota Display** | Real-time used/total in status bar (9 providers) |
| **AES-256-GCM Vault** | Argon2id + AES-256-GCM vault when keyring unavailable |
| **Ephemeral OAuth Ports** | OS-assigned random port for callback |
| **Memory Zeroization** | Passwords cleared from memory via zeroize/secrecy |
| **Multi-Format Archives** | ZIP, 7z, TAR, GZ, XZ, BZ2, RAR (7 formats) with compression levels and password protection |
| **Archive Browser** | Browse archive contents in-app without extracting, selective single-file extraction |
| **AeroFile Mode** | Local-only file manager with resizable preview panel, image resolution display, and file info sidebar |
| **Workspace Export** | Auto-export Google Docs/Sheets/Slides to DOCX/XLSX/PPTX |
| **Change Tracking** | Delta sync foundation via Google Drive changes API |
| **Cross-Provider Search** | Remote search on all 13 providers (30 connection options with presets) |
| **File Versions** | Version history on Google Drive, Dropbox, OneDrive, Box, pCloud |

---

## Competitor Strengths (Gaps to Close)

| Competitor | Strength | Priority for AeroFTP |
|------------|----------|---------------------|
| **FileZilla** | SFTP native, 47 languages, stability | CLOSED (51 langs, SFTP done, MLSD done) |
| **Cyberduck** | Cryptomator encryption | LOW: Cryptomator matched (v1.7.0). Cloud providers matched (13 native). |
| **WinSCP** | Scripting/automation, PuTTY integration | MEDIUM: CLI/Scripting (v1.6.0) |
| **Transmit** | Raw speed, macOS polish | LOW: Already fast |
| **ForkLift** | Complete file manager | LOW: Different focus |

---

## Prioritized Roadmap

### Completed (v1.0.0 - v1.4.1)
- FTP/FTPS/SFTP/WebDAV/S3 protocols
- Google Drive/Dropbox/OneDrive/MEGA integration
- 51 languages, AeroCloud sync, Monaco editor, Terminal, AI
- Archive support (ZIP/7z/TAR/RAR with AES-256 password encryption dialog)
- Security: OS Keyring, AES-256-GCM vault, SFTP TOFU, OAuth2 PKCE, AI API keys in Keyring
- Cross-provider: search, quota, versions, thumbnails, permissions, locking
- FTPS: Full TLS support (explicit, implicit, cert verification options)
- FTP: MLSD/MLST (RFC 3659), resume transfers
- S3: multipart upload (>5MB), OneDrive: resumable upload
- ErrorBoundary, modularized hooks architecture
- 4 new native providers: Box, pCloud, Azure Blob, Filen (v1.5.0)
- FTP default changed to opportunistic TLS (v1.5.0)
- WebDAV directory detection fix for Koofr and other servers (v1.5.1)
- Provider keep-alive pings for all non-FTP providers (v1.5.1)
- Session tab and saved server drag-to-reorder (v1.5.1)
- Provider logos in saved servers, session tabs, and connection forms (v1.5.1)
- 4 new S3/WebDAV presets: Jianguoyun, InfiniCLOUD, Alibaba Cloud OSS, Tencent Cloud COS (v1.5.1)
- WebDAV presets promoted to Stable: Koofr, Jianguoyun, InfiniCLOUD (v1.5.1)
- Multi-protocol sync via StorageProvider trait (v1.5.2)
- Sync index cache for faster re-scans and true conflict detection (v1.5.3)
- Storage quota display in status bar for 9 providers (v1.5.3)
- OAuth session switching with keyring fallback (v1.5.3)
- FTP transfer retry with exponential backoff (v1.5.3)

### v1.5.3 - Done
- Sync index cache, storage quota display, native clipboard, cross-panel drag & drop
- Terminal themes/tabs/SSH shell, i18n expansion (108 keys), OAuth session fix

### v1.5.4 - Done
- In-app auto-updater with download progress, AppImage auto-install

### v1.6.0 - Done
- AeroAgent Pro: native function calling (SEC-002), streaming responses
- Provider-agnostic tools (14), chat history, cost tracking, context awareness, 122 i18n keys

### v1.7.0 - Done
- Encryption Block: AeroVault (AES-256 containers), archive browser (ZIP/7z/TAR/RAR)
- Selective extraction, Cryptomator format 8 support, CompressDialog with levels
- AeroFile mode (local-only file manager), resizable preview panel, Type column

### v1.8.0 - Done
- Smart Sync (3 intelligent modes), Batch Rename (4 modes), Inline Rename
- AeroVault v2 (AES-256-GCM-SIV + AES-KW + AES-SIV + Argon2id 128MiB + HMAC-SHA512 + ChaCha20 cascade)

### v1.9.0 - Planned
- AeroAgent Intelligence (vision, multi-step), CLI/Scripting foundation
- Remote vault open/save, Cryptomator vault creation

---

## Market Positioning

```
                    CLOUD INTEGRATION
                          |
         Cyberduck        |        AeroFTP
                          |        (v1.7.0)
    ----------------------+----------------------> PRO FEATURES
         FileZilla        |
                          |
              WinSCP      |
                          |
                    TRADITIONAL FTP
```

**AeroFTP Position:** Upper-right quadrant - maximum cloud integration + maximum pro features with modern UX.

---

*This document is maintained as part of AeroFTP strategic planning.*
