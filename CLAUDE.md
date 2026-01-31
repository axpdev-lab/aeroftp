# AeroFTP Development Guidelines

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
- `COMPETITOR-ANALYSIS.md` - Market comparison
- `PROTOCOL-FEATURES.md` - Feature matrix
- `TRANSLATIONS.md` - i18n guide

### Internal (docs/dev/) - Gitignored
Development-only files:
- TODO files, roadmaps, agent instructions
- Not pushed to GitHub

---

## Release Process

### Steps
1. Update version in: `package.json`, `tauri.conf.json`, `Cargo.toml`, `snapcraft.yaml`
2. **Update `CHANGELOG.md`** (critical - this becomes the GitHub Release body):
   - Add a new `## [X.Y.Z] - YYYY-MM-DD` section at the top
   - Write a short subtitle summarizing the release theme (e.g. `### Secure Credential Storage`)
   - Optionally add a 1-2 sentence description paragraph
   - Group changes under `#### Added`, `#### Fixed`, `#### Changed`, `#### Removed` as needed
   - Each entry should be a concise, user-facing description with **bold lead** and explanation
   - This text is extracted automatically by CI and published as the GitHub Release notes
3. **Sync i18n translations**: Run `npm run i18n:sync` to propagate new keys to all 51 languages, then translate Italian (`it.json`) manually. Other languages get `[NEEDS TRANSLATION]` placeholders.
4. **Validate i18n**: Run `npm run i18n:validate` to ensure no missing keys
5. Commit: `chore(release): vX.Y.Z Short Release Title`
6. Tag: `git tag -a vX.Y.Z -m "Release vX.Y.Z - Short Release Title"`
7. Push: `git push origin main --tags`
8. GitHub Actions builds, extracts CHANGELOG section, and publishes the release automatically

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
- All 51 languages must stay at 100%
- Run `npm run i18n:validate` before commits
- Technical terms (FTP, SFTP, OAuth) are not translated

---

## Stato Progetto (v1.5.0)

### Versione corrente: v1.5.0 (in testing)

### Sicurezza (0 vulnerabilita aperte)
- CVE-2025-54804: **Risolta** - russh aggiornato a v0.57
- SFTP Host Key Verification: TOFU con modulo built-in russh (`known_hosts`)
- OAuth2: Porta ephemeral (OS-assigned, porta 0) — 5 provider (Google, Dropbox, OneDrive, Box, pCloud)
- FTP: Default TLS opportunistico (explicit_if_available), warning amber solo se utente sceglie plain
- FTPS: Explicit TLS, Implicit TLS, Explicit-if-available, verifica certificato
- Credenziali: OS Keyring (primario) + AES-256-GCM vault con Argon2id (fallback)
- AI API keys: OS Keyring (migrato da localStorage)
- Filen: Zero-knowledge E2E con PBKDF2 + AES-256-GCM
- Memoria: zeroize/secrecy per tutte le password e chiavi SSH

### Stack tecnologico
- **Backend**: Rust (Tauri 2) con russh 0.57, suppaftp 8, reqwest 0.13, quick-xml 0.39, zip 7
- **Frontend**: React 18 + TypeScript + Tailwind CSS
- **Protocolli**: FTP, FTPS, SFTP, WebDAV, S3, Google Drive, Dropbox, OneDrive, MEGA.nz, **Box**, **pCloud**, **Azure Blob**, **Filen** (13 totali)
- **Archivi**: ZIP (AES-256), 7z (AES-256), TAR, GZ, XZ, BZ2, RAR (extract)
- **i18n**: 51 lingue al 100%
- **CI/CD**: GitHub Actions → GitHub Releases + Snap Store

