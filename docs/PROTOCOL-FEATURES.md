# AeroFTP Protocol Features Matrix

> Last Updated: 26 February 2026
> Version: v2.7.0 (FileLu Native API — 19th Protocol)

---

## Protocol Security Matrix

### Connection Security by Protocol

| Protocol | Encryption | Auth Method | Credential Storage | Host Verification |
|----------|-----------|-------------|-------------------|-------------------|
| **FTP** | None | Password | Universal Vault | N/A |
| **FTPS** | TLS/SSL (Explicit/Implicit) | Password | Universal Vault | TLS Certificate |
| **SFTP** | SSH | Password / SSH Key | Universal Vault | TOFU + known_hosts |
| **WebDAV** | HTTPS | Password (Basic + Digest RFC 2617) | Universal Vault | TLS Certificate |
| **S3** | HTTPS | Access Key + Secret | Universal Vault | TLS Certificate |
| **Google Drive** | HTTPS | OAuth2 PKCE | Universal Vault | TLS + CSRF State |
| **Dropbox** | HTTPS | OAuth2 PKCE | Universal Vault | TLS + CSRF State |
| **OneDrive** | HTTPS | OAuth2 PKCE | Universal Vault | TLS + CSRF State |
| **MEGA.nz** | Client-side AES | Password (MEGAcmd) | secrecy (zero-on-drop) | E2E Encrypted |
| **Box** | HTTPS | OAuth2 PKCE | Universal Vault | TLS + CSRF State |
| **pCloud** | HTTPS | OAuth2 PKCE | Universal Vault | TLS + CSRF State |
| **Azure Blob** | HTTPS | Shared Key HMAC / SAS | Universal Vault | TLS Certificate |
| **4shared** | HTTPS | OAuth 1.0 (HMAC-SHA1) | Universal Vault | TLS Certificate |
| **Filen** | Client-side AES-256-GCM | Password (PBKDF2) | secrecy (zero-on-drop) | E2E Encrypted |
| **Zoho WorkDrive** | HTTPS | OAuth2 PKCE | Universal Vault | TLS + CSRF State |
| **Internxt Drive** | Client-side AES-256-CTR | Password (PBKDF2 + BIP39) | secrecy (zero-on-drop) | E2E Encrypted |
| **kDrive** | HTTPS | API Token (Bearer) | Universal Vault | TLS Certificate |
| **FileLu** | HTTPS | API Key | Universal Vault | TLS Certificate |

### Security Features by Protocol

| Feature | FTP | FTPS | SFTP | WebDAV | S3 | OAuth Providers | MEGA | Box | pCloud | Azure | 4shared | Filen | Internxt | kDrive | FileLu |
|---------|-----|------|------|--------|-----|-----------------|------|-----|--------|-------|---------|-------|----------|--------|--------|
| Insecure Warning | Yes | - | - | - | - | - | - | - | - | - | - | - | - | - | - |
| TLS/SSL | No | Yes | - | Yes | Yes | Yes | - | Yes | Yes | Yes | Yes | - | - | Yes | Yes |
| SSH Tunnel | - | - | Yes | - | - | - | - | - | - | - | - | - | - | - | - |
| Host Key Check | - | - | TOFU | - | - | - | - | - | - | - | - | - | - | - | - |
| PKCE Flow | - | - | - | - | - | Yes | - | Yes | Yes | - | - | - | - | - | - |
| Digest Auth (RFC 2617) | - | - | - | Yes | - | - | - | - | - | - | - | - | - | - | - |
| Ephemeral Port | - | - | - | - | - | Yes | - | Yes | Yes | - | Yes | - | - | - | - |
| OAuth 1.0 Flow | - | - | - | - | - | - | - | - | - | - | Yes | - | - | - | - |
| E2E Encryption | - | - | - | - | - | - | Yes | - | - | - | - | Yes | Yes | - | - |
| Memory Zeroize | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |

---

## File Operations Matrix

### Core Operations

