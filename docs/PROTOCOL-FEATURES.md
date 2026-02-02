# AeroFTP Protocol Features Matrix

> Last Updated: 2 February 2026
> Version: v1.5.3 (Sync Index Cache + Storage Quota + FTP Retry)

---

## Protocol Security Matrix

### Connection Security by Protocol

| Protocol | Encryption | Auth Method | Credential Storage | Host Verification |
|----------|-----------|-------------|-------------------|-------------------|
| **FTP** | None | Password | OS Keyring / Vault | N/A |
| **FTPS** | TLS/SSL (Explicit/Implicit) | Password | OS Keyring / Vault | TLS Certificate |
| **SFTP** | SSH | Password / SSH Key | OS Keyring / Vault | TOFU + known_hosts |
| **WebDAV** | HTTPS | Password | OS Keyring / Vault | TLS Certificate |
| **S3** | HTTPS | Access Key + Secret | OS Keyring / Vault | TLS Certificate |
| **Google Drive** | HTTPS | OAuth2 PKCE | OS Keyring / Vault | TLS + CSRF State |
| **Dropbox** | HTTPS | OAuth2 PKCE | OS Keyring / Vault | TLS + CSRF State |
| **OneDrive** | HTTPS | OAuth2 PKCE | OS Keyring / Vault | TLS + CSRF State |
| **MEGA.nz** | Client-side AES | Password (MEGAcmd) | secrecy (zero-on-drop) | E2E Encrypted |
| **Box** | HTTPS | OAuth2 PKCE | OS Keyring / Vault | TLS + CSRF State |
| **pCloud** | HTTPS | OAuth2 PKCE | OS Keyring / Vault | TLS + CSRF State |
| **Azure Blob** | HTTPS | Shared Key HMAC / SAS | OS Keyring / Vault | TLS Certificate |
| **Filen** | Client-side AES-256-GCM | Password (PBKDF2) | secrecy (zero-on-drop) | E2E Encrypted |

### Security Features by Protocol

| Feature | FTP | FTPS | SFTP | WebDAV | S3 | OAuth Providers | MEGA | Box | pCloud | Azure | Filen |
|---------|-----|------|------|--------|-----|-----------------|------|-----|--------|-------|-------|
| Insecure Warning | Yes | - | - | - | - | - | - | - | - | - | - |
| TLS/SSL | No | Yes | - | Yes | Yes | Yes | - | Yes | Yes | Yes | - |
| SSH Tunnel | - | - | Yes | - | - | - | - | - | - | - | - |
| Host Key Check | - | - | TOFU | - | - | - | - | - | - | - | - |
| PKCE Flow | - | - | - | - | - | Yes | - | Yes | Yes | - | - |
| Ephemeral Port | - | - | - | - | - | Yes | - | Yes | Yes | - | - |
| E2E Encryption | - | - | - | - | - | - | Yes | - | - | - | Yes |
| Memory Zeroize | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |

---

## File Operations Matrix

### Core Operations

| Operation | FTP | FTPS | SFTP | WebDAV | S3 | Google Drive | Dropbox | OneDrive | MEGA | Box | pCloud | Azure | Filen |
|-----------|-----|------|------|--------|-----|--------------|---------|----------|------|-----|--------|-------|-------|
| List | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Upload | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Download | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Delete | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Rename | Yes | Yes | Yes | Yes | Yes* | Yes | Yes | Yes | Yes | Yes | Yes | Yes** | Yes |
| Mkdir | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Chmod | Yes | Yes | Yes | No | No | No | No | No | No | No | No | No | No |
| Stat | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Share Link | AeroCloud | AeroCloud | AeroCloud | AeroCloud | Yes | Yes | Yes | Yes | Yes | Yes | Yes | No | Yes |

*S3 rename = copy+delete
**Azure rename = copy+delete

### Advanced Operations (v1.4.0)

| Operation | FTP | FTPS | SFTP | WebDAV | S3 | GDrive | Dropbox | OneDrive | MEGA | Box | pCloud | Azure | Filen |
|-----------|-----|------|------|--------|-----|--------|---------|----------|------|-----|--------|-------|-------|
| **Server Copy** | - | - | - | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | - | - |
| **Remote Search** | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| **Storage Quota** | - | - | Yes | Yes | - | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| **File Versions** | - | - | - | - | - | Yes | Yes | Yes | - | Yes | Yes | - | - |
| **Thumbnails** | - | - | - | - | - | Yes | Yes | Yes | - | Yes | Yes | - | - |
| **Permissions** | - | - | - | - | - | Yes | - | Yes | - | - | - | - | - |
| **Locking** | - | - | - | Yes | - | - | - | - | - | - | - | - | - |
| **Resume Transfer** | Yes | Yes | - | - | - | - | - | Yes | - | - | - | - | - |
| **Resumable Upload** | - | - | - | - | Yes | Yes | - | Yes | - | - | - | - | - |
| **Workspace Export** | - | - | - | - | - | Yes | - | - | - | - | - | - | - |
| **Change Tracking** | - | - | - | - | - | Yes | - | - | - | - | - | - | - |
| **MLSD/MLST** | Yes | Yes | - | - | - | - | - | - | - | - | - | - | - |
| **Speed Limit** | - | - | - | - | - | - | - | - | Yes | - | - | - | - |
| **Import Link** | - | - | - | - | - | - | - | - | Yes | - | - | - | - |
| **Multipart Upload** | - | - | - | - | Yes | - | - | - | - | - | - | - | - |

