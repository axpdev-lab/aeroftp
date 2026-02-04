# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.8.2] - 2026-02-04

### Security UX Improvements

Quick polish release with security indicator and credential info display.

#### Added
- **Security indicator**: Lock icon in header toolbar shows master password status
  - Green locked icon when protection is enabled
  - Gray unlocked icon when not configured
  - Click to open Settings → Security directly
- **Credential security info**: Connection screen now displays security details
  - OS Keyring storage (libsecret/Keychain/Credential Manager)
  - AES-256-GCM + Argon2id encryption specifications
  - Privacy assurance (no cloud sync, no telemetry)
  - TLS 1.3 / SSH encryption indicator

---

## [1.8.1] - 2026-02-04

### Master Password Protection

App-wide master password protection with military-grade cryptography for securing access to AeroFTP.

#### Added
- **Master Password protection**: Secure the entire app with a master password
  - *Argon2id KDF*: 128 MiB memory, 4 iterations, 4-way parallelism (OWASP 2024 high-security)
  - *AES-256-GCM*: Authenticated encryption for password verification
  - *HMAC-SHA512*: Integrity verification with timing-safe comparison
  - *Secure zeroization*: Automatic memory clearing on lock
- **Security tab**: New Settings → Security panel for master password management
  - Set, change, or remove master password
  - View encryption details (algorithm specifications)
  - Configure auto-lock timeout
- **Auto-lock timeout**: Automatically lock the app after configurable inactivity period (0-60 minutes)
  - Activity tracking (mouse, keyboard, scroll events)
  - Persistent timeout setting across sessions
- **Lock Screen**: Full-screen overlay with password input when app is locked
  - Gradient background with pattern
  - Show/hide password toggle
  - Loading state during unlock

#### Fixed
- **CVE-2026-25537**: Updated jsonwebtoken to 10.3 (Cryptomator JWT parsing security fix)

---

## [1.8.0] - 2026-02-04

### Smart Sync & AeroVault v2 Military-Grade Encryption

Intelligent file conflict resolution, batch rename with live preview, and a completely redesigned AeroVault format with military-grade cryptography.

#### Added
- **AeroVault v2**: Military-grade encrypted containers with enhanced security stack:
  - *AES-256-GCM-SIV* (RFC 8452): Nonce misuse-resistant content encryption
  - *AES-256-KW* (RFC 3394): Key wrapping for master key protection
  - *AES-256-SIV*: Deterministic filename encryption (hides file names)
  - *Argon2id KDF*: 128 MiB memory, 4 iterations, 4-way parallelism (exceeds OWASP 2024)
  - *HMAC-SHA512*: Header integrity verification
  - *ChaCha20-Poly1305*: Optional cascade mode for defense-in-depth
  - *64 KB chunks*: Optimal balance of security and performance
- **Cryptomator to context menu**: Legacy format support moved from toolbar to folder context menu
- **Smart Sync options**: Three new intelligent conflict resolution modes in Settings → File Handling:
  - *Overwrite if source is newer*: Transfers only when source file has more recent timestamp (1s tolerance)
  - *Overwrite if date or size differs*: Syncs files when either attribute changes
  - *Skip if identical*: Skips transfer when both date and size match
- **Batch Rename dialog**: Rename multiple files at once with four modes:
  - Find & Replace (with case-sensitive option)
  - Add Prefix
  - Add Suffix (before extension)
  - Sequential numbering (customizable base name, start number, padding)
  - Live preview with conflict detection
- **Inline Rename**: Click directly on filename (when selected) or press F2 to rename in place — works on both local and remote panels
- **AeroVault v2 change password**: Re-wraps master key and MAC key with new KEK derived from new password — generates new salt for fresh cryptographic state
- **AeroVault v2 delete file**: Remove individual files from v2 vaults via manifest update — data section remains until future compaction

#### Changed
- **Unified date format**: Both panels now use `Intl.DateTimeFormat` for locale-aware dates — automatically adapts to user's browser language (IT: "03 feb 2026, 14:30", US: "Feb 03, 2026, 2:30 PM")
- **Responsive PERMS column**: Hidden below 1280px viewport width, no text wrapping
- **Toolbar reorganization**: Cleaner layout with visual separators — Donate | AeroVault | Menu toggle, Theme, Settings (Cryptomator moved to context menu)
- **Disconnect button icon**: Changed from X to LogOut for better UX clarity
- **AeroVault v2 security badges**: VaultPanel now displays cryptographic primitives (AES-256-GCM-SIV, Argon2id, AES-KW, HMAC-SHA512)

#### Fixed
- **Date column wrapping**: Added `whitespace-nowrap` to prevent date text breaking across lines
- **Inconsistent date display**: Remote panel showed raw FTP format while local showed localized dates — now unified
- **AeroVault v2 creation failure**: Fixed `WRAPPED_KEY_SIZE` constant (48 → 40 bytes) that caused "Key wrap failed: InvalidOutputSize { expected: 40 }" error when creating Advanced/Paranoid vaults
- **Security level detection**: New `vault_v2_peek` command reads header without password — UI now correctly shows "Paranoid" vs "Advanced" before unlocking
- **VaultPanel security display**: Dialog now shows correct security level badge with icon and color (blue/emerald/purple) matching vault type

---

## [1.7.1] - 2026-02-03

### Italian i18n + Terminal Cursor Fix

Hotfix release addressing missing Italian translations and invisible terminal cursor.

#### Fixed
- **Italian translations**: Translated 200+ keys with `[NEEDS TRANSLATION]` placeholders across vault, cryptomator, archive, AeroAgent, settings, toast, UI, and context menu sections
- **Terminal cursor invisible**: Removed legacy CSS injection that interfered with xterm v5 canvas-based cursor rendering — cursor now uses native theme `cursor` color property correctly
- **Dead code cleanup**: Removed unused `cursorCss` property from all 8 terminal themes

---

## [1.7.0] - 2026-02-03

### Encryption Block — AeroVault, Archive Browser, Cryptomator, AeroFile

Client-side encryption features, in-app archive browsing, encrypted vault containers, Cryptomator vault compatibility, and a new local-only file manager mode.

#### Added
- **Archive browser**: List contents of ZIP, 7z, TAR (gz/xz/bz2), and RAR archives without extracting
- **Selective extraction**: Extract individual files from any supported archive format
- **AeroVault**: AES-256 encrypted container files (.aerovault) for secure file storage
- **AeroVault operations**: Create, add files, remove files, extract, change password
- **Cryptomator vault support**: Unlock, browse, decrypt, and encrypt files in Cryptomator format 8 vaults
- **Cryptomator crypto**: scrypt KDF, AES Key Wrap, AES-SIV filename encryption, AES-GCM content chunks
- **CompressDialog**: Unified compression dialog with format selection (ZIP/7z/TAR/GZ/XZ/BZ2), compression levels (Store/Fast/Normal/Maximum), password protection (ZIP/7z), editable archive name, and file info display
- **AeroFile mode**: Local-only file manager mode — remote panel hides when not connected, toolbar toggle to switch between dual-panel and local-only even when connected
- **Preview panel**: Resizable sidebar preview in AeroFile mode with image thumbnail, file info (size, type, resolution, modified, extension, path), and quick actions (Open Preview, View Source, Copy Path)
- **Image resolution display**: Automatic width × height detection for image files in preview panel
- **Type column**: Sortable file type column in both local and remote file lists, responsive (hidden below xl breakpoint)
- **AeroVault file icon**: Shield icon in emerald green for `.aerovault` files in file lists
- **i18n**: 60+ new keys for archive, vault, Cryptomator, and compress UI across 51 languages
- **AeroAgent personality**: Enhanced system prompt with identity, tone, protocol expertise for all 13 providers, and behavior rules
- **AeroAgent server context**: Dynamic injection of connected server host, port, and user into AI context
- **AeroAgent local tools**: `local_mkdir`, `local_delete`, `local_write`, `local_rename`, `local_search` for full local file management
- **AeroAgent edit tools**: `local_edit` and `remote_edit` for find & replace text operations in local and remote files
- **AeroAgent batch transfers**: `upload_files` and `download_files` for multi-file upload/download operations
- **AeroAgent tool display**: Styled inline chips with wrench icon replace raw TOOL/ARGS text blocks in chat
- **AeroAgent tool count**: Expanded from 14 to 24 provider-agnostic tools

#### Fixed
- **7z password detection**: Fixed encrypted 7z archives opening without password prompt — now probes content decryption via `for_each_entries` since 7z metadata is unencrypted even when content is encrypted
- **Compression levels**: ZIP, 7z, TAR.GZ, TAR.XZ, TAR.BZ2 now accept `compression_level` parameter from frontend

#### Changed
- **New Rust modules**: `archive_browse.rs`, `aerovault.rs`, `cryptomator.rs` (modular architecture)
- **AeroAgent tools module**: `ai_tools.rs` expanded with 10 new tool handlers (24 total)
- **20+ new Tauri commands**: 8 archive browsing, 7 AeroVault, 5 Cryptomator
- **New frontend components**: `ArchiveBrowser.tsx`, `VaultPanel.tsx`, `CryptomatorBrowser.tsx`, `CompressDialog.tsx`
- **Compress submenu replaced**: 6 separate context menu items (~130 lines) replaced with single "Compress" action opening CompressDialog
- **Preview panel exclusive to AeroFile**: Preview button and panel only visible in local-only mode, auto-hidden when connecting
- **5 new Cargo dependencies**: scrypt, aes-kw, aes-siv, data-encoding, jsonwebtoken

