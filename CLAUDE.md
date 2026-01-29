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
🚀 Added new feature          # No emojis
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

## Stato Progetto (v1.3.4-dev)

### Versione corrente: v1.3.3 (rilasciata) → v1.3.4 (in sviluppo)

### Sicurezza (0 vulnerabilita aperte)
- CVE-2025-54804: **Risolta** - russh aggiornato da v0.48 a v0.54.5, rimossa dipendenza russh-keys
- SFTP Host Key Verification: TOFU con modulo built-in russh (`known_hosts`)
- OAuth2: Porta ephemeral (OS-assigned, porta 0)
- FTP: Warning visivo rosso "Insecure" con banner
- Credenziali: OS Keyring (primario) + AES-256-GCM vault con Argon2id (fallback)
- Memoria: zeroize/secrecy per tutte le password e chiavi SSH

### Stack tecnologico
- **Backend**: Rust (Tauri 2) con russh 0.54, suppaftp 8, reqwest 0.12, zip 7
- **Frontend**: React 18 + TypeScript + Tailwind CSS
- **Protocolli**: FTP, FTPS, SFTP, WebDAV, S3, Google Drive, Dropbox, OneDrive, MEGA.nz
- **Archivi**: ZIP, 7z (AES-256), TAR, GZ, XZ, BZ2
- **i18n**: 51 lingue al 100%
- **CI/CD**: GitHub Actions → GitHub Releases + Snap Store

### Dipendenze critiche
| Crate | Versione | Note |
|-------|----------|------|
| russh | 0.54 | SSH/SFTP (CVE risolta) |
| russh-sftp | 2.1 | Operazioni SFTP |
| keyring | 3 (linux-native) | OS Keyring |
| argon2 | 0.5 | KDF per vault |
| aes-gcm | 0.10 | Cifratura vault |
| sevenz-rust | 0.6 | 7z con AES-256 |

### Dependency Upgrade Roadmap

#### Completati in v1.3.4
| Crate | Da | A | Note |
|-------|-----|-----|------|
| secrecy | 0.8 | 0.10 | SecretString API, Rust Edition 2021 |
| bzip2 | 0.4 | 0.6 | Backend pure-Rust (libbz2-rs-sys) |
| thiserror | 1 | 2 | Supporto no-std, Edition 2021 |
| suppaftp | 7 | 8 | Solo rustls breaking, tokio API invariata |
| zip | 2 | 7 | ZIP encryption support, ZIP64 fixes |

#### Pendenti
| Crate | Attuale | Target | Priorita | Note |
|-------|---------|--------|----------|------|
| russh | 0.54 | 0.57 | v1.4.0 | Nuovi cipher, future-compat fix |
| reqwest | 0.12 | 0.13 | v1.4.0 | HTTP/3, performance |
| quick-xml | 0.31 | 0.39 | v1.4.0 | WebDAV parsing migliorato |
| oauth2 | 4 | 5 | v1.5.0 | Nuova API, PKCE nativo |

### Prossime versioni pianificate
- **v1.4.0**: Archive encryption (ZIP AES-256 + 7z AES-256 write), RAR extraction, bandwidth throttling, dep upgrades (russh, reqwest, quick-xml)
- **v1.5.0**: AeroVault (location crittografata), CLI/Scripting, Azure Blob, dep upgrade oauth2
- **v1.7.0**: Cryptomator import/export

---

*Last updated: January 2026*
