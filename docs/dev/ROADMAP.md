# AeroFTP Unified Roadmap

> Single source of truth for development planning.
> Ordered by priority and architectural dependencies.
>
> Last Updated: 17 March 2026
> Current Version: v2.9.9

---

## Version History (one-liner per release)

| Version | Date | Summary |
|---------|------|---------|
| **v2.9.9** | 2026-03-17 | CLI vault profiles (`--profile`), FTP/TLS upload truncation fix, WebDAV HTTP 0-byte fix, dual 10-auditor security hardening (Claude + GPT 5.4), OAuth CLI support |
| **v2.9.8** | 2026-03-16 | OpenDrive (22nd protocol), Yandex Disk trash manager, Windows credential persistence fix |
| **v2.9.7** | 2026-03-15 | Yandex Disk (21st protocol), AeroCloud sync ping-pong fix (critical), OAuth UX, file preview extension, AI Agent CLI docs |
| **v2.9.6** | 2026-03-14 | Remote Timestamp Timezone Fix: UTC Z suffix on all providers, CLI help banner |
| **v2.9.5** | 2026-03-13 | Dual-Engine Security Audit Fix & Yandex Object Storage S3 preset |
| **v2.9.1** | 2026-03-08 | Snap Store Fix & UX Polish: Snap metadata redflag fix, MEGA session badge removed (backend auto-reauth), Jottacloud token persistence, Activity Log auto-open |
| **v2.9.0** | 2026-03-07 | Cloud Provider Audit & Security Hardening: 6 GPT-5.4 residual findings resolved, AeroCloud multi-protocol rebrand, AeroVault crate extraction to crates.io |
| **v2.8.9** | 2026-03-07 | AeroVault Crate Extraction: AeroVault v2 engine extracted to standalone `aerovault` crate on crates.io, inline crypto replaced with crate dependency |
| **v2.8.8** | 2026-03-07 | Updater Hotfix & Privacy Policy: Fixed updater URL whitelist (axpdev-lab/aeroftp), added PRIVACY.md for SignPath OSS |
| **v2.8.7** | 2026-03-07 | Security Audit Grade A- & Server Duplication: 45+ audit fixes, updater URL whitelist, CSP hardened, server clone feature |
| **v2.8.6** | 2026-03-05 | Cloud Provider Audit & Security Hardening: 13 provider fixes, upload/delete/CSP hardening |
| **v2.8.5** | 2026-03-05 | pCloud Fix & UX Polish: pCloud auth/upload, OAuth auto-renew, AeroVault Tab fix |
| **v2.8.4** | 2026-03-04 | OAuth Setup & UI Consistency: redirect URI display, S3 subtitles, dark theme modals |
| **v2.7.5** | 2026-03-02 | AeroCloud Multi-Protocol Fix & CLI JSON: SFTP/WebDAV sync fixes, WebDAV root boundary, CLI --json output |
| **v2.7.4** | 2026-02-28 | Complete Provider Integration: Box/Google Drive/OneDrive/Zoho features, PRO badge, Providers dialog |
| **v2.7.3** | 2026-02-27 | UI Stability: Stabilized home intro panels that were resizing in certain circumstances |
| **v2.6.4** | 2026-02-24 | Custom Server Icons & Favicon Detection: icon picker for all servers, auto-favicon from FTP/SFTP, Transfer Queue dismiss-respect + DOM windowing (200 items), concurrent transfers default 5→8, Rust 1.93 clippy compat |
| **v2.6.3** | 2026-02-23 | AeroCloud Tab Switch: provider tab disconnection fix, session protocol routing via `isNonFtpProvider()`, "Check for Updates" menu item restored |
| **v2.6.2** | 2026-02-23 | Amazon S3 Provider: dedicated provider with 28 AWS region presets, bucket name trimming, Azure Blob + Drime Cloud re-enabled for production |
| **v2.6.1** | 2026-02-23 | Unified Titlebar: VS Code-style custom titlebar with React dropdown menus, 147 provider audit findings fixed across 8 cloud providers, consistent modal animations, new default backgrounds (Waves/Isometric) |
| **v2.6.0** | 2026-02-21 | AeroAgent Ecosystem: Plugin Registry (GitHub-based), Plugin Browser UI, Plugin Hooks system, Command Palette (Ctrl+Shift+P), Context Menu AI Actions, AI Status Widget, Drag & Drop to Agent, 4 new AI providers (AI21, Cerebras, SambaNova, Fireworks — 19 total), 19 i18n keys |
| **v2.5.0** | 2026-02-20 | AeroFile Pro: LocalFilePanel extraction, local path tabs (12 max), file tags SQLite (7 labels), keyboard nav, ARIA a11y, event-driven volume watcher, network eject, octal escaping fix, macOS FinderSync .appex, Cryptomator creation, Canary Sync, Signed Audit Log, context menu overhaul, 23 i18n keys |
| **v2.4.0** | 2026-02-19 | Zoho WorkDrive (16th protocol), 12-auditor audit (A-), streaming uploads, SecretString, quick-xml, SFTP TOFU dialog, Polkit update, AeroSync quick wins |
| **v2.3.0** | 2026-02-19 | Chat History SQLite + FTS5 redesign, retention policies, session export/import, 55+ audit findings |
| **v2.2.5** | 2026-02-18 | AI provider polish — macOS CI, Qwen international endpoint |
| **v2.2.4** | 2026-02-18 | Provider Marketplace (15 providers), TOTP 2FA, Remote Vault, CLI, CSP reporter |
| **v2.2.3** | 2026-02-17 | AeroAgent welcome screen, shell_execute backend, i18n structural audit (1188 fixes) |
| **v2.2.2** | 2026-02-17 | AeroAgent File Management Pro (17 new tools, 44 total), Extreme Mode |
| **v2.2.0** | 2026-02-16 | AeroSync Phase 3A+ frontend integration, Speed Modes, Scheduler, Templates, Rollback, 31 security fixes |
| **v2.1.2** | 2026-02-15 | AeroSync Phase 3A: Sync Profiles, Conflict Resolution, Bandwidth, Plugin SHA-256, FS scope |
| **v2.1.0** | 2026-02-14 | AeroSync Phase 2: Transfer journal, SHA-256 scan, error taxonomy, retry, verification |
| **v2.0.x** | 2026-02 | Theme system, Security Toolkit, Places Sidebar Pro, Windows badges, 4shared, CloudMe, CSP fix, AeroFile Pro |
| **v2.0.0** | 2026-02 | AeroAgent Pro 5 phases: Provider Intelligence, Tool Execution DAG, Context Intelligence, Professional UX, Provider Features |
| **v1.9.0** | 2026-01 | Multi-step tools, Unified Keystore, Vault directories, AeroPlayer rewrite, RAG, Plugins |
| **v1.8.x** | 2026-01 | Smart Sync, AeroVault v2 (AES-256-GCM-SIV), Batch Rename, Vision AI, Security audits |
| **v1.7.0** | 2025-12 | AeroVault, Archive browser, Cryptomator format 8, AeroFile mode, AeroAgent 24 tools |
| **v1.6.0** | 2025-12 | AeroAgent Pro: native function calling, streaming, 14 tools, chat history, cost tracking |
| **v1.5.x** | 2025-11 | 4 cloud providers, auto-updater, clipboard, drag & drop, OAuth security, i18n |

