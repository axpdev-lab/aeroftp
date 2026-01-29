# AeroFTP Protocol Features Matrix

> Last Updated: 29 January 2026
> Version: v1.3.4-dev (Security Hardening)

---

## Protocol Security Matrix

### Connection Security by Protocol

| Protocol | Encryption | Auth Method | Credential Storage | Host Verification |
|----------|-----------|-------------|-------------------|-------------------|
| **FTP** | None | Password | OS Keyring / Vault | N/A |
| **FTPS** | TLS/SSL (Implicit) | Password | OS Keyring / Vault | TLS Certificate |
| **SFTP** | SSH | Password / SSH Key | OS Keyring / Vault | TOFU + known_hosts |
| **WebDAV** | HTTPS | Password | OS Keyring / Vault | TLS Certificate |
| **S3** | HTTPS | Access Key + Secret | OS Keyring / Vault | TLS Certificate |
| **Google Drive** | HTTPS | OAuth2 PKCE | OS Keyring / Vault | TLS + CSRF State |
| **Dropbox** | HTTPS | OAuth2 PKCE | OS Keyring / Vault | TLS + CSRF State |
| **OneDrive** | HTTPS | OAuth2 PKCE | OS Keyring / Vault | TLS + CSRF State |
| **MEGA.nz** | Client-side AES | Password (MEGAcmd) | secrecy (zero-on-drop) | E2E Encrypted |

### Security Features by Protocol

| Feature | FTP | FTPS | SFTP | WebDAV | S3 | OAuth Providers | MEGA |
|---------|-----|------|------|--------|-----|-----------------|------|
| Insecure Warning | Yes | - | - | - | - | - | - |
| TLS/SSL | No | Yes | - | Yes | Yes | Yes | - |
| SSH Tunnel | - | - | Yes | - | - | - | - |
| Host Key Check | - | - | TOFU | - | - | - | - |
| PKCE Flow | - | - | - | - | - | Yes | - |
| Ephemeral Port | - | - | - | - | - | Yes | - |
| E2E Encryption | - | - | - | - | - | - | Yes |
| Memory Zeroize | Yes | Yes | Yes | Yes | Yes | Yes | Yes |

---

## Share Link Support

### Current Implementation Status

| Protocol | Share Link Support | Implementation | Notes |
|----------|-------------------|----------------|-------|
| **FTP** | Via AeroCloud | `generate_share_link` | Requires AeroCloud setup with `public_url_base` |
| **FTPS** | Via AeroCloud | `generate_share_link` | Same as FTP |
| **SFTP** | Via AeroCloud | `generate_share_link` | Same as FTP, new in v1.3.0 |
| **WebDAV** | Via AeroCloud | `generate_share_link` | No native support |
| **S3** | Native (Pre-signed URLs) | `provider_create_share_link` | 7-day expiry default |
| **Google Drive** | Native | `provider_create_share_link` | Permanent "anyone with link" |
| **Dropbox** | Native | `provider_create_share_link` | Uses shared_links API |
| **OneDrive** | Native | `provider_create_share_link` | "view" permission link |
| **MEGA.nz** | Not Available | N/A | API doesn't expose share links |

---

## File Operations Matrix

### Operation Support by Protocol

| Operation | FTP | FTPS | SFTP | WebDAV | S3 | Google Drive | Dropbox | OneDrive | MEGA |
|-----------|-----|------|------|--------|-----|--------------|---------|----------|------|
| List | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Upload | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Download | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Delete | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Rename | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Move | Yes | Yes | Yes | Yes | Yes* | Yes | Yes | Yes | Yes |
| Copy | Yes | Yes | Yes | Yes* | Yes* | Yes | Yes | Yes | Yes |
| Mkdir | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Chmod | Yes | Yes | Yes | No | No | No | No | No | No |
| Stat | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Share Link | AeroCloud | AeroCloud | AeroCloud | AeroCloud | Yes | Yes | Yes | Yes | No |
| Sync | AeroCloud | AeroCloud | AeroCloud | No | No | No | No | No | No |

*Note: S3/WebDAV copy/move is implemented as copy+delete (no native server-side operation)*

---

## Archive Support Matrix

### Compression Formats (v1.3.1+)

| Format | Compress | Extract | Encryption | Backend |
|--------|----------|---------|------------|---------|
| **ZIP** | Yes | Yes | Planned (v1.4.0) | `zip` crate v7.2 (Deflate level 6) |
| **7z** | Yes | Yes | AES-256 read (write v1.4.0) | `sevenz-rust` v0.6 (LZMA2) |
| **TAR** | Yes | Yes | No | `tar` crate v0.4 |
| **TAR.GZ** | Yes | Yes | No | `tar` + `flate2` v1.0 |
| **TAR.XZ** | Yes | Yes | No | `tar` + `xz2` v0.1 |
| **TAR.BZ2** | Yes | Yes | No | `tar` + `bzip2` v0.6 |
| **RAR** | No | Planned | - | Planned via p7zip CLI |

### Context Menu Integration

| Action | Keyboard | Available When |
|--------|----------|----------------|
| Compress to ZIP | - | Single/multi file selection |
| Compress to 7z | - | Single/multi file selection |
| Compress to TAR family | - | Single/multi file selection |
| Extract Here | - | Archive file selected |
| Extract to Folder | - | Archive file selected |
| Password prompt | - | Encrypted 7z detected |