---

## [1.6.0] - 2026-02-02

### AeroAgent Pro — AI Evolution

Complete overhaul of AeroAgent with native function calling, streaming responses, provider-agnostic tool execution, persistent chat history, cost tracking, and context awareness across all 13 protocols.

#### Added
- **Native function calling**: OpenAI `tools[]`, Anthropic `tool_use`, and Gemini `functionDeclarations` replace fragile regex-based tool parsing (SEC-002). Text-based fallback retained for Ollama and custom endpoints
- **Streaming responses**: Real-time incremental rendering for all 7 provider types — OpenAI SSE, Anthropic `content_block_delta`, Gemini `streamGenerateContent`, Ollama NDJSON. New `ai_stream.rs` backend module with Tauri event emission
- **Provider-agnostic tools**: 14 tools via unified `execute_ai_tool` command routing through `StorageProvider` trait — works identically across FTP, SFTP, WebDAV, S3, and all 8 cloud providers. New `ai_tools.rs` module
- **Remote tools**: `remote_list`, `remote_read`, `remote_upload`, `remote_download`, `remote_delete`, `remote_rename`, `remote_mkdir`, `remote_search`, `remote_info` — all protocol-agnostic
- **Local tools**: `local_list`, `local_read` for filesystem operations within AI context
- **Advanced tools**: `sync_preview`, `archive_create`, `archive_extract` integrated into AI tool system
- **Chat history persistence**: Conversations saved to `appConfigDir()/ai_history.json` via Tauri plugin-fs with 50-conversation, 200-message-per-conversation limits. Sidebar with conversation switching, deletion, and new chat creation
- **Cost tracking**: Per-message token count and cost display (input/output tokens, calculated from model pricing). Parsed from OpenAI `usage`, Anthropic `usage`, and Gemini `usageMetadata`
- **Context awareness**: Dynamic system prompt injection with active provider type, connection status, current remote/local paths, and selected files
- **AI i18n**: 122 new translation keys in `ai.*` namespace covering AIChat, ToolApproval, and AISettingsPanel — synced to all 51 languages
- **Tool JSON Schema**: `toJSONSchema()` and `toNativeDefinitions()` helpers convert tool definitions to provider-native format

#### Changed
- **Tool names**: Renamed from FTP-specific (`list_files`, `read_file`) to provider-agnostic (`remote_list`, `remote_read`). `FTP_TOOLS` aliased to `AGENT_TOOLS` for backwards compatibility
- **Tool execution**: Single `invoke('execute_ai_tool')` replaces protocol-specific routing through multiple Tauri commands
- **AI request/response types**: `AIRequest` gains `tools` and `tool_results` fields; `AIResponse` gains `tool_calls`, `input_tokens`, `output_tokens` fields
- **Gemini types**: `GeminiPart.text` now `Option<String>` to support `functionCall` parts; response parsing extracts both text and function call blocks
- **Anthropic types**: `AnthropicContent.text` now `Option<String>` with `content_type`, `id`, `name`, `input` fields for `tool_use` block parsing
- **OpenAI types**: `OpenAIMessage.content` now `Option<String>` with `tool_call_id` for tool result messages

#### Security
- **SEC-002 resolved**: Native function calling replaces regex-based tool call parsing for OpenAI, Anthropic, and Gemini providers
- **Tool path validation**: Null byte rejection, path traversal prevention, length limits, and tool name whitelist enforcement in `ai_tools.rs`
- **Content size limits**: Remote file reads capped at 5KB, directory listings capped at 50 entries for AI context window safety

---

## [1.5.4] - 2026-02-02

### In-App Auto-Updater + Terminal Polish

Full in-app update download experience with progress bar, AppImage auto-install, and terminal first-tab rendering fix.

#### Added
- **Auto-updater periodic check**: Background update check every 24 hours in addition to startup check, plus tray menu "Check for Updates" handler
- **In-app update download**: Download updates directly from the notification toast with real-time progress bar showing percentage, speed (MB/s), and ETA. Completed downloads show file path with "Open in File Manager" button
- **AppImage auto-install**: AppImage format receives an "Install & Restart" button that replaces the current executable, sets permissions, and relaunches the app automatically — no manual file management required
- **Terminal empty-start pattern**: Terminal opens with no tabs, user clicks "+" to create first tab — avoids xterm.js FitAddon race condition with container layout timing

#### Fixed
- **Terminal first-tab rendering**: Fixed broken box art on initial terminal tab caused by xterm FitAddon running before container had dimensions; resolved with empty-start pattern and ResizeObserver fallback
- **Update toast i18n**: Fixed raw key `settings.currentVersion` displayed instead of translated text; corrected to `ui.currentVersion` with proper interpolation
- **Tray menu update check**: "Check for Updates" tray menu item was not wired to any handler; now triggers manual update check

#### Changed
- **Update toast redesign**: Replaced external download link with inline download flow — 4-state toast (notify → progress → complete → error) with Lucide icons replacing emoji
- **New dependency**: `futures-util` 0.3 for reqwest stream consumption in update download

---

## [1.5.3] - 2026-02-02

### Sync Index + Storage Quota + FTP Retry + Session Fix

Enhanced sync with persistent index cache for conflict detection, storage quota display in status bar for 9 providers, FTP retry with exponential backoff for large batch transfers, and critical OAuth session switch fix.

#### Added
- **Sync index cache**: Persistent file index saved after each sync to `~/.config/aeroftp/sync-index/`, enabling true conflict detection (both sides changed since last sync) and faster subsequent comparisons
- **Storage quota in status bar**: Used/total space display with progress bar for Google Drive, Dropbox, OneDrive, Box, pCloud, Filen, SFTP (statvfs), WebDAV (RFC 4331), and MEGA (mega-df)
- **FTP retry with exponential backoff**: Automatic retry (up to 3 attempts) for "Data connection already open" errors during FTP sync, with NOOP reset and 500ms/1000ms backoff between attempts
- **Sync panel editable paths**: Local and remote paths in sync panel are now editable input fields, independent from main file browser navigation
- **Empty directory sync**: Standalone empty directories are now created during sync operations, with directory count shown in completion report
- **Sync index UI indicator**: "Index cached" badge with Zap icon appears next to scan button when a sync index exists for the current path pair
- **Native clipboard command**: `copy_to_clipboard` Rust command using `arboard` crate, bypassing WebView clipboard restrictions for all copy operations after async calls
- **i18n coverage expansion**: 108 new translation keys added — context menus, notifications, tooltips, settings labels, and server dialog fields now fully internationalized across all 51 languages
- **Cross-panel drag & drop**: Drag files from the local panel to the remote panel to upload, or from remote to local to download. Visual feedback with blue ring highlight on the target panel, `copy` cursor to distinguish from intra-panel moves, and support for multi-file drag transfers via the existing transfer queue
- **Terminal themes**: 8 built-in themes — Tokyo Night, Dracula, Monokai, Nord, Catppuccin Mocha, GitHub Dark, Solarized Dark, and Solarized Light — with theme selector dropdown and persistent preference
- **Terminal font size control**: Configurable font size (8-28px) via Ctrl+/- zoom, Ctrl+0 reset, and toolbar buttons with persisted preference
- **Multiple terminal tabs**: Support for multiple concurrent terminal sessions with tab bar, individual start/stop controls, and per-tab PTY session management
- **SSH remote shell**: Interactive SSH shell sessions to active SFTP servers directly from the terminal panel, using russh for independent SSH connections with password and key-based authentication
- **Terminal session persistence**: Scrollback buffer saved to localStorage on tab close or component unmount, restored with "Session restored" indicator on next open

#### Fixed
- **Share link clipboard on Linux/WebView**: Fixed share link not copying to clipboard after async invoke by replacing `navigator.clipboard.writeText()` (fails when user gesture context is lost after await) with native `arboard` clipboard via Rust command, using `SetExtLinux::wait()` for reliable X11 clipboard manager handoff
- **Dropbox share link scope**: Added `sharing.write` OAuth scope for Dropbox, enabling share link creation; added `missing_scope` error detection with actionable message
- **OAuth session switching**: Fixed "OAuth credentials not found" error when switching between provider tabs by adding OS keyring fallback for credential lookup in `switchSession`
- **Storage quota not updating on tab switch**: Fixed quota display showing stale data or disappearing when switching sessions, by fetching quota directly after reconnection instead of relying on React effect timing
- **Google Drive keyring key mismatch**: Fixed credential lookup using wrong keyring key format (`google_drive` vs `googledrive`) during session reconnection
- **Azure listed in quota support**: Removed Azure Blob from `supportsStorageQuota()` list since it has no backend implementation
- **Dropbox `download_to_bytes` HTTP check**: Added missing HTTP status check — error responses were previously returned as file content
- **Dropbox `remove_share_link`**: Implemented missing backend method using `sharing/revoke_shared_link` API
- **FTP inter-transfer delay**: Increased from 150ms to 350ms to reduce "Data connection already open" errors on rapid sequential transfers
- **Hardcoded English in CJK/Arabic UI**: Replaced ~113 hardcoded English strings in App.tsx and SettingsPanel.tsx with i18n `t()` calls — context menu labels, notification messages, settings fields, and tooltips now respect the selected language

#### Changed
- **Status bar quota format**: Changed from "X free" to "used / total" format with color-coded progress bar (purple < 70%, amber < 90%, red > 90%)
- **SFTP and WebDAV added to quota support list**: These protocols support storage info via statvfs and RFC 4331 respectively

---

## [1.5.2] - 2026-02-02