| Operation | FTP | FTPS | SFTP | WebDAV | S3 | Google Drive | Dropbox | OneDrive | MEGA | Box | pCloud | Azure | 4shared | Filen | Zoho WD | Internxt | kDrive | FileLu |
|-----------|-----|------|------|--------|-----|--------------|---------|----------|------|-----|--------|-------|---------|-------|---------|----------|--------|--------|
| List | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Upload | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Download | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Delete | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Rename | Yes | Yes | Yes | Yes | Yes* | Yes | Yes | Yes | Yes | Yes | Yes | Yes** | Yes | Yes | Yes | Yes | Yes*** | Yes |
| Mkdir | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Chmod | Yes | Yes | Yes | No | No | No | No | No | No | No | No | No | No | No | No | No | No | No |
| Stat | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Share Link | AeroCloud | AeroCloud | AeroCloud | AeroCloud | Yes | Yes | Yes | Yes | Yes | Yes | Yes | No | No | Yes | Yes | Yes | No | Yes |

*S3 rename = copy+delete
**Azure rename = copy+delete
***kDrive rename = move to same parent with new name

### Advanced Operations (v1.4.0)

| Operation | FTP | FTPS | SFTP | WebDAV | S3 | GDrive | Dropbox | OneDrive | MEGA | Box | pCloud | Azure | 4shared | Filen | Zoho WD | Internxt | kDrive | FileLu |
|-----------|-----|------|------|--------|-----|--------|---------|----------|------|-----|--------|-------|---------|-------|---------|----------|--------|--------|
| **Server Copy** | - | - | - | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | - | - | - | Yes | - | Yes | Yes† |
| **Remote Search** | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| **Storage Quota** | - | - | Yes | Yes | - | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| **File Versions** | - | - | - | - | - | Yes | Yes | Yes | - | Yes | Yes | - | - | - | - | - | - | - |
| **Thumbnails** | - | - | - | - | - | Yes | Yes | Yes | - | Yes | Yes | - | - | - | - | - | - | - |
| **Permissions** | - | - | - | - | - | Yes | - | Yes | - | - | - | - | - | - | - | - | - | - |
| **Locking** | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - |
| **Resume Transfer** | Yes | Yes | - | - | - | - | - | Yes | - | - | - | - | - | - | - | - | - | - |
| **Resumable Upload** | - | - | - | - | Yes | Yes | - | Yes | - | - | - | - | - | - | - | - | - | - |
| **Workspace Export** | - | - | - | - | - | Yes | - | - | - | - | - | - | - | - | - | - | - | - |
| **Change Tracking** | - | - | - | - | - | Yes | - | - | - | - | - | - | - | - | - | - | - | - |
| **MLSD/MLST** | Yes | Yes | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - |
| **Speed Limit** | - | - | - | - | - | - | - | - | Yes | - | - | - | - | - | - | - | - | - |
| **Import Link** | - | - | - | - | - | - | - | - | Yes | - | - | - | - | - | - | - | - | - |
| **Multipart Upload** | - | - | - | - | Yes | - | - | - | - | - | - | - | - | - | - | - | - | - |
| **Remote URL Fetch** | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | Yes |
| **File Password** | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | Yes |
| **Privacy Toggle** | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | Yes |

†FileLu Server Copy = server-side clone (`filelu_clone_file`)

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
| **4shared** | — | — | Not yet implemented |
| **Filen** | Native | `provider_create_share_link` | E2E encrypted share link |
| **Zoho WorkDrive** | Native | `provider_create_share_link` | Team share link |
| **Internxt Drive** | Native | `provider_create_share_link` | E2E encrypted share link |
| **kDrive** | — | — | Not yet implemented |
| **FileLu** | Native | `provider_create_share_link` | Public share link via FileLu API |

---

## FileLu Special Features (v2.7.0)

FileLu exposes privacy and management features beyond generic file operations:

| Feature | API Endpoint | Tauri Command | Notes |
|---------|-------------|---------------|-------|
| **File Password** | `/file/set_password` | `filelu_set_file_password` | Set or remove password on any file |
| **Privacy Toggle** | `/file/only_me` | `filelu_set_file_privacy` | Toggle private (only-me) or public visibility |
| **Server Clone** | `/file/clone` | `filelu_clone_file` | Duplicate file server-side; returns share URL |
| **Folder Password** | `/folder/set_password` | `filelu_set_folder_password` | Requires `fld_token` from folder listing |
| **Folder Settings** | `/folder/setting` | `filelu_set_folder_settings` | FileDrop (anonymous uploads) + Public Folder |
| **List Trash** | `/files/deleted` | `filelu_list_deleted` | Returns deleted files with timestamps |
| **Restore File** | `/file/restore` | `filelu_restore_file` | Restore by `file_code` |
| **Restore Folder** | `/folder/restore` | `filelu_restore_folder` | Restore by `fld_id` |
| **Permanent Delete** | `/file/remove` | `filelu_permanent_delete` | Bypass trash, irrecoverable |
| **Remote URL Upload** | `/upload/url` | `filelu_remote_url_upload` | FileLu fetches file from URL server-side |

---

## Archive Support Matrix (v1.7.0)

| Format | Compress | Extract | Browse | Selective Extract | Encryption | Levels | Backend |
|--------|----------|---------|--------|-------------------|------------|--------|---------|
| **ZIP** | Yes | Yes | Yes | Yes | AES-256 (read+write) | Store/Fast/Normal/Max | `zip` v7.2 |
| **7z** | Yes | Yes | Yes | Yes | AES-256 (read+write) | Fast/Normal/Max | `sevenz-rust` v0.6 |
| **TAR** | Yes | Yes | Yes | Yes | No | — | `tar` v0.4 |
| **TAR.GZ** | Yes | Yes | Yes | Yes | No | Fast/Normal/Max | `tar` + `flate2` v1.0 |
| **TAR.XZ** | Yes | Yes | Yes | Yes | No | Fast/Normal/Max | `tar` + `xz2` v0.1 |
| **TAR.BZ2** | Yes | Yes | Yes | Yes | No | Fast/Normal/Max | `tar` + `bzip2` v0.6 |
| **RAR** | No | Yes | Yes | Yes | Password support | — | `unrar` v0.5 |

**Archive Browser** (v1.7.0): Browse archive contents in-app without extracting. Password dialog for encrypted ZIP/7z/RAR. Selective extraction of individual files.

**CompressDialog** (v1.7.0): Unified compression UI with format selection, compression levels, editable archive name, password protection (ZIP/7z), and file info display.

---

## Client-Side Encryption (v1.8.0)

### AeroVault v2 — Military-Grade Containers

| Component | Algorithm | RFC/Standard | Notes |
|-----------|-----------|--------------|-------|
| **Content encryption** | AES-256-GCM-SIV | RFC 8452 | Nonce misuse-resistant AEAD |
| **Key wrapping** | AES-256-KW | RFC 3394 | Integrity-protected key encapsulation |
| **Filename encryption** | AES-256-SIV | RFC 5297 | Deterministic, hides file names |
| **Key derivation** | Argon2id | IETF draft | 128 MiB / 4 iterations / 4 parallelism |
| **Header integrity** | HMAC-SHA512 | RFC 2104 | 512-bit MAC, detects tampering |
| **Cascade mode** | ChaCha20-Poly1305 | RFC 8439 | Optional double encryption |
| **Chunk size** | 64 KB | — | Per-chunk nonce + auth tag |

### AeroVault v2 vs Cryptomator

| Feature | AeroVault v2 | Cryptomator |
|---------|--------------|-------------|
| **Nonce misuse resistance** | Yes (GCM-SIV) | No (GCM) |
| **KDF memory** | 128 MiB | 64-128 MiB |
| **KDF algorithm** | Argon2id | scrypt |
| **Header integrity** | SHA-512 | SHA-256 |
| **Cascade encryption** | Optional | No |
| **Chunk size** | 64 KB | 32 KB |