---

## Completed — Full Archive

<details>
<summary>All completed features by domain (click to expand for audit reference)</summary>

### Protocols & Providers (21 total)

- FTP, FTPS, SFTP, WebDAV, S3, Amazon S3, Google Drive, Dropbox, OneDrive, MEGA, Box, pCloud, Azure Blob, 4shared, Filen, Zoho WorkDrive, Internxt Drive, kDrive, Koofr, Jottacloud, FileLu, Yandex Disk, Drime Cloud (dev-only), CloudMe/Seafile/DriveHQ (WebDAV presets), Yandex Object Storage (S3 preset)
- Amazon S3 dedicated provider (v2.6.2): 28 AWS region presets, official AWS logo, bucket name trimming
- Azure Blob + Drime Cloud re-enabled for production (v2.6.2)
- StorageProvider trait: 18 methods (stat, search, move, trash, share, quota, versions)
- Streaming uploads: FTP, Dropbox, OneDrive, Google Drive, Box (file-handle chunks, no full-buffer OOM)
- Streaming downloads: FTP (8KB chunks), Filen (decrypt per-chunk), SFTP (32KB)
- SecretString for all provider credentials across 18 providers
- quick-xml 0.39 migration (WebDAV, Azure, S3 — replaced regex parsing)
- HTTP retry with exponential backoff, jitter, Retry-After (http_retry.rs)
- FTP TLS downgrade detection (tls_downgraded flag)
- OneDrive 4MB auto-threshold (resumable upload)
- Pagination: S3 continuation token, Azure NextMarker, 4shared ID-based
- pCloud region in OAuth2 flow
- SFTP rmdir_recursive symlink safety (lstat check)
- SFTP read_range 100MB cap
- S3 presigned URL URI-encoded
- Error message sanitization (sanitize_api_error, 95 occurrences)
- WebDAV verify_cert option
- Share link expiry parameter (Zoho/Google/Dropbox/OneDrive)
- OAuth 1.0 signing (4shared HMAC-SHA1)
- Filen 2FA passthrough
- Custom server icon picker (v2.6.4): any image as server icon, displayed in Saved Servers/session tabs/forms
- Favicon auto-detection (v2.6.4): FTP/FTPS/SFTP favicon.ico + manifest.json fallback
- Icon rendering hierarchy (v2.6.4): custom > favicon > provider logo > initial letter gradient
- AeroCloud tab switch fix (v2.6.3): provider disconnect before reconnect, isNonFtpProvider() routing
- Koofr native REST API (v2.8.0): OAuth2 PKCE, EU-based, 10GB free, trash management
- Jottacloud REST API (v2.9.0): Norwegian-hosted, unlimited plans, token persistence
- FileLu native REST API (v2.7.0): API key auth, 10 special commands (file passwords, privacy, clone)
- AeroVault crate extraction (v2.8.9): standalone `aerovault` crate on crates.io, inline crypto replaced
- Production CLI (v2.8.0): 13 commands, URL-based connections, `--json` output, indicatif progress bars
- AeroAgent server_exec (v2.8.0): `server_list_saved` + `server_exec` (10 operations), vault credential resolution

### AeroSync (Phase 1-3B complete)

- Transfer journal (checkpoint/resume), SHA-256 checksum scan
- Error taxonomy (10 categories), retry with exponential backoff
- Post-transfer verification (4 policies), sync profiles (Mirror/Two-way/Backup)
- Conflict Resolution Center (per-file + batch), bandwidth control
- Journal auto-cleanup (30-day TTL), parallel streams (1-8)
- Sync Scheduler (interval, time window, day picker, pause/resume)
- Filesystem Watcher (notify v8.2, debounce, inotify warnings)
- Multi-Path Editor, Sync Templates (.aerosync), Rollback Snapshots
- Delta Sync (rsync algorithm, Adler-32 + SHA-256, 512B-8KB blocks)
- Transfer Compression (SSH zlib, auto-filter pre-compressed)
- Provider Optimization Hints (S3 multipart, FTP resume, SFTP delta)
- Speed Modes (Normal/Fast/Turbo/Extreme/Maniac), Quick Sync tab
- Dry-run export (JSON/CSV), Safety Score badge, Explainable decisions
- Canary Sync (v2.5.0): 2-phase subset validation
- Signed Audit Log (v2.5.0): HMAC-SHA256 over journal JSON
- 95+ Rust unit tests, 150+ i18n keys