### Dipendenze critiche
| Crate | Versione | Note |
|-------|----------|------|
| russh | 0.57 | SSH/SFTP |
| russh-sftp | 2.1 | Operazioni SFTP |
| suppaftp | 8 | FTP/FTPS con TLS (native-tls), MLSD/MLST/FEAT |
| reqwest | 0.13 | HTTP client (json, stream, multipart) |
| quick-xml | 0.39 | WebDAV/Azure XML parsing |
| keyring | 3 (linux-native) | OS Keyring |
| argon2 | 0.5 | KDF per vault |
| aes-gcm | 0.10 | Cifratura vault + Filen E2E |
| sevenz-rust | 0.6 | 7z con AES-256 |
| zip | 7.2 | ZIP con AES-256 |
| unrar | 0.5 | RAR extraction |
| oauth2 | 4.4 | OAuth2 PKCE (upgrade a v5 planned) |
| pbkdf2 | 0.12 | Filen key derivation |
| uuid | 1 | Filen upload UUIDs |
| mime_guess | 2 | Upload content types |

### Completato in v1.5.0
- **4 nuovi provider nativi**: Box (OAuth2), pCloud (OAuth2, US/EU), Azure Blob (HMAC/SAS), Filen (E2E AES-256-GCM)
- **FTP security defaults**: TLS opportunistico come default, badge amber "TLS", warning solo su plain FTP esplicito
- **S3/WebDAV promossi**: Badge "Beta" → "Secure" (stabili e testati)
- **Provider logos**: SVG ufficiali per Box, pCloud, Azure, Filen
- **OAuth2 esteso**: Box e pCloud aggiunti a OAuth2Manager con PKCE
- **Frontend integration**: ProtocolSelector, SessionTabs, ProviderLogos per tutti i 4 nuovi provider
- **Azure fields**: Input container name nel form connessione
- **pCloud region**: Radio button US/EU nel form connessione
- AI API keys migrate a OS Keyring, password dialog ZIP/7z, ErrorBoundary, dead code cleanup

### Completato in v1.4.0
- FTPS: Explicit TLS (AUTH TLS), Implicit TLS, Explicit-if-available, cert verification
- FTP: MLSD/MLST (RFC 3659), resume transfers (REST/APPE)
- Cross-provider: remote search, storage quota, file versions, thumbnails, share permissions, WebDAV locking
- Archive encryption: ZIP AES-256 (read+write), 7z AES-256 (read+write), RAR extraction
- S3 multipart upload (>5MB), OneDrive resumable upload
- Dependency upgrades: russh 0.57, reqwest 0.13, quick-xml 0.39, zip 7.2, suppaftp 8, secrecy 0.10, thiserror 2, bzip2 0.6

### Dependency Upgrade Pendenti
| Crate | Attuale | Target | Priorita | Note |
|-------|---------|--------|----------|------|
| oauth2 | 4.4 | 5 | v1.6.0 | Nuova API, PKCE nativo |

### Roadmap

#### v1.5.1 — Bugfix + Testing nuovi provider

- Test completi Box, pCloud, Azure, Filen con account reali
- Fix bug emersi dai test
- i18n: chiavi descrizione per i 4 nuovi provider (51 lingue)
- Promuovere provider da Beta a stabile dopo test

#### v1.6.0 — AeroAgent Pro + CLI Foundation

**AeroAgent — Supporto tutti i provider e azioni avanzate:**
Le 14 tool attuali sono base (file ops locali). Obiettivo: dare ad AeroAgent azioni su tutti i 13 provider.

- Nuove tool per provider remoti:
  - `remote_list`, `remote_read`, `remote_upload`, `remote_download` (su connessione attiva)
  - `remote_search` (usa search cross-provider esistente)
  - `remote_delete`, `remote_rename`, `remote_mkdir` (con conferma danger:high)
  - `remote_info` (quota, versioni, proprietà file)
- Tool avanzate:
  - `transfer` (copia tra locale e remoto con progress)
  - `sync_preview` (anteprima diff AeroCloud prima di sincronizzare)
  - `archive_create`, `archive_extract` (con supporto password AES-256)
  - `connection_manage` (connetti/disconnetti/switch provider)