---

## Competitor Comparison: File Operations

### Context Menu Operations

| Operation | AeroFTP | FileZilla | Cyberduck | WinSCP | Transmit |
|-----------|---------|-----------|-----------|--------|----------|
| Download | Yes | Yes | Yes | Yes | Yes |
| Upload | Yes | Yes | Yes | Yes | Yes |
| Rename (F2) | Yes | Yes | Yes | Yes | Yes |
| Delete (Del) | Yes | Yes | Yes | Yes | Yes |
| New Folder | Yes | Yes | Yes | Yes | Yes |
| Copy Path | Yes | No | Yes | Yes | Yes |
| Copy FTP URL | Yes | No | Yes | Yes | No |
| Open With | No | No | Yes | Yes | Yes |
| Preview | Yes | No | Yes | No | Yes |
| Edit | Yes (Monaco) | Yes (External) | Yes (External) | Yes (Internal) | Yes (External) |
| Share Link | Yes | No | Yes | No | No |
| Properties | Yes | Yes | Yes | Yes | Yes |
| Compress | Yes (6 formats) | No | Yes | Yes | Yes |
| Checksum | Yes | No | Yes | Yes | No |
| Overwrite Dialog | Yes | Yes | Yes | Yes | Yes |

### Keyboard Shortcuts (v1.3.1+)

| Shortcut | Action |
|----------|--------|
| F2 | Rename selected file |
| Delete | Delete selected file |
| Ctrl+C | Copy file |
| Ctrl+V | Paste file |
| Ctrl+A | Select all |

---

## Protocol-Specific Limitations

### FTP/FTPS
- No native move (uses rename)
- Limited metadata (no creation time)
- Connection keep-alive required
- **FTP is unencrypted** - visual warning shown in UI (v1.3.4)

### SFTP (v1.3.0+)
- SSH key authentication (id_rsa, id_ed25519, encrypted keys)
- Chmod support (unlike cloud providers)
- Full Unix permissions
- **Host key verification** via `~/.ssh/known_hosts` with TOFU (v1.3.4)
- CVE-2025-54804 resolved (upgraded to russh v0.54.5)

### WebDAV
- No chmod support
- PROPFIND for metadata
- Some servers have limited MOVE support

### S3
- No native move (copy+delete)
- Pre-signed URLs for sharing (expiry)
- No real directories (prefix-based)

### OAuth Providers (Google/Dropbox/OneDrive)
- Token refresh via PKCE with SHA-256 code challenge (v1.2.8+)
- OAuth2 callback on ephemeral port (v1.3.4)
- Rate limits apply
- No chmod support
- Share link permissions vary

### MEGA
- Client-side AES encryption (zero-knowledge)
- No native share link API exposed
- Large file upload chunking required
- Password protected via `secrecy` crate (zero-on-drop)

---

## Credential Storage Architecture (v1.3.2+)

### Storage Layers

| Layer | Method | When Used |
|-------|--------|-----------|
| **Primary** | OS Keyring (gnome-keyring / macOS Keychain / Windows Credential Manager) | Always attempted first |
| **Fallback** | AES-256-GCM encrypted vault (`~/.config/aeroftp/vault.db`) | When keyring unavailable |
| **OAuth Tokens** | OS Keyring or vault | Stored after OAuth2 flow |
| **MEGA** | secrecy crate (zero-on-drop) | In-memory only during session |

### Key Derivation (Vault)

| Parameter | Value |
|-----------|-------|
| Algorithm | Argon2id |
| Memory | 64 MB |
| Iterations | 3 |
| Parallelism | 4 threads |
| Output | 256-bit key |
| Nonce | 12 bytes random per entry |

### File Permissions

| Path | Permission | Description |
|------|-----------|-------------|
| `~/.config/aeroftp/` | 0700 | Config directory |
| `vault.db` | 0600 | Encrypted credentials vault |
| OAuth token files | 0600 | Fallback token storage |
| `~/.ssh/known_hosts` | 0600 | SFTP host keys (created by AeroFTP if needed) |

---

## Recommendations

### Completed

| Version | Feature | Status |
|---------|---------|--------|
| v1.2.8 | Properties Dialog, Compress/Archive, Checksum, Overwrite, Drag & Drop | Done |
| v1.3.0 | SFTP Integration, 7z Archives, Analytics | Done |
| v1.3.1 | Multi-format TAR, Keyboard Shortcuts, Context Submenus | Done |
| v1.3.2 | Secure Credential Storage, Argon2id Vault, Permission Hardening | Done |
| v1.3.3 | OS Keyring Fix (Linux), Migration Removal, Session Tabs Fix | Done |
| v1.3.4 | SFTP Host Key Verification, Ephemeral OAuth Port, FTP Warning | Done |

### Planned

| Version | Feature |
|---------|---------|
| v1.4.0 | 7z AES-256 Write, RAR Extraction, Bandwidth Throttling |
| v1.5.0 | AeroVault (encrypted virtual location), CLI/Scripting, Azure Blob |
| v1.7.0 | Cryptomator Import/Export |

---

*This document is maintained as part of AeroFTP protocol documentation.*
