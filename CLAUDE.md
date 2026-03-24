# AeroFTP Development Guidelines

## Language

Always respond in **Italian** (italiano). All conversations, explanations, and comments to the user must be in Italian. Code, commit messages, and documentation remain in English.

---

## Commit Message Standards

This repository follows **Conventional Commits** with a professional, academic style suitable for code review and publication.

### Format
```
<type>(<scope>): <description>

[optional body]

[optional footer]
```

### Types
- `feat`: New feature
- `fix`: Bug fix
- `docs`: Documentation only
- `style`: Code style (formatting, no logic change)
- `refactor`: Code restructuring without behavior change
- `perf`: Performance improvement
- `test`: Adding/updating tests
- `build`: Build system or dependencies
- `ci`: CI/CD configuration
- `chore`: Maintenance tasks

### Rules
1. **NO EMOJIS** in commit messages
2. Use lowercase for type and scope
3. Description in imperative mood ("add" not "added")
4. Keep first line under 72 characters
5. Reference issues when applicable: `fixes #123`

### Examples
```
feat(i18n): add Privacy section translations for 51 languages
fix(session): restore previous session on back button click
docs(changelog): update for v1.2.9 release
refactor(providers): simplify OAuth reconnection logic
```

### Bad Examples (avoid)
```
Added new feature             # Missing type
feat: Add New Feature         # Use lowercase
feat: added a new feature     # Use imperative mood
```

---

## Code Style

### TypeScript/React
- Use functional components with hooks
- Prefer `const` over `let`
- Use TypeScript strict mode
- Keep components under 300 lines

### Code Hygiene
- Remove dead code immediately: unused functions, variables, imports, and hook files
- Never leave commented-out code in the codebase — use git history instead
- Remove stale TODO/FIXME comments once resolved
- Delete files that are no longer used or referenced
- Keep the codebase clean: no orphan exports, no legacy compatibility shims

### Rust
- Follow `rustfmt` defaults
- Use `clippy` for linting
- Document public APIs with `///`

---

## Documentation

### Public (docs/)
Files visible on GitHub:
- `PROTOCOL-FEATURES.md` - Feature matrix
- `TRANSLATIONS.md` - i18n guide

### Internal (docs/dev/) - Gitignored
Development-only files:
- TODO files, roadmaps, agent instructions
- Audit files and review results
- Not pushed to GitHub

---

## Release Process

### Steps
1. Update version in: `package.json`, `tauri.conf.json`, `Cargo.toml`, `snapcraft.yaml`, `public/splash.html`
2. **Update `com.aeroftp.AeroFTP.metainfo.xml`**: Add new `<release>` entry with version, date, and description. This is what Ubuntu App Center / GNOME Software displays for license, release notes, and app info.
3. **Update `CHANGELOG.md`** (critical - this becomes the GitHub Release body):
   - Add a new `## [X.Y.Z] - YYYY-MM-DD` section at the top
   - Write a short subtitle summarizing the release theme (e.g. `### Secure Credential Storage`)
   - Optionally add a 1-2 sentence description paragraph
   - Group changes under `#### Added`, `#### Fixed`, `#### Changed`, `#### Removed` as needed
   - Each entry should be a concise, user-facing description with **bold lead** and explanation
   - This text is extracted automatically by CI and published as the GitHub Release notes
4. **Sync i18n translations**: Run `npm run i18n:sync` to propagate new keys to all 47 languages, then translate Italian (`it.json`) manually. Other languages get `[NEEDS TRANSLATION]` placeholders.
5. **Validate i18n**: Run `npm run i18n:validate` to ensure no missing keys
6. Commit: `chore(release): vX.Y.Z Short Release Title`
7. Tag: `git tag -a vX.Y.Z -m "Release vX.Y.Z - Short Release Title"`
8. Push: `git push origin main --tags`
9. GitHub Actions builds, extracts CHANGELOG section, and publishes the release automatically

### Automated CI/CD (.github/workflows/build.yml)
When a tag is pushed, GitHub Actions automatically:

| Platform | Artifacts | Destination |
|----------|-----------|-------------|
| Linux | `.deb`, `.rpm`, `.AppImage`, `.snap` | GitHub Releases |
| Windows | `.msi`, `.exe` | GitHub Releases |
| macOS | `.dmg` | GitHub Releases |
| **Snap** | `.snap` | **Snap Store (stable)** |

**Snap Store auto-publish**: The workflow uploads to Snap Store using `snapcraft upload --release=stable`. Requires `SNAPCRAFT_STORE_CREDENTIALS` secret configured in GitHub repo settings.

### Verify Release
```bash
# Check workflow status
gh run list --limit 5

# Check specific run
gh run view <run-id>
```

### Manual Snap Upload (fallback)
Only if CI fails or secret is not configured:
```bash
snapcraft upload aeroftp_X.Y.Z_amd64.snap --release=stable
```

---

## i18n Guidelines

- English (`en.json`) is the reference
- All 47 languages must stay at 100%
- Run `npm run i18n:validate` before commits
- Technical terms (FTP, SFTP, OAuth) are not translated
- **Backend errors are currently in English** — all 22 providers return hardcoded English error strings. Frontend shows translated toast titles but raw English bodies. Planned: structured error codes + frontend i18n mapping (see ROADMAP.md)

---

## Versione corrente: v3.1.0

### Stack tecnologico
- **Backend**: Rust (Tauri 2) con russh 0.57 + ssh2 0.9 (SFTP hybrid), suppaftp 8, reqwest 0.13, quick-xml 0.39, zip 7
- **Frontend**: React 18 + TypeScript + Tailwind CSS
- **Protocolli**: FTP, FTPS, SFTP, WebDAV, S3, Google Drive, Dropbox, OneDrive, MEGA, Box, pCloud, Azure Blob, 4shared, Filen, Zoho WorkDrive, Internxt, kDrive, Koofr, Jottacloud, FileLu, Yandex Disk, OpenDrive (22 totali, Drime Cloud dev-only)
- **Archivi**: ZIP (AES-256), 7z (AES-256), TAR, GZ, XZ, BZ2, RAR (extract)
- **i18n**: 47 lingue al 100%
- **CI/CD**: GitHub Actions → GitHub Releases (Snap Store auto-publish paused pending review)

### Dipendenze critiche
| Crate | Versione | Note |
|-------|----------|------|
| russh | 0.57 | SSH/SFTP (connection, listing, download) |
| ssh2 | 0.9.5 | SFTP upload via SCP (vendored OpenSSL) |
| suppaftp | 8 | FTP/FTPS con TLS, MLSD/MLST/FEAT |
| reqwest | 0.13 | HTTP client |
| quick-xml | 0.39 | WebDAV/Azure XML parsing |
| keyring | 3 (linux-native) | OS Keyring |
| oauth2 | 5 | OAuth2 PKCE |
| scrypt | 0.11 | Cryptomator KDF |
| aes-kw | 0.2 | AES Key Wrap (RFC 3394) |
| aes-siv | 0.7 | AES-SIV filename encryption |
| aes-gcm-siv | 0.11 | AeroVault v2 nonce-misuse resistant (RFC 8452) |
| chacha20poly1305 | 0.10 | AeroVault v2 cascade mode |
| tokio-util | 0.7 | Streaming I/O (AsyncReadExt) |

### Completato in v1.5.2

- ~~Fix SEC-001: zeroize ZIP password con secrecy crate~~ Done
- ~~Fix SEC-004: wrap OAuth tokens in SecretString~~ Done
- ~~Multi-protocol sync (provider_compare_directories)~~ Done
- ~~Codebase audit: rimossi 7 crate, 3 componenti orfani, duplicati crypto~~ Done
- ~~Fix credential loading al primo avvio (keyring probe fallback)~~ Done

### Completato in v1.5.4

- ~~In-app download con progress bar (%, MB/s, ETA)~~ Done
- ~~AppImage auto-install (backup → replace → restart)~~ Done
- ~~Periodic update check ogni 24h~~ Done
- ~~Terminal empty-start pattern (no tabs al mount)~~ Done
- ~~Fix tray menu "Check for Updates" handler~~ Done
- ~~Fix i18n update toast~~ Done

### Completato in v1.6.0

- ~~Native function calling (OpenAI, Anthropic, Gemini) — SEC-002 resolved~~ Done
- ~~Streaming responses (SSE/NDJSON per tutti i 7 provider)~~ Done
- ~~Provider-agnostic tools (14 tools via StorageProvider trait)~~ Done
- ~~Chat history persistence (Tauri plugin-fs, 50 conv / 200 msg)~~ Done
- ~~Cost tracking (token count + cost per messaggio)~~ Done
- ~~Context awareness (provider, path, selected files nel system prompt)~~ Done
- ~~i18n complete (122 nuove chiavi `ai.*`, 51 lingue)~~ Done

### Completato in v1.7.0

- ~~Archive browser (ZIP/7z/TAR/RAR list + selective extraction)~~ Done
- ~~AeroVault (AES-256 encrypted containers, .aerovault format)~~ Done
- ~~Cryptomator vault format 8 (scrypt + AES-KW + AES-SIV + AES-GCM)~~ Done
- ~~CompressDialog (format selection, compression levels, password, file info)~~ Done
- ~~AeroFile mode (local-only file manager, toggle remoto, preview panel)~~ Done
- ~~Preview panel ridimensionabile con info file, risoluzione immagini, path~~ Done
- ~~Colonna Type nelle tabelle file (responsive, sortable)~~ Done
- ~~Fix 7z password detection (content probe via for_each_entries)~~ Done
- ~~Icona .aerovault (Shield emerald) in file list~~ Done
- ~~4 nuovi componenti: ArchiveBrowser, VaultPanel, CryptomatorBrowser, CompressDialog~~ Done
- ~~5 nuove dipendenze Cargo: scrypt, aes-kw, aes-siv, data-encoding, jsonwebtoken~~ Done
- ~~i18n: 60+ nuove chiavi (archive, vault, cryptomator, compress), 51 lingue~~ Done
- ~~AeroAgent personality: system prompt con identity, tone, protocol expertise (13 provider)~~ Done
- ~~AeroAgent server context: host, port, user nel system prompt dinamico~~ Done
- ~~AeroAgent local tools: local_mkdir, local_delete, local_write, local_rename, local_search, local_edit~~ Done
- ~~AeroAgent remote_edit: find & replace in file remoti (download → edit → upload)~~ Done
- ~~AeroAgent batch transfers: upload_files, download_files (multi-file)~~ Done
- ~~AeroAgent styled tool display: chip inline con wrench icon, 24 tool labels~~ Done
- ~~AeroAgent tool count: da 14 a 24 tool provider-agnostic~~ Done