- Context awareness: AeroAgent riceve automaticamente provider attivo, path corrente, file selezionati
- Persistenza chat history (`~/.config/aeroftp/ai_history.json`)
- Cost tracking con conteggio token per provider
- i18n completo (da 3 chiavi a copertura totale 51 lingue)

**CLI/Scripting — Foundation:**
- Binary CLI: `aeroftp connect`, `aeroftp ls`, `aeroftp get`, `aeroftp put`, `aeroftp sync`
- Supporto tutti i 13 provider
- Output JSON per automazione (`--json` flag)
- Script files `.aeroftp` con sequenze di comandi
- Batch mode non-interattivo
- Exit codes per CI/CD

**Altre feature:**
- AeroVault (location virtuale crittografata)
- oauth2 crate upgrade a v5
- S3/WebDAV Presets addizionali (Wasabi, Oracle Cloud, IDrive e2, DigitalOcean Spaces)

**Auto-Updater fix:**
- Rimuovere o usare `tauri-plugin-updater` (attualmente dead dependency)
- Check periodico in background (ogni 24h)

#### v1.7.0 — AeroAgent Intelligence + Terminal Pro

**AeroAgent — Intelligence:**
- Streaming responses (SSE/chunked in ai.rs + rendering incrementale frontend)
- Native function calling (OpenAI tools, Anthropic tool_use, Google function_calling) al posto di regex
- Vision/multimodal: analisi immagini e screenshot per provider compatibili
- Multi-step autonomo: catena di tool calls con supervisione utente
- Export conversazioni (markdown/JSON)
- System prompt completamente personalizzabile
- Ollama model auto-detection (`GET /api/tags`)
- Sliding window per gestione context lungo

**Terminal Pro:**
- Selezione tema (Tokyo Night, Dracula, Solarized, Monokai, light themes)
- Font size configurabile (zoom Ctrl+/-)
- Persistenza sessione/scrollback tra restart
- SSH remote shell (connessione diretta a server SFTP attivo)
- Windows PTY support (`conpty`)
- Terminal tabs multipli

**CLI/Scripting — Advanced:**
- Scriptable transfers con glob patterns
- Cron-like scheduling
- Watch mode (monitor directory changes)

#### v1.8.0 — Cryptomator + Automazione

- Cryptomator Import/Export
- AeroAgent workflow autonomi (task multi-step con approvazione batch)
- CLI plugin system per tool custom
- RAG integration (indicizzazione file locali/remoti per context AeroAgent)

### Cloud Provider Expansion — Stato

| Provider | Free Tier | Protocollo | Auth | Stato | Note |
|----------|-----------|-----------|------|-------|------|
| **Cloudflare R2** | 10 GB | S3-compatible | Access Key | **Preset attivo** | Zero egress |
| **Backblaze B2** | 10 GB | S3-compatible | Access Key | **Preset attivo** | |
| **Storj** | 25 GB | S3-compatible | Access Key | **Preset attivo** | Decentralizzato |
| **Koofr** | 10 GB | WebDAV | Password | **Preset attivo** | EU-based |
| **Box** | 10 GB | REST API | OAuth2 PKCE | **v1.5.0 Beta** | Enterprise, API eccellente |
| **pCloud** | 10 GB* | REST API | OAuth2 | **v1.5.0 Beta** | US/EU regions |
| **Azure Blob** | Pay-as-you-go | REST API | HMAC / SAS | **v1.5.0 Beta** | Enterprise storage |
| **Filen** | 10 GB | REST API | Password + E2E | **v1.5.0 Beta** | Zero-knowledge AES-256-GCM |
| ~~Icedrive~~ | 10 GB | WebDAV | — | Scartato | WebDAV solo piani a pagamento |
| ~~TeraBox~~ | 1 TB | Nessuna API | — | Scartato | No API ufficiale, rischio ban |

**Conteggio provider per versione:**
- v1.4.0: 9 provider nativi
- **v1.5.0: 13 provider nativi** (+Box, +pCloud, +Azure Blob, +Filen) + preset S3/WebDAV
- v1.6.0: 13 provider + AeroAgent remoto + CLI
- v1.7.0: 13 provider + Terminal Pro + AI Intelligence