---

## Share Link Support

| Protocol | Share Link Support | Implementation | Notes |
|----------|-------------------|----------------|-------|
| **FTP/FTPS/SFTP** | Via AeroCloud | `generate_share_link` | Requires `public_url_base` config |
| **WebDAV** | Via AeroCloud | `generate_share_link` | No native support |
| **S3** | Native (Pre-signed URLs) | `provider_create_share_link` | 7-day expiry default |
| **Google Drive** | Native | `provider_create_share_link` | Permanent "anyone with link" |
| **Dropbox** | Native | `provider_create_share_link` | Uses shared_links API |
| **OneDrive** | Native | `provider_create_share_link` | "view" permission link |
| **MEGA.nz** | Native | `provider_create_share_link` | `mega-export` via MEGAcmd |
| **Box** | Native | `provider_create_share_link` | "open" access shared link |
| **pCloud** | Native | `provider_create_share_link` | Public link via `getfilepublink` |
| **Filen** | Native | `provider_create_share_link` | E2E encrypted share link |

---

## Archive Support Matrix (v1.4.0)

| Format | Compress | Extract | Encryption | Backend |
|--------|----------|---------|------------|---------|
| **ZIP** | Yes | Yes | AES-256 (read+write) | `zip` v7.2 |
| **7z** | Yes | Yes | AES-256 (read+write) | `sevenz-rust` v0.6 |
| **TAR** | Yes | Yes | No | `tar` v0.4 |
| **TAR.GZ** | Yes | Yes | No | `tar` + `flate2` v1.0 |
| **TAR.XZ** | Yes | Yes | No | `tar` + `xz2` v0.1 |
| **TAR.BZ2** | Yes | Yes | No | `tar` + `bzip2` v0.6 |
| **RAR** | No | Yes | Password support | `unrar` v0.5 |

---

## FTP Protocol Enhancements (v1.4.0)

### MLSD/MLST (RFC 3659)
- **FEAT detection**: Server capabilities checked on connect
- **MLSD listings**: Machine-readable format preferred over LIST
- **MLST stat**: Single-file info without listing parent directory
- **Automatic fallback**: Falls back to LIST when MLSD not supported
- **Fact parsing**: type, size, modify, unix.mode, unix.owner/group, perm

### Resume Transfers (REST)
- **resume_download**: REST offset + RETR for partial downloads
- **resume_upload**: APPE (append) for partial uploads

### FTPS TLS Encryption (v1.4.0)
- **Explicit TLS (AUTH TLS)**: Upgrades plain connection on port 21 via `into_secure()`
- **Implicit TLS**: Direct TLS connection on port 990
- **Explicit if available**: Attempts AUTH TLS, falls back to plain FTP if unsupported
- **Certificate verification**: Configurable per-connection (accept self-signed certs)
- **Backend**: suppaftp v8 with `tokio-async-native-tls` feature

**Default changed in v1.5.0**: FTP now defaults to 'explicit_if_available' (TLS opportunistic) instead of plain FTP

---

## Directory Sync (v1.5.2)

Bidirectional directory synchronization compares local and remote files by timestamp and size, then uploads/downloads as needed.

### Sync Support by Protocol

| Protocol | Compare | Upload | Download | Progress | Notes |
|----------|---------|--------|----------|----------|-------|
| **FTP** | Yes | Yes | Yes | Yes | Via `ftp_manager` (legacy path) |
| **FTPS** | Yes | Yes | Yes | Yes | Via `ftp_manager` (legacy path) |
| **SFTP** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **WebDAV** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **S3** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **Google Drive** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **Dropbox** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **OneDrive** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **MEGA** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **Box** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **pCloud** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **Azure Blob** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **Filen** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |

### Sync Modes
- **Remote → Local**: Download newer remote files
- **Local → Remote**: Upload newer local files
- **Bidirectional**: Sync in both directions (default)

### Comparison Options
- Timestamp comparison (2-second tolerance for filesystem differences)
- File size comparison
- Configurable exclude patterns (`node_modules`, `.git`, `.DS_Store`, etc.)