### Cryptomator (Format 8) — Legacy Support

Accessible via folder context menu "Open as Cryptomator Vault":

| Component | Algorithm |
|-----------|-----------|
| Master key derivation | scrypt (N=2^15, r=8, p=1) |
| Key wrapping | AES-256-KW (RFC 3394) |
| Filename encryption | AES-SIV |
| Content encryption | AES-256-GCM (32KB chunks) |

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
| **4shared** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **Filen** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **Zoho WorkDrive** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **Internxt Drive** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **kDrive** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **FileLu** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |

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

All non-FTP providers receive periodic keep-alive pings to prevent connection timeouts during idle sessions. This applies to WebDAV, S3, Google Drive, Dropbox, OneDrive, MEGA, Box, pCloud, Azure Blob, 4shared, Filen, Zoho WorkDrive, Internxt Drive, kDrive, and FileLu.

---

## WebDAV Presets (v1.5.1)

| Preset | Status | Notes |
|--------|--------|-------|
| **Koofr** | Stable | EU-based, 10 GB free |
| **Jianguoyun** | Stable | China-based WebDAV |
| **InfiniCLOUD** | Stable | Japan-based, 20 GB free |
| **CloudMe** | Stable | Swedish, 3 GB free, Digest auth (auto-detected) |
| **Nextcloud** | Beta | Self-hosted WebDAV |
| **Seafile** | Stable | Self-hosted or cloud, WebDAV via `seafdav` endpoint |

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

### 4shared (v2.0.5)
- Native REST API v1.2 with OAuth 1.0 (HMAC-SHA1) authentication
- ID-based file system with folder/file caching
- API: `https://api.4shared.com/v1_2/`
- Upload: `https://upload.4shared.com/v1_2/files`
- 15 GB free storage, storage quota support

### Filen (Beta)
- Zero-knowledge E2E encryption: PBKDF2(SHA512, 200k iterations) + AES-256-GCM
- All metadata and file content encrypted client-side
- Chunk-based upload/download (1MB chunks)
- API: `https://gateway.filen.io/`

### Zoho WorkDrive (v2.4.0)
- OAuth2 PKCE with 8 regional endpoints (US/EU/IN/AU/JP/UK/CA/SA)
- Team-based storage with team ID auto-detection
- Share links, trash management, storage quota
- API: `https://www.zohoapis.{region}/workdrive/api/v1/`

### Internxt Drive (v2.6.0)
- Zero-knowledge E2E encryption: AES-256-CTR with BIP39 mnemonic key derivation
- Auth: PBKDF2-SHA1 + AES-CBC encrypted password + JWT
- Client-side file encryption/decryption, plainName metadata
- API: `https://drive.internxt.com/`

### Infomaniak kDrive (v2.6.0)
- Swiss-hosted cloud storage (GDPR + FADP compliant)
- Auth: Bearer API Token (generated from Infomaniak dashboard)
- ID-based file system, cursor-based pagination, server-side copy
- API: `https://api.infomaniak.com/`

### FileLu (v2.7.0)

- Privacy-focused cloud storage with 20 GB free
- Auth: API Key (generated from Account Settings → Developer API Key)
- Native REST API with full `StorageProvider` trait implementation
- FTP/FTPS/WebDAV/S3-compatible access also available as separate presets
- Special features: file/folder password, private/public toggle, server-side clone, trash management, remote URL upload
- API: `https://api.filelu.com/`

---

## AeroAgent AI (v1.6.0)

### AI Provider Support