### Codebase Audit + Multi-Protocol Sync + Credential Fix

#### Added
- **Multi-protocol directory sync**: Sync Files now works with all 13 protocols (FTP, FTPS, SFTP, WebDAV, S3, Google Drive, Dropbox, OneDrive, MEGA, Box, pCloud, Azure, Filen) via new `provider_compare_directories` command using the `StorageProvider` trait
- **Sync toolbar button**: Dedicated "Sync Files" button in the toolbar for quick access, distinct from the synchronized navigation toggle
- **Sync progress bar**: Real-time byte-level progress indicator during sync operations, showing current file, speed, and percentage
- **Sync completion report**: Summary panel after sync showing uploaded/downloaded/skipped/error counts, total bytes transferred, and duration
- **Shared crypto module**: Consolidated duplicate Argon2id + AES-256-GCM cryptographic primitives into `crypto.rs`, consumed by credential store and profile export
- **Credential keyring fallback**: Direct keyring access bypasses conservative probe when OS keyring appears unavailable on first launch
- **Windows terminal support**: Removed Unix-only restriction from PTY module — terminal now works on Windows via conpty (PowerShell) in addition to Linux/macOS

#### Fixed
- **Credential loading on first launch**: Saved server passwords failed to load when OS keyring probe returned false on startup (common with gnome-keyring on Linux). Now falls back to direct keyring access and shows explanatory message with auto-redirect to edit form if still unavailable
- **SEC-001: Archive password zeroization**: ZIP, 7z, and RAR archive passwords are now wrapped in `secrecy::SecretString`, ensuring automatic memory zeroization on drop instead of lingering as plain strings
- **SEC-004: OAuth token memory protection**: OAuth2 access tokens returned by `get_valid_token()` are now wrapped in `SecretString` across all 5 OAuth providers (Google Drive, Dropbox, OneDrive, Box, pCloud)
- **FTP connection failure**: Fixed critical bug where FTP/FTPS connections routed through `provider_connect` instead of `connect_ftp`, causing "Not connected to server" errors on first directory navigation
- **Dropbox OAuth connection**: Fixed token exchange failure caused by ephemeral callback port not matching registered redirect URI. Dropbox now uses fixed port 17548 and requests `token_access_type=offline` for persistent refresh tokens
- **Navigation sync for OAuth/cloud providers**: Fixed synced navigation breaking when navigating to root directory on providers where remote base path is `/` (empty string after path normalization). Also fixed local Up button not triggering sync for cloud providers
- **Sync upload subdirectory creation**: Pre-create parent directories before uploading files in subdirectories during sync, preventing "Parent directory does not exist" errors on WebDAV and cloud providers
- **WebDAV self-reference filtering**: Fixed PROPFIND response parsing for WebDAV servers with non-root base paths (e.g., Jianguoyun `/dav/`) where the self-reference entry was not filtered, causing path resolution errors
- **Keep-alive for cloud providers**: Fixed FTP NOOP being sent on non-FTP connections by adding `activeSessionId` to the keep-alive effect dependency array
- **Export/Import translations**: Added proper translations for 18 export/import dialog keys across all 49 non-English languages
- **Filen/MEGA debug logging removed**: Replaced plaintext file logging (`/tmp/filen_debug.log`, `/tmp/aeroftp-mega.log`) with `tracing::debug!` to prevent sensitive data exposure in production
- **Sync panel i18n**: Replaced hardcoded Italian text and emoji icons with Lucide icons and 45 new i18n keys across all 51 languages
- **AI provider naming collision**: Renamed `ai.rs::ProviderType` to `AIProviderType` to avoid collision with storage providers
- **S3 logging inconsistency**: Changed `s3.rs` from `log` crate to `tracing` for consistency

#### Removed
- **Dead useAnalytics hook**: Removed unused analytics hook and related exports (Aptabase integration was never activated)
- **Abandoned components**: Deleted orphaned `MigrationDialog.tsx`, `MasterPasswordDialog.tsx`, `SnapNoticeDialog.tsx`
- **7 unused Cargo dependencies**: Removed `aes`, `cbc`, `ctr`, `zeroize`, `futures`, `tokio-util`, `tauri-plugin-aptabase`
- **Dead type exports**: Removed unused `AppState`, `Theme`, `SyncOperation`, `DownloadFolderParams`, `UploadFolderParams` from types.ts
- **Exposed API key**: Removed commented Aptabase analytics code containing hardcoded API key
- **Debug artifacts**: Removed commented `/tmp/webdav_debug.xml` write in webdav.rs

#### Changed
- **Provider preset ordering**: Stable providers now appear before Beta in S3 and WebDAV dropdown lists
- **IDrive e2**: Promoted from Beta to Stable after successful testing
- **DriveHQ logo**: Updated to higher resolution with 1.4x scale factor for better visibility
- **package.json cleanup**: Moved `@types/howler` and `@types/prismjs` from dependencies to devDependencies

---

## [1.5.1] - 2026-02-01

### WebDAV Compatibility + Provider Keep-Alive + UI Polish

#### Added
- **4 new S3/WebDAV presets**: Jianguoyun (WebDAV), InfiniCLOUD (WebDAV), Alibaba Cloud OSS (S3), Tencent Cloud COS (S3) — total 30 connection options
- **Provider logos in saved servers**: Saved servers sidebar and Settings panel now display official provider SVG logos instead of generic letter/cloud icons
- **Provider logos in session tabs**: S3/WebDAV connections show the specific provider logo (Cloudflare R2, Backblaze, etc.) instead of generic database/cloud icons
- **Provider identity tracking**: `providerId` field added to ServerProfile and session data, preserving which preset was used across save/connect/tab lifecycle
- **Documentation links**: All S3/WebDAV presets show a "Docs" link in the connection form header, linking to the provider's official setup guide
- **Official provider logos**: Added SVG/PNG logos for Jianguoyun, InfiniCLOUD, Alibaba Cloud, Tencent Cloud, DriveHQ; updated IDrive e2 to official logo
- **OAuth account email retrieval**: All OAuth providers (Google Drive, Dropbox, OneDrive, Box, pCloud) now fetch and store the authenticated user's email after connection
- **Password visibility toggle**: Eye icon on all password fields (FTP, S3, MEGA, Filen) to show/hide passwords in the connection form
- **Settings > Cloud Provider**: Added Box and pCloud credential configuration sections (Client ID + Client Secret with OS Keyring storage)
- **Settings > Server > Add Server**: Added Filen, Box, pCloud, and Azure Blob to the protocol dropdown
- **Provider keep-alive**: Non-FTP providers (WebDAV, S3, SFTP, OAuth) now receive periodic keep-alive pings every 60 seconds to prevent server-side connection timeouts
- **Session tab drag-to-reorder**: Drag and drop session tabs to rearrange connection order
- **Saved server drag-to-reorder**: Drag and drop saved servers to rearrange with grip handle

#### Fixed
- **WebDAV directory detection (Koofr)**: Rewrote `<resourcetype>` XML parsing to search for "collection" keyword within the resourcetype block instead of pattern-matching specific tag formats. Fixes Koofr's non-standard `<D:collection xmlns:D="DAV:"/>` format and ensures compatibility with all WebDAV servers (Nextcloud, DriveHQ, ownCloud, etc.)
- **WebDAV directory detection in cd/stat**: Applied the same robust collection detection to `cd()` and `stat()` methods, which still used the old fragile pattern matching
- **WebDAV self-reference filtering**: Fixed root directory listing including itself as an entry due to empty string `ends_with` matching
- **Koofr preset URL**: Corrected default WebDAV URL to `https://app.koofr.net/dav/Koofr` with proper base path `/dav/Koofr/`
- **Jianguoyun preset URL**: Corrected default WebDAV URL to `https://dav.jianguoyun.com/dav` with proper base path `/dav/`
- **Saved server click behavior**: Only clicking the server icon initiates a connection; the text area no longer triggers accidental connections. Icon has hover effect (scale + ring) to indicate it's a button
- **Session tab names**: OAuth provider connections from saved servers now display the custom saved name instead of the generic provider name
- **Edit server flow**: Saving an edited server now resets the form and returns to the saved servers list
- **SettingsPanel missing colors**: Added gradient colors for Box, pCloud, Azure, Filen protocols (were falling back to gray)

#### Changed
- **Koofr**: Promoted from Beta to Stable after comprehensive WebDAV testing
- **Jianguoyun**: Promoted from Beta to Stable
- **InfiniCLOUD**: Promoted from Beta to Stable
- **Cloudflare R2**: Promoted from Beta to Stable after successful testing
- **Provider logos in connection form**: S3/WebDAV preset header now shows the official SVG logo (Cloudflare, Backblaze, etc.) instead of a generic cloud icon
- **Saved server subtitles**: Unified display schema across sidebar and Settings panel
  - S3: `bucket — Cloudflare R2` (auto-detected from endpoint)
  - WebDAV: `user@host` (without `https://` prefix and port)
  - OAuth: `OAuth2 — user@email.com` (or provider name as fallback)
  - MEGA: `E2E AES-128 — user@email.com`
  - Filen: `E2E AES-256 — user@email.com`

---

## [1.5.0] - 2026-01-31

### 4 New Cloud Providers + FTP Security Defaults + UI Refresh

Major release adding four new native cloud storage providers (Box, pCloud, Azure Blob Storage, Filen), bringing AeroFTP to 13 native protocols. FTP now defaults to opportunistic TLS encryption for safer connections out of the box. Modal dialogs redesigned with flat headers, unified search bar, and full light/dark theme support.