### AeroAgent (47 tools + clipboard_read_image, 19 providers)

- Native function calling (OpenAI, Anthropic, Gemini)
- SSE/NDJSON streaming for all providers
- DAG-based tool pipeline, intelligent retry, validation layer
- Composite macros ({{var}} templates), tool progress indicators
- Project-aware context (10 languages), file dependency graph (6 languages)
- Persistent agent memory (.aeroagent), conversation branching
- Smart context injection, token budget optimizer
- Streaming markdown renderer, code block actions (Copy/Apply/Diff/Run)
- Thought visualization (Anthropic/OpenAI o3/Gemini)
- 15 prompt templates, multi-file diff preview, cost budget tracking
- Chat search (Ctrl+F), keyboard shortcuts
- Anthropic prompt caching + thinking, OpenAI structured outputs
- Ollama templates/pull/GPU monitoring, Gemini code execution + context caching
- Provider Marketplace (6 categories), 19 providers total
- shell_execute (backend, stdout/stderr/exit_code, 30s timeout, denylist)
- 9 file management + 8 power tools, Extreme Mode (50-step autonomous)
- Duplicate tool call prevention (executedToolSignaturesRef)
- Welcome screen (3x3 capability grid, context-aware quick prompts)
- AI Provider Tier 3 (v2.6.0): AI21 Labs, Cerebras, SambaNova, Fireworks AI — all OpenAI-compatible
- Context Menu AI Actions (v2.6.0): "Ask AeroAgent" in local/remote file context menus
- Command Palette (v2.6.0): Ctrl+Shift+P, fuzzy search, category grouping, keyboard navigation
- AI Status Widget (v2.6.0): StatusBar indicator (idle/streaming/tool-execution) with pulse animation
- Drag & Drop to Agent (v2.6.0): Drop files onto chat area to analyze
- Plugin Registry (v2.6.0): GitHub-based registry, SHA-256 integrity, 1h cache, one-click install
- Plugin Browser UI (v2.6.0): Browse/search/install modal in AI Settings, installed management
- Plugin Hooks (v2.6.0): Event-driven hooks (file:created, transfer:complete, sync:complete) with glob filters

### AeroFile (v2.5.0 complete)