### Completato in v1.8.0

- ~~Smart Sync: 3 modalità intelligenti (overwrite_if_newer, overwrite_if_different, skip_if_identical)~~ Done
- ~~Batch Rename dialog con 4 modalità (Find/Replace, Prefix, Suffix, Sequential) + live preview~~ Done
- ~~Inline Rename: F2 o click su filename selezionato, entrambi i pannelli~~ Done
- ~~Unified date format: `Intl.DateTimeFormat` per tutte le 51 lingue~~ Done
- ~~Colonna PERMS responsive (hidden sotto xl breakpoint, no wrapping)~~ Done
- ~~Toolbar reorganization con separatori visivi~~ Done
- ~~Disconnect icon: X → LogOut per UX clarity~~ Done
- ~~**AeroVault v2**: Military-grade encryption che supera Cryptomator~~ Done
  - AES-256-GCM-SIV (RFC 8452) — nonce misuse-resistant content encryption
  - AES-256-KW (RFC 3394) — key wrapping per master key protection
  - AES-256-SIV — deterministic filename encryption
  - Argon2id — 128 MiB / t=4 / p=4 (supera OWASP 2024)
  - HMAC-SHA512 — header integrity verification
  - ChaCha20-Poly1305 — optional cascade mode per defense-in-depth
  - 64KB chunks — optimal balance security/performance
- ~~Cryptomator spostato da toolbar a context menu (legacy support)~~ Done
- ~~VaultPanel security badges (AES-256-GCM-SIV, Argon2id, AES-KW, HMAC-SHA512)~~ Done
- ~~2 nuove dipendenze Cargo: aes-gcm-siv 0.11, chacha20poly1305 0.10~~ Done
- ~~Audit completo AeroVault v2 in docs/dev/AEROVAULT-V2-AUDIT.md~~ Done

### Completato in v1.9.0

- ~~Consolidare duplicati `formatBytes`, `getMimeType`, `UpdateInfo`~~ Done (v1.8.7)
- ~~Gating console.log dietro debug mode~~ Done (v1.8.7)
- ~~Vision/multimodal per GPT-4o, Gemini, Claude, Ollama~~ Done (v1.8.8)
- ~~Multi-step autonomous tool calls (fino a 10 step, safe=auto, medium/high=pause, stop button)~~ Done
- ~~Ollama model auto-detection via `GET /api/tags` — pulsante "Detect" in AI Settings~~ Done
- ~~Sliding window context management — token budget 70% di maxTokens, summary automatico~~ Done
- ~~Conversation export (Markdown/JSON) — download icon in chat header~~ Done
- ~~Full system prompt editor — 5a tab "System Prompt" in AI Settings con toggle e textarea~~ Done
- ~~Monaco → Agent context menu "Ask AeroAgent" (Ctrl+Shift+A)~~ Done
- ~~Agent → Monaco live sync — `file-changed` e `editor-reload` custom events~~ Done
- ~~Agent → Terminal commands — `terminal_execute` tool, dispatch a PTY integrato~~ Done
- ~~Unified Keystore Consolidation — server profiles, AI config, OAuth in vault.db (AES-256-GCM + Argon2id)~~ Done
- ~~Keystore Backup/Restore — `keystore_export.rs`, file `.aeroftp-keystore` (Argon2id + AES-256-GCM)~~ Done
- ~~Migration Wizard — 4 step (Detect → Preview → Migrate → Cleanup), auto-trigger al primo avvio~~ Done
- ~~RAG Integration — `rag_index` (scansione dir + preview) + `rag_search` (full-text), auto-context nel system prompt~~ Done
- ~~Plugin System — `plugins.rs` (list/execute/install/remove), JSON manifest + shell scripts, tab Plugins in AI Settings~~ Done
- ~~Dual Security Audit — Claude Opus 4.6 (B+) + GPT-5.2-Codex (7 findings) — tutti risolti~~ Done
- ~~AeroAgent tool count: da 25 a 27 (+ rag_index, rag_search)~~ Done
- ~~AI Settings tabs: da 5 a 6 con nuova tab "Plugins"~~ Done
- ~~Filen 2FA passthrough support — campo condizionale `twoFactorCode`~~ Done
- ~~OpenAI header hardening — no panic su header invalidi + HTTP status check~~ Done
- ~~URL scheme allowlist — solo http/https/mailto in `openUrl.ts`~~ Done
- ~~Secure delete chunked — 1 MiB chunks con OpenOptions (no truncate)~~ Done
- ~~AeroVault directory support — `vault_v2_create_directory` con auto-intermediate dirs, breadcrumb navigation, "New Folder" UI~~ Done
- ~~AeroVault recursive delete — `vault_v2_delete_entries` con recursive support, `vault_v2_add_files_to_dir` per aggiunta file in subdirectory~~ Done
- ~~AeroPlayer engine rewrite — Howler.js rimosso, native HTML5 `<audio>` + Web Audio API graph~~ Done
- ~~AeroPlayer 10-band EQ reale — BiquadFilterNode per banda, 10 preset, StereoPannerNode balance~~ Done
- ~~AeroPlayer beat detection — onset energy con circular buffer, exponential decay 0.92~~ Done
- ~~AeroPlayer 6 WebGL shader — Wave Glitch, VHS, Mandelbrot, Raymarch Tunnel, Metaball, Particles (port da CyberPulse)~~ Done
- ~~AeroPlayer post-processing — vignette, chromatic aberration, CRT scanlines, glitch on beat~~ Done
- ~~AeroPlayer 14 modalita visualizer — 8 Canvas 2D + 6 WebGL 2 GPU, tasto V cicla tutte~~ Done
- ~~AIChat.tsx modularizzazione — da 2215 a ~1436 righe, 7 moduli estratti (types, utils, tokenInfo, systemPrompt, images hook, conversations hook, header)~~ Done
- ~~Fix chat persist effect — rimossa guardia `activeConversationId`, nuove conversazioni salvate subito~~ Done
- ~~Fix computeTokenInfo fallback — gestione provider che restituiscono solo `tokens_used`~~ Done
- ~~Plugin tool approval resolution — ToolApproval/BatchToolApproval usano `allTools` prop per danger level accurati~~ Done
- ~~requiresApproval esteso — supporto parametro opzionale `allTools` per plugin tools~~ Done

### Completato in v2.0.7 (Translation Quality Audit)

- ~~605 silent intruder keys eliminated across 46 locales~~ Done
- ~~Armenian script restoration: 63 Latin romanizations + 53 English translations~~ Done
- ~~Chinese native review corrections: 16 fixes~~ Done
- ~~Placeholder format standardized: 173 `{{param}}` → `{param}` fixes~~ Done
- ~~5 connection keys translated in 45 languages~~ Done
- ~~11 orphaned keys removed from ja/ko/zh~~ Done
- ~~metainfo.xml languages list corrected (47 actual languages)~~ Done
- ~~TRANSLATIONS.md batch workflow documentation~~ Done

### Completato in v2.0.6 (Theme System, Security Toolkit & Complete i18n)