### Frontend Architecture (post v1.4.1 cleanup)

- **App.tsx**: 4,137 righe (da 4,484 pre-cleanup, -347)
- **Hook attivi**: 14 files in src/hooks/ (da 23, rimossi 10 dead code, aggiunti 2 nuovi)
  - Usati da App.tsx (11): useSettings, useAutoUpdate, usePreview, useOverwriteCheck, useDragAndDrop, useTheme, useActivityLog, useHumanizedLog, useKeyboardShortcuts, useTransferEvents, useCloudSync
  - Usati da componenti (2): useOAuth2 (OAuthConnect), useTraySync (CloudPanel)
  - Utility (1): useAnalytics
- **Componenti**: 52 files in src/components/ + ErrorBoundary
- **ErrorBoundary**: attivo in main.tsx, wrap di tutta l'app
- **Provider utilities**: types.ts esporta isOAuthProvider, isNonFtpProvider, isFtpProtocol, supportsStorageQuota, supportsNativeShareLink
- **AI API keys**: OS Keyring (migrazione automatica da localStorage)

**Blocchi rimanenti in App.tsx (non estraibili senza over-engineering):**
- Context menus (~450 righe) — 25+ dipendenze su state App.tsx
- Connection manager (~560 righe) — 30+ dipendenze, 3 reconnect paths
- handleCloudTabClick (~190 righe) — dipende da session/connection state

### Feature Audit — Stato attuale (v1.4.0)

Audit completi in `docs/dev/`:
- `AEROAGENT-AUDIT.md` — 7 provider, 14 tool, 4 componenti frontend, gap critici
- `TERMINAL-UPDATER-AUDIT.md` — Terminal xterm.js + PTY, Auto-Updater GitHub API
- `Agents_reviews/AUDIT-FINALE-App.tsx-Claude-Opus-4.5.md` — Audit certificato App.tsx con valutazione multi-agent

**AeroAgent:**
- Backend: `ai.rs` (413 righe), 7 provider AI (Google, OpenAI, Anthropic, xAI, OpenRouter, Ollama, Custom)
- Frontend: AIChat.tsx (786), AISettingsPanel.tsx (915), ToolApproval.tsx (143), AIIcons.tsx (72)
- 14 tool base (solo file ops locali), 3 livelli pericolo (safe/medium/high)
- 6 task type per auto-routing
- Comandi eseguiti con successo — sistema funzionante, pronto per espansione

**Terminale:**
- xterm.js v5.3.0 + portable-pty v0.8
- Shell locale (bash/$SHELL), Tokyo Night theme, resize, web links
- Solo Unix, no SSH remoto, no selezione tema

**Auto-Updater:**
- Custom GitHub API check (bypassa tauri-plugin-updater)
- Detect formato: deb/rpm/appimage/snap/exe/msi/dmg
- Toast + status bar badge + notifica OS
- No download automatico, solo link

**Gap critici cross-feature:**
| Gap | Feature | Severita | Stato |
|-----|---------|----------|-------|
| ~~API keys in localStorage~~ | AeroAgent | ~~Alta~~ | **Risolto v1.4.1** |
| Solo 14 tool locali, no provider remoti | AeroAgent | Alta | v1.6.0 |
| No streaming AI | AeroAgent | Alta | v1.7.0 |
| Tool calls via regex (fragile) | AeroAgent | Media | v1.7.0 |
| No Windows PTY | Terminal | Alta | v1.7.0 |
| tauri-plugin-updater dead dep | Updater | Media | v1.6.0 |
| No auto download/install | Updater | Media | v1.6.0 |
| CLI non esiste | CLI | N/A | v1.6.0 (planned) |
| Box/pCloud/Azure/Filen in Beta | Provider | Media | v1.5.1 (test) |

---

*Last updated: January 2026*
