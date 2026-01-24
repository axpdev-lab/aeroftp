# AeroCloud 2.0 - Multi-Provider Cloud Storage Roadmap

> Created: 20 January 2026
> Version: AeroFTP 1.0.0 → 1.2.7
> Status: Sprint 2.5 Complete (v1.2.7 Released)

---

## 📦 Release Log

### v1.2.7 (25 January 2026)
**MEGA.nz Integration Complete:**
- ✅ **MEGA Official Logo**: Red circle with white "M" icon everywhere
- ✅ **Keep-Alive Fix**: No more false disconnects (MEGA is stateless)
- ✅ **Directory Navigation**: Fixed absolute path handling
- ✅ **Terminal Theme**: Tokyo Night restored (cursor visible, colors distinct)
- ✅ **Protocol Selector UX**: Form hides when dropdown open
- ✅ **Edit Saved Servers**: Direct to form for S3/WebDAV/MEGA

### v1.2.6 (22 January 2026)
**Features:**
- ✅ **Auto-Update System**: Automatic check for new versions on startup
- ✅ **Smart Format Detection**: Detects installation format (DEB, AppImage, Snap, Flatpak, RPM)
- ✅ **Update Toast**: Elegant notification badge with download link for specific format
- ✅ **Tray Menu Check**: "Check for Updates" option in system tray menu
- ✅ **Activity Log Integration**: Update detection logged with [Auto]/[Manual] distinction

### v1.2.5 (23 January 2026)
**Features & Fixes:**
- ✅ **Multi-Session OAuth Switching**: Full support for switching between Google Drive, Dropbox, OneDrive tabs
  - Reconnects OAuth provider with correct credentials when switching tabs
  - Disconnects previous provider before connecting new one
- ✅ **StatusBar OAuth Fix**: Shows provider name (Google Drive, Dropbox, OneDrive) instead of undefined
- ✅ **File Operations for OAuth**: mkdir, delete, rename now use provider_* commands for OAuth providers
- ✅ **Backend Multi-Session Architecture**: Created `session_manager.rs` and `session_commands.rs`
  - `MultiProviderState` with HashMap<session_id, provider>
  - Session lifecycle commands: connect, disconnect, switch, list
- ✅ **useSession Hook**: New React hook for multi-session management

### v1.2.2 (20 January 2026)
**Features & Fixes:**
- ✅ **Share Link for OAuth Providers**: Native share link creation for Google Drive, Dropbox, and OneDrive
  - Google Drive: Creates "anyone with link can view" permission and gets webViewLink
  - Dropbox: Uses sharing/create_shared_link_with_settings API
  - OneDrive: Uses Graph API createLink with anonymous scope
- ✅ **Share Link for AeroCloud**: Context menu option when `public_url_base` is configured
- ✅ OAuth Folder Download: Added `provider_download_folder` command for recursive folder downloads
- ✅ FTP After OAuth: Fixed issue where FTP connection failed after OAuth - now disconnects OAuth provider first
- ✅ OAuth Callback Page: Simplified design, removed emoji, cleaner branding
- ✅ Tab Switching: All multi-session switching bugs resolved

### v1.2.1 (20 January 2026)
**Features & Fixes:**
- ✅ Tab Switching: Fixed remote file list not updating when switching FTP servers
- ✅ OAuth to FTP Switch: Fixed connection screen not showing
- ✅ AeroCloud Tab: Fixed protocol parameter for server connections
- ✅ New Tab Button: Fixed "+" button not showing connection screen
- ✅ OAuth Callback Page: Professional branded page with animations
- ✅ AeroCloud Custom Name: Ability to set custom tab name in settings
- ✅ Translations: Custom cloud name feature in EN, IT, FR, ES, ZH

### v1.2.0 (20 January 2026)
**Commit:** `ed0d57f5e924a44e2c9286810344c7b79ce0a9c7`

**Features:**
- Google Drive OAuth2 integration (browse, download, upload, delete)
- Provider-specific tab icons
- OAuth credentials loading from Settings panel
- Session switching workaround for OAuth providers

**Packaging Updates:**
- ✅ Snap: Pushed to edge channel
- ✅ Flatpak PR: https://github.com/flathub/flathub/pull/7608
  - `com.aeroftp.AeroFTP.yml` → tag v1.2.0
  - `cargo-sources.json` → rigenerato (8184 righe)
  - `node-sources.json` → rigenerato (622 sources)
- ⏳ In attesa review Flathub

---

## 🎯 Vision

Transform AeroFTP from a pure FTP client into a **Universal Cloud Storage Browser** while maintaining the simplicity and elegance of the current design.

