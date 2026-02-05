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
- `COMPETITOR-ANALYSIS.md` - Market comparison
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

## Versione corrente: v1.8.7

### Stack tecnologico
- **Backend**: Rust (Tauri 2) con russh 0.57, suppaftp 8, reqwest 0.13, quick-xml 0.39, zip 7
- **Frontend**: React 18 + TypeScript + Tailwind CSS
- **Protocolli**: FTP, FTPS, SFTP, WebDAV, S3, Google Drive, Dropbox, OneDrive, MEGA, Box, pCloud, Azure Blob, Filen (13 totali)
- **Archivi**: ZIP (AES-256), 7z (AES-256), TAR, GZ, XZ, BZ2, RAR (extract)
- **i18n**: 51 lingue al 100%
- **CI/CD**: GitHub Actions → GitHub Releases + Snap Store

### Dipendenze critiche
| Crate | Versione | Note |
|-------|----------|------|
| russh | 0.57 | SSH/SFTP |
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

### Prossimi task (v1.9.0)

- Consolidare duplicati `formatBytes`, `getMimeType`, `UpdateInfo` (vedi audit report)
- Gating console.log dietro debug mode (76 statement in 13 file)
- CLI Foundation (`aeroftp connect/ls/get/put/sync`)
- Vision/multimodal per GPT-4o, Gemini, Claude
- Multi-step autonomous tool calls

### Roadmap futura

Dettagli completi in `docs/dev/ROADMAP.md`:
- **v1.9.0**: AeroAgent Intelligence (vision, multi-step) + CLI Foundation
- **v2.0.0**: Master Password + Unified Keystore + Settings Consolidation
- **v2.1.0**: Remote vault open/save, Cryptomator vault creation, provider feature gaps

---

*Last updated: February 2026*