- ~~4-Theme System: Light, Dark, Tokyo Night, Cyber con CSS custom properties e `data-theme` attribute~~ Done
- ~~Theme toggle cycle: auto → light → dark → tokyo → cyber, themed PNG icons per connection screen~~ Done
- ~~Security Toolkit (Cyber only): Hash Forge (MD5, SHA-1, SHA-256, SHA-512, BLAKE3), CryptoLab (AES-256-GCM, ChaCha20-Poly1305), Password Forge (CSPRNG + BIP39 passphrase)~~ Done
- ~~8 nuovi comandi Rust in `cyber_tools.rs`: hash_text, hash_file, compare_hashes, crypto_encrypt_text, crypto_decrypt_text, generate_password, generate_passphrase, calculate_entropy~~ Done
- ~~Terminal theme auto-sync: Light→Solarized Light, Dark→GitHub Dark, Tokyo→Tokyo Night, Cyber→Cyber, con `userOverrideRef` pattern~~ Done
- ~~Nuovo tema terminale `cyber`: verde neon su nero profondo (#0a0e17)~~ Done
- ~~Monaco Cyber theme: syntax highlighting verde neon, registrato in `handleEditorDidMount`~~ Done
- ~~Monaco dev mode fix: Vite plugin `configureServer()` middleware per servire AMD assets da `node_modules`~~ Done
- ~~About dialog credits: rimosso "AI: Claude Opus 4.5 + support models", sostituito con "Rust + React 18 + TypeScript"~~ Done
- ~~Complete i18n: 360 nuove chiavi, 47 lingue tradotte al 100%, zero `[NEEDS TRANSLATION]`~~ Done
- ~~RTL locale rimossi: ar.json, fa.json, he.json, ur.json — RTL CSS non ancora implementato~~ Done
- ~~BLAKE3 crate aggiunto a Cargo.toml~~ Done

### Completato in v2.0.5 (System Startup, Tray Icon & Windows Explorer Badge)

- ~~Autostart on system startup — `tauri-plugin-autostart`, toggle in Settings > General > Startup, cross-platform (LaunchAgent/`.desktop`/Registry), OS state sync, idempotent enable/disable with UI rollback~~ Done
- ~~White monochrome tray icon — `AeroFTP_simbol_white_120x120.png` replacing full-color, standard system tray style (Dropbox/Slack/Discord), both `lib.rs` initial icon and `tray_badge.rs` BASE_ICON_BYTES~~ Done
- ~~3 i18n keys (startupOptions, launchOnStartup, launchOnStartupDesc) — 51 lingue, it.json tradotto manualmente~~ Done
- ~~Security counter-audit (3 Opus agents): A- security, B+ quality, 9/9 integration — 2 Medium fixed (TOCTOU eliminated, error rollback)~~ Done
- ~~Windows Named Pipe IPC server (#102) — `\\.\pipe\aerocloud-sync`, `first_pipe_instance(true)` anti-squatting, Semaphore(10), same protocol as Unix~~ Done
- ~~Windows Cloud Filter API badges (#101) — `cloud_filter_badge.rs`, `CfSetInSyncState`/`CfRegisterSyncRoot`, native Explorer sync icons~~ Done
- ~~Cross-platform protocol refactor — generic `handle_client_generic<R,W>` + `handle_protocol_line_generic<R,W>` via `AsyncBufRead + AsyncWrite` traits~~ Done
- ~~NSIS installer hooks stub (#104) — `installer/hooks.nsh` prepared for future COM DLL~~ Done
- ~~Tray badge refinement — removed white border (solid fill like Ubuntu Livepatch), bottom-right position fine-tuned~~ Done
- ~~Platform-aware SettingsPanel — Windows shows "managed automatically via Cloud Filter API"~~ Done
- ~~Windows crate 0.58 conditional dependency — `#[cfg(windows)]` with Cloud Filter, Shell, Foundation features~~ Done
- ~~OwnCloud WebDAV preset removed — Kiteworks acquisition, paid-only plans, rimosso da registry, ProtocolSelector, ProviderLogos, SavedServers, AI prompt, 51 locales, README, snapcraft~~ Done
- ~~Protocol Selector UX fix — Edit button chiude dropdown, re-open azzera selezione, sync stato interno ProtocolSelector~~ Done
- ~~Security audit v2.0.5 (3 Opus agents, 7 findings): SBA-001 bounded reader rewrite (fill_buf+to_vec), SBA-002 reject_remote_clients, SBA-004 idle timeout 60s, SBA-005 UNC path blocking, GB2-001 CfRegisterSyncRoot simplification, GB2-002 #[cfg(unix)] GIO emblems, GB2-012/013 RwLock poison recovery~~ Done

### Completato in v2.0.5 (Places Sidebar Pro + Windows Explorer Badge)

- ~~Places Sidebar GVFS network shares — scansione `/run/user/<uid>/gvfs/`, parsing nomi (SMB/SFTP/FTP/WebDAV/NFS/AFP), Globe icon, eject via `gio mount -u`~~ Done
- ~~Places Sidebar unmounted partitions — `lsblk -J -b`, mount via `udisksctl`, filtro EFI/swap/recovery/MSR~~ Done
- ~~Places Sidebar EFI hidden — `/boot/efi`, `/boot`, `/efi` filtrati come Nautilus~~ Done
- ~~Recent locations individual delete — pulsante X su hover, `pointer-events-none` quando nascosto~~ Done
- ~~Recent/Clear All scrollbar overlap — `pr-4`/`right-4` per evitare sovrapposizione scrollbar~~ Done
- ~~Autostart on system startup — `tauri-plugin-autostart`, toggle in Settings~~ Done
- ~~Windows Named Pipe IPC server — `\\.\pipe\aerocloud-sync`, anti-squatting, rate limiter~~ Done
- ~~Windows Cloud Filter API badges — `CfSetInSyncState`, no COM DLL, Windows 10 1709+~~ Done
- ~~OwnCloud removal — rimosso dopo acquisizione Kiteworks~~ Done
- ~~Protocol Selector fix — reset on re-open, sync isOpen state~~ Done
- ~~Security audit v2.0.5 (30 findings, 6 fixed) — 3 revisori Opus: GVFS eject, lsblk size fallback, EFI /efi, pointer-events, scrollbar alignment, PSEUDO_FS_TYPES comment~~ Done

### Completato in v2.0.5 (4shared Native API + CloudMe WebDAV)

- ~~4shared native REST API provider — `fourshared.rs` completo con OAuth 1.0 (HMAC-SHA1), ID-based file system, folder/file caching, 15GB free~~ Done
- ~~OAuth 1.0 signing module — `oauth1.rs` riutilizzabile, RFC 5849 compliant, HMAC-SHA1, 3-step token flow~~ Done
- ~~CloudMe WebDAV preset — `webdav.cloudme.com:443`, 3GB free, username/password~~ Done
- ~~Custom serde deserializer `string_or_i64` — gestione robusta dei Long ID come stringhe JSON~~ Done
- ~~Per-entry JSON parsing — `parse_folder_list`/`parse_file_list` never-fail con skip on failure~~ Done
- ~~`resolve_path()` — risoluzione path relativi contro `current_path`, applicato a tutti i 10 metodi StorageProvider~~ Done
- ~~StatusBar path/quota overlap fix — `min-w-0 flex-1` invece di `max-w-md` fisso~~ Done
- ~~6 comandi Tauri — `fourshared_start_auth`, `fourshared_complete_auth`, `fourshared_full_auth`, `fourshared_connect`, `fourshared_has_tokens`, `fourshared_logout`~~ Done

### Completato in v2.0.3 (Production Rendering Fix)

- ~~Critical: CSP nonce injection root cause — Tauri 2 nonces override `unsafe-inline`, blocking all dynamic styles, workers, WebGL, IPC~~ Done
- ~~Monaco AMD loading — switched from ESM blob proxy to AMD copy-assets approach (Vite plugin + `loader.config`)~~ Done
- ~~WebKitGTK hardening — `WEBKIT_DISABLE_DMABUF_RENDERER=1`, `allowTransparency: false`, `drawBoldTextInBrightColors: true`~~ Done
- ~~HTML Preview CSS fix — removed iframe `sandbox` attribute, blob URL provides origin isolation~~ Done
- ~~AeroPlayer WebGL fix — CSP was blocking shader compilation, now fully functional~~ Done
- ~~Dead code cleanup — removed `monacoSetup.ts` (ESM worker proxy), removed `open_devtools()` and `devtools` feature flag~~ Done

### Completato in v2.0.0 (AeroAgent Pro)

#### Phase 1 — Provider Intelligence Layer
- ~~Per-provider system prompt profiles — `aiProviderProfiles.ts` con 7 profili (OpenAI, Anthropic, Gemini, xAI, OpenRouter, Ollama, Custom)~~ Done
- ~~Model capability registry — capabilities embedded nei provider profiles~~ Done
- ~~Provider-specific parameter presets — `PARAMETER_PRESETS` (creative/balanced/precise)~~ Done
- ~~Counter-audit Phase 1+2: 19 findings (3 critical, 5 high, 11 medium) — tutti risolti~~ Done

#### Phase 2 — Advanced Tool Execution Engine
- ~~DAG-based tool pipeline — `aiChatToolPipeline.ts` con topological sort e parallel execution~~ Done
- ~~Diff preview for edits — `preview_edit` Rust command + preview in ToolApproval~~ Done
- ~~Intelligent retry — `aiChatToolRetry.ts` con 8+ error strategies~~ Done
- ~~Tool validation layer — `aiChatToolValidation.ts` + `validate_tool_args` Rust command~~ Done
- ~~Composite tool macros — `aiChatToolMacros.ts` con `{{var}}` templates, 7a tab "Macros"~~ Done
- ~~Tool progress indicators — `ToolProgressIndicator.tsx` con `ai-tool-progress` events~~ Done

#### Phase 3 — Context Intelligence
- ~~Project-aware context — `context_intelligence.rs` detect_project_context (10 linguaggi)~~ Done
- ~~File dependency graph — `scan_file_imports` (6 linguaggi, LazyLock regex)~~ Done
- ~~Persistent agent memory — `.aeroagent` file, `useAgentMemory.ts` hook, `agent_memory_write` tool (#28)~~ Done
- ~~Conversation branching — fork/switch/delete, `ConversationBranch.tsx`, branch persistence~~ Done
- ~~Smart context injection — `aiChatSmartContext.ts` con intent analysis e priority allocation~~ Done
- ~~Token budget optimizer — `TokenBudgetIndicator.tsx`, `computeTokenBudget()`, 3 budget modes~~ Done
- ~~Counter-audit Phase 3: 93 findings (0C, 8H, 25M, 37L, 23I) — tutti risolti (37 fix)~~ Done
- ~~Path validation — `validate_context_path()` per tutti i 5 comandi context_intelligence~~ Done
- ~~19 regex LazyLock — compilazione statica per performance~~ Done
- ~~TOCTOU mutex — `MEMORY_WRITE_LOCK` per agent memory writes~~ Done
- ~~Dead code removal — ForkButton, FTP_TOOLS, requiresApproval, PARAMETER_PRESETS export, tipi duplicati~~ Done
- ~~Type consolidation — TokenBudgetBreakdown e BudgetMode in single source of truth~~ Done

#### Phase 4 — Professional UX
- ~~Streaming markdown renderer — FinalizedSegment (React.memo) + StreamingSegment~~ Done
- ~~Code block actions — Copy/Apply/Diff/Run buttons, DiffPreview component~~ Done
- ~~Agent thought visualization — ThinkingBlock con token display e duration~~ Done
- ~~Prompt template library — 15 built-in templates, `/` prefix activation, vault persistence~~ Done
- ~~Multi-file diff preview — PR-style diff con per-file checkboxes~~ Done
- ~~Cost budget tracking — per-provider monthly limits, conversation cost, vault-persisted~~ Done
- ~~Chat search — Ctrl+F overlay con role filter e keyboard navigation~~ Done
- ~~Keyboard shortcuts — Ctrl+L/Shift+N/Shift+E/F/÷ con useKeyboardShortcuts hook~~ Done

#### Phase 5 — Provider-Specific Features
##### Tier 1 — High Impact
- ~~Anthropic prompt caching — cache_control ephemeral, 90% read discount, cache savings display~~ Done
- ~~OpenAI structured outputs — strict:true + additionalProperties:false per OpenAI/xAI/OpenRouter~~ Done
- ~~Ollama model-specific templates — 8 family profiles, detectOllamaModelFamily(), getOllamaPromptStyle()~~ Done
- ~~Ollama pull model from UI — POST /api/pull, NDJSON streaming, progress bar in AISettingsPanel~~ Done
- ~~Thinking budget presets — 5 preset (Off/Light/Balanced/Deep/Maximum) + range slider 0-100K~~ Done

##### Tier 2 — Medium Impact
- ~~Gemini code execution — executableCode/codeExecutionResult parsing, GeminiCodeBlock.tsx~~ Done
- ~~Gemini system_instruction — top-level field instead of in-message~~ Done
- ~~Gemini context caching — gemini_create_cache command, cachedContent passthrough~~ Done
- ~~Ollama GPU monitoring — ollama_list_running command, OllamaGpuMonitor.tsx~~ Done
- ~~Native parallel tool calls (#58) — DAG pipeline in tutti i 4 path di esecuzione~~ Done
- ~~Unified thinking/reasoning (#59) — ThinkingBlock per Anthropic/OpenAI o3/Gemini~~ Done
- ~~Analyze UI + Performance templates — /analyze-ui e /performance in prompt library~~ Done

### Completato in v2.1.0 (AeroSync Phase 2 — Operational Reliability)

- ~~**Transfer journal with checkpoint/resume**: Persistent JSON journal in `~/.config/aeroftp/sync-journal/`, keyed by path-pair hash, resume banner UI~~ Done
- ~~**SHA-256 checksum during scan**: Streaming 64KB-chunk SHA-256 in `get_local_files_recursive()` when `compare_checksum=true`~~ Done
- ~~**Structured error taxonomy**: `SyncErrorKind` enum (10 categories), `classify_sync_error()` with pattern matching, retryability hints~~ Done
- ~~**Post-transfer verification**: 4 policies (None, SizeOnly, SizeAndMtime, Full), `verify_local_file()` with size/mtime/hash checks~~ Done
- ~~**Configurable retry with exponential backoff**: `RetryPolicy` with base/max delay, multiplier, per-file timeout (default: 3 retries, 500ms base, 2x, 10s cap, 2min timeout)~~ Done
- ~~**Error breakdown in sync report**: Grouped by `SyncErrorKind` with dedicated icons, retryable vs non-retryable counts~~ Done
- ~~**Session tab context menu**: Right-click Close Tab/Close Other Tabs/Close All Tabs~~ Done
- ~~**Insecure certificate confirmation modal**: Styled modal replacing `window.confirm()` dialogs~~ Done
- ~~**Filen 2FA login fix**: Always send `twoFactorCode` field with `"XXXXXX"` default~~ Done
- ~~**12 Rust unit tests**: Error classification, retry policy, journal resumability, file verification~~ Done
- ~~**23 new i18n keys in 47 languages**: Journal, verification, retry, error taxonomy~~ Done

### Completato post-release v2.1.0 (UX Polish, Rebrand & Transfer Stability)

- ~~**Unified TransferProgressBar**: 7 transfer bars → 1 reusable component (4 feature levels: base/details/batch/graph). Theme-aware shimmer animation, CSS transitions~~ Done
- ~~**SpeedGraph canvas component**: Canvas 2D area chart with quadratic bezier, auto-scale Y axis, stats overlay (current/avg/peak)~~ Done
- ~~**SyncPanel dancing fix**: Progress bar moved to bottom of modal (persistent during sync), slide-down animation, speed graph toggle~~ Done
- ~~**Splash screen removal**: Production builds start instantly — splash was dev-mode-only benefit. Zero traces left in code~~ Done
- ~~**AeroSync rebrand**: syncPanel.title "Synchronize Files" → "AeroSync" in 47 languages~~ Done
- ~~**AeroTools rebrand**: DevTools → AeroTools in all user-facing strings (4 i18n values × 47 languages). Aero Family complete: AeroSync, AeroVault, AeroPlayer, AeroAgent, AeroTools~~ Done
- ~~**TransferToast theme fix**: Toast ora supporta tutti 4 temi (light/dark/tokyo/cyber) + risoluzione tema "auto". `getToastStyles()` per stili per-tema~~ Done
- ~~**TransferToast flicker fix**: Debounce 500ms su dismiss tra file consecutivi in batch. Nuovo `clearToastTimer` ref cancella il timer se arriva nuovo progress~~ Done
- ~~**Transfer state isolation**: `activeTransfer` da `useState` a `useRef` + `useState<boolean>`. `TransferToastContainer` componente isolato subscrive via DOM event `transfer-toast-update`. Zero re-render di App.tsx durante progress~~ Done
- ~~**Theme initialization hardening**: `isDark` inizializzato sincronicamente da saved theme + `prefers-color-scheme`. Eliminato frame di startup con tema errato~~ Done
- ~~**TransferProgressBar side-effect free**: Rimossa dipendenza `useTheme()` — tema letto passivamente da DOM classes via `resolveThemeFromDom()`. Mount/unmount non causano mutazioni tema globali~~ Done
- ~~**prefers-reduced-motion**: `@media (prefers-reduced-motion: reduce)` disabilita shimmer, overlay, indeterminate animations e transizioni nella TransferProgressBar~~ Done
- ~~**AeroPlayer autoplay fix**: Fixed in v2.0.8 — HTML5 Audio prebuffer strategy (MIN_START_BUFFER_SECONDS). Increased prebuffer from 4.0s to 6.0s for reliability~~ Done
- ~~**CSP Phase 1A**: Baseline CSP active in `tauri.conf.json` with `dangerousDisableAssetCspModification: true`. Explicit permissive directives for script-src, style-src, connect-src, worker-src, img-src, font-src, media-src, frame-src~~ Done

### Completato in v2.1.2 (AeroSync Phase 3A — Professional Sync Engine)

- ~~**Sync Profiles (#140)**: 3 built-in presets (Mirror, Two-way, Backup) + custom save/load/delete. `SyncProfile` struct in sync.rs. Profile selector dropdown in SyncPanel header. Each preset bundles direction, compare options, retry/verify policies, delete_orphans~~ Done
- ~~**Conflict Resolution Center (#143)**: Per-file resolution (keep local/remote/skip) + batch actions (Keep Newer All, Keep Local All, Keep Remote All, Skip All). `conflictResolutions` Map integrated in sync flow. Journal entries track conflict resolution~~ Done
- ~~**Bandwidth control UI (#144)**: Upload/download speed limit selectors (128 KB/s to 10 MB/s + unlimited). Auto-detects FTP vs provider backend. Loads current limits on panel open~~ Done
- ~~**Journal auto-cleanup (#145)**: Auto-delete journals >30 days on panel open. "Clear History" button with confirmation. `list_sync_journals`, `cleanup_old_journals`, `clear_all_journals` Rust commands~~ Done
- ~~**Backend progress throttle (F6)**: Emit every 150ms or 2% delta in download_file/upload_file. ~90% IPC reduction on large file transfers~~ Done
- ~~**Parallel local+remote scan (F2)**: `tokio::join!` in `compare_directories` — concurrent filesystem + remote scan~~ Done
- ~~**Compact JSON for journal/index (F7)**: `to_string` instead of `to_string_pretty` in save_sync_journal/save_sync_index~~ Done
- ~~**Plugin integrity verification (SEC-P2-02)**: SHA-256 hash at install, verified before execution. Counter-audit: 6 findings fixed (hash validation, symlink escape, env isolation)~~ Done
- ~~**Tauri FS scope hardening (SEC-P2-03)**: Explicit fs:scope, deny-webview-data, shell:default, removed 5 redundant permissions~~ Done
- ~~**39 new i18n keys**: Sync profiles, conflict resolution, bandwidth control, icon themes, session context menu — translated in all 47 languages~~ Done

### Completato in v2.2.0 (AeroSync Phase 3A+ — Complete Frontend Integration)

- ~~**Tab-based Sync UX**: Quick Sync (3 preset cards) + Advanced (granular controls). SyncPanel refactored from 1867 to ~1100 lines with 9 extracted sub-components~~ Done
- ~~**Speed Mode presets**: 5 levels (Normal/Fast/Turbo/Extreme/Maniac) auto-configuring parallel streams, compression, and delta sync~~ Done
- ~~**Maniac Mode**: Cyber-theme easter egg — disables all safety for max speed, mandatory post-sync verification~~ Done
- ~~**Sync Scheduler UI**: Interval selector, pause/resume, countdown, time window with day picker~~ Done
- ~~**Filesystem Watcher status**: Real-time health indicator with inotify capacity warnings~~ Done
- ~~**Multi-Path Editor**: CRUD for multiple local↔remote path pairs~~ Done
- ~~**Sync Template dialog**: Export/import `.aerosync` files via Tauri file dialog~~ Done
- ~~**Rollback Snapshot dialog**: List/create/delete snapshots, file preview~~ Done
- ~~**Security Audit Remediation**: 31 findings fixed across 6 auditors (4x Opus + GPT-5.3 + GLM-5). Grade: B → A-~~ Done
  - CF-001: Maniac max_retries=0 silent failure + Math.max safety net
  - CF-002: Template export/import API rewrite
  - CF-003/004: Path traversal prevention in 8 sync commands + validate_relative_path
  - CF-005: Scheduler empty days crash (serde default + null→[])
  - CF-007: Symlink following → symlink_metadata
  - CF-008: Atomic writes for journal/index (temp+rename)
  - CF-009: JoinSet error collection
  - H-004: Overnight scheduler window carry-over + 5 tests
  - RB-001: Non-deterministic hash → DJB2 stable
  - RB-003: NaN/Infinity retry delay guard
  - RB-020: Hidden file blanket exclusion → specific blacklist
  - RB-033/034/036: Delta sync hardening (readability, 64MB literal cap, bounds check)
  - SP-004: Journal O(1) lookup via Map
  - SP-009/010: Timer leak + cancelled state fix
  - 18 hardcoded strings internationalized
- ~~**43 new i18n keys in 47 languages**: Speed modes, scheduler, watcher, templates, rollback, audit fixes~~ Done
- ~~**Splash screen with loading sequence**: 21-step simulated module loading (Tauri runtime → protocol handlers → encryption → AeroAgent → Monaco → IPC bridge). Variable timing + jitter. 10s safety timeout~~ Done
- ~~**Advanced tab accordion**: 4 collapsible sections (Direction, Compare, Transfer, Automation) with smooth CSS transitions~~ Done
- ~~**Password Forge 24-word passphrases**: Max from 12 to 24 words, BIP-39 disclaimer at 12+ words, translated in 47 languages~~ Done
- ~~**Cyber shield icon**: Anonymous mask → shield+lock in theme selector and Settings~~ Done
- ~~**Scrollbar z-index fix**: `html.modal-open` hides non-modal scrollbars on WebKitGTK~~ Done
- ~~**Splash menu flash fix**: Splash created before `app.set_menu()` to prevent GTK menu inheritance~~ Done

### Completato in v2.2.2 (AeroAgent File Management Pro)

- ~~**AeroAgent 9 file management tools**: `local_move_files` (batch move), `local_batch_rename` (regex/prefix/suffix/sequential), `local_copy_files` (batch copy), `local_trash` (recycle bin), `local_file_info` (metadata), `local_disk_usage` (recursive size), `local_find_duplicates` (hash-based), `archive_compress` (ZIP/7z/TAR), `archive_decompress` (extract archives)~~ Done
- ~~**AeroAgent 8 power tools**: `local_grep` (regex search in files), `local_head` (first N lines), `local_tail` (last N lines), `local_stat_batch` (batch metadata, 100 paths), `local_diff` (unified diff via `similar` crate), `local_tree` (recursive dir tree), `clipboard_read`, `clipboard_write`. Tool count: 36 → 44~~ Done
- ~~**Extreme Mode**: Cyber-only, auto-approves all tool calls, 50-step limit, fully autonomous~~ Done
- ~~**Duplicate tool call prevention**: `executedToolSignaturesRef` dedup — prevents Llama/etc from repeating identical tool calls in multi-step execution~~ Done
- ~~**Connection context awareness**: System prompt distinguishes AeroCloud vs Server vs AeroFile mode~~ Done
- ~~**Relative path resolution**: All local tools resolve relative filenames via `resolve_local_path()`~~ Done
- ~~**AeroSync + AI Settings 4-theme CSS**: SyncPanel and AISettingsPanel follow all 4 themes via CSS custom properties~~ Done
- ~~**21 new i18n keys in 47 languages**: 18 tool labels + 3 Extreme Mode keys~~ Done
- ~~**`similar = "2"` crate**: Unified diff library for `local_diff` tool~~ Done

### Completato in v2.2.3 (AeroAgent Welcome Screen & Shell Execute)

- ~~**AeroAgent welcome screen redesign**: Lucide icons, 3x3 capability grid (Files, Code, Search, Archives, Shell, Vault, Sync, Context, Vision), API key setup banner, clickable quick prompts (context-aware: connected vs local)~~ Done
- ~~**`shell_execute` backend tool**: Replaces `terminal_execute` frontend-only approach — real stdout/stderr/exit_code capture via Rust `Command`, 30s timeout, 1MB output limit~~ Done
- ~~**i18n error path fix**: 9 wrong paths in `formatProviderError` corrected (e.g. `ai.errors.network` → `ai.providerErrors.network`)~~ Done
- ~~**i18n structural audit**: 1188 leaked keys fixed across 46 locales, validation script rewritten with 8 checks~~ Done
- ~~**29 new i18n keys in 47 languages**: Welcome screen capabilities, quick prompts, setup banner~~ Done
- ~~**AeroAgent tool count**: 44 → 45 (shell_execute replaces terminal_execute)~~ Done

### Completato in v2.2.4 (Provider Marketplace, 2FA, Remote Vault, CLI)

- ~~**Provider Marketplace**: Searchable modal grid (6 categorie, feature badges, pricing tiers) sostituisce i bottoni "Add:" in AI Settings~~ Done
- ~~**5 nuovi provider AI**: Mistral, Groq, Perplexity, Cohere, Together AI — tutti OpenAI-compatible con icone SVG, model registry, provider profiles. Totale: 10 → 15~~ Done
- ~~**TOTP 2FA per vault**: Secondo fattore RFC 6238 opzionale per master password. QR code setup, 6-digit verification, enable/disable in Settings > Security. Rate limiting esponenziale (5 tentativi → lockout 30s-15min)~~ Done
- ~~**Remote Vault**: Apertura file `.aerovault` su server remoti — download → operazioni locali → "Save & Close" upload. Validazione sicurezza: null bytes, path traversal, symlink, canonicalize, Unix 0o600~~ Done
- ~~**CLI foundation**: Binary `aeroftp-cli` con `connect`, `ls`, `get`, `put`, `sync` via Clap. URL-based connection, progress bars indicatif~~ Done
- ~~**CSP violation reporter**: Listener `securitypolicyviolation` con logging debug-gated, guardia idempotenza, URI troncato~~ Done
- ~~**Security audit remediation**: 5 revisori indipendenti (4x Claude Opus 4.6 + GPT-5.3 Codex), 13 finding fixati su 8 file~~ Done
- ~~**TOTP hardening**: Singolo `Mutex<TotpInner>`, gate `setup_verified`, rate limit esponenziale, `OsRng`, zeroize bytes, poison recovery~~ Done
- ~~**Modal accessibility**: ARIA attributes, Escape handler, state cleanup, focus management per TotpSetup e ProviderMarketplace~~ Done
- ~~**Provider API fix**: Cohere baseUrl `/v2` → `/compatibility`, Perplexity toolFormat `native` → `text`~~ Done
- ~~**SVG gradient fix**: `useId()` per QwenIcon/CohereIcon, dead default export rimosso da AIIcons.tsx~~ Done
- ~~**AISettingsPanel performance**: `useMemo` su provider type Set~~ Done

### Completato in v2.3.0 (Chat History SQLite Redesign)

- ~~**SQLite WAL + FTS5 backend**: `chat_history.rs` — WAL mode, FTS5 full-text search, 4 tables (sessions, messages, branches, stats), in-memory fallback~~ Done
- ~~**18 Tauri commands**: CRUD (save/load/delete/rename sessions + save/load messages), search (FTS5 + snippet), export/import (JSON), branches (list/save/load/delete), stats, cleanup, clear-all~~ Done
- ~~**Chat History Manager UI**: `ChatHistoryManager.tsx` — retention policies (7/30/90/180/365/unlimited), full-text search with `<mark>` snippets, session browser, stats dashboard (DB size, tokens, cost)~~ Done
- ~~**FTS5 XSS prevention**: `sanitize_fts_snippet()` — escapes HTML then restores `<mark>` tags. `sanitize_fts_query()` — wraps in double quotes~~ Done
- ~~**Retention auto-apply**: `retentionAppliedRef` + `cleanupHistory(days)` in AIChat.tsx useEffect — applies on mount per session~~ Done
- ~~**Dedicated clear-all**: `chat_history_clear_all` Rust command — explicit DELETE across all 4 tables instead of iterative delete~~ Done
- ~~**In-memory fallback**: Graceful degradation if SQLite file cannot be opened — zero data loss, zero crash~~ Done
- ~~**55+ audit findings resolved**: 5 independent auditors (4x Claude Opus 4.6 + GPT-5.3 Codex) — SQL injection, XSS, WAL mode, retention, error handling~~ Done
- ~~**24 i18n keys**: Translated in all 47 languages via GLM batch method~~ Done

### Completato in v2.4.0 (Provider Integration Audit & Zoho WorkDrive)

- ~~**Zoho WorkDrive provider** (16° protocollo): Full OAuth2 con 8 endpoint regionali (US/EU/IN/AU/JP/UK/CA/SA), team ID detection, file operations, trash management, share links, storage quota. `zoho_workdrive.rs` ~900 righe~~ Done
- ~~**Streaming upload refactor**: FTP, Dropbox, OneDrive, Google Drive, Box — chunk reading da file handle invece di full-buffer. Elimina OOM su file grandi (BT-GPT-H01)~~ Done
- ~~**FTP TLS downgrade detection**: Flag `tls_downgraded` + security warning logging quando TLS upgrade fallisce su `ExplicitIfAvailable` (BT-GPT-H02)~~ Done
- ~~**SecretString per tutte le credenziali**: Access tokens, refresh tokens, API keys wrapped in `secrecy::SecretString` su tutti i 16 provider~~ Done
- ~~**Migrazione quick-xml 0.39**: Tutto il parsing XML regex rimpiazzato con parser event-based (WebDAV, Azure, S3). `trim_text(true)` per Azure (BT-API-012)~~ Done
- ~~**StorageProvider trait expansion**: 18 metodi totali — `stat()`, `search()`, `move_file()`, `list_trash()`, `restore_from_trash()`, `permanent_delete()`, `create_share_link()`, `get_storage_quota()`, `list_versions()`, `download_version()`, `restore_version()`~~ Done
- ~~**OneDrive 4MB auto-threshold**: Auto-switch a resumable upload per file >4MB~~ Done
- ~~**Fix paginazione**: S3 continuation token loop, Azure NextMarker loop, 4shared ID-based pagination~~ Done
- ~~**12-auditor security audit**: 4 fasi (A: Capabilities, B: Security GPT-5.3, C: Integration Opus, D: Bugs Terminator Counter-Audit). Grado A-. Tutti i finding risolti~~ Done
- ~~**ZohoTrashManager.tsx**: Componente dedicato gestione cestino Zoho con restore e permanent delete~~ Done
- ~~**Zoho i18n**: Nuove chiavi (zohoworkdrive, zohoworkdriveDesc, selettore regione) in tutte le 47 lingue~~ Done
- ~~**Storj + Cloudflare R2**: Promossi a `stable: true` nel registry~~ Done
- ~~**tokio-util 0.7**: Aggiunto a Cargo.toml per streaming I/O~~ Done

### Completato in v2.4.0 (Security Hardening & UX)

- ~~**SFTP TOFU visual dialog** (SEC-P1-06): PuTTY-style host key verification with SHA-256 fingerprint, MITM warning. `host_key_check.rs` + `HostKeyDialog.tsx` + 3 insertion points~~ Done
- ~~**AeroSync quick wins** (#146, #148, #149): Dry-run export (JSON/CSV), Safety Score badge, explainable sync decisions~~ Done
- ~~**Branded Polkit update dialog**: Custom `com.aeroftp.update.policy` with AeroFTP icon, localized messages (10 langs), `aeroftp-update-helper` script with path validation~~ Done
- ~~**Auto-update restart fix**: Detached shell relaunch (`sleep 1 && exec`) for deb/rpm, preventing silent restart failure~~ Done

### Completato in v2.5.0 (AeroFile Pro: Modularization, Tabs & Tags)

- ~~**LocalFilePanel extraction**: ~730 lines of local panel rendering extracted from App.tsx into `src/components/LocalFilePanel.tsx`. Pure rendering extraction — state/logic remain in App.tsx~~ Done
- ~~**Multiple local path tabs**: `LocalPathTabs.tsx` — up to 12 tabs, drag-to-reorder, context menu (Close/Close Others/Close All), middle-click close, localStorage persistence~~ Done
- ~~**File tags SQLite backend**: `file_tags.rs` — WAL mode, 7 preset Finder-style color labels, 9 Tauri commands for label CRUD, batch tag operations, label counts~~ Done
- ~~**File tag badges**: `FileTagBadge.tsx` — colored dot badges in list/grid views, max 3 with "+N" overflow, React.memo optimized~~ Done
- ~~**Tags context menu submenu**: 7 color labels with toggle semantics + "Clear All Tags" option~~ Done
- ~~**Tags sidebar section**: PlacesSidebar tag labels with colored dots, counts, click-to-filter~~ Done
- ~~**useFileTags hook**: Debounced batch queries (150ms), Map cache, label CRUD, sidebar filter state~~ Done
- ~~**23 new i18n keys**: Local tabs (6) + tags (17 including 7 color names) in all 47 languages~~ Done

### Completato in v2.6.0 (AeroAgent Ecosystem, App-Wide AI Interaction & Provider Tier 3)

- ~~**4 new AI providers (Tier 3)**: AI21 Labs, Cerebras, SambaNova, Fireworks AI — all OpenAI-compatible. Total: 15 → 19 providers~~ Done
- ~~**Command Palette (Ctrl+Shift+P)**: VS Code-style global command palette with ~25 commands across 5 categories~~ Done
- ~~**Plugin Registry (GitHub-based)**: `plugin_registry.rs` with fetch/install Tauri commands, SHA-256 integrity~~ Done
- ~~**Plugin Browser UI**: Searchable modal with Installed/Browse/Updates tabs in AI Settings~~ Done
- ~~**Plugin Hooks system**: Event-driven plugin execution (file:created, transfer:complete, sync:complete)~~ Done
- ~~**Context Menu AI Actions**: "Ask AeroAgent" in local and remote file context menus~~ Done
- ~~**AI Status Widget**: Compact indicator in StatusBar (Ready/Thinking/Running tool/Error)~~ Done
- ~~**Drag & Drop to AeroAgent**: Drag files from file manager into chat area for analysis~~ Done
- ~~**AUR package**: `aeroftp-bin` published on Arch User Repository~~ Done
- ~~**README reorganization**: 4-row badge layout, expanded Installation section with AUR/Launchpad~~ Done
- ~~**19 new i18n keys**: Command palette, plugin browser, context menu AI, drag-to-analyze in 47 languages~~ Done
- ~~**Internxt Drive provider** (17° protocollo): E2E encrypted cloud, OAuth2 PKCE, zero-knowledge architecture. `internxt.rs` ~800 righe~~ Done
- ~~**kDrive provider** (18° protocollo): Infomaniak kDrive, OAuth2 with drive_id, cursor pagination. `kdrive.rs` ~850 righe~~ Done
- **Drime Cloud provider** (dev-only, disabled in release): REST API, Bearer token auth, 20GB free, page-based pagination. `drime_cloud.rs` ~700 righe — partial integration
- ~~**10 new i18n keys**: Internxt (5) + kDrive (5) translated in 47 languages~~ Done
- ~~**Protocol count**: 16 → 18 across all docs, README, metainfo, snapcraft, AI system prompts (Drime Cloud excluded from release)~~ Done

### Completato in v2.6.1 (VS Code-Style Titlebar & UX Polish)

- ~~**VS Code-style unified titlebar**: 4 custom dropdown menus (File, Edit, View, Help) replacing native GTK menu bar. Hover-to-switch, Escape/click-outside close, keyboard shortcut labels~~ Done
- ~~**Header eliminated**: Entire `<header>` block (~110 lines) removed from App.tsx, all toolbar buttons migrated to titlebar~~ Done
- ~~**Cut/Copy/Paste in Edit menu**: File clipboard operations with selection-aware disabled states. `menu.cut/copy/paste` i18n keys in 47 languages~~ Done
- ~~**Theme-aware menu hover**: Menu items highlight with `--color-accent` (blue/purple/green per theme) — VS Code-style~~ Done
- ~~**Settings button in titlebar**: Gear icon between Support and Theme Toggle~~ Done
- ~~**Consistent modal animations**: `animate-scale-in` on all 42 modal dialogs, single CSS rule (0.25s ease-out)~~ Done
- ~~**Click-outside-to-close**: Added to AeroVault and Master Password dialogs~~ Done
- ~~**Modal top alignment**: AeroVault and Master Password use `pt-[5vh]` like Settings/About/Support~~ Done
- ~~**Default backgrounds**: App → Waves, Lock screen → Isometric for new installations~~ Done
- ~~**Menu labels simplified**: "Toggle X" / "Mostra/Nascondi X" → just "X" in all 47 languages~~ Done
- ~~**Dark theme menu fix**: Dropdown background uses `--color-bg-secondary` instead of `--color-bg-primary` (was pure black)~~ Done
- ~~**147 provider audit findings**: Security audit across 8 cloud providers (S3, pCloud, kDrive, Azure, 4shared, Filen, Internxt, MEGA)~~ Done
- ~~**ErrorBoundary fix**: Moved inside I18nProvider in main.tsx~~ Done
- ~~**DebugPanel fix**: try-catch for frozen `__TAURI_INTERNALS__` invoke patching~~ Done
- ~~**Native GTK menu removed**: `showMenuBar`, `systemMenuVisible`, F10 shortcut — all dead code removed~~ Done

### Completato in v2.6.7 (Update Restart, UX Polish & AeroCloud Multi-Protocol)

- ~~**Update restart setsid fix**: .deb, .rpm, AppImage restart after update now works — `spawn_detached_relaunch()` with `libc::setsid()` via `pre_exec` creates independent session surviving parent exit~~ Done
- ~~**AeroCloud multi-protocol support**: Background sync now works with all 18 providers via `cloud_provider_factory.rs` — direct-auth, OAuth2, OAuth1 dispatch~~ Done
- ~~**CloudConfig protocol fields**: `protocol_type` + `connection_params` with `serde(default)` backward compatibility~~ Done
- ~~**CloudPanel 4-step wizard**: Protocol selection grid (Servers, Cloud, OAuth), dynamic connection fields, OAuth authorize flow~~ Done
- ~~**Connection save race condition**: `secureStoreAndClean` was fire-and-forget — vault returned stale data before write completed. All 6 call sites now awaited~~ Done
- ~~**SFTP symlink directory detection**: `list()` follows symlinks via `sftp.metadata()` for NAS devices (WD MyCloud, Synology)~~ Done
- ~~**Dark theme modal consistency**: VaultPanel + SettingsPanel `dark:bg-gray-800` → `dark:bg-gray-900` matching all other modals~~ Done
- ~~**FTP TLS badge dynamic**: Badge hides when encryption set to "none" — `ftpTlsMode` prop on ProtocolSelector~~ Done
- ~~**ProviderSelector unified style**: S3/WebDAV preset cards → horizontal rows matching ProtocolSelector~~ Done
- ~~**S3/WebDAV info cards**: Protocol info card with access requirements and ideal use cases~~ Done
- ~~**AeroVault modal narrower**: `max-w-3xl` (768px) → `max-w-[700px]`~~ Done
- ~~**AeroFile icon-only button**: Text removed, icon with tooltip for symmetry~~ Done
- ~~**14 new i18n keys**: Cloud wizard (10) + S3/WebDAV info (4) — translated in all 47 languages~~ Done

### Completato in v2.6.8 (Selective Server Export)

- ~~**Selective server export**: Export dialog checklist with per-server checkboxes, Select All / Deselect All, color badges, protocol labels, "X / Y selected" counter. Export button shows count and disables when none selected~~ Done

### Completato in v2.6.9 (Seafile Preset & Transfer Circuit Breaker)

- ~~**Seafile WebDAV preset**: Added to registry with dedicated logo and `seafdav` endpoint preset~~ Done
- ~~**Seafile i18n**: New Seafile keys propagated to all 47 locales~~ Done
- ~~**Transfer circuit breaker**: Batch transfer guard with consecutive-error trip, reconnect flow, and exponential backoff retry~~ Done

### Completato in v2.7.0 (FileLu Native API — 19th Protocol)

- ~~**FileLu provider** (19° protocollo): Native REST API (`filelu.rs`), API key auth, full `StorageProvider` trait, 4 presets (FTP/FTPS/WebDAV/S3)~~ Done
- ~~**FileLu special features**: `set_file_password`, `set_file_privacy`, `clone_file`, `set_folder_password`, `set_folder_settings`, `list_deleted_files`, `restore_deleted_file`, `restore_deleted_folder`, `permanent_delete_file`, `remote_url_upload` — 10 nuovi comandi Tauri~~ Done
- ~~**FileLuTrashManager.tsx**: Modal per gestione cestino con multi-select, restore e permanent delete~~ Done
- ~~**39 nuove chiavi i18n**: FileLu UI + `settings.protocolFilelu` + `toast.localPathNotFound` — tutte le 47 lingue~~ Done
- ~~**BaseProtocol fix**: Aggiunto `'filelu'` e `'ftps'` + `endpoint`/`tls_mode` ai defaults in `src/providers/types.ts`~~ Done

### Completato in v2.7.1 (S3 Provider Preset UX & Cloudflare R2 Account ID)

- ~~**S3 endpoint auto-resolution**: endpointTemplate providers (Wasabi, DO, Alibaba, Tencent) auto-select first region and compute endpoint on provider select~~ Done
- ~~**S3 endpoint edit backward compat**: Saved servers without stored endpoint get it resolved from registry on edit~~ Done
- ~~**Cloudflare R2 Account ID field**: Dedicated input with inline endpoint suffix (`.r2.cloudflarestorage.com`), auto-compute, Edit escape hatch~~ Done
- ~~**resolveS3Endpoint extra params**: Supports `{accountId}` and any template variable beyond `{region}`~~ Done
- ~~**SettingsPanel S3 endpoint always visible**: Endpoint field renders for all S3 servers~~ Done
- ~~**SettingsPanel vault password loading**: useEffect loads stored credentials on edit modal open~~ Done
- ~~**Duplicate signup/docs links removed**: Provider header no longer duplicates ProtocolFields links~~ Done

### Completato in v2.7.4 (Complete Provider Integration)

- ~~**Box full feature set**: Trash management, file move, comments, collaborations, watermark (Enterprise), folder locks (Enterprise), tags with inline chips, PRO badge system~~ Done
- ~~**Google Drive starring**: Star/unstar files from context menu, starred status in file metadata~~ Done
- ~~**Google Drive comments**: Add comments to files via context menu prompt dialog~~ Done
- ~~**Google Drive file properties**: Set custom key-value properties and description via API~~ Done
- ~~**Google Drive API fields expanded**: File listing now includes `starred`, `description`, `properties` fields~~ Done
- ~~**Dropbox tag management**: Full tag CRUD via Dropbox Tags API, reuses generic BoxTagsDialog component~~ Done
- ~~**Dropbox Trash Manager**: Dedicated modal for deleted files — restore and permanent delete~~ Done
- ~~**OneDrive Trash Manager**: Full recycle bin — move to trash, list, restore, permanent delete~~ Done
- ~~**BoxTagsDialog made generic**: Accepts optional `command` and `providerName` props for cross-provider reuse~~ Done
- ~~**Zoho WorkDrive labels**: Full label management — list team labels, get/add/remove labels on files, ZohoLabelsDialog with color-coded toggle list~~ Done
- ~~**Zoho WorkDrive versioning**: List versions, download specific version, restore/promote version via StorageProvider trait~~ Done
- ~~**Providers & Integrations dialog**: Tabbed modal (OAuth/API, S3, WebDAV) from Help menu showing feature matrix for all 31 providers~~ Done
- ~~**37 new Tauri commands**: 18 Box + 6 Google Drive + 5 Dropbox + 4 OneDrive + 4 Zoho WorkDrive~~ Done
- ~~**63 new i18n keys**: All translated in 47 languages~~ Done
- ~~**Auto-update fix**: `useAutoUpdate` hook `updateCheckedRef` surviving React 18 strict mode remounts~~ Done

### Completato in v2.7.5 (AeroCloud Multi-Protocol Fix & CLI JSON)

- ~~**AeroCloud SFTP/WebDAV fix**: `parse_server_field()` in `cloud_provider_factory.rs` separates hostname from embedded port — fixes DNS failure on SFTP and malformed URL on WebDAV~~ Done
- ~~**WebDAV root boundary**: `cd()` and `cd_up()` enforce `initial_path` — prevents navigation above configured WebDAV root~~ Done
- ~~**CloudPanel port handling**: Host saved without port, port in `connectionParams` — clean separation~~ Done
- ~~**DNS hostname extraction**: App.tsx strips path and port for WebDAV-style servers in catch block~~ Done
- ~~**CLI `--json` output**: Global `--json` / `--format json` flag on all 5 CLI commands with structured serializable output~~ Done
- ~~**7 unit tests**: `parse_server_field()` coverage for all server field format variations~~ Done

### Completato in v2.8.0 (Koofr Native API, Production CLI & AeroAgent Server Exec)

- ~~**Koofr native REST API** (20° protocollo): `koofr.rs` — OAuth2 PKCE, EU-based (Slovenia), 10GB free, full StorageProvider trait, trash management (list/restore/empty), 3 Tauri commands~~ Done
- ~~**Production CLI** (`aeroftp-cli`): Separate `[[bin]]` target, 13 commands (connect, ls, get, put, mkdir, rm, mv, cat, find, stat, df, tree, sync), URL-based connections (`ftp://user@host`), `--json` output, indicatif progress bars, 13 unit tests, 5 live protocol tests~~ Done
- ~~**AeroAgent server_exec tools**: `server_list_saved` (safe, lists servers without passwords) + `server_exec` (high danger, 10 operations: ls/cat/get/put/mkdir/rm/mv/stat/find/df). Passwords resolved from vault in Rust — never exposed to AI model. Fuzzy server name matching. Tool count: 45 → 47~~ Done
- ~~**Ed25519 license verification**: `license.rs` — offline-first token verification with `ed25519-dalek`, vault.db persistence, 5 Tauri commands~~ Done
- ~~**License UI (dev-only)**: LicenseTab in Settings, NagBanner, useLicense hook — gated behind `import.meta.env.DEV`~~ Done
- ~~**Supabase Edge Functions**: `verify-purchase` (Google Play + Ed25519 signing) + `activate-device` (multi-device, max 5)~~ Done
- ~~**PostgreSQL schema**: `licenses` + `device_activations` tables, RLS, `enforce_max_devices` trigger~~ Done
- ~~**Human-readable keys**: `AERO-XXXX-XXXX-XXXX-XXXX` via SHA-256+BASE32, consistent Rust/TypeScript~~ Done
- ~~**Grace period hardened**: 30-day window based on stored `last_verified` timestamp, not unsigned payload~~ Done
- ~~**3-auditor security review**: 71 findings total (7 CRITICAL, 13 HIGH) — all resolved. Grade C+/D → B+~~ Done
- ~~**CORS hardened**: Origin whitelist replacing wildcard on both Edge Functions~~ Done
- ~~**TOCTOU fix**: Atomic upsert + PostgreSQL trigger for device activation~~ Done
- ~~**Dead code removed**: ProBadge.tsx, LicenseActivationDialog.tsx deleted~~ Done
- ~~**Dark theme modal alignment**: `dark:bg-gray-800` → `dark:bg-gray-900` consistency across VaultPanel and SettingsPanel~~ Done
- ~~**License + server_exec i18n**: 36 keys translated in all 47 languages~~ Done

### Completato in v2.9.2 (CLI Expansion & Security Hardening)

- ~~**Batch scripting engine (`.aeroftp`)**: 17 commands (SET/ECHO/ON_ERROR/CONNECT/DISCONNECT/GET/PUT/RM/MV/LS/CAT/STAT/FIND/DF/MKDIR/TREE/SYNC), shell-like quoting, single-pass variable expansion (injection-safe), ON_ERROR CONTINUE/FAIL policies, 1MB file limit, max 256 variables~~ Done
- ~~**Glob pattern transfers**: `aeroftp put "*.csv"` via globset crate, remote glob filter for get~~ Done
- ~~**`tree` command**: Unicode box-drawing characters, BFS depth-limited (`-d`), JSON output with recursive structure~~ Done
- ~~**Exit codes**: 9 semantic codes (0=success, 1=connection, 2=not found, 3=permission, 4=transfer, 5=config, 6=auth, 7=not supported, 8=timeout, 99=unknown)~~ Done
- ~~**Path traversal protection**: `validate_relative_path()` in recursive download/sync — rejects `..`, absolute paths, drive letters, UNC paths~~ Done
- ~~**BFS depth/entry caps**: `MAX_SCAN_DEPTH=100`, `MAX_SCAN_ENTRIES=500_000` in get_recursive and find fallback~~ Done
- ~~**NO_COLOR standard**: `use_color()` helper respecting `NO_COLOR` env var + TTY detection, progress bars hidden when no color~~ Done
- ~~**SIGPIPE handling**: `libc::signal(SIGPIPE, SIG_DFL)` for POSIX pipe compliance~~ Done
- ~~**Double Ctrl+C**: First graceful cancel, second force exit with code 130~~ Done
- ~~**JSON errors to stderr**: `print_error` in JSON mode writes to stderr, keeping stdout clean for piping~~ Done
- ~~**Password warning unconditional**: URL-embedded password warning always shown, not gated behind `-v`~~ Done
- ~~**ls/find summary to stderr**: Summary lines use `eprintln!` for pipe safety~~ Done
- ~~**Dead `--retries` flag removed**: Unused global flag removed from CLI struct~~ Done
- ~~**5-auditor security review**: 97 findings (20 HIGH), all critical/high resolved. Clippy clean~~ Done

### Completato in v2.9.2 (CLI Expansion, AI Core Refactor & AeroVault OS Integration)

- ~~**CLI batch scripting engine (`.aeroftp`)**: 17 commands, shell-like quoting, single-pass variable expansion (injection-safe), ON_ERROR CONTINUE/FAIL, 1MB file limit, max 256 variables~~ Done
- ~~**CLI tree command**: Unicode box-drawing, BFS depth-limited (`-d`), JSON output, cycle detection~~ Done
- ~~**CLI glob uploads**: `aeroftp put "*.csv"` via globset crate~~ Done
- ~~**CLI documentation**: `docs/CLI-GUIDE.md` with usage examples, batch scripting reference, CI/CD patterns~~ Done
- ~~**AI Core abstraction layer**: `EventSink`, `CredentialProvider`, `RemoteBackend` traits in `src-tauri/src/ai_core/` — decouples AI streaming from Tauri for CLI agent mode~~ Done
- ~~**AI streaming refactored**: `ai_chat_stream_with_sink()` replaces direct `app.emit()` — all stream functions accept `&dyn EventSink`~~ Done
- ~~**API key sanitization**: `sanitize_error_message()` with 5 LazyLock regex patterns (Anthropic/OpenAI keys, Bearer tokens, x-api-key)~~ Done
- ~~**AeroVault MIME type icon**: Shield+lock icon in 8 PNG sizes (16-512px) + SVG + ICO + ICNS for all platforms~~ Done
- ~~**AeroVault file association**: Cross-platform `.aerovault` double-click open (Linux .deb/Snap, Windows NSIS, macOS)~~ Done
- ~~**AeroVault deep-link handler**: Single-instance argv forwarding + first-launch file open with `canonicalize()` + `symlink_metadata()` validation~~ Done
- ~~**VaultIcon unified**: Shield+lock design matching OS MIME icon in frontend (modal, context menus, icon themes)~~ Done
- ~~**Snap Store compliance**: Description rewrite removing flagged keywords to pass `metadata-snap-v2_snap_metadata_redflag`~~ Done
- ~~**45-finding CLI security audit**: Path traversal, ANSI sanitization, BFS caps, OOM protection, NO_COLOR, stderr separation~~ Done
- ~~**aerovault crate v0.3.2**: `MIME_TYPE` + `ICON_SVG` constants, published to crates.io, updated in AeroFTP~~ Done

### Completato in v2.9.4 (Server Health Check, Sync Fix, mtime & AeroVault Pro)

- ~~**Server Health Check**: Real-time network diagnostics (DNS, TCP, TLS, HTTP probes) with latency measurements, health scoring (0-100), SVG radial gauge, latency bars, Canvas 2D area chart~~ Done
- ~~**Server context menu**: Right-click on saved server cards — Connect, Edit, Duplicate, Health Check, Delete via `useContextMenu` hook~~ Done
- ~~**Batch health check**: Parallel diagnostics across all saved servers with healthy/degraded/unreachable summary~~ Done
- ~~**Cloud API host mapping**: Health checks probe actual API endpoints for all 20 protocols~~ Done
- ~~**Download mtime preservation**: `filetime` crate sets local file mtime to remote server's original timestamp after every download — fixes sync/overwrite-if-newer producing incorrect results~~ Done
- ~~**AeroSync Pull preset**: Remote → Local mirror with orphan deletion and size verification~~ Done
- ~~**AeroSync Remote Backup preset**: Remote → Local with checksum verify, no deletes~~ Done
- ~~**AeroVault Pro modular architecture**: VaultPanel refactored from 1117-line monolith into 5 components (VaultHome, VaultCreate, VaultOpen, VaultBrowse, useVaultState)~~ Done
- ~~**Recent Vaults**: SQLite WAL-backed vault history with last-opened tracking, security badges, one-click reopen~~ Done
- ~~**Folder encryption**: Encrypt directories as AeroVault containers with recursive `walkdir` scan, progress events, folder preview~~ Done
- ~~**AeroVault icon consistency**: Titlebar and modal use outline-only shield+lock icon, removed redundant Lock badge~~ Done
- ~~**Provider Integration Guide**: Comprehensive developer reference for adding new storage protocols~~ Done
- ~~**i18n vault keys cleanup**: 6 missing + 5 stale + 4 extra keys fixed across 46 locales~~ Done
- ~~**Snap Store auto-publish disabled**: Paused pending manual review — `.snap` still in GitHub Releases~~ Done

### Monetization Model

- **Desktop**: 100% free and open-source. No Pro tier, no license gating, no nag banners, no telemetry
- **Mobile**: Paid app on Google Play Store (€3.49 one-time, no ads, no subscriptions, no IAP)
- **Old model abandoned**: Ed25519/Supabase license system removed — over-engineered, 71 audit findings, unnecessary friction
- **Details**: See `aeroftp-mobile-app/.docs/LICENSE-SYSTEM-SPEC.md` and `aeroftp-mobile-app/.docs/ANDROID-LAUNCH-PLAN.md`

### Completato in v2.9.8 (OpenDrive, Yandex Trash & Windows Credential Fix)

- ~~**OpenDrive provider** (22° protocollo): Native REST API, session auth, 5 GB free, trash management, MD5 checksums, zlib compression, expiring share links. `opendrive.rs` ~1562 righe. OpenDriveTrashManager.tsx, 4 Tauri trash commands. Integrated in cloud provider factory, CLI, AeroAgent~~ Done
- ~~**Yandex Disk Trash Manager**: Full trash lifecycle — list, restore, permanent delete, empty trash. `YandexTrashManager.tsx` component, 4 Tauri commands, context menu entry~~ Done
- ~~**Yandex Disk + Zoho WorkDrive OAuth in Settings**: Client ID/Secret fields added to Settings > Cloud Providers tab~~ Done
- ~~**Windows credential persistence fix** (critical): `secureStoreAndClean` now keeps localStorage as write-through backup, preventing permanent server profile loss on Windows~~ Done
- ~~**Import/export credential error handling**: Replaced silent `let _ =` with proper error logging and vault-not-ready detection~~ Done
- ~~**OpenDrive i18n**: 9 keys translated in all 47 languages~~ Done
- ~~**Protocol count 21→22**: Updated across all docs, README, snapcraft, AUR, metainfo, AI prompts~~ Done

### Completato in v3.0.5 (SFTP Upload Fix, Atomic Downloads & GitHub PEM Vault)

- ~~**SFTP upload 0-byte fix** (critical): Replaced russh-sftp AsyncWrite upload with ssh2/libssh2 SCP backend. Root cause: russh 0.57 write buffering race condition producing empty files on embedded SFTP servers (WD MyCloud NAS). Downloads, listing, navigation unchanged (russh)~~ Done
- ~~**Atomic download writes**: All 22 providers now write to `.aerotmp` temp file, atomically renamed on completion. Prevents 0-byte files on interrupted downloads~~ Done
- ~~**GitHub PEM vault storage**: `.pem` keys encrypted (AES-256-GCM) in vault on import. Reconnection from vault — original file deletable~~ Done
- ~~**GitHub token expiry badges**: Dynamic green/amber status for valid/expiring/expired tokens with auto-refresh~~ Done
- ~~**GitHub PEM error messages**: Specific messages for missing, empty, invalid PEM files~~ Done
- ~~**0 B file alert badge**: Warning triangle indicator on files with 0 bytes in both list and grid views (local + remote)~~ Done
- ~~**ssh2 dependency**: `ssh2 0.9.5` (vendored OpenSSL) for SFTP upload backend~~ Done
- ~~**7 i18n keys**: GitHub PEM vault + token + 0B warning in 47 languages~~ Done

### Prossimi task (v3.1.x)

- **GitHub pre-push local commits**: When uploading via AeroFTP Contents API, local unpushed commits are bypassed — the API creates commits on remote HEAD. Add a pre-check that detects unpushed local commits and runs `git push` before creating new commits via API. Critical for release workflow: version bump commits must be on remote BEFORE README/CHANGELOG push and tag creation. Without this, CI builds from tags that miss the code changes
- **GitHub batch commit for multi-file upload**: Current upload creates 1 commit per file. Wire multi-file upload to existing `github_batch_commit` backend so uploading N files produces a single commit. Critical for clean commit history when pushing README+CHANGELOG together from AeroFTP
- **Filen Encrypted Notes: exit Beta**: Fix `participants/add` contactUUID format (current workaround: best-effort). Investigate Filen free plan 10-note limit ghost notes cleanup. Add rich text editor for "rich" note type
- **CSP Phase 2 tightening**: Replace wildcard `*` sources with specific origins, directive-by-directive compatibility matrix
- **Biometric Unlock**: macOS Touch ID, Windows Hello for vault
- **Provider-optimized transfers**: Protocol-specific chunking, parallel streams

### Roadmap futura

Dettagli completi in `docs/dev/ROADMAP.md`:

- **v2.9.x**: CSP tightening + Biometric unlock + Provider-optimized transfers

#### New Provider Roadmap (analisi 15 marzo 2026)

**Tier 1 — Quick wins (prossime versioni)**:
- **Blomp** (40 GB free): OpenStack Swift / S3-compatible backend. Potenzialmente zero Rust nuovo — S3 preset come Yandex Object Storage. Effort: ~2h
- ~~**OpenDrive** (5 GB free): REST API documentata, rclone Tier 1 support, username/password auth, MD5 checksums. Effort: ~500 righe Rust~~ Done (v2.9.8)
- **MediaFire** (10 GB free): REST API pubblica su mediafire.com/developers/, OAuth2 + API key. File hosting pubblico, non private cloud. Effort: ~700 righe Rust

**Tier 2 — Sfide interessanti**:
- **Proton Drive** (5 GB free, E2E): Nessuna API ufficiale ma clients open source (Go). Richiede SRP auth + PGP E2E encryption per ogni operazione. Ref: `henrybear327/Proton-API-Bridge` (archiviato feb 2026). Crates: `srp` + `sequoia-pgp`. Allineato con identità encryption-first di AeroFTP. Effort: 200-300h — candidato v3.0
- **Degoo** (20 GB free): GraphQL reverse-engineered, GCS upload. API instabile, ToS risk. Effort: 60-80h — sconsigliato
- **rsync protocol** (nice-to-have, big effort): Native rsync wire protocol for byte-level delta sync. Not prioritized

**Scartati** (analisi completa):
- IDrive consumer: API 2012 obsoleta, password plaintext, rclone l'ha abbandonato. Solo IDrive e2 (S3) è valido — già supportato
- Icedrive: WebDAV solo a pagamento, nessuna API pubblica
- Sync.com: Impossibile — E2E senza API pubblica
- NordLocker: Impossibile — zero documentazione, protocollo chiuso
- Tresorit: Solo enterprise a pagamento, 0 GB free
- Felicloud: Troppo nuovo, nessuna info API

---

*Last updated: 15 March 2026*