#### Added - New Cloud Storage Providers
- **Box** (Beta): OAuth2 PKCE authentication, ID-based file API, share links, storage quota, folder operations
- **pCloud** (Beta): OAuth2 authentication with US/EU region selection, path-based REST API, share links, storage quota
- **Azure Blob Storage** (Beta): Shared Key HMAC-SHA256 and SAS token authentication, container-based storage, XML response parsing
- **Filen** (Beta): Zero-knowledge E2E encryption with PBKDF2 + AES-256-GCM, encrypted metadata and content, chunk-based transfers

#### Added - Backend Infrastructure
- **4 new Rust provider modules**: `box_provider.rs`, `pcloud.rs`, `azure.rs`, `filen.rs` implementing full `StorageProvider` trait
- **OAuth2 extensions**: Box and pCloud added to `OAuth2Manager` with PKCE support
- **New dependencies**: `pbkdf2 0.12`, `uuid 1` (v4), `mime_guess 2`, `reqwest` multipart feature

#### Added - Frontend Integration
- **Provider logos**: Official SVG icons for Box, pCloud, Azure, and Filen in `ProviderLogos.tsx`
- **Protocol selector**: 4 new entries in cloud storage section with Beta badges
- **Session tabs**: Provider icons and colors for all 4 new providers
- **Azure fields**: Container name input in connection form
- **pCloud fields**: US/EU region selector radio buttons

#### Changed - FTP Security Defaults
- **Default encryption**: FTP now defaults to "Use explicit FTP over TLS if available" instead of plain FTP
- **Badge**: FTP badge changed from red "Insecure" to orange "TLS" — communicates encryption without alarming users
- **Warning banner**: Changed from red to amber, only shown when user explicitly selects plain FTP (no TLS)
- **Dropdown order**: "TLS if available" is now first option, "plain FTP (none)" moved to last

#### Changed - Protocol Badges
- **S3**: Badge upgraded from "Beta" to "Secure" (stable, tested)
- **WebDAV**: Badge upgraded from "Beta" to "Secure" (stable, tested)
- **Box, pCloud, Azure, Filen**: "Beta" badge until fully tested

#### Changed - UI Refinements
- **Unified local/remote search**: Single search bar with scope toggle (local/remote) replacing dual search inputs
- **Modal redesign**: About and Support dialogs refactored to flat header style matching Dependencies panel (no gradients)
- **About dialog**: Updated protocol list (13 protocols on two lines), version moved under logo, full light/dark theme support
- **Provider reordering**: Disabled providers (pCloud, Azure) moved to end of cloud provider list
- **pCloud disabled in production**: Hidden behind `import.meta.env.DEV` flag until fully tested
- **Azure disabled in production**: Hidden behind `import.meta.env.DEV` flag until fully tested
- **MEGA stable**: Removed intermediate provider selection step, marked as stable
- **Column width**: Main content area expanded from `max-w-4xl` to `max-w-5xl`
- **OAuth badges**: Lock icon added to OAuth provider badges in protocol selector

#### Changed - Security Hardening (from v1.4.1)
- **AI API keys migrated to OS Keyring**: API keys stored securely in OS Keyring instead of localStorage
- **Archive encryption dialog**: ZIP and 7z compression now offer optional AES-256 password encryption
- **ErrorBoundary**: Global error recovery UI wrapping the entire application
- **Dead code cleanup**: Removed 10 unused hook files totaling 3,543 lines
- **Memory leak fix**: Blob URLs from `URL.createObjectURL` now properly revoked in usePreview

#### Fixed
- **Filen saved server display**: Fixed provider name not showing in saved servers list
- **Missing i18n keys**: Added `searchLocal`, `searchRemote`, `searchScope` keys across 51 languages

## [1.4.1] - 2026-01-31

### Security Hardening + Code Quality

Focused release on security, code quality, and developer experience. AI API keys migrated to OS Keyring, archive compression now supports password-protected encryption, and significant dead code cleanup.

#### Added
- **Archive encryption dialog**: ZIP and 7z compression now offer optional AES-256 password encryption via interactive dialog
- **ErrorBoundary**: Global error recovery UI wrapping the entire application
- **Provider utility functions**: `isNonFtpProvider()`, `isFtpProtocol()`, `supportsStorageQuota()`, `supportsNativeShareLink()` in types.ts

#### Changed
- **AI API keys migrated to OS Keyring**: API keys for AI providers (OpenAI, Anthropic, Google, etc.) stored securely in OS Keyring instead of localStorage, with automatic migration on first load
- **Extracted useCloudSync hook**: AeroCloud state and event listeners moved from App.tsx (-140 lines)
- **Extracted useTransferEvents hook**: Transfer/delete event handling moved from App.tsx (-207 lines)
- **App.tsx reduced**: 4,484 to 4,137 lines (-347 lines total)

#### Fixed
- **Memory leak in usePreview**: Blob URLs from `URL.createObjectURL` now properly revoked when preview is replaced without closing
- **Dead code cleanup**: Removed 10 unused hook files totaling 3,543 lines

#### Security
- **AI API keys**: No longer stored in plain text in localStorage; uses OS Keyring (gnome-keyring / macOS Keychain / Windows Credential Manager) with encrypted vault fallback

## [1.4.0] - 2026-01-31

### Cross-Provider Enhancements + FTPS TLS + Performance

Major feature release bringing cross-provider operations (search, versions, thumbnails, permissions, locking), full FTPS TLS encryption with 4 modes, FTP protocol enhancements (MLSD/MLST, resume transfers), S3 multipart upload, archive encryption, and 8 dependency upgrades.

#### Added - FTPS TLS Encryption
- **Explicit TLS (AUTH TLS)**: Connects plain on port 21, upgrades to TLS before login via `into_secure()`
- **Implicit TLS**: Direct TLS connection on port 990
- **Explicit if available**: Attempts AUTH TLS, falls back to plain FTP if server doesn't support it
- **Certificate verification**: Configurable per-connection, allows self-signed certificates
- **TLS backend**: `native-tls` (OpenSSL on Linux, Secure Transport on macOS, SChannel on Windows)
- **UI encryption dropdown**: Protocol-specific dropdown for FTP (default: None) and FTPS (default: Implicit)

#### Added - Cross-Provider Features
- **Remote Search**: Search bar with real-time results across all 9 protocols
- **File Versions**: Version history dialog for Google Drive, Dropbox, and OneDrive
- **Thumbnails**: Image previews in file listings for cloud providers (Google Drive, Dropbox, OneDrive)
- **Share Permissions**: Permission management dialog for Google Drive and OneDrive
- **WebDAV Locking**: Lock/Unlock context menu for WebDAV (RFC 4918)
- **Storage Quota**: Quota display for SFTP, WebDAV, Google Drive, Dropbox, OneDrive, MEGA

#### Added - FTP Protocol Enhancements
- **MLSD/MLST (RFC 3659)**: Machine-readable directory listings with FEAT detection and automatic fallback to LIST
- **Resume Downloads (REST)**: Partial download resume via REST offset + RETR
- **Resume Uploads (APPE)**: Partial upload resume via APPE (append)

#### Added - Archive Encryption
- **ZIP AES-256**: Read and write encrypted ZIP archives via `zip` v7.2
- **7z AES-256**: Read and write encrypted 7z archives via `sevenz-rust` v0.6
- **RAR password extraction**: Password-protected RAR extraction via p7zip

#### Added - S3 & OneDrive Enhancements
- **S3 Multipart Upload**: Large file uploads with automatic part splitting
- **OneDrive Resumable Upload**: Resumable upload sessions for large files

#### Changed - Dependency Upgrades
- **russh 0.54 -> 0.57**: New ciphers, future-compat fixes
- **reqwest 0.12 -> 0.13**: HTTP/3 support, performance improvements
- **quick-xml 0.31 -> 0.39**: Improved WebDAV XML parsing
- **suppaftp 8**: Added `tokio-async-native-tls` feature for FTPS TLS support
- **zip 7.2**: AES-256 encryption support
- **sevenz-rust 0.6**: AES-256 encryption support
- **ring 0.17.14**: Updated cryptographic backend
- **zeroize 1.8.2**: Memory safety for credentials

#### Changed - UI Improvements
- **Toolbar badge threshold**: File count badge on Upload/Delete buttons now only shows for 2+ selected files
- **FTP insecure warning**: Red "Insecure" badge and warning banner when plain FTP is selected

#### Fixed - i18n
- **537 keys across 51 languages**: All new cross-provider, FTPS, and archive keys synced
- **Italian fully translated**: All new keys translated for Italian locale

---

## [1.3.4] - 2026-01-29

### Security Hardening + Debug Mode + Dependency Upgrades

Security release with SFTP host key verification, OAuth2 hardening, FTP warnings, debug/diagnostics tools, and 5 major dependency upgrades.

#### Added - SFTP Host Key Verification
- **Trust On First Use (TOFU)**: First connection to a new host accepts the key and saves it to `~/.ssh/known_hosts`
- **MITM rejection**: Subsequent connections reject if the server key has changed, logging a clear error with the mismatched algorithm
- **Automatic `~/.ssh/` setup**: Creates directory (0700) and `known_hosts` file (0600) if they don't exist
- **Non-standard port support**: Stores keys as `[host]:port` format for ports other than 22

#### Added - FTP Insecure Connection Warning
- **"Insecure" badge**: Red badge on FTP protocol in the selector grid, replacing the previous unlabeled entry
- **Warning banner**: Red alert box shown when FTP is selected, recommending FTPS or SFTP
- **i18n support**: Warning text translated for English and Italian (`ftpWarningTitle`, `ftpWarningDesc`)