| Provider | Native Tools | Streaming | Token Counting | Auth |
|----------|-------------|-----------|----------------|------|
| **Google Gemini** | Yes (`functionDeclarations`) | Yes (SSE) | Yes (`usageMetadata`) | API Key |
| **OpenAI** | Yes (`tools[]`) | Yes (SSE) | Yes (`usage`) | API Key |
| **Anthropic** | Yes (`tool_use`) | Yes (`content_block_delta`) | Yes (`usage`) | API Key |
| **xAI (Grok)** | Yes (OpenAI-compat) | Yes (SSE) | Yes | API Key |
| **OpenRouter** | Yes (OpenAI-compat) | Yes (SSE) | Yes | API Key |
| **Ollama** | No (text fallback) | Yes (NDJSON) | Yes (`eval_count`) | None |
| **Kimi (Moonshot)** | Yes (OpenAI-compat) | Yes (SSE) | Yes | API Key |
| **Qwen (Alibaba)** | Yes (OpenAI-compat) | Yes (SSE) | Yes | API Key |
| **DeepSeek** | Yes (OpenAI-compat) | Yes (SSE) | Yes | API Key |
| **Mistral** | Yes (OpenAI-compat) | Yes (SSE) | Yes | API Key |
| **Groq** | Yes (OpenAI-compat) | Yes (SSE) | Yes | API Key |
| **Perplexity** | No (text fallback) | Yes (SSE) | Yes | API Key |
| **Cohere** | Yes (OpenAI-compat) | Yes (SSE) | Yes | API Key |
| **Together AI** | Yes (OpenAI-compat) | Yes (SSE) | Yes | API Key |
| **AI21 Labs** | Yes (OpenAI-compat) | Yes (SSE) | Yes | API Key |
| **Cerebras** | Yes (OpenAI-compat) | Yes (SSE) | Yes | API Key |
| **SambaNova** | Yes (OpenAI-compat) | Yes (SSE) | Yes | API Key |
| **Fireworks AI** | Yes (OpenAI-compat) | Yes (SSE) | Yes | API Key |
| **Custom** | No (text fallback) | Yes (SSE) | Varies | API Key |

### AI Tool Support by Protocol

All 45 tools work identically across all 19 protocols via the `StorageProvider` trait:

| Tool | Danger | Description |
|------|--------|-------------|
| `remote_list` | Safe | List directory contents |
| `remote_read` | Safe | Read file content (5KB limit) |
| `remote_info` | Safe | Get file metadata |
| `remote_search` | Safe | Search files by pattern |
| `local_list` | Safe | List local directory |
| `local_read` | Safe | Read local file (5KB limit) |
| `local_search` | Safe | Search local files by pattern |
| `local_mkdir` | Medium | Create local directory |
| `local_write` | Medium | Write local text file |
| `local_rename` | Medium | Rename/move local file |
| `local_edit` | Medium | Find & replace in local file |
| `remote_edit` | Medium | Find & replace in remote file |
| `remote_download` | Medium | Download file to local |
| `remote_upload` | Medium | Upload file to remote |
| `upload_files` | Medium | Upload multiple files |
| `download_files` | Medium | Download multiple files |
| `remote_mkdir` | Medium | Create remote directory |
| `remote_rename` | Medium | Rename remote file |
| `sync_preview` | Medium | Preview directory sync |
| `archive_create` | Medium | Create archive |
| `archive_extract` | Medium | Extract archive |
| `rag_index` | Medium | Index directory for RAG search |
| `rag_search` | Medium | Full-text search over indexed files |
| `agent_memory_write` | Medium | Write to persistent agent memory |
| `remote_delete` | High | Delete remote file |
| `terminal_exec` | High | Execute terminal command |
| `local_delete` | High | Delete local file/directory |

### AI Features

