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
2. Update `CHANGELOG.md`
3. Commit: `chore(release): bump version to vX.Y.Z`
4. Tag: `git tag -a vX.Y.Z -m "Release vX.Y.Z - Description"`
5. Push: `git push origin main --tags`
6. GitHub Actions builds and publishes automatically

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

*Last updated: January 2026*