- AeroFile mode (local-only file manager, remote toggle)
- LocalFilePanel extraction (~730 lines from App.tsx) — GAP-C04 resolved
- Multiple local path tabs (12 max, drag-to-reorder, localStorage persistence)
- File tags SQLite backend (7 preset labels, 9 Tauri commands, WAL mode)
- FileTagBadge colored dots, tags context menu, PlacesSidebar tag filter
- Places Sidebar: user dirs, GVFS shares, unmounted partitions, EFI filtering, custom locations, recent paths, mount point octal escaping (UTF-8 safe), event-driven volume detection (#113 — poll + inotify replaces 5s setInterval), network volume eject (#114 — fusermount fallback for SSHFS/NFS/CIFS)
- Keyboard navigation: Arrow Up/Down for file selection, Shift+Arrow for range selection (both panels)
- ARIA accessibility baseline: role=grid/row/aria-selected on file tables, role=region/toolbar/status on panels
- Quick Look (Spacebar), folder size calculation, duplicate finder, disk usage treemap
- Properties dialog (3 dates, permissions matrix, checksums), trash browser
- Customizable columns, sort folders first, show/hide extensions
- Breadcrumb navigation, inline rename (F2/click)
- Cross-platform trash (Linux+Windows), Windows volume detection (WinAPI)

### AeroVault & Encryption

- AeroVault v2: AES-256-GCM-SIV (RFC 8452), AES-KW, AES-SIV, Argon2id 128MiB, HMAC-SHA512, optional ChaCha20 cascade
- Vault directories, recursive delete, change password, security info, peek
- Chunk index in AAD, vault key zeroize on Drop
- Remote vault open/save (download → operate → upload)
- TOTP 2FA for vault unlock (RFC 6238, rate limiting, zeroize)
- Cryptomator format 8 (read), Cryptomator vault creation (v2.5.0)
- Drag & drop into vault (Tauri onDragDropEvent, directory targeting)
- Archive browser (ZIP/7z/TAR/RAR), CompressDialog, selective extraction

### Chat History (v2.3.0)

- SQLite WAL + FTS5 full-text search, 18 Tauri commands
- Configurable retention (7-365 days + unlimited), auto-cleanup
- Chat History Manager UI, session export/import, branch management
- In-memory fallback, 55+ audit findings resolved

### OS Integration

- Linux: Nautilus/Nemo badge extensions, GIO emblems, tray badges
- Windows: Cloud Filter API badges, Named Pipe IPC, NSIS stub
- macOS: FinderSync .appex extension (v2.5.0), universal binary (arm64+x86_64), 6 badge types, Unix socket bridge (#105/#106/#107)
- Autostart (tauri-plugin-autostart), white monochrome tray icon
- Branded Polkit update dialog (10 languages), auto-update restart fix
- SFTP TOFU visual dialog (PuTTY-style, fingerprint, MITM warning)
- Platform-specific cfg gating: GIO emblems (Linux-only), Cloud Filter (Windows-only), FinderSync (macOS-only)
- AUR package (v2.6.0): `aeroftp-bin` on Arch User Repository with PKGBUILD + .SRCINFO
- 147 provider audit findings fixed (v2.6.1): S3, pCloud, kDrive, Azure, 4shared, Filen, Internxt, MEGA

### UI/UX

- 4 themes (Light, Dark, Tokyo Night, Cyberpunk), terminal/Monaco theme sync
- Security Toolkit (Hash Forge, CryptoLab, Password Forge)
- AeroPlayer (HTML5 Audio, Web Audio API, 10-band EQ, 14 visualizers, 6 WebGL shaders)
- Unified TransferProgressBar (4 levels), SpeedGraph canvas
- Splash screen with loading sequence, session tabs
- Aero Family: AeroSync, AeroVault, AeroPlayer, AeroAgent, AeroTools
- VS Code-style unified titlebar (v2.6.1): 4 dropdown menus, theme-aware hover, header eliminated
- Consistent modal animations (v2.6.1): `animate-scale-in` on all 42 modal dialogs
- Transfer Queue dismiss-respect (v2.6.4): `userDismissedRef` preserved during batch transfers
- Transfer Queue DOM windowing (v2.6.4): render last 200 items only, single-pass useMemo counting
- Concurrent transfers default 5 (v2.6.4): options 1-8, previously default 2 max 5

### Security

- SEC-P1-01: Plugin shell injection → argv direct execution
- SEC-P1-02: TOFU fail-closed on unknown errors
- SEC-P1-03: OAuth vault-only (localStorage fallback removed)
- SEC-P1-05: FTP cert UX hardening (persistent badge, double-confirm)
- SEC-P1-06: SFTP TOFU visual dialog (v2.4.0)
- SEC-P2-01: shell_execute command denylist (12 patterns)
- SEC-P2-02: Plugin SHA-256 integrity (v2.1.2)
- SEC-P2-03: Tauri FS scope hardening (v2.1.2)
- SEC-P2-04: Security regression suite (.github/scripts, CI)
- SEC-P3-02: Settings vault migration (vault-first, one-way)
- SEC-P3-03: Invoke hardening — closed N/A (single-window)
- Resource Management: 17 issues resolved (800MB→8MB peak RAM)
- Cross-Audit Gap Analysis: 57 gaps, 18 fixed in v2.4.0, rest resolved/false-positive
- GAP-A01–A12, A14: All FIXED v2.4.0 (retry, symlinks, allocation, encoding, sanitization, cert, expiry)
- GAP-B07/B14/B21/B22: All FIXED v2.4.0 (RAG opt-in, saveSettings await, stale closure, resizeImage timeout)
- GAP-B11/B13/B16: All FALSE POSITIVE (already uses invoke, randomUUID, ASCII boundaries)
- GAP-C01/C02/C04, GAP-D03, GAP-E01: All FIXED (trash, volumes, modularization, AAD, zeroize)
- 12+ independent security audits, grade: A-

### i18n

- 47 languages at 100% coverage, 2700+ keys
- GLM batch method (3 agent batches + merge script)
- Armenian Unicode escape sequence handling
- Aero Family brand names never translated

</details>

---

## Risk-Accepted (not scheduled)

| Risk ID | Severity | Description | Rationale |
|---------|----------|-------------|-----------|
| GAP-A05 | Medium | Zoho folder cache poisoning | Requires same team+privatespace name — improbable. Cache session-scoped |
| GAP-A08 | Medium | OAuth2 tokens as plain String in `oauth2` crate | Crate internals — can't wrap without fork. Tokens encrypted in vault |
| GAP-A09 | Medium | pCloud token without expiry | pCloud API design — not controllable. User can revoke manually |
| GAP-B06 | Medium | SSRF via `base_url` in AI settings | Desktop app — user configures intentionally |
| GAP-B08 | Medium | AI settings in localStorage | Only model names and base URLs — not credentials |
| GAP-B09 | Medium | Regex in parseToolCall | Native function calling replaced regex for all supported providers |
| GAP-B10 | Medium | SSE buffer never flushed | Mitigated by HTTP timeout (120s/15s) |
| GAP-C03 | Low | Disk usage no cancellation | Mitigated by max_depth/max_entries limits |
| RISK-240-01 | Medium | FTP PASV IP validation | Requires suppaftp protocol-level changes. Server-initiated |
| RISK-240-03 | Low | MEGA `.bat` injection (Windows) | Backend-only execution, no user-controlled args |
| RISK-240-04 | Low | WebDAV Digest auth MD5 | Server-negotiated protocol limitation |

---

## Active & Planned

### Security Hardening (remaining)

| # | Area | Status | Impact | Notes |
|---|------|--------|--------|-------|
| SEC-P1-04 | CSP Phase 2 | **In Progress** | +0.4 | Baseline CSP enabled. Next: tighten directive-by-directive with compatibility matrix (Monaco, xterm, WebGL, AeroAgent) on all 3 platforms. Checklist: `docs/dev/guides/CSP-EXECUTION-CHECKLIST.md` |
| SEC-P3-01 | External crypto audit | Planned | +0.2 | Independent review of AeroVault v2 (Argon2id + AES-GCM-SIV) by recognized cryptographic firm |

**Current estimated score**: 8.5/10. Target: 9.0 (CSP) → 10.0 (crypto audit).

### Sigstore Supply Chain Signing

| # | Task | Status | Notes |
|---|------|--------|-------|
| SIG-01 | Add `id-token: write` permission to `build.yml` | Planned | Required for GitHub OIDC keyless signing |
| SIG-02 | Install cosign via `sigstore/cosign-installer@v4` | Planned | Per-platform, only on tag push |
| SIG-03 | Sign all release artifacts with `cosign sign-blob` | Planned | .exe, .msi, .deb, .rpm, .AppImage, .snap, .dmg |
| SIG-04 | Upload `.sigstore.json` bundles to GitHub Releases | Planned | Alongside existing binaries |
| SIG-05 | Add "Verifying Release Integrity" section to README | Planned | `cosign verify-blob` instructions |

**Effort**: ~2h. **Cost**: $0 (keyless via GitHub OIDC). **Secrets**: None required.
**What it does**: Proves artifacts were built by `axpdev-lab/aeroftp` CI from tagged commits. Entries logged in Rekor transparency log.
**What it does NOT do**: Does not replace Authenticode (Windows SmartScreen). Fulcio root is not in Windows Trust Store.
**Execution plan**: `docs/dev/SIGSTORE-INTEGRATION.md`

### CLI Expansion

| # | Task | Status | Notes |
|---|------|--------|-------|
| 25 | JSON output (`--json`) | **Done v2.7.5** | Global `--json` / `--format json` flag on all 5 commands |
| 26 | Script files (`.aeroftp`) | **Done** | Batch engine with SET/ECHO/ON_ERROR, shell-like quoting, single-pass variable expansion, 17 commands, 1MB file limit |
| 27 | Glob pattern transfers | **Done** | `aeroftp put "*.csv"` via globset, `aeroftp get "*.csv"` via remote glob filter |
| 28 | `tree` command | **Done** | Unicode box-drawing, BFS depth-limited (`-d`), JSON output with recursive structure |
| 29 | Exit codes | **Done** | 9 semantic codes (0-8, 99) for scripting. Documented in `--help` |
| 30 | Security hardening | **Done** | Path traversal protection, BFS depth/entry caps, NO_COLOR, SIGPIPE, double Ctrl+C, stderr separation |
| 31 | `--profile` vault profiles | **Done v2.9.9** | Connect to any saved server without exposing credentials. 22 protocols. Fuzzy name matching with disambiguation |
| 32 | `profiles` command | **Done v2.9.9** | List saved servers (table + JSON). Never shows credentials |
| 33 | OAuth CLI support | **Done v2.9.9** | Google Drive, Dropbox, OneDrive, Box, pCloud, Zoho, Yandex via `--profile` when authorized from GUI |
| 34 | FTP/TLS upload fix | **Done v2.9.9** | flush + drain delay before close_notify. Binary transfer mode. Verified 5KB-5MB |
| 35 | WebDAV streaming upload | **Done v2.9.9** | Content-Length + ReaderStream. Fixes HTTP 0-byte and OOM on large files |

### CLI Audit Backlog (10-auditor dual review, March 2026)

Findings from Claude Opus 4.6 (5 auditors) + GPT 5.4 (5 auditors) security/quality review.
Full reports in `docs/dev/audit/GPT5-4-CLI/`. Fix individually in dedicated sessions to avoid regressions.

| ID | Severity | Description | Status |
|----|----------|-------------|--------|
| VER-001 | HIGH | 4shared OAuth1 not working via `--profile` — factory rejects, no dedicated branch | Planned |
| VER-002 | MEDIUM | FTP upload uses sleep heuristic before finalize — suppaftp/native-tls limitation | Accepted (workaround) |
| VER-003 | MEDIUM | Connection disconnect duplicated across ~20 handlers, errors discarded | Planned (refactor) |
| VER-004 | MEDIUM | credential_store pub API too wide (unlock_with_master, cache_vault, store_internal) | Planned (pub(crate)) |
| VER-005 | MEDIUM | Vault first-run init has no dedicated lock (TOCTOU on concurrent CLI+GUI) | Planned |
| VER-006 | MEDIUM | Master password and passphrase not zeroized after use in CLI | Planned (secrecy crate) |
| VER-007 | LOW | OAuth StoredTokens uses String instead of SecretString in memory | Planned |
| UX-001 | MEDIUM | StorageProvider trait has more capabilities than CLI exposes (share, checksum, chmod, versions) | Planned (incremental) |
| UX-002 | MEDIUM | `--profile` and URL should be mutually exclusive via clap arg groups | Planned |
| UX-004 | MEDIUM | No shell completion support (bash/zsh/fish via clap_complete) | Planned |
| UX-005 | MEDIUM | No --retry / --resume / --verify flags for transfer resilience | Planned |
| PERF-002 | MEDIUM | Batch mode creates new connection per command instead of reusing | Planned (refactor) |

### GitHub Integration Completion (post v3.0.0)

Remaining features planned per GPT 5.4 completion plan (`docs/dev/GITHUB-PROVIDER-GPT54-COMPLETION-PLAN-APPENDIX.md`).
Backend foundations exist — these need UI/CLI exposure and testing.

| # | Feature | Backend | UI/CLI | Priority |
|---|---------|---------|--------|----------|
| G1 | **Create releases** | `releases_mode.rs` create_release() exists | Needs Tauri command + dialog | HIGH |
| G2 | **Open Pull Request** from branch workflow | `mod.rs` ensure_pull_request() exists | Needs button in branch workflow mode + dialog | HIGH |
| G3 | **GraphQL atomic batch commit** | `graphql.rs` batch_commit() exists | Needs wiring in multi-upload flow (single commit for N files) | MEDIUM |
| G4 | **Gists** mode | Not started | Phase 5 of Epic v1.1 | LOW |
| G5 | **GitHub Pages** deploy | Not started | Phase 6 of Epic v1.1 | LOW |
| G6 | **Actions** trigger/monitor | Not started | Phase 4 of Epic v1.1 | LOW |

### Vault & Encryption Enhancements

| # | Task | Status | Notes |
|---|------|--------|-------|
| 37 | Biometric unlock | Planned | macOS Touch ID, Windows Hello. Linux FIDO2 limited |
| 51 | Vault sync (bidirectional) | Planned | Track vault versions, conflict detection |
| — | Vault compaction | Planned | Deleted file data remains — compaction rewrites without orphans |
| — | Chat history encryption | Planned | Option to store chat DB inside vault.db (AES-256-GCM) |

### AeroVault OS Integration

The `.aerovault` format is now a standalone crate on crates.io. Next steps focus on making it a first-class citizen on all platforms.

| # | Task | Platform | Status | Notes |
|---|------|----------|--------|-------|
| 1 | `.aerovault` file association | All | **Done** | Double-click opens AeroFTP vault browser. MIME type `application/x-aerovault`, magic bytes, Tauri fileAssociations, NSIS registry |
| 2 | MIME type registration | Linux | **Done** | `com.aeroftp.AeroVault.xml` installed via .deb to `/usr/share/mime/packages/`, magic `AEROVAULT2` |
| 3 | File type icon | All | **Done** | Shield+lock icon (8 PNG sizes + SVG for Linux hicolor, .ico for Windows NSIS). Installed via .deb, Snap, NSIS, and shell integration script |
| 4 | Nautilus/Nemo context menu | Linux | **Done** | `.desktop` MimeType=application/x-aerovault + `%f` argument — "Open with AeroFTP" via xdg-open |
| 5 | Windows Explorer context menu | Windows | **Done** | NSIS registry-based shell\open\command with "Open with AeroFTP", clean uninstall, SHChangeNotify |
| 6 | macOS Quick Look plugin | macOS | Planned | `.qlgenerator` showing vault metadata (file count, size, encryption mode) without password. FinderSync exists for AeroCloud badges only |
| 7 | ~~Thumbnailer~~ | Linux | **N/A** | Not applicable — encrypted containers cannot show content previews. MIME type icon (#3) already provides the OS file manager icon. Freedesktop thumbnailers are designed for content-previewable formats (images, PDFs, videos) |
| 8 | `aerovault` CLI packaging | All | **Done** | `aerovault-cli` crate published on crates.io (v0.3.2). 8 commands: create, list, add, extract, mkdir, rm, info, passwd. `cargo install aerovault-cli` works. Homebrew/AUR pending |
| 9 | Python bindings (PyO3) | All | Planned | `pip install aerovault` — open/create/extract vaults from Python scripts |
| 10 | WASM target | Web | Planned | `aerovault` compiled to WebAssembly for browser-based vault operations |
| 11 | Android/iOS library | Mobile | **Partial** | Android `VaultPlugin.java` (~560 lines) complete with Tink AES-GCM-SIV + BouncyCastle Argon2id. iOS Swift bindings not started |
| 12 | Drag & drop vault creation | All | Planned | Drag files onto `.aerovault` in OS file manager to add them (requires background daemon) |

**Priority order**: 6 (macOS Quick Look) → 9-10 (ecosystem)

### Provider Feature Gaps (remaining)

| Provider | Gap | Notes |
|----------|-----|-------|
| Box | Chunked upload >150MB | Large file support |
| Azure | Block List upload >256MB | GAP-A07 |
| pCloud | Checksum verification | Post-transfer integrity |
| All | List virtualization (react-window) | Performance for 10K+ file listings |

### OpenDrive Phase 2 (post-integration)

| # | Task | Status | Notes |
|---|------|--------|-------|
| OD-01 | Dedicated OpenDrive session/account metadata contract | Planned | Keep generic `StorageInfo` unchanged. Add a separate OpenDrive-specific payload sourced from `users/info.json` for `BwMax`, `BwUsed`, `IsAccountUser`, `AccessUserID`, `MaxAccountUsers`, `FVersioning`, `FVersions`, and normalized session/role flags |
| OD-02 | Bandwidth usage UI | Planned | Show OpenDrive bandwidth consumed/total in UI using `users/info.json`. Treat it as account quota telemetry, not live transfer speed. Surface it near storage quota/status summary |
| OD-03 | Session role visibility | Planned | Show whether the active OpenDrive session is owner/admin or access user, and surface `basic` vs `manager` when reliably available. Preferred UX: compact badge near connection identity/status bar |
| OD-04 | Read-only business capability inventory | Planned | Before exposing mutations, add a read-only OpenDrive account summary for versioning flags, account-user limits, group/admin availability, and assigned storage/bandwidth ceilings |
| OD-05 | Controlled business/admin rollout | Planned | Expose account-user/group administration only inside a dedicated OpenDrive admin area with backend capability checks, explicit confirmations, and no leakage into generic file menus |
| OD-06 | Phased delivery strategy | Planned | Phase 1: backend metadata + badges. Phase 2: read-only admin/business visibility. Phase 3: guarded mutations for account users, groups, and delegated quota/bandwidth management |

### Platform Distribution

| # | Task | Status | Notes |
|---|------|--------|-------|
| 43 | Flathub submission | Planned | Manifest ready in `docs/dev/platform/` |
| 44 | AUR (Arch) package | **Done v2.6.0** | `aeroftp-bin` on AUR, PKGBUILD + .SRCINFO in `aur/` |
| 45 | Homebrew Cask (macOS) | Planned | — |

### Wayland Compatibility

| Task | Status | Notes |
|------|--------|-------|
| Custom titlebar (frameless window) | **Done v2.6.1** | VS Code-style React titlebar with `data-tauri-drag-region`, 4 dropdown menus |
| Document `GDK_BACKEND=x11` workaround | Planned | Add to README/FAQ for Wayland users |

> **Context**: Window controls unresponsive on Ubuntu Wayland until double-click on titlebar. Known Tauri 2 upstream: [#13440](https://github.com/tauri-apps/tauri/issues/13440), [#11631](https://github.com/tauri-apps/tauri/issues/11631). Custom titlebar resolves this.

### AeroAgent — App-Wide Interaction Expansion

**Done in v2.6.0**: Context Menu AI Actions, Command Palette (Ctrl+Shift+P), AI Status Widget, Drag & Drop to Agent.

| Feature Area | Planned Tool | Notes |
|-------------|-------------|-------|
| AeroVault control | `vault_manage` | Create/open/add/extract/delete vault entries |
| AeroSync control | `sync_control` | Start/pause/resume sync, select profile, speed mode |
| Theme switching | `set_theme` | Change theme (light/dark/tokyo/cyber) |
| AeroPlayer control | `player_control` | Play/pause/stop, next/prev, volume, EQ preset |
| Terminal v2 | `terminal_execute` v2 | Sandboxed command whitelist (no destructive) |

> **Not planned**: Server profile access (security: AI should not access credentials).

### AeroAgent — Plugin Ecosystem

**Done in v2.6.0**: Plugin Registry (GitHub-based, SHA-256), Plugin Browser UI, Plugin Hooks system.

| Task | Notes |
| ---- | ----- |
| Built-in sample plugins (3-5) | Git Status, System Info, Image Resize, CSV Viewer, Linter Runner |
| Plugin SDK documentation | Manifest format, stdin/stdout protocol, danger levels, examples |
| Plugin auto-update | Version checking + update mechanism |

### AI Provider Tier 3 (nice to have)

**Done in v2.6.0**: AI21 Labs, Cerebras, SambaNova, Fireworks AI (19 providers total).

| Provider | Feature | Notes |
|----------|---------|-------|
| Anthropic | Computer use integration | Visual UI testing and automation |
| Google | Gemini grounding with Search | Factual/API documentation queries |
| xAI | Grok real-time knowledge | X/Twitter-sourced real-time tech info |
| Custom | OpenAPI spec import | Paste URL → auto-generate tool definitions |
| All | A/B model comparison | Side-by-side prompt execution (quality/speed/cost) |

### Public Documentation

| Task | Status | Notes |
|------|--------|-------|
| Provider Integration Guide | **Done** | `docs/PROVIDER-INTEGRATION-GUIDE.md` — 16 sections, 20 providers, 7 auth patterns, code examples |
| OAuth2 Implementation Guide | **Done** | Included in Provider Guide §3.1 — PKCE walkthrough, 7 providers, token refresh race guard, callback server |
| OAuth 1.0 Implementation Guide | **Done** | Included in Provider Guide §3.2 — 4shared HMAC-SHA1, RFC 5849, 3-step flow |
| Streaming Upload Patterns | **Done** | Included in Provider Guide §4 — 4 patterns (simple/chunked/resumable/multipart), per-provider thresholds |
| XML Parsing with quick-xml | **Done** | Included in Provider Guide §7 — state machine parser pattern, WebDAV/S3/Azure examples |
| E2E Encryption Providers | **Done** | Included in Provider Guide §3.7 — MEGA AES-128, Filen AES-256-GCM, Internxt XChaCha20 |
| StorageProvider Trait Design | **Done** | Included in Provider Guide §2 — 20 required + 30 optional methods, capability matrix |
| Security Best Practices | **Done** | Included in Provider Guide §10 — SecretString, token hierarchy, zeroization, error sanitization |
| Testing & Audit Methodology | **Done** | Included in Provider Guide §14 — multi-auditor approach, regression suite, clippy enforcement |

**Published**: `docs/PROVIDER-INTEGRATION-GUIDE.md` (public, GitHub-visible)

### Internationalized Error Messages

Backend Rust errors are currently hardcoded in English across all 22 providers. The frontend shows translated titles (`t('connection.connectionFailed')`) but raw English error bodies from the backend. To reach true 100% i18n coverage, errors need translation too.

| Task | Status | Notes |
|------|--------|-------|
| Define error code enum | Planned | Structured error codes (e.g., `INSTALLATION_NOT_FOUND`, `AUTH_FAILED`, `RATE_LIMITED`) returned by backend instead of free-text strings |
| Frontend error mapping | Planned | Map error codes to i18n keys in a central `errorMessages.ts` utility |
| Provider error taxonomy | Planned | Unified error codes across all 22 providers (GitHub, FTP, SFTP, WebDAV, S3, etc.) |
| Fallback for unknown errors | Planned | Show raw English message with a "Report this error" link for unmapped codes |

**Effort**: ~20-30h (touches all providers). **Impact**: True 100% i18n — every user-visible string translated.

### Accessibility (A11y)

| Task | Status | Notes |
|------|--------|-------|
| ARIA labels across UI | **Done v2.5.0** | role=grid/row/aria-selected on file tables, role=region/toolbar/status on panels, role=navigation on sidebar |
| Keyboard-only navigation | **Done v2.5.0** | Arrow Up/Down, Shift+Arrow range, Tab panel switch, Enter open, F2 rename, Space preview, Ctrl+A/C/X/V |
| Text-to-Speech for AI responses | Planned | `window.speechSynthesis` API. No WebKitGTK support — macOS/Windows only |
| Voice commands for tool approval | Planned | "Yes"/"No"/"Execute" via speech recognition. No WebKitGTK support |
| High contrast mode | Planned | Additional 5th theme for low-vision users. Large fonts, strong borders |

---

## Competitive Positioning

### AeroSync vs Competition

| Feature | AeroSync | WinSCP | rclone |
|---------|----------|--------|--------|
| Sync profiles | **Yes (3+custom)** | Yes | Yes |
| Parallel streams | **Yes (1-8 UI)** | Yes | 4 |
| Scheduler | **Yes (Full UI)** | Yes | cron |
| Conflict resolution | **Per-file wizard** | Yes | Yes |
| Filesystem watcher | **Yes (Full UI)** | Yes | bisync |
| Multi-path sync | **Yes (Full UI)** | No | Yes |
| Sync templates | **Yes (.aerosync)** | No | No |
| Rollback snapshots | **Yes** | No | No |
| Delta sync | **Yes (rsync-style)** | No | No |
| SSH compression | **Yes** | No | No |
| Speed mode presets | **Yes (5 levels)** | No | No |
| Explainable decisions | **Yes** | No | No |
| Safety score | **Yes** | No | No |
| Dry-run export | **JSON+CSV** | No | No |
| Canary sync | **Yes (v2.5.0)** | No | No |
| Signed audit log | **Yes (v2.5.0)** | No | No |
| Multi-protocol (20) | **Yes** | 5 | 40+ |
| Speed graph | **Yes** | No | No |

### Chat History vs Competition

| Feature | AeroFTP | VS Code | Cursor | Claude Code |
|---------|---------|---------|--------|-------------|
| Format | SQLite + FTS5 | SQLite + JSON | SQLite + JSON blob | JSONL per session |
| Auto-cleanup | Configurable TTL | None | None | 30 days |
| Search | FTS5 full-text | None | None | None |
| Delete | Bulk + date range | Buggy | SQL only | Manual (rm) |

---

## AeroFTP Mobile (Long-Term Vision)

> **Status**: Exploratory — validated via Lumo Cloud Mobile (Capacitor 6 Android app with photo backup, gallery, file management). The mobile foundation proves the architecture is viable.

### Concept

A simplified AeroFTP for Android (and potentially iOS) — multi-protocol file manager with cloud provider support, built on the same design language as the desktop app.

### Architecture Options

| Approach | Pros | Cons |
|----------|------|------|
| **Capacitor + WebView** (like Lumo Cloud) | Shared JS/CSS codebase, fast iteration, proven stack | No native FTP/SFTP sockets in WebView |
| **Rust via JNI/NDK** | Reuse `suppaftp`/`russh` crates directly | Complex cross-compilation, JNI boilerplate |
| **Java native libraries** | Apache Commons Net (FTP), sshj (SFTP) | Protocol layer rewrite, no code sharing with desktop |

**Recommended**: Start with Capacitor (HTTP-based protocols), add Java native libs for FTP/SFTP later.

### Phase 1 — AeroFTP Mobile Lite (HTTP protocols)

| Feature | Notes |
|---------|-------|
| WebDAV browser | Connect to any WebDAV server (NAS, Nextcloud, Seafile) |
| S3 browser | AWS, Backblaze B2, Cloudflare R2, Wasabi, MinIO |
| Google Drive | OAuth2 PKCE, same flow as desktop |
| Dropbox | OAuth2 PKCE |
| OneDrive | OAuth2 PKCE |
| File preview | Images (pinch-to-zoom), video, audio, PDF, text |
| Upload from device | Camera, gallery, file picker |
| Background upload | WorkManager, rate-limit aware (proven in Lumo Cloud) |
| Multi-account | Switch between saved server profiles |
| Biometric lock | Android BiometricPrompt for app/vault access |

### Phase 2 — Full Protocol Support

| Feature | Notes |
|---------|-------|
| FTP/FTPS | Apache Commons Net or `suppaftp` via NDK |
| SFTP | sshj (Java) or `russh` via NDK |
| Host key verification | TOFU dialog (reuse desktop UX) |
| Transfer queue | Background transfers with notification progress |
| AeroVault mobile | Open/create `.aerovault` files on remote servers |

### Phase 3 — Ecosystem Integration

| Feature | Notes |
|---------|-------|
| AeroSync mobile | Bidirectional sync between phone and server |
| AeroAgent mobile | Chat UI with tool execution (subset of desktop tools) |
| Desktop ↔ Mobile handoff | Share server profiles via QR code or vault export |
| Tablet layout | Two-pane file manager (local + remote) |
| iOS port | Same Capacitor codebase, minimal platform-specific code |

### Lessons from Lumo Cloud Mobile

Key insights validated during development:

- **WorkManager** is reliable for background uploads (survives app close, respects Doze)
- **Rate limiting** requires 6s+ delay between uploads on NAS hardware — build retry logic from day one
- **MediaStore** Bundle-based queries are required for Android 10+ (LIMIT in sortOrder is broken)
- **Pinch-to-zoom** and touch gestures are essential for photo-heavy use cases
- **Pagination** must be unlimited for photo folders (1000+ items common)
- **Capacitor 6** vanilla JS (no bundler) works well for rapid prototyping — consider migrating to React for AeroFTP Mobile to share component logic with desktop

---

## New Provider Roadmap (16 March 2026)

Comprehensive analysis of candidate cloud providers for future AeroFTP integration.

### Tier 1 — Quick Wins (next versions)

| Provider | Free | API | Auth | Effort | Priority |
|----------|------|-----|------|--------|----------|
| **Blomp** | 40 GB | OpenStack Swift / S3-compatible | Username/password → Keystone token | ~2h (S3 preset) | High |
| **OpenDrive** | 5 GB | REST API (rclone Tier 1) | Username/password | **Integrated in v2.9.x** | Follow-up tracked in OpenDrive Phase 2 |
| **MediaFire** | 10 GB | REST API (mediafire.com/developers/) | OAuth2 + API key | ~700 lines Rust | Medium |

### Tier 2 — Major Features

| Provider | Free | API | Challenge | Effort | Priority |
|----------|------|-----|-----------|--------|----------|
| **Proton Drive** | 5 GB | Open-source Go clients (no official API) | SRP auth + PGP E2E encryption per operation | 200-300h | v3.0 candidate |
| **Degoo** | 20 GB | GraphQL reverse-engineered | Unstable API, ToS risk, GCS upload | 60-80h | Low (risk) |

### Tier 3 — Rejected (with rationale)

| Provider | Reason |
|----------|--------|
| **IDrive consumer** | API from 2012, plaintext passwords, rclone abandoned support. IDrive e2 (S3) already supported |
| **Icedrive** | WebDAV paid-only, no public API |
| **Sync.com** | Impossible — E2E with no public API |
| **NordLocker** | Impossible — zero documentation, proprietary closed protocol |
| **Tresorit** | Enterprise-only paid API, 0 GB free tier |
| **Felicloud** | Too new, no API information available |

### Proton Drive — Deep Dive

Most requested provider by users. Aligns with AeroFTP's encryption-first identity (AeroVault).

**Technical path**:
1. Reference: `henrybear327/Proton-API-Bridge` (Go, MIT, archived Feb 2026)
2. Rust crates needed: `srp` (Secure Remote Password auth), `sequoia-pgp` (OpenPGP operations)
3. Every operation requires PGP: list (decrypt metadata), download (decrypt content), upload (encrypt + sign)
4. User keyring management (private PGP keys for session key decryption)
5. No WebDAV/SFTP access — REST-only with custom crypto layer

**Estimated scope**: 200-300h, dedicated v3.0 feature. Would be a significant competitive advantage — no other open-source FTP/cloud client supports Proton Drive natively.

---

*This document supersedes individual roadmap files in `docs/dev/roadmap/`.*
*See CLAUDE.md for project guidelines and release process.*