**Target Audience:**
- **Sprint 1-2**: Pro users, developers, sysadmins (WebDAV, S3)
- **Sprint 3+**: End users, consumers (Google Drive, Dropbox, OneDrive)

---

## 🏗️ Architecture Overview

### Provider Abstraction Layer

```
┌─────────────────────────────────────────────────────────────┐
│                    AeroFTP Frontend                         │
│  ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐           │
│  │   FTP   │ │ WebDAV  │ │   S3    │ │  Cloud  │ ← Tabs    │
│  └────┬────┘ └────┬────┘ └────┬────┘ └────┬────┘           │
└───────┼───────────┼───────────┼───────────┼─────────────────┘
        │           │           │           │
        ▼           ▼           ▼           ▼
┌─────────────────────────────────────────────────────────────┐
│              Rust Backend - Provider Layer                   │
│  ┌─────────────────────────────────────────────────────┐   │
│  │     trait StorageProvider (async)                    │   │
│  │   ├── connect()     ├── list()     ├── mkdir()      │   │
│  │   ├── disconnect()  ├── download() ├── delete()     │   │
│  │   ├── upload()      ├── rename()   ├── get_info()   │   │
│  └─────────────────────────────────────────────────────┘   │
│           │              │              │                   │
│           ▼              ▼              ▼                   │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐         │
│  │ FtpProvider │  │WebDavProvider│ │  S3Provider │         │
│  │ (suppaftp)  │  │  (reqwest)  │  │ (aws-sdk)   │         │
│  └─────────────┘  └─────────────┘  └─────────────┘         │
└─────────────────────────────────────────────────────────────┘
```

---

## 📋 Sprint 1: WebDAV + S3 Support (v1.1.0)

### Goals
- [x] Create unified `StorageProvider` trait
- [x] Refactor FTP to implement `StorageProvider`
- [x] Implement WebDAV provider
- [x] Implement S3-compatible provider
- [x] Update frontend for protocol selection

### Tasks

#### 1. Backend - Provider Abstraction (`src-tauri/src/providers/`)
- [x] Create `providers/mod.rs` with `StorageProvider` trait
- [x] Create `providers/types.rs` with shared types (`RemoteEntry`, `ProviderConfig`)
- [x] Create `providers/ftp.rs` - FTP provider implementation
- [x] Create `providers/webdav.rs` - WebDAV implementation
- [x] Create `providers/s3.rs` - S3 implementation with SigV4 signing

#### 2. Frontend - Protocol Selection
- [x] Add `ProviderType` to types.ts
- [x] Create `ProtocolSelector.tsx` component
- [x] Update ConnectionScreen with protocol dropdown
- [x] Add S3-specific fields (bucket, region, endpoint)
- [x] Add WebDAV hints and documentation

#### 3. Backend - Integration (Tauri Commands)
- [x] Create `provider_commands.rs` with Tauri commands
- [x] Add ProviderState for active provider management  
- [x] Register all provider commands in invoke_handler
- [x] Create `useProvider.ts` React hook
- [x] Wire up frontend to use provider commands instead of FTP-only
- [x] Update AeroCloud sync to work with any provider

#### 4. Testing & Documentation
- [ ] Test with Nextcloud (WebDAV)
- [ ] Test with MinIO (S3-compatible)
- [ ] Test with AWS S3
- [ ] Update README with new features

### Dependencies to Add (Cargo.toml)
```toml
# S3 Support
aws-sdk-s3 = "1"
aws-config = "1"

# Already have reqwest for WebDAV
```

---

## 📋 Sprint 2: OAuth2 Cloud Providers (v1.2.0)

### Goals
- [x] Google Drive integration (PARTIAL)
- [ ] Dropbox integration  
- [ ] OneDrive integration

### Completed (v1.2.0)
- [x] OAuth2 flow with local callback server
- [x] Google Drive API v3 provider (browse, download, upload, delete)
- [x] Add OAuth providers to ProtocolSelector
- [x] Provider-specific tab icons (Google Drive, Dropbox, OneDrive)
- [x] OAuth credentials loading from Settings panel

### Known Limitations
> ✅ **Session Switching Fixed in v1.2.3**: Full multi-session OAuth switching now works correctly.

### Tested Providers ✅
| Protocol | Provider     | Status    | Notes                                             |
| -------- | ------------ | --------- | ------------------------------------------------- |
| WebDAV   | DriveHQ      | ✅ Tested  | Full support, works with `/wwwhome` path          |
| S3       | Backblaze B2 | ✅ Tested  | Full support, requires bucket + endpoint + region |
| OAuth    | Google Drive | ✅ Tested  | Browse, upload, download, delete, share           |
| OAuth    | Dropbox      | ✅ Tested  | v1.2.2 implementation                             |
| OAuth    | OneDrive     | 🔄 Partial | Needs more testing                                |

