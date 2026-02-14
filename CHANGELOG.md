# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [2.1.0] - 2026-02-14

### AeroSync Phase 2 — Operational Reliability

Enterprise-grade sync hardening with transfer journal for checkpoint/resume, SHA-256 checksum verification during scan, structured error taxonomy with 10 error categories, post-transfer verification policies, and configurable retry with exponential backoff. Plus session tab context menu, improved certificate warning UX, and Filen 2FA compatibility fix.

#### Added

- **Transfer journal with checkpoint/resume**: Persistent JSON journal in `~/.config/aeroftp/sync-journal/` tracks every sync operation. Interrupted syncs can be resumed from where they left off, with automatic detection of incomplete journals on next sync
- **SHA-256 checksum during scan**: When `compare_checksum` is enabled, local files are hashed with streaming SHA-256 (64KB chunks) during the scan phase for accurate content-based comparison
- **Structured error taxonomy**: 10 error categories (Network, Auth, PathNotFound, PermissionDenied, QuotaExceeded, RateLimit, Timeout, FileLocked, DiskError, Unknown) with automatic classification from raw error messages and retryability hints
- **Post-transfer verification**: 4 verification policies (None, Size, Size+Time, Full) applied after each download to confirm transfer integrity
- **Configurable retry with exponential backoff**: Per-file retry policy with base delay, max delay cap, backoff multiplier, and per-file timeout. Default: 3 retries, 500ms base, 2x multiplier, 10s cap, 2min timeout
- **Journal resume banner**: Amber notification banner when an interrupted sync is detected, showing completion progress and last update time, with Resume and Dismiss actions
- **Error breakdown in sync report**: Post-sync report groups errors by category with dedicated icons, showing retryable vs non-retryable counts
- **Verify and retry policy dropdowns**: New UI controls in sync options for selecting verification and retry policies before starting sync
- **Session tab context menu**: Right-click on session tabs for Close Tab, Close Other Tabs, and Close All Tabs actions with drag-and-drop reordering preserved
- **Insecure certificate confirmation modal**: Replaced double `window.confirm()` dialogs with a styled modal featuring ShieldAlert icon, risk explanation, and clear Accept/Cancel buttons
- **12 Rust unit tests**: Full test coverage for error classification, retry policy delay calculation, journal resumability, and file verification
- **23 new i18n keys**: Journal, verification, retry, and error taxonomy strings translated in all 47 languages

#### Fixed

- **Filen 2FA login compatibility**: Always send `twoFactorCode` field in login request with `"XXXXXX"` default when 2FA is not enabled, matching Filen API v3 requirements
- **Main content overflow**: Fixed scroll behavior on connection screen transition by using consistent `overflow-auto` class

---

## [2.0.11] - 2026-02-13

### Complete i18n for Connection Intelligence

Translated all 14 new connection logging and security badge keys across 45 languages with native quality. Placeholder corrections applied automatically via merge script.

#### Added

- **Full i18n coverage for connection step logging**: DNS resolution, TLS/SSH/HTTPS establishment, authentication, and directory listing messages translated in all 47 languages
- **Full i18n coverage for security badges**: Secure connection, insecure connection, and update badge labels translated in all 47 languages

#### Fixed

- **Placeholder alignment**: Corrected 360 placeholder mismatches ({host}→{hostname}, {cipher}→{mode}, {user}→{username}, etc.) from external translation tool output

---

## [2.0.10] - 2026-02-13

### Connection Intelligence, Security Badges & Windows Build Fix

Enhanced connection logging with DNS resolution and protocol-level step details, redesigned StatusBar security badges, and fixed Windows build regression from suppaftp 8.0.2.

#### Added

- **DNS resolution in Activity Log**: Hostname resolution with resolved IP address displayed before connection attempt, similar to FileZilla's connection log
- **Protocol step logging**: TLS/SSH/HTTPS establishment, authentication method (password or SSH key), and directory listing completion now logged as individual activity entries
- **Secure connection badge**: Green ShieldCheck badge in StatusBar showing protocol abbreviation (TLS/SSH/HTTPS/E2EE) when connected via encrypted channel
- **3-tier connection security model**: StatusBar now distinguishes `insecure` (plain FTP, red), `warning` (FTPS without cert verification, amber), and `secure` (all encrypted protocols, green)
- **New Rust command `resolve_hostname`**: Async DNS resolution via `tokio::net::lookup_host` for pre-connection hostname lookup
- **14 new i18n keys**: 4 StatusBar security keys and 10 Activity Log connection step keys, translated in English and Italian

#### Changed

- **StatusBar insecure badge**: Now icon-only (AlertTriangle) with full explanation in tooltip, reducing visual noise
- **StatusBar update badge**: Shortened from "Update Available" to just "Update" with full text in tooltip

#### Fixed

- **Windows build failure**: Pinned `suppaftp` to `=8.0.1` to avoid `std::os::fd::AsFd` Unix-only API regression in v8.0.2 that broke Windows compilation

---

## [2.0.9] - 2026-02-13

### Smart Auto-Update with Platform-Native Installation

Enhanced the in-app update system with platform-aware install capabilities, smarter asset availability detection, and complete i18n coverage for update UI strings.

#### Added

- **Platform-native update installation**: One-click Install & Restart for `.deb` (via `pkexec dpkg -i`), `.rpm` (via `pkexec rpm -U`), and `.msi`/`.exe` (Windows launcher) — no more manual file manager step
- **Asset availability detection**: When a new GitHub release exists but the CI-built artifact for the installed format is not yet available, the update check silently retries every hour instead of showing a premature notification
- **StatusBar update re-trigger**: Clicking the purple "Update Available" badge in the status bar re-opens the download toast if previously dismissed
- **Skip for now button**: Explicit "Later, not now" dismiss option in both pre-download and post-download states
- **2 new i18n keys**: `update.openInstaller` and `update.skipForNow` translated in all 47 languages with native quality (Armenian manually verified)

#### Changed

- **Update toast UX redesigned**: Clearer state progression (Ready → Downloading → Complete → Error), wider layout, secondary dismiss actions, and error state now shows both Retry and Open Folder options
- **Version detection from runtime**: `check_update` now reads version from `app.package_info()` instead of compile-time `env!("CARGO_PKG_VERSION")`, ensuring correct detection after in-place updates

#### Fixed

- **Greek `openInstaller`**: Was "Άνοιγμα Installer" (English leak), now "Άνοιγμα εγκαταστάτη"
- **Danish `openInstaller`**: Was "Åbn installer" (English leak), now "Åbn installationsprogram"
- **22 languages `skipForNow`**: Abbreviated translations (e.g. "Später", "Později", "後で") expanded to full casual form matching English "Later, not now"

---

## [2.0.8] - 2026-02-13

### Security Hardening Completion, Full i18n Coverage, and Media Startup Stabilization

Completed security hardening remediations, finalized all locale keys (including Armenian), and stabilized Linux media first-play behavior with buffered startup.

#### Changed

- **SEC-P3-02 completed**: app settings (`aeroftp_settings`) now use vault-first storage (`app_settings`) with one-way idempotent migration and plaintext cleanup during writes
- **SEC-P1-04 hardening baseline landed (compatibility-first CSP)**: explicit `csp` and `devCsp` profiles added in Tauri security config while retaining `dangerousDisableAssetCspModification: true` as a controlled baseline
- **Settings load/save flow unified**: `useSettings`, `SettingsPanel`, and `App` no longer rely on direct localStorage writes for core app preferences
- **Overwrite policy source aligned**: overwrite behavior now reads in-memory settings state instead of reading `aeroftp_settings` directly in transfer hooks
- **Security UX surfaced in protocol/status UI**: insecure connection labels and certificate confirmation copy are now present and localized across all supported locales
- **Media startup flow hardened**: `AudioPlayer` now queues first play during buffering and auto-starts when minimum prebuffer is available, avoiding the Linux “first click no start” race
- **Roadmap status updated**: SEC-P3-02 marked completed in `docs/dev/ROADMAP.md`

#### Added

- **Security regression coverage for settings vault migration**: new `Settings vault migration` check in `scripts/security-regression.cjs`

#### Verified

- **Security regression suite**: `npm run security:regression` passed (Terminal denylist, Host-key fail-closed, OAuth/settings leak guard, Settings vault migration, Plugin sandbox constraints)
- **Build + CSP baseline sanity**: `npm run build` completed after CSP config introduction without build-time regressions
- **i18n validation complete**: all required activity + security keys are present across all locales, including final Armenian (`hy`) completion and placeholder integrity checks
- **Media behavior check**: first Play action now transitions into visible prebuffer state and starts automatically when buffered threshold is reached

---

## [2.0.7] - 2026-02-12

### Translation Quality Audit and Linguistic Integrity

Comprehensive quality audit across all 47 languages, eliminating 605 silent intruder keys (untranslated values left in English without markers), fixing placeholder format inconsistencies, restoring Armenian script from Latin romanizations, and applying native speaker corrections for Chinese.

#### Fixed

