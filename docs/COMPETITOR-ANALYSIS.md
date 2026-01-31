# AeroFTP Competitor Analysis

> Last Updated: 31 January 2026
> Version: v1.4.0

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
| Backblaze B2 | Yes | No | Yes | No | Yes | No |
| Azure Blob | Planned | No | Yes | No | Yes | No |

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
| AI Assistant | Yes | No | No | No | No | No |
| Media Player | Yes | No | No | No | No | Quick Look |
| Activity Log | Yes | Yes | Yes | Yes | No | No |
| Remote Search | Yes (all 9) | No | Yes | No | No | No |
| File Versions | Yes (3 providers) | No | Yes | No | No | No |
| File Locking | Yes (WebDAV) | No | Yes | No | No | No |

### Sync & Automation

| Feature | AeroFTP | FileZilla | Cyberduck | WinSCP | Transmit | ForkLift |
|---------|---------|-----------|-----------|--------|----------|----------|
| Personal Cloud | Yes (AeroCloud) | No | No | No | No | No |
| Background Sync | Yes (Tray) | No | No | No | No | No |
| Folder Sync | Yes | Yes | No | Yes | Yes | Yes |
| Scripting | Planned | No | No | Yes | No | No |
| Queue Management | Yes | Yes | Yes | Yes | Yes | Yes |

### Security

| Feature | AeroFTP | FileZilla | Cyberduck | WinSCP | Transmit | ForkLift |
|---------|---------|-----------|-----------|--------|----------|----------|
| Cryptomator | Planned (v1.7) | No | Yes | No | No | No |
| Share Links | Yes | No | Yes | No | No | No |
| Keychain/Keyring | Yes | Yes | Yes | Yes | Yes | Yes |
| Encrypted Vault (AES-256-GCM) | Yes | No | No | No | No | No |
| Argon2id Key Derivation | Yes | No | No | No | No | No |
| SFTP Host Key Verification | Yes (TOFU) | Yes | Yes | Yes | Yes | Yes |
| OAuth2 PKCE Flow | Yes | No | Yes | No | Yes | No |
| Ephemeral OAuth Port | Yes | No | No | No | No | No |
| FTP Insecure Warning | Yes | No | No | No | No | No |
| Memory Zeroization | Yes | No | No | No | No | No |
| 7z AES-256 Archives | Yes | No | No | No | No | No |
| ZIP AES-256 Archives | Yes | No | No | No | No | No |
| RAR Extraction | Yes | No | No | No | No | No |

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
| **MEGA.nz Support** | Only client with native MEGA integration (20GB free E2E storage) |
| **Monaco Editor** | VS Code engine for remote file editing |
| **AeroAgent AI** | AI assistant for commands and file analysis |
| **Modern Stack** | Rust backend + React frontend (performance + security) |
| **Tray Background Sync** | Continuous sync without main window |
| **AES-256-GCM Vault** | Argon2id + AES-256-GCM vault when keyring unavailable |
| **Ephemeral OAuth Ports** | OS-assigned random port for callback |
| **Memory Zeroization** | Passwords cleared from memory via zeroize/secrecy |
| **Multi-Format Archives** | ZIP, 7z, TAR, GZ, XZ, BZ2, RAR (7 formats) |
| **Cross-Provider Search** | Remote search on all 9 providers |
| **File Versions** | Version history on Google Drive, Dropbox, OneDrive |

---

## Competitor Strengths (Gaps to Close)

| Competitor | Strength | Priority for AeroFTP |
|------------|----------|---------------------|
| **FileZilla** | SFTP native, 47 languages, stability | CLOSED (51 langs, SFTP done, MLSD done) |
| **Cyberduck** | Cryptomator encryption, more clouds | MEDIUM: Cryptomator (v1.7.0) |
| **WinSCP** | Scripting/automation, PuTTY integration | MEDIUM: CLI/Scripting (v1.5.0) |
| **Transmit** | Raw speed, macOS polish | LOW: Already fast |
| **ForkLift** | Complete file manager | LOW: Different focus |

---

## Prioritized Roadmap

### Completed (v1.0.0 - v1.4.0)
- FTP/FTPS/SFTP/WebDAV/S3 protocols
- Google Drive/Dropbox/OneDrive/MEGA integration
- 51 languages, AeroCloud sync, Monaco editor, Terminal, AI
- Archive support (ZIP/7z/TAR/RAR with encryption)
- Security: OS Keyring, AES-256-GCM vault, SFTP TOFU, OAuth2 PKCE
- Cross-provider: search, quota, versions, thumbnails, permissions, locking
- FTPS: Full TLS support (explicit, implicit, cert verification options)
- FTP: MLSD/MLST (RFC 3659), resume transfers
- S3: multipart upload (>5MB), OneDrive: resumable upload
- Dependencies: russh 0.57, reqwest 0.13, quick-xml 0.39

### v1.5.0 - Planned
- AeroVault (encrypted virtual location)
- CLI/Scripting
- Azure Blob Storage

### v1.7.0 - Planned
- Cryptomator Import/Export

---

## Market Positioning

```
                    CLOUD INTEGRATION
                          |
         Cyberduck        |        AeroFTP
                          |        (v1.4.0)
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