#### Changed - OAuth2 Security
- **Ephemeral callback port**: OAuth2 callback server now binds to port 0 (OS-assigned random port) instead of fixed port 17548
- **Dynamic redirect URI**: `redirect_uri` is generated with the actual port after binding, passed to the auth flow
- **Race condition eliminated**: Local processes can no longer predict the callback port to intercept tokens

#### Changed - CI/CD
- **Release notes from CHANGELOG**: GitHub Releases now extract the body from `CHANGELOG.md` instead of using hardcoded text
- **Downloads section appended**: Platform download list is automatically added to each release

#### Changed - Documentation
- **SECURITY.md**: Complete rewrite with full security architecture (credential storage, protocols, OAuth2, encryption, memory safety, known issues)
- **CLAUDE.md**: Added i18n sync/validate steps to the release workflow
- **COMPETITOR-ANALYSIS.md**: Expanded security comparison table (12 features vs 6 competitors)

#### Fixed - Security (CVE)
- **CVE-2025-54804 resolved**: Upgraded from russh v0.48 to v0.54.5, eliminating the medium-severity DoS vulnerability
- **Removed russh-keys dependency**: SFTP now uses russh's built-in key handling and known_hosts module, reducing attack surface
- **SFTP host key verification**: Migrated from custom implementation to russh's native `known_hosts` module with hashed host support

#### Added - Debug Mode
- **Debug Mode toggle**: File menu item with Ctrl+Shift+F12 shortcut, persistent in settings
- **Debug Panel**: 5-tab panel (Connection, Network, System, Logs, Frontend) with real-time diagnostics
- **Dependencies Panel**: Live crate version checking against crates.io with category grouping, status badges, and copy-to-clipboard
- **StatusBar badge**: Amber [DEBUG] indicator when debug mode is active

#### Changed - Dependency Upgrades
- **secrecy 0.8 -> 0.10**: Migrated to `SecretString` API (Rust Edition 2021)
- **bzip2 0.4 -> 0.6**: Pure-Rust backend via `libbz2-rs-sys` (no C dependency)
- **thiserror 1 -> 2**: no-std support, Edition 2021
- **suppaftp 7 -> 8**: rustls backend selection (tokio API unchanged)
- **zip 2 -> 7**: ZIP encryption support (AES-256), ZIP64 improvements

#### Changed - Documentation
- **README.md**: Updated to v1.3.4 with security section, debug tools, archive formats, roadmap
- **PROTOCOL-FEATURES.md**: Updated archive matrix with new crate versions and encryption status

#### Fixed - i18n
- **51 languages synced**: All new keys propagated via `npm run i18n:sync`
- **Italian contextMenu translated**: 16 archive-related keys (compress, extract, password) translated from English placeholders

## [1.3.3] - 2026-01-29

### OS Keyring Integration + Critical Security & Stability Fixes

Critical hotfix release that fixes the OS keyring credential storage, removes the broken migration system, and resolves multiple stability issues.

#### Fixed - Credential Storage (Critical)
- **OS Keyring now functional on Linux**: Added `linux-native` feature to `keyring` crate - previously the keyring compiled without a backend, silently failing all store/get operations
- **Removed broken migration dialog**: The v1.3.2 migration system was deleting passwords from localStorage before confirming keyring availability, locking users out of all saved servers
- **Passwords stored securely in OS keyring**: Credentials are now properly saved and retrieved via gnome-keyring/Secret Service on Linux, macOS Keychain, and Windows Credential Manager

#### Fixed - Session Tabs
- **Session tabs now appear for FTP/FTPS connections**: Quick Connect for standard protocols was missing `createSession()` call, so no tabs were created after connecting
- **"+" button resets connection form**: Previously showed stale data from the last connection instead of a clean "Select protocol" screen

#### Fixed - Update Notifications
- **Toast dismiss independent from status bar**: Closing the update toast no longer hides the status bar update badge
- **Removed pulse animation from status bar badge**: The gradient badge is visible enough without `animate-pulse`
- **Fixed `{protocol}` literal in activity log**: AeroCloud connection logs now show the actual protocol instead of the template variable

#### Fixed - AeroCloud
- **Sync completion notifications in Activity Log**: Manual and background sync now log results (uploaded/downloaded file counts or errors) instead of only writing to console
- **Cloud name description in setup wizard**: Step 1 now shows "Custom name displayed in the tab" instead of "Choose Folder" under the Cloud Name input
- **Added `id` field to CloudPanel savedServers**: Fixes credential lookup during AeroCloud wizard setup
- **AeroCloud tab shows connection log**: Switching to AeroCloud on an already-connected server now logs the connection event

#### Added
- **`contextMenu.delete` i18n key**: Added missing translation for delete button tooltip (EN + IT)

#### Removed
- **Migration dialog system**: Removed `MigrationDialog` component, startup migration check, and all related code - replaced by direct OS keyring storage on save

## [1.3.2] - 2026-01-29

### Secure Credential Storage

## [1.3.1] - 2026-01-29

### Multi-Format Archives + Keyboard Shortcuts + UX Overhaul

Feature-rich release adding TAR archive family support, full keyboard navigation, smart toolbar redesign, and extraction submenu.

#### Added - TAR Archive Family (Backend)
- **TAR Compression**: Create `.tar`, `.tar.gz`, `.tar.xz`, `.tar.bz2` archives
  - Rust backend using `tar`, `flate2`, `xz2`, `bzip2` crates
  - Single command `compress_tar` with format parameter
  - Recursive folder support with relative paths
- **TAR Extraction**: Auto-detect format from extension
  - Supports `.tar`, `.tar.gz`, `.tgz`, `.tar.xz`, `.txz`, `.tar.bz2`, `.tbz2`
  - `create_subfolder` option for organized extraction
- **ZIP Extraction**: Added `create_subfolder` parameter (was missing)

#### Added - Context Menu Submenus
- **Compress Submenu**: Single "Compress" item expands to 6 formats (ZIP, 7z, TAR, TAR.GZ, TAR.XZ, TAR.BZ2)
  - Hover-to-expand with `createPortal` rendering (avoids mouseLeave issues)
  - 200ms delayed close for smooth mouse transitions
  - Auto-repositioning to stay within viewport
- **Extract Submenu**: "Extract" item expands to "Extract Here" and "Extract to Folder"
  - Works for all archive types (ZIP, 7z, TAR variants)
  - 7z password prompt preserved for encrypted archives
  - Activity log now shows destination folder for "Extract to Folder"

#### Added - Full Keyboard Shortcuts (11 new)
- **Delete**: Delete selected files with confirmation dialog
- **Enter**: Open selected folder
- **Backspace**: Navigate up one directory
- **Tab**: Switch between Remote/Local panels
- **F2**: Rename selected file
- **Ctrl+N**: Create new folder
- **Ctrl+A**: Select all files in active panel
- **Ctrl+U**: Upload selected local files
- **Ctrl+D**: Download selected remote files
- **Ctrl+R**: Refresh active panel
- **Ctrl+F**: Focus search/filter input
- All shortcuts respect input focus (disabled when typing in text fields)

#### Changed - Smart Toolbar Redesign
- **Dynamic Upload/Download button**: Shows "Upload Files" when Local panel active, "Download Files" when Remote panel active
- **Selection count badges**: Upload/Download and Delete buttons show badge with count of selected items
- **Delete button**: Added to dynamic toolbar (was only in context menu)
- **Visual separator**: Clear division between dynamic buttons (Up, Refresh, New, Open, Grid, Upload/Download, Delete) and connection buttons (Cancel, Sync)
- **Auto panel switching**: Clicking files in a panel automatically activates that panel's toolbar context

#### Changed - Address Bar Improvements
- **Refresh button**: Added inline refresh icon (RefreshCw) inside both address bars, with 600ms spin animation on click
- **Reduced icon-to-URL spacing**: Tighter padding between protocol/disk icon and path text

#### Fixed - File Selection
- **Toggle deselection**: Clicking an already-selected file now deselects it
- **Refresh clears selection**: Both `loadLocalFiles` and `loadRemoteFiles` reset selection state

#### Added - i18n (51 Languages)
- New keys: `compressSubmenu`, `extractSubmenu`, `extractToFolder`, `extractTarHere` translated in all 51 locales

#### Technical
- **Rust**: Added `tar 0.4`, `flate2 1.0`, `xz2 0.1`, `bzip2 0.4` dependencies
- **React**: `ContextMenu` component rewritten with `createPortal` for submenu rendering
- **TypeScript**: Zero type errors, all shortcuts properly typed

---

## [1.3.0] - 2026-01-28

### 7z AES-256 Encryption + Privacy-First Analytics

Major release introducing encrypted archive support and opt-in analytics infrastructure.

#### Added - 7z Archive Support (AES-256)
- **7z Compression (LZMA2)**: Right-click → "Compress to 7z" for superior compression ratio
  - Multi-file/folder selection support
  - LZMA2 algorithm (better than Deflate)
  - Future: password-protected archive creation

- **7z Extraction with AES-256 Decryption**: Full support for encrypted 7z archives
  - Automatic encryption detection
  - Password prompt for encrypted archives
  - Creates subfolder by archive name on extraction
  - Secure: passwords cleared from memory after use

- **Dual Archive Formats**: Choose between ZIP (compatibility) and 7z (compression/security)

#### Added - Privacy-First Analytics
- **Aptabase Integration** (opt-in, default OFF):
  - EU data residency (GDPR compliant)
  - No PII collected ever
  - Tracks only: protocol types used, feature popularity, transfer size ranges
  - Helps prioritize development based on real usage