### Remaining Tasks
- [x] Dropbox API v2 provider implementation (v1.2.2)
- [ ] Microsoft Graph API provider (OneDrive) - partially working
- [x] MEGA.nz provider (MEGAcmd REST API) - ✅ Completed (v1.2.6)
- [ ] WebDAV testing: Nextcloud, Synology, Other providers
- [ ] S3 testing: AWS, MinIO, Cloudflare R2, Wasabi
- [ ] Secure token storage (keyring)
- [ ] Token refresh handling
- [ ] Test OAuth flow on macOS/Windows

### Planned: Certified Providers in Connect Screen
> Idea: Instead of generic "WebDAV" / "S3" options, offer **certified providers** with pre-configured:
> - Provider-specific placeholders and hints
> - Correct default endpoints and ports
> - Tested and verified integration
> 
> **Proposed structure:**
> ```
> WebDAV Providers:
>   - DriveHQ (tested ✅)
>   - Nextcloud
>   - Synology NAS
>   - pCloud
>   - Custom WebDAV (manual config)
> 
> S3 Providers:
>   - Backblaze B2 (tested ✅)
>   - AWS S3
>   - Cloudflare R2
>   - MinIO
>   - Wasabi
>   - Custom S3 (manual config)
> ```
> This certifies each integration and provides appropriate UX for each provider.

---

## 📋 Sprint 2.5: UX Enhancements (Drag & Drop, Move)

### Advanced File Management
- [ ] **Native "Move" Function**:
  - Add "Move to..." context menu item
  - Implement visual dialog for destination folder selection
  - Map to `provider_rename` (Provider side move)
- [ ] **Advanced Drag & Drop**:
  - **Nested Drop**: Drop file onto a folder row to move it there
  - **Panel-to-Panel**: Drag form Remote <-> Local for direct transfer (Upload/Download)
  - Visual feedback ("ghost" image dragging)
- [ ] **Smart Disconnect Policy**:
  - Disable aggressive Keep-Alive for stateless providers (MEGA, Local)
  - Prevent UI from resetting to login screen on transient network errors
- [ ] **Keyboard Shortcuts**:
  - F2 (Rename), Del (Delete), Ctrl+C/V (Copy/Paste planned)


---

## 📋 Sprint 3: SFTP + Encryption (v1.3.0) - PRIORITY

> Based on [Competitor Analysis](docs/COMPETITOR-ANALYSIS.md) - Closing critical gaps

### HIGH PRIORITY - Gap Closure
All major competitors (FileZilla, Cyberduck, WinSCP) have these features:

- [ ] **SFTP Support** ⚡ CRITICAL - Foundation Complete
  - ✅ Created `SftpProvider` stub implementing `StorageProvider` trait
  - ✅ Added `SftpConfig` with password and key authentication fields
  - ✅ Added SFTP to ProtocolSelector (port 22 default, "Secure" badge)
  - ✅ Added SFTP options to TypeScript types (private_key_path, passphrase, timeout)
  - ✅ Prepared Cargo.toml with `russh` dependencies (commented until full impl)
  - [ ] Implement full SSH connection using `russh` crate
  - [ ] Support key-based authentication (id_rsa, id_ed25519)
  - [ ] Support password authentication
  - [ ] Test with OpenSSH servers

- [ ] **Cryptomator Encryption** (like Cyberduck)
  - Implement Cryptomator vault format v8
  - AES-256-GCM encryption layer
  - Filename encryption/obfuscation
  - Support vaults on any provider (FTP, S3, WebDAV, Cloud)

- [ ] **Keyboard Shortcuts**
  - F2 → Rename
  - Del/Backspace → Delete (with confirmation)
  - Ctrl+C/Ctrl+V → Copy/Paste (planned)
  - Enter → Open file/folder
  - Ctrl+R → Refresh

### Multi-Session Architecture ✅ COMPLETED (v1.2.3)
```rust
pub struct MultiProviderState {
    pub sessions: RwLock<HashMap<String, ProviderSession>>,
    pub active_session_id: RwLock<Option<String>>,
}
```

---

## 📋 Sprint 4: UX & Performance (v1.4.0)

### Advanced File Management
- [ ] **Drag & Drop Cross-Panel**
  - Remote → Local = Download
  - Local → Remote = Upload
  - Drop on folder = Move into folder
  - Visual feedback ("ghost" image)