| Feature | Status | Notes |
|---------|--------|-------|
| Native function calling | Done (v1.6.0) | OpenAI, Anthropic, Gemini; text fallback for Ollama |
| Streaming responses | Done (v1.6.0) | Incremental rendering via Tauri events |
| Chat history | Done (v1.6.0) | 50 conversations, 200 msgs each, persisted to disk |
| Cost tracking | Done (v1.6.0) | Per-message token count + cost estimate |
| Context awareness | Done (v1.7.0) | Provider, server host/port/user, path, selected files |
| Protocol expertise | Done (v1.7.0) | System prompt with all 14 provider configs, ports, auth |
| Styled tool display | Done (v1.7.0) | Inline chips with wrench icon replace raw TOOL/ARGS |
| Auto-routing | Done (v1.4.0) | Task-type detection routes to optimal model |
| Rate limiting | Done (v1.4.0) | 20 RPM per provider, frontend token bucket |
| Speech input | Done (v1.4.0) | Web Speech API |
| Multi-step autonomous tools | Done (v1.9.0) | Agent chains multiple tool calls without repeated prompts |
| Ollama auto-detection | Done (v1.9.0) | Discovers local Ollama instances and available models |
| Conversation export | Done (v1.9.0) | Export as Markdown or JSON |
| Monaco bidirectional sync | Done (v1.9.0) | Live two-way sync between editor and AI agent |
| Terminal command execution | Done (v1.9.0) | AI executes terminal commands with user approval |
| Streaming markdown renderer | Done (v2.0.0) | Incremental rendering with finalized segments (React.memo) |
| Code block actions | Done (v2.0.0) | Copy/Apply/Diff/Run buttons on AI code blocks |
| Thought visualization | Done (v2.0.0) | ThinkingBlock for Anthropic/OpenAI/Gemini reasoning |
| Prompt templates | Done (v2.0.0) | 15 built-in templates via `/` prefix, vault-persisted custom |
| Multi-file diff | Done (v2.0.0) | PR-style diff with per-file checkboxes |
| Cost budget tracking | Done (v2.0.0) | Per-provider monthly limits, vault-persisted |
| Chat search | Done (v2.0.0) | Ctrl+F with role filter and keyboard navigation |
| Anthropic prompt caching | Done (v2.0.0) | cache_control ephemeral, 90% read discount |
| OpenAI structured outputs | Done (v2.0.0) | strict: true for OpenAI/xAI/OpenRouter |
| Ollama model templates | Done (v2.0.0) | 8 family profiles with tailored prompts |
| Ollama pull from UI | Done (v2.0.0) | NDJSON streaming progress in AI Settings |
| Gemini code execution | Done (v2.0.0) | executableCode/codeExecutionResult parsing |
| Kimi web search | Done (v2.0.1) | `$web_search` builtin_function tool injection |
| Kimi context caching | Done (v2.0.1) | `/v1/caching` endpoint, cache_id passthrough |
| Kimi file analysis | Done (v2.0.1) | `/v1/files` upload, fileid:// references |
| Qwen thinking mode | Done (v2.0.1) | `enable_thinking` + `thinking_budget` parameters |
| Qwen web search | Done (v2.0.1) | `enable_search` + `search_options.search_strategy` |
| DeepSeek thinking mode | Done (v2.0.1) | `reasoning_content` streaming (shared with o3) |
| DeepSeek FIM completion | Done (v2.0.1) | `/beta/completions` with prompt + suffix |
| DeepSeek prefix completion | Done (v2.0.1) | `prefix: true` on last assistant message |

---

## AeroAgent Pro (v2.0.0)

### AI Provider Feature Matrix