- **Analytics Hook** (`useAnalytics`):
  - `trackAppStarted`: App version, language, OS
  - `trackConnectionSuccess`: Protocol type only
  - `trackTransferCompleted`: Direction and size range
  - `trackFeatureUsed`: Which features are popular

#### Added - i18n (51 Languages)
- 15 new translation keys for 7z features:
  - `contextMenu.compressToZip`, `contextMenu.compressTo7z`
  - `contextMenu.extract7zHere`, `contextMenu.extractZipHere`
  - `contextMenu.passwordRequired`, `contextMenu.enterArchivePassword`
  - `contextMenu.wrongPassword`, `contextMenu.compressionFailed`
  - And more for notifications and progress

#### Technical
- **Rust**: Added `sevenz-rust` crate with `aes256` and `compress` features
- **Rust**: Added `tauri-plugin-aptabase` v1.0 for Tauri v2
- **Commands**: `compress_7z`, `extract_7z`, `is_7z_encrypted`
- **Hooks**: New `useAnalytics` hook exports for feature tracking
- All 51 languages updated with archive-related translations

#### Security Notes
- CVE-2025-54804 (russh): Documented in Cargo.toml, awaiting upstream fix
- 7z passwords handled securely, never logged or persisted

---

## [1.2.9] - 2026-01-26

### Privacy & Security + i18n Completion

Minor release adding Privacy settings and completing internationalization.

#### Added
- **Privacy & Security Tab** in Settings:
  - Analytics toggle (default: OFF) - opt-in anonymous usage statistics
  - Security information panel showing data protection measures
  - Clear explanation of what data is/isn't collected
- **Analytics Implementation Plan**: Documentation for future Aptabase integration
- **9 new i18n keys** for Privacy section across all 51 languages

#### Fixed
- **Back Button Navigation**: "Back" button now restores previous session instead of opening offline file manager
- **Back Button Flash**: Fixed brief flash of connection form when clicking back

#### Technical
- New settings: `analytics_enabled` persisted in localStorage
- New i18n keys: `settings.privacy`, `settings.privacyDesc`, `settings.sendAnalytics`, `settings.analyticsDesc`, `settings.securityTitle`, `settings.securityLocal`, `settings.securityOAuth`, `settings.securityNoSend`, `settings.securityTLS`
- All 51 languages at 100% completion (434 keys each)

---

## [1.2.8] - 2026-01-25

### Global Multilingual + File Management Pro Features

Major release closing feature gaps with competitors and expanding to 51 languages worldwide.

#### Added - File Management Pro Features
- **Overwrite Confirmation Dialog**: Smart file conflict resolution
  - Shows when uploading/downloading files that already exist
  - Comparison view: source vs destination (size, date)
  - Color-coded: "Source is newer" (green) / "Source is older" (orange)
  - Actions: Overwrite, Skip, Rename, Cancel
  - "Apply to all" checkbox for batch operations
  - All skipped files tracked in Activity Log

- **Activity Log: File Move Tracking**:
  - New 'MOVE' operation type with teal icon
  - Logs when files are dragged into folders
  - Shows source file and destination folder
  - Full theme support (light, dark, cyber)

- **Properties Dialog**: View detailed file/folder metadata
  - File name, path, size, type, MIME type
  - Permissions (with octal conversion)
  - Modified date
  - Protocol-specific info (remote files show protocol name)
  - Integrated checksum calculation

- **Checksum Verification** (MD5/SHA-256):
  - Calculate checksums directly in Properties dialog
  - Click "Calculate" to compute on-demand
  - Copy checksum to clipboard
  - Uses streaming for large files (64KB buffer)

- **Compress/Archive**:
  - Right-click → "Compress" to create ZIP from selected files/folders
  - Supports multi-select compression
  - Recursive folder compression with preserved structure
  - "Extract Here" option for ZIP files
  - Deflate compression (level 6)

- **Drag & Drop (Basic)**: Move files within the same panel by dragging to folders
  - Works in both list and grid views
  - Multi-select drag support (drag selected files)
  - Visual feedback: green highlight on valid drop targets
  - Works for both local and remote files
  - Supported on all protocols (FTP, SFTP, WebDAV, S3, Cloud providers)

- **SFTP Protocol Support** (v1.3.0 Feature Preview):
  - Full SFTP implementation using `russh` + `russh-sftp`
  - Password and SSH key authentication (id_rsa, id_ed25519)
  - Encrypted key support with passphrase
  - All file operations: list, upload, download, mkdir, delete, rename, stat, chmod
  - Symlink detection and resolution
  - UI: Private key file browser, passphrase field, timeout configuration

#### Added - Global Multilingual Expansion (51 Languages)
- **46 New Languages**: From 5 to 51 total languages
  - **European**: German, Portuguese, Russian, Dutch, Polish, Swedish, Danish, Norwegian, Finnish, Czech, Hungarian, Romanian, Ukrainian, Greek, Slovak, Bulgarian, Croatian, Serbian, Slovenian, Macedonian, Lithuanian, Latvian, Estonian, Catalan, Galician, Basque, Welsh, Icelandic
  - **Asian**: Japanese, Korean, Vietnamese, Thai, Indonesian, Khmer, Georgian, Armenian
  - **Middle East (RTL)**: Arabic, Hebrew, Persian, Urdu
  - **Future-proof**: Bengali, Hindi, Swahili, Filipino support ready
- **RTL Language Support**: Full right-to-left layout for Arabic, Hebrew, Urdu, Persian
- **i18n Automation Scripts**: Validation, sync, stats tools for translation maintenance

#### Fixed
- **Disconnect Button Translation**: Fixed broken translation key (`connection.disconnect` → `common.disconnect`)
- **Connect Button Translation**: Same fix for connect button
- **Support Modal Theme**: Full light/dark theme support for all elements (buttons, borders, text, crypto panel)
- **Support Modal Icons**: Official SVG logos for GitHub, Buy Me a Coffee, Litecoin
- **Support Modal Width**: Increased to 540px to prevent crypto addresses from wrapping
- **Crypto Icons**: Official Bitcoin, Ethereum, Solana, Litecoin icons with brand colors
- **File Exists Setting**: Overwrite dialog now respects Settings > File Handling > "When file exists" preference
  - If set to "Overwrite", "Skip", or "Rename", applies automatically without showing dialog
  - Only shows dialog when set to "Ask each time" (default)
- **Address Bar Icons**: Favicon-style icons no longer have colored backgrounds (Chrome-style)
- **S3 Share Link**: S3 pre-signed URLs now available in context menu (was missing)
- **Share Link Logic**: Updated to include S3 in native share link providers list
- **Rust Compilation Warnings**: Fixed all 37 warnings with proper `#[allow(dead_code)]` annotations for future API methods

#### Technical
- New Rust dependencies: `md-5`, `zip`, `walkdir`
- New commands: `calculate_checksum`, `compress_files`, `extract_archive`
- New component: `PropertiesDialog` with MIME type detection
- New component: `OverwriteDialog` for file conflict resolution
- New Activity Log operation: `MOVE` with icon and translations
- Updated `HumanizedLogParams` with `destination` field
- Added 46 locale JSON files with complete translations

#### Documentation
- Created `docs/PROTOCOL-FEATURES.md` with comprehensive protocol feature matrix
- Documented Share Link support per protocol
- Competitor comparison for file operations
- Drag & Drop analysis and roadmap
- Updated multilingual expansion plan

---

## [1.2.7] - 2026-01-25

### 🔴 MEGA.nz Integration Complete

Full integration of MEGA.nz cloud storage with official branding and stability improvements.

#### Added
- **MEGA Official Logo**: Red circle with white "M" icon in protocol selector and session tabs
- **MEGA Provider Recognition**: Proper icon display in session tabs, status indicators, and badges
- **Update Available Button**: Purple gradient button in status bar when new version is available (links to download)
- **Check for Updates in Settings**: Re-enabled manual update check button in Settings > General

#### Fixed
- **MEGA Keep-Alive**: Fixed false "disconnected" status by excluding MEGA from FTP keep-alive ping (MEGA is stateless REST API)
- **MEGA Directory Navigation**: Fixed folder navigation using absolute paths instead of relative
- **Terminal Theme**: Restored Tokyo Night theme for better readability (cursor visible, distinct colors)
- **Protocol Selector UX**: Form now hides when protocol dropdown is open (prevents visual clutter)
- **Edit Saved Servers**: S3/WebDAV/MEGA servers now open directly to form when editing (skip provider selector)
- **Form State on Connect**: Form no longer appears briefly when connecting to saved server; only shows on Edit
- **OAuth Reconnection**: Fixed Google Drive/Dropbox/OneDrive reconnection when switching tabs (protocol fallback)
- **MEGA Quick Connect Styling**: Removed red background from MEGA in protocol selector (now uses standard styling)

#### Changed
- **Session Tab Icons**: Standardized icons for protocol consistency across the app:
  - FTPS: Shield icon (green) - secure FTP over TLS
  - SFTP: Lock icon (emerald) - secure SSH file transfer
  - WebDAV: Cloud icon (orange) - matches protocol selector
  - S3: Database icon (amber) - storage bucket style
  - Disconnected state: WifiOff icon for FTP protocols