- **605 silent intruder keys eliminated**: Systematic audit discovered 605 translation keys across 46 non-English locales that contained English text without `[NEEDS TRANSLATION]` markers. All replaced with proper native translations across 30+ namespaces (common, connection, migration, masterPassword, overwrite, settings, toast, transfer, ai, statusbar, and more)
- **Armenian (hy) script restoration**: 63 keys contained Latin romanizations instead of Armenian Unicode script (e.g., "Chegharkel" instead of the proper Armenian form). Reverse-transliterated using phonetic mapping with manual overrides for aspirated consonants and loanwords. Additional 53 English-identical keys translated to proper Armenian
- **Chinese (zh) native review corrections**: 16 corrections applied from native speaker review — 12 previously untranslated keys translated, 3 placeholder format fixes, 1 improved translation
- **Placeholder format standardized**: 173 instances of `{{param}}` (double-brace) corrected to `{param}` (single-brace) across all 46 locale files in 4 keys. The custom i18n system uses single-brace interpolation
- **5 connection keys translated in 45 languages**: `accessKeyId`, `secretAccessKey`, `selectSshKey`, `sshKeys`, and `megaPasswordPlaceholder` were systematically left in English across nearly all locales — now properly translated
- **11 orphaned keys removed**: Extra keys in ja/ko/zh (masterPassword and migration duplicates) that had no corresponding entries in en.json cleaned up

#### Changed

- **metainfo.xml languages list**: Updated to reflect actual 47 supported languages (removed phantom entries for languages never shipped, added missing ones)
- **metainfo.xml description**: Corrected language count from "51 languages with RTL" to "47 languages (all LTR)" — RTL locales were removed in v2.0.6
- **TRANSLATIONS.md**: Added comprehensive batch translation workflow documentation with language group splitting strategy, non-Latin script handling guidelines, and scripts reference table documenting ~6,350 translations applied

---

## [2.0.6] - 2026-02-11

### Theme System, Security Toolkit, Complete i18n Coverage

Four built-in themes (Light, Dark, Tokyo Night, Cyberpunk), Security Toolkit with Hash Forge / CryptoLab / Password Forge, terminal and Monaco editor theme synchronization, and comprehensive i18n pass with 360 new keys across 47 languages.

#### Added

- **4-Theme System**: Light, Dark, Tokyo Night, and Cyberpunk themes with CSS custom properties and `data-theme` attribute. Theme toggle cycles through all four. Auto mode follows OS preference (light/dark). Themed PNG icons for connection screen logo
- **Security Toolkit** (Cyberpunk theme): 3-tab modal with Hash Forge (MD5, SHA-1, SHA-256, SHA-512, BLAKE3 — text and file hashing with timing-safe compare), CryptoLab (AES-256-GCM and ChaCha20-Poly1305 text encryption with Argon2id key derivation), and Password Forge (CSPRNG random passwords + BIP39 passphrases with entropy calculation). 8 new Rust backend commands
- **Terminal theme auto-sync**: Terminal theme now follows the app theme automatically (Light→Solarized Light, Dark→GitHub Dark, Tokyo Night→Tokyo Night, Cyberpunk→Cyber). New `cyber` terminal theme with neon green palette. Manual theme override preserved via `userOverrideRef` pattern
- **Monaco Cyber theme**: Neon green syntax highlighting on deep black background, matching the Cyberpunk app theme. Registered alongside existing light/dark/tokyo-night themes
- **360 new i18n keys** across 16 sections: `preview.*` (104 keys — audio player, video player, image viewer, PDF viewer, text viewer, mixer, WebGL), `activityPanel.*` (20 keys — log filters, badges, controls), `transfer.*` (9 keys — queue actions, status labels), `ai.toolLabels.*` (31 keys — all 31 AI tool display names), `ai.toolApproval.*` (10 keys — approval dialog actions), `ai.thinking.*` (16 keys — animated thinking status variants), `ai.error.*` (4 keys), `protocol.*Tooltip` (14 keys — protocol selector tooltips for all 14 protocols), `connection.oauth.*` (16 keys — OAuth connect flow), `connection.fourshared.*` (16 keys — 4shared connect flow), `ui.language.*` (14 keys — language selector), `ui.session.*` (8 keys — session tab labels), `permissions.*` (8 keys — chmod dialog), `error.*` (4 keys — error boundary), `devtools.*` (7 keys — DevTools panel), `savedServers.*` (8 keys — server display templates), `vault.*` (7 keys — vault panel), `cryptomator.*` (4 keys), `cloud.*` (5 keys — cloud panel), `settings.*` (22 keys — placeholders and alerts), `toast.*` (15 keys — notification messages)
- **Full translations for all 47 languages**: Bulgarian, Bengali, Catalan, Czech, Welsh, Danish, German, Greek, Spanish, Estonian, Basque, Finnish, French, Galician, Hindi, Croatian, Hungarian, Armenian, Indonesian, Icelandic, Italian (manual), Japanese, Georgian, Khmer, Korean, Lithuanian, Latvian, Macedonian, Malay, Dutch, Norwegian, Polish, Portuguese, Romanian, Russian, Slovak, Slovenian, Serbian, Swedish, Swahili, Thai, Filipino, Turkish, Ukrainian, Vietnamese, Chinese Simplified
- **`ai.thinking` converted from string to object**: 16 animated thinking status variants — each translated into all 47 languages
- **BLAKE3 hashing**: New `blake3 = "1"` Cargo dependency for Security Toolkit file hashing

#### Changed

- **About dialog credits**: Replaced AI model attribution with technology stack display ("Rust + React 18 + TypeScript")
- **Preview components i18n** (8 files): `AudioPlayer.tsx`, `VideoPlayer.tsx`, `ImageViewer.tsx`, `PDFViewer.tsx`, `TextViewer.tsx`, `AudioMixer.tsx`, `WebGLVisualizer.tsx`, `UniversalPreview.tsx` — all hardcoded labels replaced with `t()` calls
- **App.tsx i18n**: ~40 hardcoded toast notifications, SortableHeader labels, and `|| 'Fallback'` patterns replaced with proper `t()` calls
- **DevToolsV2 + AIChat i18n**: Panel tooltips, empty states, thinking status, tool errors, image analysis text
- **ToolApproval + BatchToolApproval i18n**: All approval dialog buttons, status labels, and tooltips
- **TransferQueue + ActivityLogPanel i18n**: Queue actions, status labels, filter buttons, badge labels
- **OAuthConnect + FourSharedConnect i18n**: Connection flow text, credential forms, buttons
- **ProtocolSelector + SettingsPanel i18n**: Protocol tooltips for all 14 protocols, form placeholders, alerts
- **SavedServers i18n**: Display templates for OAuth, E2E, S3, WebDAV entries, drag tooltip, errors
- **VaultPanel + CryptomatorBrowser i18n**: Security badges, file operation toasts, breadcrumb, decrypt button
- **CloudPanel + UI components i18n**: LanguageSelector, CustomTitlebar, SessionTabs, PermissionsDialog, ErrorBoundary

#### Fixed

- **Monaco Editor 404 in dev mode**: Vite plugin `copyMonacoAssets()` now serves Monaco AMD assets from `node_modules` via dev server middleware, fixing `loader.js` 404 errors during development
- **ActivityLogPanel `LogEntryRow` missing `t()`**: Added `useTranslation()` hook call inside memoized component
- **VaultPanel/CryptomatorBrowser destructuring**: `const { t } = useTranslation()` corrected to `const t = useTranslation()`
- **useCloudSync undefined `tr`**: Fixed by destructuring `t: tr` from `callbacksRef.current`
- **SettingsPanel type error**: Optional fields now use `|| ''` fallback for TypeScript compatibility

#### Removed

- **RTL locale files**: Removed `ar.json` (Arabic), `fa.json` (Persian), `he.json` (Hebrew), `ur.json` (Urdu) — RTL layout support not yet implemented. Will be re-added with proper RTL CSS support
- **`|| 'Fallback'` patterns**: ~40 instances removed from `App.tsx` — all keys now exist in `en.json`

---

## [2.0.5] - 2026-02-10

### 4shared Native API, System Startup, Places Sidebar Pro, Windows Explorer Badge

14th cloud protocol: 4shared native REST API with OAuth 1.0 (replacing broken WebDAV), CloudMe WebDAV preset. Desktop integration: Places Sidebar with GVFS network shares and unmounted partitions, autostart on system boot, monochrome tray icon, Windows Explorer badges via Cloud Filter API, OwnCloud removal, and Protocol Selector UX improvements.

#### Added

