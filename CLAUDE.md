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

## Versione corrente: v1.5.3

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
| oauth2 | 4.4 | OAuth2 PKCE (upgrade a v5 in v1.6.0) |

### Completato in v1.5.2

- ~~Fix SEC-001: zeroize ZIP password con secrecy crate~~ Done
- ~~Fix SEC-004: wrap OAuth tokens in SecretString~~ Done
- ~~Multi-protocol sync (provider_compare_directories)~~ Done
- ~~Codebase audit: rimossi 7 crate, 3 componenti orfani, duplicati crypto~~ Done
- ~~Fix credential loading al primo avvio (keyring probe fallback)~~ Done

### Prossimi task (v1.5.3+)

- Test Filen e Box con account reali, promuovere a stable
- Cross-panel drag & drop (local↔remote upload/download)
- Consolidare duplicati `formatBytes`, `getMimeType`, `UpdateInfo` (vedi audit report)
- Gating console.log dietro debug mode (76 statement in 13 file)

### Roadmap futura

Dettagli completi in `docs/dev/roadmap/ROADMAP.md`:
- **v1.6.0**: AeroAgent Pro (remote tools, streaming, native function calling) + CLI Foundation + oauth2 v5
- **v1.7.0**: Terminal Pro (themes, SSH remote, Windows PTY) + AI Intelligence (vision, multi-step)
- **v1.8.0**: Cryptomator + AeroVault + RAG + Plugin system

---

*Last updated: February 2026*