| Feature | OpenAI | Anthropic | Google Gemini | xAI | OpenRouter | Ollama | Kimi | Qwen | DeepSeek | Custom |
|---------|--------|-----------|---------------|-----|------------|--------|------|------|----------|--------|
| Native Tool Calling | Yes | Yes | Yes | Yes | Yes | Text format | Yes | Yes | Yes | Yes |
| Streaming | SSE | SSE | SSE | SSE | SSE | NDJSON | SSE | SSE | SSE | SSE |
| Vision/Multimodal | Yes | Yes | Yes | Yes | Via model | Yes | Yes | Yes | N/A | Via model |
| Structured Outputs | **strict: true** | N/A | N/A | **strict: true** | **strict: true** | N/A | N/A | N/A | N/A | N/A |
| Prompt Caching | N/A | **Ephemeral** | **cachedContent** | N/A | N/A | N/A | **Context cache** | N/A | N/A | N/A |
| Extended Thinking | o3/o3-mini | Claude 3.5+ | Gemini 2.0+ | N/A | Via model | deepseek-r1 | N/A | **enable_thinking** | **reasoning_content** | N/A |
| Web Search | N/A | N/A | N/A | N/A | N/A | N/A | **$web_search** | **enable_search** | N/A | N/A |
| Code Execution | N/A | N/A | **Python sandbox** | N/A | N/A | N/A | N/A | N/A | N/A | N/A |
| FIM Completion | N/A | N/A | N/A | N/A | N/A | N/A | N/A | N/A | **beta/completions** | N/A |
| File Analysis | N/A | N/A | N/A | N/A | N/A | N/A | **v1/files** | N/A | N/A | N/A |
| Prefix Completion | N/A | N/A | N/A | N/A | N/A | N/A | N/A | N/A | **prefix: true** | N/A |
| Model Pull from UI | N/A | N/A | N/A | N/A | N/A | **Yes** | N/A | N/A | N/A | N/A |
| GPU Monitoring | N/A | N/A | N/A | N/A | N/A | **Yes** | N/A | N/A | N/A | N/A |
| Model Family Templates | N/A | N/A | N/A | N/A | N/A | **8 families** | N/A | N/A | N/A | N/A |

### AeroAgent Tool Categories (45 tools)

| Category | Tools | Danger Level |
|----------|-------|-------------|
| Remote File Ops | remote_list, remote_read, remote_download, remote_upload, remote_delete, remote_rename, remote_mkdir, remote_edit, remote_info, remote_search | medium-high |
| Local File Ops | local_list, local_read, local_write, local_delete, local_rename, local_mkdir, local_search, local_edit, local_move_files, local_copy_files, local_trash, local_file_info, local_disk_usage, local_find_duplicates, local_batch_rename | medium |
| Batch Operations | upload_files, download_files | medium |
| Archives | archive_compress, archive_decompress | medium |
| Power Tools | local_grep, local_head, local_tail, local_stat_batch, local_diff, local_tree | safe-medium |
| Clipboard | clipboard_read, clipboard_write, clipboard_read_image | safe |
| Shell | shell_execute | high |
| Sync & Compare | sync_preview | medium |
| RAG | rag_index, rag_search | medium |
| Memory | agent_memory_write | medium |

---

## Credential Storage Architecture (v1.9.0)

### Unified Keystore

Since v1.9.0, **all sensitive data** is stored in the Universal Vault (`vault.db`). The previous layered approach (OS Keyring primary, vault fallback) has been replaced by a single encrypted backend.

| Data Type | Storage | Notes |
|-----------|---------|-------|
| **Server passwords** | vault.db (AES-256-GCM) | Per-entry encryption with random nonce |
| **Server profiles** | vault.db (AES-256-GCM) | Host, port, username, protocol config (v1.9.0) |
| **OAuth tokens** | vault.db (AES-256-GCM) | Access + refresh tokens for all 5 OAuth providers |
| **AI API keys** | vault.db (AES-256-GCM) | All 7 AI provider keys |
| **AI settings** | vault.db (AES-256-GCM) | Model selection, provider config (v1.9.0) |
| **App config** | vault.db (AES-256-GCM) | Sensitive application settings (v1.9.0) |
| **MEGA credentials** | secrecy crate (zero-on-drop) | In-memory only during session |

### Keystore Backup/Restore (v1.9.0)

| Parameter | Value |
|-----------|-------|
| Format | `.aeroftp-keystore` binary |
| KDF | Argon2id (64 MB, t=3, p=4) |
| Encryption | AES-256-GCM |
| Integrity | HMAC-SHA256 |
| Merge modes | Skip existing, Overwrite all |

### Key Derivation (Vault)