- **4shared native REST API provider**: Full cloud storage integration via 4shared REST API v1.2 with OAuth 1.0 (HMAC-SHA1) authentication. 15 GB free storage. ID-based file system with folder/file caching, per-entry JSON parsing with `string_or_i64` deserializer for robust API response handling, status filtering (deleted/trashed/incomplete), and `resolve_path()` for relative path navigation across all StorageProvider trait methods
- **OAuth 1.0 signing module**: Reusable `oauth1.rs` with RFC 5849 compliant HMAC-SHA1 signature generation, percent encoding, nonce/timestamp generation, and 3-step token flow (request token → authorize → access token). Zero new Cargo dependencies (reuses existing hmac, sha1, base64, rand, urlencoding)
- **WebDAV HTTP Digest Authentication (RFC 2617)**: Auto-detection of Digest auth for WebDAV servers that require it (e.g., CloudMe). When server responds 401 with `WWW-Authenticate: Digest`, AeroFTP transparently switches from Basic to Digest auth with HMAC-MD5 challenge-response. Password never transmitted, nonce-based replay protection, request integrity via method+URI hashing. Zero new dependencies (reuses existing `md-5` and `rand` crates). CloudMe is the only cloud service requiring exclusively Digest auth — most competing clients (rclone, Joplin, Zotero) do not support it
- **CloudMe WebDAV preset**: Swedish cloud storage with 3 GB free. Pre-configured WebDAV endpoint (`webdav.cloudme.com:443`), Digest authentication auto-detected
- **Places Sidebar: GVFS network share detection**: Network mounts via Nautilus/GIO (SMB, SFTP, FTP, WebDAV, NFS, AFP) now appear in Other Locations with Globe icon, size info, and Eject button. GVFS directory names parsed into friendly display names (e.g. "ale su mycloudex2ultra.local")
- **Places Sidebar: Unmounted partition detection**: Block device partitions not currently mounted (e.g. Windows NTFS) shown in Other Locations with Play button to mount via `udisksctl`. EFI, swap, recovery, and MSR partitions automatically hidden
- **Places Sidebar: EFI partition hidden**: `/boot/efi`, `/boot`, and `/efi` mount points now filtered from volume listing, matching Nautilus behavior
- **Recent locations: individual delete**: Each recent location entry now has an X button on hover to remove it individually (previously only "Clear All" was available)
- **Autostart on system startup**: New `tauri-plugin-autostart` integration with toggle in Settings > General > Startup. Cross-platform: LaunchAgent (macOS), `.desktop` (Linux), Registry (Windows). OS state synced on panel open, idempotent enable/disable with UI rollback on failure. Recommended hint for AeroCloud Sync users
- **Windows Named Pipe IPC server (#102)**: Badge daemon now serves the Nextcloud-compatible protocol over `\\.\pipe\aerocloud-sync` on Windows. Same security measures as Unix: `first_pipe_instance(true)` anti-squatting, `reject_remote_clients(true)` local-only IPC, Semaphore(10) connection limit, sliding window rate limiter (100 query/s), bounded reads (8192 bytes), 60-second idle timeout
- **Windows Cloud Filter API badges (#101)**: Native Explorer sync status icons via `CfSetInSyncState` / `CfRegisterSyncRoot`. Synced files show green checkmark, pending files show sync arrows — no COM DLL required, works on Windows 10 1709+
- **NSIS installer hooks stub (#104)**: `installer/hooks.nsh` prepared for future Shell Icon Overlay COM DLL registration
- **Cross-platform protocol engine**: IPC protocol refactored into generic async functions (`read_line_limited_generic`, `handle_protocol_line_generic`, `handle_client_generic`) using `AsyncBufRead + AsyncWrite` traits — Unix socket and Named Pipe share identical protocol logic
- **Platform-aware badge UI**: SettingsPanel detects Windows and shows "managed automatically via Cloud Filter API" instead of Install/Uninstall/Restart buttons
- **New Windows dependency**: `windows 0.58` crate (conditional `#[cfg(windows)]`) with Cloud Filter, Shell, Foundation, FileSystem, Security features
- **3 new i18n keys**: `settings.startupOptions`, `settings.launchOnStartup`, `settings.launchOnStartupDesc` — translated for Italian, 50 other languages with placeholders

#### Fixed

- **Protocol Selector Edit button**: Clicking "Edit" on a saved server while the protocol dropdown was open now correctly closes the dropdown and shows the connection form
- **Protocol Selector reset on re-open**: Re-clicking the "Select protocol" dropdown while a protocol is already selected now clears the previous selection, preventing desync between internal ProtocolSelector state and parent ConnectionScreen state
- **StatusBar path/quota overlap**: Long remote paths no longer overlap with storage quota display — left section now uses `min-w-0 flex-1` with flexible truncation instead of fixed `max-w-md`
- **GVFS eject support**: Network shares mounted via Nautilus/GIO now correctly unmount with `gio mount -u` instead of `udisksctl`/`umount` (audit fix INT-001)
- **Recent delete scrollbar overlap**: Clear All and per-item delete buttons repositioned with `pr-4`/`right-4` padding to avoid scrollbar overlap when Other Locations is expanded
- **Invisible delete button hit area**: Recent location delete buttons now use `pointer-events-none` when hidden (opacity-0), preventing ghost clicks on the invisible element (audit fix PS-006)
- **lsblk size string fallback**: Unmounted partition size parsing now handles string-type size values from older lsblk versions (audit fix FS-002)
- **EFI mount point `/efi`**: Added systemd-boot EFI path to mount point filter (audit fix FS-008)
- **Security audit (7 findings, all fixed)**: 3-auditor Opus security review — SBA-001 (High: bounded line reader rewritten with `fill_buf()` + `to_vec()` to enforce limits before buffer growth), SBA-002 (Medium: `reject_remote_clients(true)` on all Named Pipe instances), SBA-004 (Medium: 60s idle timeout for connected clients), SBA-005 (Medium: UNC path blocking `\\` in `validate_path()`), GB2-001 (Medium: simplified `CfRegisterSyncRoot` unsafe block), GB2-002 (Medium: `#[cfg(unix)]` on GIO emblem functions), GB2-012/013 (Low: RwLock poison recovery via `unwrap_or_else`)
- **Places Sidebar audit (30 findings, 6 fixed)**: 3-auditor Opus review of GVFS/unmounted/recent code — INT-001 (High: GVFS eject via gio), FS-002 (Medium: lsblk size fallback), FS-004 (Medium: PSEUDO_FS_TYPES/GVFS comment), FS-008 (Low: /efi filter), PS-006 (Low: pointer-events-none), PS-007 (Low: scrollbar alignment)

#### Changed

- **White monochrome tray icon**: Replaced full-color tray icon with standard white monochrome `AeroFTP_simbol_white_120x120.png`, matching the system tray conventions used by Dropbox, Slack, Discord, and other professional desktop apps. Both initial tray icon (`lib.rs`) and badge system base icon (`tray_badge.rs`) updated for consistency
- **Tray badge style**: Removed white border from badge dot for solid color fill matching Ubuntu Livepatch style. Badge position fine-tuned to bottom-right

#### Removed

- **OwnCloud WebDAV preset**: Removed after Kiteworks acquisition — OwnCloud now offers only paid plans with no developer access. Removed from provider registry, ProtocolSelector, ProviderLogos, SavedServers hostname auto-detect, AI system prompt, 51 locale files, README, and snapcraft description. Historical CHANGELOG references preserved

---

## [2.0.4] - 2026-02-10

### Mission Green Badge, Folder Transfer UX and Transfer Reliability

Native overlay badges in Nautilus/Nemo file managers showing AeroCloud sync status — competing directly with Dropbox, OneDrive, and Google Drive. Tray icon with dynamic badge dots including overlay icons (checkmark, sync arrows, X). Major folder transfer UX improvements with cancel support, file-count progress, and path-aware logging. Transfer event system rewritten for zero event loss.

#### Added

- **Badge daemon (`sync_badge.rs`)**: Unix socket server with Nextcloud-compatible IPC protocol, 6 sync states (Synced/Syncing/Error/Ignored/Conflict/New), LRU state tracker (100K entries), Semaphore-based connection limiting (max 10 concurrent), sliding window rate limiter, `read_line_limited()` DoS protection, RwLock poisoning recovery
- **Nautilus Python extension**: `aerocloud_nautilus.py` — `Nautilus.InfoProvider` with custom emblem overlay, thread-safe 5s TTL cache, persistent socket connection, 100ms query timeout, 30s reconnect interval. Supports Nautilus 4.0 (GNOME 43+) with 3.0 fallback
- **Nemo Python extension**: `aerocloud_nemo.py` — fork for Cinnamon/Linux Mint desktop environments
- **GIO emblem fallback**: `gio set metadata::emblems` support for Thunar (XFCE), PCManFM, and all GIO-based file managers
- **6 custom SVG emblem icons**: `emblem-aerocloud-synced/syncing/error/ignored/conflict/new` — installed to `~/.icons/hicolor/scalable/emblems/`
- **Shell extension installer**: One-click Install/Uninstall buttons in Settings panel with inline feedback banner (replaces native alert dialogs), copies Python extensions + SVG emblems with proper 0644 permissions
- **Restart File Manager button**: After installing shell extensions, one-click graceful restart of Nautilus/Nemo via `nautilus -q` / `nemo -q`
- **Tray icon overlay icons**: White checkmark inside green badge (synced, like Ubuntu Livepatch), sync arrows inside blue badge (syncing), X mark inside red badge (error) — pixel-level rendering with distance-based line rasterization, proportional to any icon size
- **Sync pipeline integration**: `start_background_sync()` auto-starts badge server + registers sync root; `stop_background_sync()` stops server + clears states; `background_sync_worker()` updates directory states (Syncing before sync, Synced/Error after)
- **Folder transfer cancel**: Cancel button now aborts in-progress folder downloads via `cancel_flag` in provider state, with proper cleanup of all file-level log entries and queue items
- **Folder transfer progress**: File-count mode (X/Y files) with folder icon, amber gradient progress bar, and smart path truncation in progress toast
- **Path-aware transfer logging**: Full file paths tracked through transfer events (`path` field on TransferEvent/TransferProgress), displayed in activity log and transfer queue with smart truncation
- **Copy buttons**: Hover copy button on activity log entries and transfer queue items (both inline and context menu)
- **Standalone Linux installer**: `shell_integration/linux/install.sh` for manual installation outside the app
- **8 new Tauri commands**: `start_badge_server_cmd`, `stop_badge_server_cmd`, `set_file_badge`, `clear_file_badge`, `get_badge_status`, `install_shell_extension_cmd`, `uninstall_shell_extension_cmd`, `restart_file_manager_cmd`
- **New Rust dependencies**: `image = "0.25"` (tray badge generation), `libc = "0.2"` (Unix UID for socket path)

#### Fixed

- **Transfer event re-subscription race**: `useTransferEvents` rewritten with subscribe-once pattern and ref-based callback access — eliminates micro-gaps where events could be lost during React re-renders
- **Folder transfer cancel cleanup**: Cancelled folder transfers now properly clean up all file-level log entries and queue items (previously left orphaned entries)
- **Duplicate sync completion logs**: `useCloudSync` now updates the existing "Syncing..." log entry on completion instead of creating a duplicate
- **CloudPanel empty server list**: Was reading from `localStorage` directly instead of vault after v1.9.0 migration — fixed with `secureGetWithFallback`
- **Transfer queue file tracking**: Path-based composite keys (`transferId:path`) prevent confusion with duplicate filenames across subdirectories during folder transfers
- **Stop All button**: Now cancels active backend transfers (invokes `cancel_transfer`) in addition to stopping the queue
- **Security audit (43 findings, all fixed)**: 2 critical (unbounded read_line DoS, connection counter never decremented), 6 high (PID instead of UID, missing absolute path validation, RwLock panic on poisoning, newline injection in protocol, bare `except:` in Python, emblem name divergence), 12 medium (double PNG encode/decode, TOCTOU in socket cleanup, pub visibility too broad, rate limiter bypass), 14 low + 10 info

#### Changed

- **Transfer progress bar position**: Moved up from `bottom-4` to `bottom-12` to avoid overlapping with status bar
- **TransferEvent type**: Added `scanning` event type, `path` field, and `total_files` field for folder-level progress tracking
- **Activity log warning style**: Changed from orange/running to green/success status for normal operational warnings

---

## [2.0.3] - 2026-02-09

### Production Rendering Fix, Monaco AMD & WebKitGTK Hardening

Critical fix for production builds (.deb, .AppImage, .rpm): all rendering was broken due to Tauri 2 CSP nonce injection silently overriding `unsafe-inline`, blocking 81+ dynamic stylesheets, Web Workers, WebGL shaders, and IPC calls. This release resolves the root cause and switches Monaco Editor to AMD loading for full WebKitGTK compatibility.

#### Fixed

- **Critical: CSP nonce injection breaking all rendering** — Tauri 2 injects nonces into Content-Security-Policy which per CSP spec overrides `unsafe-inline`, silently blocking ALL dynamically created `<style>` elements (Monaco editor, xterm.js, Tailwind), Web Workers, WebGL shader compilation, and blob URLs. Resolved by removing CSP and setting `dangerousDisableAssetCspModification: true`
- **Monaco Editor workers SyntaxError** — ESM blob proxy approach failed because `importScripts()` cannot parse ES module `import` syntax. Switched to AMD approach: Vite plugin copies `monaco-editor/min/vs/` (IIFE format) to `dist/vs/` at build time, eliminating all worker errors
- **Terminal (xterm.js) no colors/cursor in production** — CSP blocked all dynamic style injection. Additionally set `allowTransparency: false` and `drawBoldTextInBrightColors: true` for WebKitGTK compatibility
- **HTML Preview CSS not rendering** — CSP blocked inline styles and CDN stylesheets. Removed iframe `sandbox` attribute that restricted CSS loading
- **AeroPlayer WebGL visualizer not rendering** — CSP blocked shader compilation and canvas operations. Now fully functional with all 14 visualizer modes
- **WebKitGTK canvas rendering artifacts** — Added `WEBKIT_DISABLE_DMABUF_RENDERER=1` environment variable before WebKit initialization on Linux

#### Changed

- **Monaco loading: ESM to AMD** — Replaced ESM worker blob proxy (`monacoSetup.ts`) with AMD asset copy approach. Workers now use IIFE format files from `min/vs/` directory, compatible with all WebKitGTK versions
- **Vite config simplified** — Removed `worker.format` and `manualChunks` config, replaced with `copyMonacoAssets()` plugin that copies Monaco AMD files to `dist/vs/` at build time

#### Removed

- **Dead code: `monacoSetup.ts`** — ESM worker proxy module no longer imported after AMD switch
- **Devtools in production** — Removed `window.open_devtools()` auto-open and `devtools` feature flag from Cargo.toml

---

## [2.0.2] - 2026-02-08

### AeroFile Pro, Security Hardening & AI Provider Audit

AeroFTP v2.0.2 brings the AeroFile local file manager to professional-grade with Quick Look, Trash Browser, and Enhanced Properties, alongside a massive security audit that resolved 70 findings across AeroAgent, AI providers, and AeroFile systems.

#### Added

- **Spacebar Quick Look**: Finder-style instant preview overlay for images, video, audio, code, and markdown — activated with Space, navigate with arrow keys, close with Escape
- **Recent Locations**: Sidebar section showing last 10 visited folders with one-click navigation, clear button, and localStorage persistence
- **Trash Browser**: Full trash management with table view showing original path, deletion date, and restore/empty actions — soft delete replaces permanent delete by default
- **Enhanced Properties Dialog**: 3-tab interface (General, Permissions, Checksum) with extended metadata including timestamps (created/modified/accessed), owner:group, symlink target, inode, hard links, and permission matrix
- **Folder Size Calculation**: Recursive size calculation via context menu "Calculate Size" with in-session caching for instant re-display
- **4-Algorithm Checksum**: MD5, SHA-1, SHA-256, and SHA-512 on-demand calculation with copy buttons in Properties dialog
- **Disk Usage Treemap**: Visual disk usage analysis with interactive treemap, depth control, and file type breakdown
- **Duplicate Finder**: Find and manage duplicate files by content hash with batch delete and size recovery display
- **6 new Rust commands**: `get_file_properties`, `calculate_folder_size`, `delete_to_trash`, `list_trash_items`, `restore_trash_item`, `empty_trash` in filesystem.rs
- **Alt+Enter shortcut**: Open Properties dialog for selected file
- **31 new i18n keys**: trash, quickLook, properties, sidebar, and contextMenu sections across all 51 languages

#### Fixed

- **AeroAgent security audit** (22 fixes across 13 files, grade B- to A-): HTTP client singletons with timeouts, Gemini API key sanitization in errors, token counts in OpenAI-compatible streaming, plugin tool name hijacking prevention, plugin process kill on timeout, disabled plugin execution guard, fail-closed tool validation, Ollama non-streaming endpoint fix, agent memory prompt injection defense, default models for Asian providers
- **AeroFile security audit** (48 fixes across 16 files, grade C+ to A-): Command injection in `eject_volume`, path traversal in `restore_trash_item`, resource exhaustion protections (depth/entry limits), symlink safety (`symlink_metadata` consistently), `validate_path` for all filesystem commands, blob URL memory leak fixes, async race condition prevention, stale scan result prevention, iframe sandbox hardening, filename validation in inline rename
- **QuickLook race condition**: Per-load cancellation flag prevents wrong file content from rapid navigation
- **QuickLook sorted file mapping**: Space handler now uses sorted file list for correct visual-to-data mapping
- **QuickLook media playback**: Spacebar no longer closes overlay during video/audio playback
- **QuickLook directory skip**: Arrow navigation skips directory entries automatically
- **TextViewer iframe security**: Removed `allow-scripts` from sandbox attribute
- **ImageThumbnail OOM protection**: Added 5MB size limit for thumbnail loading
- **DiskUsageTreemap stale results**: Scan ID pattern prevents applying results from previous directory
- **DuplicateFinderDialog confirmation**: Added confirmation dialog before batch delete operations
- **usePreview memory leak**: Blob URLs properly revoked on component unmount
- **Keyboard shortcuts in contentEditable**: Shortcuts no longer fire while typing in rich text fields
- **changeLocalDirectory validation**: Failed navigation no longer corrupts recent locations list
- **Inline rename validation**: Rejects path separators, traversal patterns, and null bytes in filenames
- **Folder size stale closure**: Ref pattern prevents reading outdated state in useCallback
- **Macro step danger bypass**: High-danger tools now require approval even within macros
- **Anthropic multi-block response**: All text blocks collected instead of only the first
- **Gemini thinking transition**: `thinking_done` event properly emitted on thought-to-content transition
- **AI deny-list expansion**: Added `~/.ssh/`, `~/.gnupg/`, `~/.aws/`, `~/.kube/`, `~/.config/gcloud` to blocked paths
- **local_read memory efficiency**: Reads only needed bytes instead of full file up to 10MB
- **Kimi upload path validation**: Null byte, traversal, and system path checks before file upload

#### Changed

- **Plugin danger level enforcement**: All plugin tools forced to `medium` minimum danger level (both Rust and TypeScript)
- **OpenAI strict mode restriction**: `strict: true` now only sent to providers that document support (OpenAI, xAI, OpenRouter)
- **Dead code cleanup**: Removed unused `highlightSearchMatches`, hardcoded English labels replaced with i18n

---

## [2.0.1] - 2026-02-08

### Vault Unlock & Provider Expansion

#### Fixed

- **Saved servers empty after vault unlock**: Fixed race condition where saved servers list appeared empty after entering master password — servers were loaded before the vault was fully unlocked; now the list refreshes automatically after unlock

#### Added

- **3 Asian AI providers**: Kimi (Moonshot), Qwen (Alibaba Cloud), and DeepSeek added as direct providers with official SVG logos, provider profiles, and streaming support
- **Custom provider preset**: Added Custom to the provider quick-add buttons for connecting to any OpenAI-compatible endpoint
- **Official SVG provider logos**: All 10 AI providers now display their official brand logos instead of text/emoji icons in AI Settings and chat header

---

## [2.0.0] - 2026-02-07

### AeroAgent Pro — Professional AI Experience

AeroAgent evolves from a capable assistant into a professional-grade AI development companion with 5 enhancement phases: Provider Intelligence, Advanced Tool Execution, Context Intelligence, Professional UX, and Provider-Specific Features.

#### Added

- **Streaming markdown renderer**: Incremental rendering with finalized segments (React.memo, never re-rendered) and in-progress tail. Eliminates flashing during AI responses
- **Code block actions**: "Copy", "Apply", "Diff", and "Run" buttons on every code block in AI responses. "Apply" writes code to the active editor file, "Diff" shows side-by-side comparison before applying
- **Agent thought visualization**: ThinkingBlock component shows Claude/OpenAI/Gemini reasoning process with duration timer, token count badge, and collapsible content
- **Prompt template library**: 15 built-in templates activated with `/` prefix — /review, /refactor, /explain, /debug, /tests, /docs, /security, /optimize, /fix, /convert, /commit, /summarize, /types, /analyze-ui, /performance. Custom templates storable in vault
- **Multi-file diff preview**: PR-style diff panel for reviewing changes across multiple files with per-file checkboxes and unified apply action
- **Cost budget tracking**: Per-provider monthly spending limits with warning thresholds, conversation cost display, and vault-persisted spending records
- **Chat search**: Ctrl+F search overlay with role filtering (all/user/assistant), match highlighting, and keyboard navigation between results
- **Keyboard shortcuts**: Ctrl+L (clear), Ctrl+Shift+N (new chat), Ctrl+Shift+E (export), Ctrl+F (search), Ctrl+/ (focus input)
- **Anthropic prompt caching**: System messages sent with `cache_control: {"type": "ephemeral"}` — cache reads are 90% cheaper, with savings displayed per message in cyan
- **OpenAI structured outputs**: `strict: true` on function definitions with `additionalProperties: false` for OpenAI, xAI, and OpenRouter providers — ensures reliable tool call JSON schemas
- **Ollama model-specific templates**: 8 model family profiles (llama3, codellama, deepseek-coder, qwen, mistral, phi, gemma, starcoder) with tailored prompt styles and optimal temperatures
- **Ollama pull model from UI**: Text input + progress bar in AI Settings to download models directly via `POST /api/pull` with real-time NDJSON streaming progress
- **Ollama GPU memory monitoring**: GPU Monitor panel in AI Settings shows running models, VRAM usage with color-coded bars, and auto-refresh every 15 seconds
- **Gemini code execution**: Parse and render `executableCode` and `codeExecutionResult` response parts with syntax-highlighted code blocks and collapsible output sections
- **Gemini `system_instruction`**: System prompt now passed as top-level `systemInstruction` field instead of in-message, following Google API best practices
- **Gemini context caching**: New `gemini_create_cache` command for caching large contexts (32K+ tokens) with configurable TTL — reduces latency and cost by up to 75% on subsequent requests
- **Thinking budget presets**: 5 presets (Off/Light/Balanced/Deep/Maximum) plus range slider (0-100K tokens) for fine-grained control of AI reasoning depth
- **Provider Intelligence Layer**: Per-provider system prompt profiles with optimized identity, style, and behavior rules for all 7 AI providers
- **DAG-based tool pipeline**: Topological sort of tool calls by path dependencies — independent tools execute in parallel, dependent tools run sequentially
- **Diff preview for edits**: Read-only diff preview in tool approval UI for `local_edit` and `remote_edit` tools (100KB cap)
- **Intelligent tool retry**: `analyzeToolError()` with 8+ error detection strategies and automatic retry suggestions
- **Tool argument validation**: Pre-execution validation via `validate_tool_args` Rust command — checks file existence, permissions, dangerous paths, and size limits
- **Composite tool macros**: Reusable multi-tool workflows with `{{var}}` template variables, max depth 5, new "Macros" tab in AI Settings
- **Tool progress indicators**: Real-time progress bars for long-running tools (upload, download, RAG indexing) via `ai-tool-progress` Tauri events
- **Project-aware context**: Auto-detect project type from 10 markers (Node.js, Rust, Python, PHP, Go, Java, Maven, Gradle, CMake, Make) — injects metadata into AI system prompt
- **File dependency graph**: `scan_file_imports` parses import/require/use statements in 6 languages (JS/TS, Rust, Python, PHP, Go, Java) for context-aware suggestions
- **Persistent agent memory**: `.aeroagent` file per project — AI reads at session start, can write learnings via `agent_memory_write` tool for cross-session knowledge retention
- **Conversation branching**: Fork conversations at any message to explore alternative approaches. Branch selector dropdown with create, switch, and delete operations
- **Smart context injection**: `analyzePromptIntent()` detects task type from user prompt and auto-selects relevant context (git diff, file imports, project info, agent memory) with priority allocation
- **Token budget optimizer**: Dynamic allocation based on model capacity — full/compact/minimal modes with visual segmented bar showing system/context/history/current/available token breakdown
- **Universal Preview syntax highlighting**: Prism.js-powered source code coloring for 25+ file types in the preview modal, with language badge and full text selection support
- **HTML/Markdown render toggle**: Preview modal now offers live rendering for HTML files (iframe with inlined local CSS) and Markdown files (via MarkdownRenderer), plus responsive viewport controls (mobile/tablet/desktop), zoom slider, and browser-open action
- **Image color picker**: Canvas-based pixel color sampling in ImageViewer (cross-platform), with hex value display and clipboard copy

#### Changed

- **AeroAgent tool count**: Expanded from 27 to 28 tools with addition of `agent_memory_write`
- **AI Settings tabs**: Reorganized from 6 to 7 tabs with new "Macros" tab
- **Anthropic API version**: Unified to `2025-04-15` for all Anthropic calls (caching + thinking support)
- **System prompt architecture**: Provider-aware profiles replace one-size-fits-all prompt. Ollama models get family-specific guidance
- **Token info display**: Message footer now shows cache savings (cyan arrow-down icon) when Anthropic prompt caching reduces costs
- **Settings reorganization**: Lock Screen Pattern moved from Security to Appearance tab; Vault Backup moved to dedicated Backup tab with Key icon

#### Fixed

- **Chat search navigation**: `data-message-id` attributes enable scroll-to-match functionality
- **Cost tracking accuracy**: Budget check runs before sending messages, preventing overspend
- **Template detection**: `/` prefix in chat input triggers template selector popup without false positives on regular text
- **Streaming token capture**: Cache creation and cache read tokens now correctly captured from streaming chunks
- **Gemini system prompt**: Previously sent as first user message, now correctly uses `systemInstruction` top-level field
- **Auto-lock timeout persistence**: Timeout was only stored in RAM — now persisted to config file and restored on app restart
- **Auto-lock slider default**: Fixed slider showing 5 minutes when disabled (now correctly shows 0)
- **Auto-lock save flow**: Removed broken "Save Timeout" button, timeout now auto-saves on slider change
- **EyeDropper color picker on Linux**: Hidden when EyeDropper API is unavailable (WebKitGTK), preventing runtime errors

---

## [1.9.0] - 2026-02-07

### AeroAgent Super Powers & Unified Keystore

17 new features spanning autonomous AI agent capabilities, RAG intelligence, extensibility, encrypted vault directories, and enterprise-grade credential security. AeroAgent gains multi-step tool execution, Ollama auto-detection, conversation export, system prompt customization, Monaco/Terminal integration, intelligent context management, workspace RAG indexing, and a custom plugin system. AeroVault v2 adds full directory support with hierarchical navigation and recursive delete. Security is elevated with a unified encrypted keystore, vault backup/restore, a guided migration wizard, and hardened AI/HTTP/URL handling based on dual audit (Claude Opus 4.6 + GPT-5.2-Codex). AeroPlayer completely rewritten with HTML5 Audio + Web Audio API, 10-band EQ, beat detection, and 14 visualizer modes including 6 WebGL GPU shaders.

#### Added

- **Multi-step autonomous tool calls**: AeroAgent can now chain up to 10 sequential tool calls in a single conversation turn. Safe tools execute automatically, medium/high-risk tools pause for user confirmation. Stop button provides immediate cancellation at any step (fixes #18)
- **Ollama model auto-detection**: Cyan "Detect" button in AI Settings queries `GET /api/tags` to discover locally installed Ollama models. Eliminates manual model name entry and validates Ollama connectivity (fixes #19)
- **Sliding window context management**: Token budget set to 70% of model maxTokens. When conversation exceeds budget, older messages are automatically summarized to preserve context while staying within limits (fixes #20)
- **Conversation export**: Download icon in chat header toolbar exports full conversation history as Markdown (.md) or JSON (.json) files, including tool call results and timestamps (fixes #21)
- **Full system prompt editor**: New 5th tab "System Prompt" in AI Settings with toggle switch and textarea for custom system prompts. Custom prompts are prepended to the built-in AeroAgent personality (fixes #22)
- **Agent terminal command execution**: New `terminal_execute` tool dispatches shell commands to the integrated PTY terminal with user confirmation before execution. Danger level: high (fixes #25, #27)
- **Agent-Monaco live sync**: `file-changed` and `editor-reload` custom events automatically reload Monaco editor content after AeroAgent executes `local_edit` or `local_write` tools — no manual refresh needed (fixes #26)
- **Monaco "Ask AeroAgent" action**: New context menu action (Ctrl+Shift+A) sends selected code from Monaco editor to AI chat as prompt context for analysis, explanation, or refactoring (fixes #28)
- **Unified Keystore Consolidation**: Server profiles, AI configuration, and OAuth credentials migrated from localStorage to encrypted vault.db (AES-256-GCM + Argon2id). New `secureStorage.ts` utility provides vault-first reads with automatic localStorage fallback for backwards compatibility (fixes #31)
- **Keystore Backup/Restore**: New `keystore_export.rs` module exports and imports the entire vault as `.aeroftp-keystore` files protected with Argon2id + AES-256-GCM. Security tab UI displays metadata preview and merge strategy selection (overwrite, skip existing, merge) before import (fixes #34)
- **Migration Wizard**: 4-step guided migration (Detect → Preview → Migrate → Cleanup) auto-triggered on first launch when legacy localStorage data is detected. Shows itemized preview of all data to be migrated with per-item toggle, then securely wipes legacy storage after confirmation (fixes #36)
- **RAG integration**: Two new AI tools — `rag_index` scans directories and returns file listing with type/size/preview summaries (33 text extensions, max 200 files); `rag_search` performs full-text case-insensitive search across workspace files. Auto-indexing on path change injects workspace summary into AI context (fixes #47)
- **Plugin system**: Extend AeroAgent with custom tools via JSON manifest (`plugin.json`) + shell scripts. Plugins stored in `~/.config/aeroftp/plugins/`, executed as sandboxed subprocesses with 30s timeout and 1MB output limit. New `plugins.rs` backend module with 4 Tauri commands. 6th tab "Plugins" in AI Settings shows installed plugins with tool badges and danger indicators (fixes #48)
- **AeroVault directory support**: Create nested directories inside encrypted vaults with automatic intermediate directory creation. New `vault_v2_create_directory` command adds `is_dir` manifest entries with encrypted names. VaultPanel now features hierarchical navigation with breadcrumb, "New Folder" button, and directory-aware file listing (fixes #53)
- **AeroVault recursive delete**: Delete files and directories from vaults with full recursive support. New `vault_v2_delete_entries` command removes directories and all their contents in a single operation. Files added to subdirectories via new `vault_v2_add_files_to_dir` command (fixes #54)
- **AeroPlayer media engine rewrite**: Removed Howler.js dependency entirely. Audio playback now uses native HTML5 `<audio>` element with Web Audio API graph — eliminates the play/pause bug on first click, fixes buffer overload for large MP3 files (no more full-file decode into RAM), and enables real EQ processing
- **10-band graphic equalizer**: AeroMixer now controls real Web Audio BiquadFilterNode per frequency band (32Hz-16kHz). Presets (Rock, Jazz, Electronic, etc.) and manual slider adjustments produce audible real-time changes. StereoPannerNode for L/R balance control
- **Beat detection engine**: Onset energy algorithm with circular buffer (43-sample rolling window), adaptive threshold (1.5x average), 100ms cooldown, and exponential decay (0.92 factor). Drives visual effects across all visualizer modes
- **14 visualizer modes**: 8 Canvas 2D modes (bars, waveform, radial, spectrum, fractal, vortex, plasma, kaleidoscope) enhanced with beat-reactive effects, plus 6 new WebGL 2 GPU-accelerated shader modes ported from CyberPulse engine
- **WebGL shader visualizers**: 6 GLSL fragment shaders running on GPU — Wave Glitch (chromatic aberration, data moshing), VHS (tape wobble, RGB split), Fractal Mandelbrot (200-iteration zoom), Raymarch Tunnel (3D ray marching with volumetric fog), Metaball Pulse (smooth distance blending), Particles Explosion (3-layer system with shockwave rings)
- **Post-processing effects**: Vignette overlay with bass-modulated edge darkening, chromatic aberration (RGB channel split) on beat in Cyber Mode, CRT scanlines, glitch effects with forced trigger on beat detection

#### Changed

- **AeroAgent tool count**: Expanded from 24 to 27 tools with addition of `terminal_execute`, `rag_index`, `rag_search`
- **AI Settings tabs**: Reorganized from 4 to 6 tabs with new "System Prompt" and "Plugins" tabs
- **Chat header toolbar**: Added conversation export button (download icon) alongside existing clear and settings buttons
- **Security tab**: Extended with Keystore Backup/Restore section showing vault metadata, last backup date, and export/import buttons
- **AeroPlayer visualizer menu**: Dropdown now shows 8 Canvas 2D modes plus 6 WebGL shader modes separated by divider, with emerald "GL" badge for GPU-accelerated effects
- **AeroPlayer keyboard shortcut**: 'V' key now cycles through all 14 visualizer modes (8 Canvas + 6 WebGL) with wrap-around
- **Audio dependency removed**: `howler` (v2.2.4) and `@types/howler` removed from package.json — one fewer dependency
- **AnalyserNode fftSize**: Increased from 256 to 512 (128 → 256 frequency bins) for higher resolution visualizations
- **Tool approval UI redesign**: Replaced large approval card with compact inline bar — border-left color accent by danger level, tool label + filename inline, small Allow/Reject buttons, expandable args via chevron
- **Collapsible long messages**: Assistant messages exceeding 500 characters are collapsed to 200px max-height with gradient overlay and "Show more" / "Show less" toggle — prevents file content dumps from flooding the chat
- **Multi-step auto-resume**: After user approves a medium/high-risk tool, the agent loop now automatically continues to the next step instead of stopping. Context (aiRequest, messageHistory, modelInfo) is preserved across the approval pause
- **Tool descriptions clarified**: `local_edit` and `remote_edit` descriptions now explicitly state "literal match, not regex" to prevent AI models from sending regex syntax in find parameters

#### Refactored

- **AIChat.tsx modularization**: Monolithic 2215-line component split into 8 focused modules — `aiChatTypes.ts` (types/constants), `aiChatUtils.ts` (pure functions: rate limiter, retry, token window, task routing, tool parsing), `aiChatTokenInfo.ts` (DRY token cost calculation), `aiChatSystemPrompt.ts` (system prompt builder with 13-protocol expertise), `useAIChatImages.ts` (vision image hook), `useAIChatConversations.ts` (conversation persistence hook), `AIChatHeader.tsx` (header component). Main file reduced to ~1436 lines (-35%)
- **Plugin tool approval resolution**: `ToolApproval` and `BatchToolApproval` components now accept `allTools` prop to resolve danger levels for both built-in and plugin tools via `getToolByNameFromAll()`. Previously plugin tools always defaulted to 'medium' danger level regardless of their actual configuration
- **requiresApproval extended**: `requiresApproval()` in `tools.ts` now accepts optional `allTools` parameter for consistent plugin tool resolution, matching `isSafeTool()` signature

#### Fixed

- **AeroPlayer play/pause first-click bug**: AudioContext `"suspended"` state (browser autoplay policy) now properly resumed before play — first click always works
- **AeroPlayer large file buffer overflow**: Removed Howler's full-file `decodeAudioData()` path that loaded 100MB MP3s entirely into RAM as PCM. HTML5 `<audio>` streams natively
- **local_edit BOM handling**: UTF-8 BOM (`\u{FEFF}`) stripped before text matching in both `local_edit` and `remote_edit` — prevents "string not found" on Windows-created files
- **Double confirmation eliminated**: System prompt now instructs AI to call tools directly without asking for permission in natural language — the UI approval prompt handles user confirmation
- **Hardcoded English messages removed**: Tool approval messages ("I want to execute...") replaced with model's own localized response content
- **WebGL context loss recovery**: Auto-fallback from WebGL shader to Canvas 2D visualizer when GPU context is lost, with user notification
- **Chat persist effect blocked on new conversation**: Debounced save effect skipped persistence when `activeConversationId` was null (start of new conversation), causing messages to be lost until component unmount. Removed guard since `persistConversation` already handles null ID by creating a new conversation internally
- **Token cost missing with tokens_used fallback**: `computeTokenInfo` discarded all token info when only `tokens_used` was available (some providers return only total tokens without input/output breakdown). Guard now checks all three token fields before returning undefined

#### Security

- **Credential storage hardened**: All sensitive data (server passwords, API keys, OAuth tokens) now encrypted at rest in vault.db instead of plaintext localStorage
- **Keystore export encryption**: Backup files use independent Argon2id derivation (not the vault master key) so backups remain secure even if vault password is compromised
- **Legacy data cleanup**: Migration wizard securely erases localStorage entries after successful vault migration, leaving no plaintext credential residue
- **Dual security audit remediation**: Claude Opus 4.6 audit (B+ grade, 11 findings) + GPT-5.2-Codex audit (7 findings) — all resolved:
  - AI settings migrated from localStorage to encrypted vault (AA-002)
  - OpenAI API header unwrap replaced with safe error propagation (no panic on invalid keys)
  - HTTP status check before JSON parse for all AI provider responses (clear error messages)
  - URL scheme allowlist (`http:`, `https:`, `mailto:` only) prevents `file://` and `javascript:` abuse
  - Filen 2FA passthrough support (conditional `twoFactorCode` field)
  - Secret logging redacted in Filen provider (6 log lines sanitized)
  - OAuth callback URL decode error propagation (no silent fallback on invalid encoding)
  - Secure delete rewritten: chunked 1 MiB overwrite with `OpenOptions` (no truncate) + random pass
- **Plugin subprocess isolation**: Plugin tools run in separate processes with timeout enforcement, output size limits, and plugin ID validation

#### Performance

- **Circular buffer beat detection**: Replaced Array.shift() O(n) with Float32Array circular buffer O(1) for energy history
- **Particle system cap**: Maximum 200 concurrent particles prevents memory growth during long playback sessions
- **WebGL shader rendering**: GPU-offloaded visualization with pre-allocated Float32Array spectrum buffers — zero per-frame allocations

---

## [1.8.9] - 2026-02-06

### Dynamic Version Info, Credential Fix & Dependency Updates

About dialog now displays all version information dynamically. Fixed critical credential save race condition.

#### Fixed

- **Credential save race condition**: OAuth cloud provider credentials (Google Drive, Dropbox, OneDrive, Box, pCloud) were overwriting each other when saved from Settings. All 10 `store_credential` calls fired simultaneously without `await`, causing concurrent read-modify-write on `vault.db` — only the last writer survived. Now saves sequentially with `await`, matching the pattern used by ConnectionScreen
- **Vault write serialization**: Added `VAULT_WRITE_LOCK` Mutex to Rust credential store backend, serializing all `store()` and `delete()` operations to prevent concurrent write races from any call site

#### Changed

- **Dynamic Rust version**: `rust_version` in About dialog now detected at compile time via `rustc --version` in build.rs instead of hardcoded `"1.77.2+"`
- **Dynamic frontend versions**: React, TypeScript, Tailwind CSS, Monaco Editor, and Vite versions extracted from `package.json` at build time via Vite `define` — no hardcoded values in AboutDialog
- **Crypto deps tracked**: Added 6 security crates to build.rs tracked dependencies and About dialog: `aes-gcm-siv`, `chacha20poly1305`, `hkdf`, `aes-kw`, `aes-siv`, `scrypt`
- **Build Info consolidated**: Merged Frontend Dependencies section into Build Info section in About Technical tab — single clean list for all build stack versions
- **Clipboard text dynamic**: Copy Technical Info now uses the same dynamic version sources as the UI

#### Updated

- **tauri**: 2.10.1 → 2.10.2
- **anyhow**: 1.0.100 → 1.0.101
- **zip**: 7.2.0 → 7.4.0
- **tauri-plugin-single-instance**: 2.3.7 → 2.4.0
- **tauri-build**: 2.5.4 → 2.5.5

---

## [1.8.8] - 2026-02-05

### Security Hardening, Vision AI & Agent Intelligence

Full security audit remediation (6 findings, all fixed), vision/multimodal AI, autonomous panel refresh, and code quality improvements.

#### Security

- **XSS prevention**: AI chat content is now HTML-escaped before markdown rendering — pipeline is `escapeHtml -> renderMarkdown -> formatToolCallDisplay(escapeChipHtml)`. No raw AI HTML reaches the DOM
- **CSP hardened**: Removed `unsafe-inline` and `unsafe-eval` from `script-src` in Content Security Policy
- **ZIP Slip fixed**: ZIP extraction now rejects entries containing `..` path traversal, absolute paths, and Windows drive prefixes
- **AI tool auto-execution restricted**: `local_read`, `local_list`, `local_search` changed from `safe` to `medium` — now require user confirmation before reading local files
- **Asset protocol scope narrowed**: Reduced from `/**` (entire filesystem) to `$HOME/**`, `$APPDATA/**`, `$TEMP/**`
- **OAuth token fallback secured**: When vault is locked, tokens stored in memory only (never written to disk unencrypted). Vault auto-initializes on first OAuth login. `delete_tokens()` clears vault, memory cache, and all legacy files
- **Predictable temp file fixed**: `remote_edit` temp files now use UUID v4 names instead of PID-based patterns
- **Tool chip injection fixed**: Dynamic values in AI tool chip HTML are escaped via `escapeChipHtml()` before interpolation. Tool chips use Tailwind classes instead of inline styles, Unicode instead of inline SVG

#### Added

- **Vision/Multimodal AI**: Attach images to AI chat messages for analysis by vision-capable models (GPT-4o, Claude 3.5 Sonnet, Gemini Pro Vision, Ollama llava)
  - **Image picker**: Select JPEG, PNG, GIF, WebP images from filesystem via native dialog
  - **Clipboard paste**: Paste screenshots or copied images directly into chat
  - **Preview strip**: Attached images shown as thumbnails above the input area with remove buttons
  - **Auto-resize**: Images larger than 2048px automatically downscaled to reduce token cost
  - **Validation**: Maximum 5 images per message, 20 MB per image, supported types enforced
  - **Provider-specific formats**: OpenAI image_url, Anthropic base64 source, Gemini inlineData, Ollama images array
  - **Backwards-compatible**: Text-only messages work identically; images field is optional (serde skip_serializing_if)
  - **No persistence bloat**: Images are not saved in chat history — only displayed during the current session
- **Agent panel refresh**: File panels automatically refresh after AeroAgent executes mutation tools (create, delete, rename, upload, download, edit, archive) — no more manual Ctrl+R needed
  - **15 mutation tools mapped**: remote_delete, remote_rename, remote_mkdir, remote_upload, remote_edit, upload_files, download_files, remote_download, local_write, local_delete, local_rename, local_mkdir, local_edit, archive_create, archive_extract
  - **Target-aware**: Remote mutations refresh remote panel, local mutations refresh local panel, archive operations refresh both
  - **300ms debounce**: Small delay ensures filesystem operations complete before refresh

#### Changed

- **Console.log gating**: 131 console.log statements across 11 files replaced with debug-gated `logger` utility — debug/info are no-ops in production builds, warn/error always active
- **Duplicate consolidation**: Unified `formatBytes` (12 implementations → 1), `formatSize` (6 → 1), `getMimeType` (2 → 1), `UpdateInfo` (3 → 1), `getFileExtension` (2 → 1) into shared utility modules
- **ChatMessage struct**: Extended with optional `images: Vec<ImageAttachment>` field for vision support (Rust backend)
- **AI streaming**: All 4 streaming functions (OpenAI, Anthropic, Gemini, Ollama) use shared helper methods for vision-aware message serialization

---

## [1.8.7] - 2026-02-05

### Ubuntu Verification, UX Polish & macOS Audit

Build verification, compatibility audits, and UX improvements. All 13 protocols, AeroVault v2, Cryptomator, AI Agent, and terminal verified working on Linux and macOS.

#### Added

- **Ubuntu Compatibility Audit**: Full static analysis across 4 domains (paths, security, desktop, build) — certified compatible with Ubuntu 22.04 LTS and 24.04 LTS
- **macOS Compatibility Audit**: Comprehensive 4-domain static analysis (paths/filesystem, UI/clipboard, build/CI, protocols/TLS) — certified compatible with macOS 10.13+ (Intel + Apple Silicon)
- **macOS bundle configuration**: Added `minimumSystemVersion: "10.13"` and entitlements reference in Tauri bundle config
- **macOS hardened runtime**: Entitlements updated with `cs.allow-unsigned-executable-memory: false`, `cs.allow-dyld-environment-variables: false`, Downloads and Documents folder access
- **macOS Edit menu**: Standard menu items (Undo, Redo, Cut, Copy, Paste) as native `PredefinedMenuItem` entries alongside Rename and Delete
- **Empty-area context menu**: Right-click on file panel background shows Paste, New Folder, Refresh, and Select All options (both local and remote panels, list and grid views)
- **Cross-panel paste**: Cut/Copy files in one panel and paste in the other using stored full paths (no longer depends on current directory listing)

#### Fixed

- **macOS keyboard shortcuts**: Normalized `Meta` (Cmd) to `Ctrl` in keyboard handler — both Cmd+C (macOS) and Ctrl+C (Linux/Windows) now trigger the same shortcuts across all 10+ keybindings
- **macOS Finder reveal**: `open_in_file_manager` now uses `open -R` for files to select them in Finder, matching Windows `explorer /select,` behavior
- **Context menu missing on empty space**: File panel context menu only appeared on file rows — added `onContextMenu` handlers to container divs with `data-file-row`/`data-file-card` attributes for proper target detection
- **Cross-panel paste path lookup**: `clipboardPaste` was passing file names to `downloadMultipleFiles`/`uploadMultipleFiles` which looked up in current directory listing — now uses `downloadFile`/`uploadFile` directly with clipboard paths
- **Tab navigation stuck on password toggles**: Added `tabIndex={-1}` to all 3 eye-icon toggle buttons in Settings Security tab (Current Password, New Password, Confirm Password) — Tab now skips toggles and moves to next input field
- **WebDAV self-reference filter**: Removed redundant boolean condition in directory listing that triggered clippy deny-level lint
- **Unused imports cleanup**: Removed 11 unused imports across 6 Rust source files (oauth2, box, pcloud, azure, filen, ai_stream)
- **Dead code removal**: Removed unused `get_remote_files_recursive` function and unused `VaultLocked` error variant

#### Changed

- **MasterPasswordSetupDialog restyled**: Replaced green gradient header with clean AeroFTP modal style (icon + title header, centered shield, full-width submit button with "Encrypting..." spinner, cancel link, security footer)
- **Unlock animation tuned**: Step durations adjusted to 1300ms per crypto phase for optimal pacing with Argon2id timing (~5.2s total before final "Loading credentials" step)
- **LockScreen version badge**: Updated to v1.8.7
- **i18n cleanup**: Removed 10 orphan translation keys from 50 language files; added 3 new context menu keys (`contextMenu.newFolder`, `contextMenu.refresh`, `contextMenu.selectAll`) synced to all 51 languages
- **Dead code annotations**: Added `#[allow(dead_code)]` to serde deserialization structs that need fields for JSON parsing (filen, box, cryptomator, onedrive)

---

## [1.8.6] - 2026-02-05

### Security & Windows Compatibility

**Important**: This release contains critical security improvements and Windows-specific fixes. All users are strongly encouraged to update.

Complete rewrite of the credential storage system. Replaces the dual-mode OS Keyring + Encrypted Vault approach with a single Universal Vault that works identically on all platforms. Master password is now fully optional — credentials are saved and loaded automatically without any user interaction by default.

#### Added

- **Universal Vault**: Single credential backend using `vault.key` + `vault.db` (AES-256-GCM) — no OS keyring dependency
- **Auto mode** (default): 64-byte CSPRNG passphrase stored in `vault.key` with OS file permissions (Unix 0o600, Windows ACL). Credentials available immediately on startup with zero user interaction
- **Master mode** (optional): Passphrase encrypted with Argon2id (128 MiB, t=4, p=4) + AES-256-GCM. User enters master password on app start
- **HKDF-SHA256 key derivation**: High-entropy passphrase (512 bits) derived to 256-bit vault key via HKDF (RFC 5869)
- **Standalone lock button**: Header lock icon works independently — locks app immediately when master password is set, opens setup dialog when not set
- **MasterPasswordSetupDialog**: New standalone modal for enabling master password with password confirmation and auto-lock timeout slider
- **vault.key binary format**: Compact binary format (76 bytes auto / 136 bytes master) with magic bytes, version, and mode detection

#### Changed

- **Credential store architecture**: Single `CredentialStore` with `init()` → `from_cache()` pattern replaces complex fallback chain
- **Tauri commands simplified**: `store_credential`, `get_credential`, `delete_credential` use vault-only backend via `from_cache()`
- **OAuth token storage**: All OAuth providers (Google Drive, Dropbox, OneDrive, Box, pCloud) use Universal Vault instead of OS keyring
- **Settings Security tab**: Shows "Universal Vault (AES-256-GCM)" status, master password management without keyring references
- **ConnectionScreen**: Removed credential error alert dialogs and keyring failure handling — store operations are transparent

#### Removed

- **OS Keyring dependency**: Removed `keyring` crate entirely — no platform-specific credential backend
- **Keyring health monitoring**: Removed `KEYRING_HEALTH`, `mark_keyring_broken()`, `is_keyring_available()`, write-verify pattern
- **Dual backend fallback**: Removed `Backend` enum (OsKeyring/EncryptedVault), migration functions, manifest tracking
- **Credential error dialogs**: Removed `KEYRING_BROKEN_NEED_VAULT_SETUP`, `VAULT_LOCKED` error codes and AlertDialog prompts in ConnectionScreen

#### Fixed

- **Windows credential persistence**: Credentials now persist reliably across app restarts on all platforms (was silently failing on Windows Credential Manager)
- **PowerShell terminal prompt leak**: Terminal no longer shows raw `function prompt` command on startup
- **Flag emojis invisible on Windows**: SVG flag icons (`country-flag-icons`) for consistent cross-platform rendering
- **Transfer toast stuck at 100%**: Race condition fix — late progress events no longer re-show the toast after transfer completion. Added 3-second auto-dismiss safety timer
- **Folder transfers bypass conflict settings**: Folders now respect the "When file exists" setting instead of silently overwriting all files
- **Windows drag & drop broken**: Disabled Tauri 2 WebView2 native drag interception (`dragDropEnabled: false`) that was preventing HTML5 drag & drop on Windows
- **Folder download missing queue counter**: Download folder now emits folder-level progress events matching upload behavior
- **Queue folder mapping race condition**: `start` event handler now matches queue items in both `pending` and `transferring` status, fixing folder counter not appearing when frontend calls `startTransfer` before backend emits `start`

#### Added

- **Folder conflict resolution**: Backend `file_exists_action` parameter enables per-file comparison during folder transfers (size + timestamp with 2s tolerance). Supports skip, overwrite_if_newer, overwrite_if_different, skip_if_identical
- **FolderOverwriteDialog**: New merge strategy dialog for folder transfers in "Ask" mode — Merge & Overwrite, Merge & Skip identical, Merge & Overwrite if newer, Skip folder
- **Transfer Queue improvements**: Context menu (right-click) with Retry, Copy error, Remove actions. Error tooltip on failed items. Header actions (Clear completed, Stop all, Clear all) with animated stop button. Wider popup with RTL filename truncation (always shows file extension)
- **Individual file tracking in queue**: Folder transfers now show every file as a separate queue item with live progress, across all 13 protocols (FTP/FTPS/SFTP/S3/WebDAV/GDrive/Dropbox/OneDrive/Box/pCloud/Azure/MEGA/Filen)
- **Folder progress counter**: Live file counter badge on folder queue row (e.g. 3/9 cyan while transferring, 9/9 green on completion). Works for both upload and download
- **Upload folder smart comparison**: Phase 2.5 pre-indexes remote files before upload, enabling instant skip of unchanged files in large folders
- **Cut, Copy, Paste**: Context menu and keyboard shortcuts (Ctrl+X/C/V) for file operations — cross-panel paste triggers upload/download, same-panel cut moves files, same-panel copy duplicates locally. Backend `copy_local_file` command with recursive directory copy
- **Queue copy to clipboard**: Button to copy full queue contents as formatted text for debugging and sharing

---

## [1.8.4] - 2026-02-04

### Windows Compatibility

Comprehensive Windows audit and fix pass covering 15 platform-specific issues across all severity levels. AeroFTP now runs correctly on Windows 7 through 11.

#### Fixed
- **Local path navigation broken on Windows**: All `.split('/')` calls replaced with cross-platform `split(/[\\/]/)` regex across App.tsx (8 occurrences), SyncPanel, ArchiveBrowser, VaultPanel, and ConnectionScreen
- **Hardcoded `/tmp` path crashes on Windows**: Replaced with `std::env::temp_dir()` (Rust) and `@tauri-apps/api/path` `tempDir()` (frontend) in AI remote_edit and archive preview
- **Keyring probe too aggressive on Windows**: `PlatformFailure` during probe no longer blocks Credential Manager access (transient Windows Hello / lock screen issues)
- **Terminal sends bash commands to PowerShell**: Detect platform and send PowerShell `function prompt` instead of `export PS1` on Windows
- **FTP path backslash mixing**: All `pwd()` results normalized with `.replace('\\', "/")` in connect, cd, cd_up, and pwd operations
- **SFTP private key path uses wrong separator**: Replaced `format!("{}/{}",...)` with `PathBuf::join()` for cross-platform tilde expansion
- **Explorer fails with spaced paths**: Uses `/select,` flag for files and normalizes forward slashes to backslashes on Windows
- **Clipboard potential freeze on Windows**: Clipboard write spawned in separate thread (matching Linux fix)

#### Added
- **Windows ACL file protection**: New `windows_acl` module applies `icacls` restrictions on credential vault and master password files (equivalent to Unix `chmod 0o600`)
- **MEGAcmd path resolution**: Searches `%ProgramFiles%\MEGAcmd\` and `%LOCALAPPDATA%\MEGAcmd\` on Windows when command is not in PATH
- **PowerShell fallback**: Terminal falls back to `%COMSPEC%`/`cmd.exe` if PowerShell is not available (Windows Server minimal installs)
- **Reserved filename validation**: Rejects Windows reserved names (CON, PRN, AUX, NUL, COM1-9, LPT1-9) in local file rename operations
- **Firefox scrollbar support**: Added `scrollbar-width: thin` for cross-browser scrollbar styling
- **Windows font fallback**: Added `Courier New` and `Cascadia Code` to terminal and code editor font stacks

#### Changed
- **Install format detection**: Uses `%ProgramFiles%` and `%ProgramFiles(x86)%` environment variables instead of hardcoded string matching

---

## [1.8.3] - 2026-02-04

### Critical Clipboard Fix

Emergency fix for application freeze on Linux when using Share Link functionality.

#### Fixed
- **Linux clipboard freeze**: Fixed application hang when clicking "Create Share Link" on Linux/X11
  - The `arboard` clipboard `.wait()` method blocked indefinitely waiting for a clipboard manager
  - Moved clipboard persistence to a detached thread with immediate fallback
  - Affected all providers: S3 pre-signed URLs, OAuth share links (Google Drive, Dropbox, OneDrive, Box, pCloud, Filen)

---

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