- [ ] **File Versioning** (like Mountain Duck 5)
  - Show version history for S3/cloud providers
  - Restore previous versions
  - Compare versions

- [ ] **Bandwidth Throttling** (like FileZilla)
  - Configurable upload/download speed limits
  - Per-connection or global limits
  - Useful for background sync

- [ ] **Smart Disconnect Policy**
  - Disable Keep-Alive for stateless providers
  - Graceful reconnection on network errors

---

## 📋 Sprint 5: Advanced Features (v1.5.0)

### Automation & CLI
- [ ] **CLI Mode** (like WinSCP)
  - `aeroftp connect user@host`
  - `aeroftp sync /local /remote`
  - Scriptable commands

- [ ] **More Languages**
  - FileZilla has 47 languages
  - Target: German, Portuguese, Japanese, Korean

- [ ] **Azure Blob Storage**
  - Already in Cyberduck
  - Microsoft ecosystem users

- [ ] **CDN Integration**
  - CloudFront invalidation
  - Presigned URL generation

---

## 🔧 Technical Notes

### WebDAV Implementation
- Use `reqwest` with custom headers for WebDAV methods (PROPFIND, MKCOL, MOVE, COPY)
- Parse XML responses with `quick-xml`
- Support both HTTP and HTTPS
- Handle Nextcloud/ownCloud specific extensions

### S3 Implementation
- Use official `aws-sdk-s3` for maximum compatibility
- Support custom endpoints for MinIO, Backblaze B2, R2, etc.
- Handle large file uploads with multipart
- Support presigned URLs for sharing

### Cross-Platform Considerations
- All providers must work on Linux, macOS, Windows
- Avoid platform-specific APIs
- Test on all platforms before release
- Snap/Flatpak compatibility (no direct filesystem access for OAuth tokens - use XDG portal)

---

## 📁 File Structure After Sprint 1

```
src-tauri/src/
├── lib.rs                 # Main Tauri commands
├── main.rs                # Entry point
├── providers/             # NEW: Storage providers
│   ├── mod.rs             # Provider trait + registry
│   ├── types.rs           # Shared types
│   ├── ftp.rs             # FTP provider
│   ├── webdav.rs          # WebDAV provider
│   └── s3.rs              # S3 provider
├── cloud_config.rs        # AeroCloud configuration
├── cloud_service.rs       # AeroCloud sync (updated for providers)
├── sync.rs                # File comparison logic
├── watcher.rs             # File system watcher
├── ai.rs                  # AI features
└── pty.rs                 # Terminal (Unix only)
```

---

## 🔗 References

- [Cyberduck](https://cyberduck.io/) - Feature inspiration
- [AWS S3 SDK for Rust](https://docs.aws.amazon.com/sdk-for-rust/)
- [WebDAV RFC 4918](https://datatracker.ietf.org/doc/html/rfc4918)
- [Cryptomator Vault Format](https://docs.cryptomator.org/en/latest/security/vault/)

---

## 📊 Compatibility Matrix (Current v1.2.7)

| Provider     | Browse | Upload | Download | Sync | Share | Status |
| ------------ | ------ | ------ | -------- | ---- | ----- | ------ |
| FTP/FTPS     | ✅      | ✅      | ✅        | ✅    | ❌     | Stable |
| SFTP         | 📋      | 📋      | 📋        | 📋    | ❌     | Foundation ✅ |
| WebDAV       | ✅      | ✅      | ✅        | ✅    | 📋     | Stable |
| S3           | ✅      | ✅      | ✅        | ✅    | 📋     | Stable |
| Google Drive | ✅      | ✅      | ✅        | 📋    | ✅     | Stable |
| Dropbox      | ✅      | ✅      | ✅        | 📋    | ✅     | Stable |
| OneDrive     | ✅      | ✅      | ✅        | 📋    | ✅     | Beta |
| MEGA.nz      | ✅      | ✅      | ✅        | 📋    | ❌     | Stable |

Legend: ✅ Done | 📋 Planned | ❌ Not Applicable

### Unique Features vs Competitors

| Feature | AeroFTP | FileZilla | Cyberduck | WinSCP |
|---------|---------|-----------|-----------|--------|
| MEGA.nz Support | ✅ | ❌ | ❌ | ❌ |
| AeroCloud Sync | ✅ | ❌ | ❌ | ❌ |
| Monaco Editor | ✅ | ❌ | ❌ | Basic |
| AI Assistant | ✅ | ❌ | ❌ | ❌ |
| Integrated Terminal | ✅ | ❌ | ❌ | PuTTY |

See [docs/COMPETITOR-ANALYSIS.md](docs/COMPETITOR-ANALYSIS.md) for full comparison.