| Parameter | Value |
|-----------|-------|
| Algorithm | HKDF-SHA256 (RFC 5869) |
| Input | 512-bit CSPRNG passphrase |
| Output | 256-bit vault key |
| Per-entry | AES-256-GCM with random 12-byte nonce |

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
| v1.5.4 | In-app auto-updater, download progress, AppImage auto-install, terminal empty-start | Done |
| v1.6.0 | AeroAgent Pro: native function calling (SEC-002), streaming, provider-agnostic tools, chat history, cost tracking, context awareness, 122 i18n keys | Done |
| v1.7.0 | Encryption Block: AeroVault v1, archive browser + selective extraction, Cryptomator format 8, CompressDialog, AeroFile mode, preview panel, Type column, 7z password fix | Done |
| v1.8.0 | Smart Sync (3 intelligent modes), Batch Rename (4 modes), Inline Rename, **AeroVault v2** (AES-256-GCM-SIV + AES-KW + AES-SIV + Argon2id 128MiB + HMAC-SHA512 + ChaCha20 cascade) | Done |

| v1.8.6 | Universal Credential Vault (AES-256-GCM + Argon2id + HKDF-SHA256), smart folder transfers, folder conflict resolution | Done |
| v1.8.7 | File clipboard (cut/copy/paste), cross-panel paste, Ubuntu/macOS audits | Done |
| v1.8.8 | Vision/multimodal AI, auto panel refresh, XSS hardening, security audit | Done |
| v1.9.0 | **Unified Keystore** (localStorage to vault migration), **keystore backup/restore** (.aeroftp-keystore), **migration wizard**, AI multi-step tools, Ollama auto-detection, conversation export, Monaco bidirectional sync, terminal command execution | Done |
| v2.0.0 | **AeroAgent Pro** — Provider Intelligence (7 provider profiles, model registry, parameter presets), Advanced Tool Execution (DAG pipeline, diff preview, intelligent retry, tool validation, composite macros, progress indicators), Context Intelligence (project detection, file dependency graph, persistent agent memory, conversation branching, smart context injection, token budget optimizer), Professional UX (streaming markdown, code block actions, thought visualization, prompt templates, multi-file diff, cost budget, chat search), Provider Features (Anthropic caching/thinking, OpenAI strict outputs, Ollama templates/pull/GPU, Gemini code execution) | Done |
| v2.0.1 | **3 Asian AI Providers** (Kimi, Qwen, DeepSeek) with thinking modes, web search, FIM, context caching, file analysis. **AeroFile Pro** Places Sidebar, BreadcrumbBar, Large Icons, drive detection, custom sidebar locations. Official SVG provider logos for all 10 providers | Done |
| v2.0.5 | **4shared native REST API** (OAuth 1.0, 14th protocol), CloudMe WebDAV preset, Places Sidebar GVFS/unmounted partitions, Autostart, Windows Explorer badges, OwnCloud removal | Done |

| v2.5.0 | **AeroFile Pro** — LocalFilePanel extraction, local path tabs (12 max, drag-to-reorder), file tags SQLite (7 Finder-style labels), FileTagBadge, tags context menu, sidebar filter, macOS FinderSync, event-driven volume detection, keyboard navigation, ARIA accessibility | Done |
| v2.5.2 | **AeroImage** — Built-in image editor (crop, resize, rotate, flip, color adjustments, effects, 6 output formats) | Done |
| v2.6.0 | **AeroAgent Ecosystem** — 4 new AI providers (AI21, Cerebras, SambaNova, Fireworks), Command Palette, Plugin Registry with GitHub-based browser, plugin hooks, context menu AI, AI status widget, drag & drop to agent | Done |

| v2.7.0 | **FileLu native REST API** — 19th protocol, file/folder passwords, privacy toggle, server-side clone, trash manager, remote URL upload | Done |

### Planned

| Version | Feature |
|---------|---------|
| v2.8.0 | CSP Phase 2 tightening, Biometric unlock, Provider-optimized transfers |

---

*This document is maintained as part of AeroFTP protocol documentation.*