### Sync Index Cache (v1.5.3)
Persistent JSON index stored at `~/.config/aeroftp/sync-index/` enables:
- **True conflict detection**: Both sides changed since last sync → Conflict status
- **Faster re-scans**: Unchanged files detected via cached size/mtime without full comparison
- **Per-path-pair storage**: Stable filename generated from hash of local+remote path pair
- **Auto-save after sync**: Index updated with final file states after successful sync

### FTP Transfer Retry (v1.5.3)
- Automatic retry with exponential backoff (3 attempts, 500ms base delay)
- Targets "Data connection" errors specifically
- FTP-only (cloud providers handle retries internally)
- Inter-transfer delay increased to 350ms for server stability

---

## Provider Keep-Alive (v1.5.1)

All non-FTP providers receive periodic keep-alive pings to prevent connection timeouts during idle sessions. This applies to WebDAV, S3, Google Drive, Dropbox, OneDrive, MEGA, Box, pCloud, Azure Blob, and Filen.

---

## WebDAV Presets (v1.5.1)

| Preset | Status | Notes |
|--------|--------|-------|
| **Koofr** | Stable | EU-based, 10 GB free |
| **Jianguoyun** | Stable | China-based WebDAV |
| **InfiniCLOUD** | Stable | Japan-based, 20 GB free |
| **Nextcloud** | Beta | Self-hosted WebDAV |
| **ownCloud** | Beta | Self-hosted WebDAV |

---

## New Cloud Providers (v1.5.0)

### Box (Beta)
- OAuth2 PKCE via OAuth2Manager
- API: `https://api.box.com/2.0/`, upload: `https://upload.box.com/api/2.0/`
- ID-based file system (root folder = "0"), path→ID cache
- Share links, storage quota, file versions

### pCloud (Beta)
- OAuth2 via OAuth2Manager
- API: `https://api.pcloud.com/` (US) or `https://eapi.pcloud.com/` (EU)
- Path-based REST API (simplest of all providers)
- Share links, storage quota

### Azure Blob Storage (Beta)
- Shared Key HMAC-SHA256 or SAS token authentication
- API: `https://{account}.blob.core.windows.net/{container}/`
- Flat namespace with `/` delimiter (like S3)
- XML response parsing via quick-xml

### Filen (Beta)
- Zero-knowledge E2E encryption: PBKDF2(SHA512, 200k iterations) + AES-256-GCM
- All metadata and file content encrypted client-side
- Chunk-based upload/download (1MB chunks)
- API: `https://gateway.filen.io/`

---

## Credential Storage Architecture (v1.3.2+)

### Storage Layers

| Layer | Method | When Used |
|-------|--------|-----------|
| **Primary** | OS Keyring (gnome-keyring / macOS Keychain / Windows Credential Manager) | Always attempted first |
| **Fallback** | AES-256-GCM encrypted vault (`~/.config/aeroftp/vault.db`) | When keyring unavailable |
| **OAuth Tokens** | OS Keyring or vault | Stored after OAuth2 flow |
| **AI API Keys** | OS Keyring or vault | Migrated from localStorage (v1.4.1) |
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

---

## Release History

| Version | Feature | Status |
|---------|---------|--------|
| v1.2.8 | Properties Dialog, Compress/Archive, Checksum, Overwrite, Drag & Drop | Done |
| v1.3.0 | SFTP Integration, 7z Archives, Analytics | Done |
| v1.3.1 | Multi-format TAR, Keyboard Shortcuts, Context Submenus | Done |
| v1.3.2 | Secure Credential Storage, Argon2id Vault, Permission Hardening | Done |
| v1.3.3 | OS Keyring Fix (Linux), Migration Removal, Session Tabs Fix | Done |
| v1.3.4 | SFTP Host Key Verification, Ephemeral OAuth Port, FTP Warning | Done |
| v1.4.0 | Cross-provider search/quota/versions/thumbnails/permissions/locking, S3 multipart, FTP resume + MLSD, dep upgrades | Done |
| v1.4.1 | AI API keys → OS Keyring, ZIP/7z password dialog, ErrorBoundary, hook extractions, dead code cleanup | Done |
| v1.5.0 | 4 new providers (Box, pCloud, Azure, Filen), FTP TLS default, S3/WebDAV stable badges | Done |
| v1.5.1 | WebDAV directory fix, provider keep-alive, drag-to-reorder tabs/servers, 4 new presets (30 total), provider logos | Done |
| v1.5.2 | Multi-protocol sync, codebase audit, credential fix, SEC-001/SEC-004 fixes | Done |
| v1.5.3 | Sync index cache, storage quota display, OAuth session switching fix, FTP retry with backoff | Done |

### Planned

| Version | Feature |
|---------|---------|
| v1.6.0 | AeroAgent Pro, CLI/Scripting, AeroVault, oauth2 v5 |
| v1.7.0 | AeroAgent Intelligence, Terminal Pro |
| v1.8.0 | Cryptomator Import/Export |

---

*This document is maintained as part of AeroFTP protocol documentation.*