#### Technical
- Added `'mega'` to keep-alive skip list in `useFtpOperations.ts` and `App.tsx`
- Added `MegaLogo` SVG component in `ProtocolSelector.tsx`
- Added MEGA case in `SessionTabs.tsx` (`isProviderProtocol`, `ProviderIcon`, `getProviderColor`)
- Terminal theme changed from GitHub Dark to Tokyo Night in `SSHTerminal.tsx`
- Added `onOpenChange` callback to `ProtocolSelector` for form visibility control
- Added `!editingProfileId` condition to skip provider selector when editing
- Added `UpdateInfo` interface and `updateAvailable` prop to `StatusBar.tsx`
- Uncommented `CheckUpdateButton` in `SettingsPanel.tsx`
- Added protocol fallback to active session in 15+ functions for tab switching
- Updated `SessionTabs.tsx` icons: Lock, ShieldCheck for SFTP/FTPS

---

## [1.2.6] - 2026-01-22

### 🔄 Auto-Update System

AeroFTP now checks for updates automatically and notifies you when a new version is available!

#### Added
- **Auto-Update Check**: Automatic check for new versions on startup (5 seconds after launch)
- **Smart Format Detection**: Detects your installation format (DEB, AppImage, Snap, Flatpak, RPM, EXE, DMG)
- **Update Toast**: Elegant notification badge with download link for your specific format
- **Tray Menu Check**: "Check for Updates" option in system tray menu
- **Activity Log Integration**: Update detection logged with [Auto]/[Manual] distinction
- **Tauri Updater Permissions**: Added `notification:default` and `updater:default` capabilities

#### Technical
- `detect_install_format()`: New Rust function detecting app installation method
- `UpdateInfo` struct: Extended with `install_format` field
- `check_update` command: Matches GitHub release assets to installed format
- `log_update_detection` command: Backend logging for version detection
- `useRef` for update check flag: Prevents re-render loops in React

#### UI/UX
- Toast badge: Blue theme, rounded corners, shows format (e.g., "Download .deb")
- Pulse animation on update badge
- Activity Log shows: version available, current version, and format
- Tray menu: Clean text without emojis

---

## [1.2.5] - 2025-01-23

### 🔌 Multi-Protocol Bug Fixes

#### Fixed
- **WebDAV DriveHQ Support**: Fixed WebDAV provider not showing files/folders when connecting to DriveHQ
  - Added support for `a:` namespace prefix (DriveHQ uses non-standard XML namespace)
  - Added CDATA content extraction for displayname fields
  - Added `<a:iscollection>` detection for directory identification
  - Namespace-agnostic regex patterns now support any XML prefix
- **FTP Path Persistence**: Fixed issue where FTP servers inherited WebDAV's remote directory (`/wwwhome`)
  - Fixed race condition where `connectionParams.protocol` was stale during session switching
  - Added explicit protocol parameter to `changeRemoteDirectory()` calls
  - Added FTP-specific path validation in `switchSession` to reset WebDAV paths
- **S3 Mkdir on Backblaze B2**: Fixed "411 Length Required" error when creating folders on Backblaze B2
  - Added explicit `Content-Length: 0` header for empty body PUT requests
  - Improved S3-compatible storage compatibility

#### Improved
- **Session Tab Icons**: Enhanced visual feedback for all protocols in session tabs
  - Provider-specific icons now show for S3, WebDAV, and OAuth providers
  - Connected/disconnected status indicator with opacity feedback
  - S3 tabs now show orange database icon for better recognition
  - OAuth providers (Google Drive, Dropbox, OneDrive) show branded icons with status

#### Technical
- Updated `parse_propfind_response()` regex to support any XML namespace prefix
- Updated `extract_tag_content()` to handle CDATA wrapped values
- Added `isProviderProtocol()` helper for tab icon rendering logic
- Explicit protocol passing prevents React state race conditions

---

## [1.2.4] - 2026-01-22

### 🗄️ S3 Multi-Tab Improvements

#### Added
- **S3 Recursive Folder Upload**: Full support for uploading folders with subfolders to S3
- **Go Up Always Visible**: "Go up" row now always visible in file lists (disabled at root for visual consistency)
- **S3 Fields in Settings**: Bucket and Region fields visible when editing S3 servers in Settings panel

#### Fixed
- **S3 Upload Refresh**: File list now correctly refreshes after S3 uploads
- **S3 Icon Sizing**: S3 server icons in saved servers list now have consistent sizing
- **Saved Servers Header**: Title and description text no longer overlap

#### Technical
- Deep copy of `connectionParams` in session creation to prevent state mutation
- Explicit `sessionParams` construction for S3 with preserved options
- Added `shrink-0` class for consistent icon sizing

#### Known Issues (WIP)
- S3 reconnect may fail with "bucket name required" when switching tabs
- FTP file list may be empty after S3 connection
- Activity log "LIVE" states may persist

---


## [1.2.1] - 2025-01-20

### Fixed
- **Tab Switching**: Fixed issue where switching between FTP servers didn't update the remote file list
- **OAuth to FTP Switch**: Fixed issue where switching from OAuth providers (Google Drive, Dropbox, OneDrive) to a new FTP connection didn't show the connection screen
- **AeroCloud Tab**: Fixed protocol parameter not being passed when connecting to AeroCloud server, which caused issues when switching from OAuth providers
- **New Tab Button**: Fixed "+" button not showing connection screen when opening a new tab

### Improved
- **OAuth Callback Page**: Added professional branded callback page with official AeroFTP logo, animations, and modern glassmorphism design
- **AeroCloud Custom Name**: Added ability to set a custom display name for the AeroCloud tab in settings

### Added
- Translations for custom cloud name feature in all supported languages (EN, IT, FR, ES, ZH)

---

## [1.2.2] - 2025-01-20

### Added
- **Share Link for OAuth Providers**: Native share link creation for Google Drive, Dropbox, and OneDrive files
  - Right-click → "Create Share Link" to generate public sharing URL
  - Uses native APIs: Google Drive Permissions API, Dropbox Sharing API, OneDrive Graph API
  - One-click copy to clipboard with toast notification
- **Share Link for AeroCloud**: "Copy Share Link" in context menu for files in AeroCloud folder (requires `public_url_base` config)
- **`provider_create_share_link` command**: New Tauri command for creating share links via OAuth providers

### Fixed
- **OAuth Folder Download**: Added `provider_download_folder` command to properly download folders recursively from Google Drive, Dropbox, and OneDrive
- **FTP After OAuth**: Fixed issue where connecting to FTP server after using OAuth provider would fail - now properly disconnects OAuth provider before FTP connection
- **SavedServers Protocol Dropdown**: Disabled unavailable protocols (WebDAV, S3, Dropbox, OneDrive) with "(Soon)" label
- **AeroCloud in SavedServers**: Removed AeroCloud from SavedServers dropdown - it has dedicated panel via Quick Connect
- **Tab Display Names**: Sessions now use custom `displayName` instead of raw FTP host
- **OAuth Tab Names**: OAuth providers show custom name or provider name in tab

### Improved
- **OAuth Callback Page**: Simplified design, removed emoji icon, cleaner branding

### Changed
- **ConnectionParams**: Added `displayName` field for custom session naming

---

## [1.2.3] - 2026-01-21

### 🔄 Multi-Session OAuth Switching

Full support for switching between multiple cloud provider tabs without losing connection state.

### Added
- **Multi-Session Backend Architecture**: New `session_manager.rs` and `session_commands.rs`
  - `MultiProviderState` with HashMap<session_id, provider> for concurrent connections
  - Session lifecycle commands: `session_connect`, `session_disconnect`, `session_switch`, `session_list`
  - File operation commands with session context: `session_list_files`, `session_mkdir`, `session_delete`, etc.
- **useSession Hook**: New React hook (`useSession.ts`) for multi-session provider management

### Fixed
- **OAuth Tab Switching**: Switching between Google Drive, Dropbox, and OneDrive tabs now works correctly
  - Reconnects OAuth provider with correct credentials from localStorage
  - Properly disconnects previous provider before connecting new one
- **StatusBar OAuth Display**: Shows "Google Drive", "Dropbox", "OneDrive" instead of `undefined@undefined`
- **OAuth File Operations**: mkdir, delete, rename now correctly use `provider_*` commands for OAuth providers
  - `createFolder` uses `provider_mkdir` for OAuth
  - `deleteRemoteFile` uses `provider_delete_file/dir` for OAuth  
  - `renameFile` uses `provider_rename` for OAuth

### Technical
- Added `MultiProviderState` to Tauri managed state
- Registered 14 new session_* commands in invoke_handler
- Added `isCurrentOAuthProvider` memo in App.tsx for provider detection

---

## [1.2.0] - 2026-01-20

### ☁️ Google Drive Integration (OAuth2)

AeroFTP now connects to Google Drive! More cloud providers coming in v1.2.1.

#### Added
- **Google Drive Support**: Full file management (browse, upload, download, delete)
- **OAuth2 Authentication**: Secure login via browser with local callback server
- **Provider Icons in Tabs**: Google Drive, Dropbox, OneDrive icons in session tabs
- **OAuth Settings Panel**: Configure API credentials in Settings → Cloud Storage
- **Quick Connect OAuth**: One-click connection for configured providers
- **OAuthConnect Component**: Modern OAuth sign-in UI

#### Fixed
- **Session Switching**: Fixed multi-session switching between FTP and OAuth providers
- **OAuth Credentials Loading**: Now reads from both legacy and new localStorage formats
- **Download/Upload/Delete Routing**: Proper command routing for OAuth providers

#### Coming Soon (v1.2.1)
- Dropbox integration (visible but disabled)
- OneDrive integration (visible but disabled)
- WebDAV and S3 testing (visible but disabled)
- MEGA integration (planned)

