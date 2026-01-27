# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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