#### Technical
- `oauth2.rs`: PKCE flow with local callback server (port 17548)
- `google_drive.rs`: Google Drive API v3 implementation
- `switchSession`: Reconnects OAuth provider on session switch
- `provider_disconnect`: Called before switching from OAuth to FTP

---

## [1.1.0] - 2026-01-20

### 🌐 Multi-Protocol Cloud Storage (Sprint 1)

AeroFTP now supports multiple cloud storage protocols beyond FTP!

#### Added
- **WebDAV Support**: Connect to Nextcloud, ownCloud, and any WebDAV server
- **S3 Support**: Connect to AWS S3, MinIO, Backblaze B2, Cloudflare R2, and S3-compatible storage
- **Protocol Selector**: New dropdown in connection screen to choose protocol (FTP/FTPS/WebDAV/S3)
- **S3-specific fields**: Bucket, region, and custom endpoint configuration
- **`useProvider` hook**: Unified React hook for multi-protocol operations
- **`useConnection` hook**: Backward-compatible connection wrapper
- **`StorageProvider` trait**: Rust abstraction for all storage backends
- **Provider commands**: Full Tauri command set for provider operations

#### Changed
- **AeroCloud Sync**: Now works with any storage provider (FTP, WebDAV, S3)
- **CloudService**: Refactored to use generic `StorageProvider` trait

#### Technical
- New `providers/` module with FTP, WebDAV, and S3 implementations
- `provider_commands.rs` with Tauri command bindings
- Unified `RemoteEntry` type across all providers

---

## [1.0.0] - 2026-01-20

### 🎉 First Stable Release

#### Changed
- **Logo**: Removed shadow/glow effects for cleaner appearance (animation retained for activity indicator)

#### Fixed
- **DevTools Terminal**: Fixed keyboard input not working (stale closure bug with connection state)
- **DevTools Terminal**: Improved terminal styling with GitHub Dark theme
- **DevTools Terminal**: Added colored PS1 prompt (green user@host, blue path)

#### Technical
- Updated Tauri plugin versions for compatibility
- Terminal now uses block cursor with proper focus handling

---

## [0.9.9] - 2026-01-18

### Added
- **Per-Session Navigation Sync** 🔗: Each tab now maintains its own independent sync state
  - Sync settings saved/restored when switching between tabs
  - New connections start with sync disabled by default
  - Sync state persists per-session, not globally

- **Path Coherence Check** ⚠️: Visual warning when local path doesn't match remote server
  - Orange warning icon replaces disk icon when mismatch detected
  - Path text turns orange to highlight the issue
  - Tooltip explains the mismatch

- **AeroCloud Log Filter** ☁️: Toggle button to hide/show cloud sync messages
  - New cloud icon button in Activity Log header
  - Filter out "AeroCloud sync" messages when disabled
  - Reduces log noise during active sync operations

- **Tab Switch Logging** 📋: Reconnection status now logged in Activity Log
  - "🔄 Reconnecting to {server}..." during connection
  - "✅ Reconnected to {server}" on success
  - "❌ Failed to reconnect to {server}" on error

### Fixed
- **Local path not restoring on tab switch**: Fixed race condition with explicit state capture
- **Navigation sync staying active on new connections**: Now properly reset when connecting to new server
- **Folder progress badge showing bytes**: Fixed to show file count [X/Y] instead of bytes

### Changed
- Session switching now uses captured state values to prevent race conditions
- `handleNewTabFromSavedServer` also captures state before saving session

## [0.9.7] - 2026-01-17

### Added
- **Activity Log Panel** 📋: New FileZilla-style activity log with real-time operation tracking
  - Typewriter animation effect for new log entries
  - Live indicator with pulsing animation
  - Auto-scroll with user scroll detection
  - Clear all logs functionality
  - **Badge counter** in StatusBar showing log count (0 → 99+)

- **Dual Theme System** 🎨:
  - **Professional** (default): Tokio Night/Antigravity-inspired elegant dark theme
  - **Cyber** (optional): Neon glow effects with CRT scanlines overlay
  - Theme toggle button in Activity Log header
  - Glow effects on operation text only (CONNECT, UPLOAD, etc.) in cyber mode

- **Humanized Log Messages** 🌍: Friendly, conversational log messages in 5 languages
  - English, Italian, French, Spanish, Chinese
  - Contextual messages with emojis (🚀 connection, 📁 navigation, ⬆️ upload, etc.)
  - File size and transfer time displayed for uploads/downloads
  - Smart pluralization and formatting

- **Comprehensive Operation Logging**:
  - Connection/disconnection with server details
  - Upload/download with file size and duration
  - Navigation (remote and local directories)
  - File operations: delete, rename, mkdir
  - Bulk delete operations with file count
  - Tab close events
  - Sync navigation toggle state
  - AeroCloud sync status (syncing/completed/error) with SYNCED indicator
  - Keep-alive timeout and reconnection events
  - **Monaco Editor saves**: File edits now logged with size info

### Fixed
- **Multi-tab switching bug**: Switching between FTP sessions now correctly refreshes BOTH remote and local panels
- **Keep-alive logging**: Timeout and reconnection events now properly logged with humanized messages
- **TypeScript type safety**: All theme properties properly typed across professional and cyber themes
- **Cloud sync log deduplication**: Added debounce to prevent duplicate log entries from React StrictMode

### Changed
- Activity Log is now closed by default (toggle via StatusBar)
- DevTools panel moved below Activity Log in layout
- Connection welcome messages simplified to "Benvenuto! 🚀 Connesso a {server}"

---

## [0.9.5] - 2026-01-15

### Added
- **Unified Transfer Queue**: New `TransferQueue` component showing both uploads and downloads
  - Visual indicators for upload (↑ cyan) and download (↓ orange) operations
  - Real-time progress counter `[N/total]`
  - Auto-scroll to active transfer
  - "Queue" button in StatusBar with item count badge
  - Auto-hide after 5 seconds, reopens on click
- **Sync Badge visibility fix**: Sync badges now only appear in AeroCloud folder paths

### Fixed
- **Tab switching with Cloud**: Fixed session state persistence when switching between FTP servers and AeroCloud
- **Notification spam**: Removed per-file toast notifications during bulk transfers (queue shows progress instead)
- **Logo activity animation**: Now correctly pulses during all transfer operations

### Changed
- Transfer notifications consolidated: only error/warning toasts are shown

## [0.9.1] - 2026-01-06

### Fixed
- Fixed audio/video player playback issues for local files
- Optimized media streaming performance using Blob URLs
- Fixed memory leaks in media preview

## [0.9.0] - 2026-01-06

### Changed
- **Pre-release for v1.0.0**: Stabilization and final polish
- Restructured GitHub Actions workflow for direct release upload
- Eliminated artifact storage quota issues

### Added
- Full macOS support in CI/CD pipeline (.dmg builds)

---

## [0.8.3] - 2026-01-05

### Added
- **Internationalization (i18n) System**: Complete multi-language support infrastructure
  - Lightweight React Context-based system (zero external dependencies)
  - Full TypeScript support with autocompletion
  - English (base) and Italian translations included
  - Browser language auto-detection with localStorage persistence
  - Language selector in Settings → Appearance with flag icons
  - Fallback to English for missing translations
  - Parameter interpolation support (`{variable}` syntax)
  - Documentation: `docs/TRANSLATIONS.md`
  - Migrated components: StatusBar, ConnectionScreen, SavedServers, AboutDialog, Dialogs, SettingsPanel, CloudPanel, DevToolsPanel

### Fixed
- **AeroCloud Sync Interval Persistence Bug**: Added missing sync interval setting in CloudPanel Settings
  - Users can now modify the sync interval (1-60 minutes) after initial setup
  - Previously, the interval was only configurable during setup wizard


## [0.8.2] - 2025-12-25 🎄

### Fixed
- Text file preview (TXT, MD, LOG, etc.) not loading in Universal Preview modal
- Added proper content loading for text/markdown files in both local and remote modes

## [0.5.6] - 2025-12-22

### Fixed
- TypeScript build errors in ToolApproval component (replaced && operators with ternary for type safety)
- Cross-platform compatibility issues with PTY module on Windows
- Snap package configuration for Ubuntu Software distribution

### Added
- Snap package support for easy installation on Ubuntu and other Linux distributions
- Desktop entry file for better Linux desktop integration

### Changed
- Improved GitHub Actions workflow for more reliable builds
- Updated all version numbers across package.json, tauri.conf.json, and Cargo.toml

## [0.3.2] - 2025-12-21

### Fixed
- GitHub Actions workflow to create releases only on tags
- Updated Tauri action to latest version for better compatibility

## [0.3.1] - 2025-12-20

### Fixed
- Build synchronization issues
- Updated GitHub Actions workflow for automatic releases
- Corrected versioning across all configuration files

## [0.1.0] - 2025-12-19

### Added
- Initial release of AeroFTP
- Modern, cross-platform FTP client built with Rust and React
- Beautiful UI with glass morphism effects and dark mode support
- Dual panel interface for remote and local file browsing
- Support for FTPS (FTP over TLS)
- Async file transfers
- File search functionality
- Server connection profiles
- Linux releases: .deb, .rpm, and .AppImage packages

### Features
- 🚀 Lightning fast performance with Rust backend
- 🎨 Apple-inspired design
- 🌙 Full dark mode support
- 📁 Dual panel file browser
- 🔒 Secure FTPS connections
- ⚡ Non-blocking transfers
- 🔍 Quick file search
- 💾 Saved server profiles