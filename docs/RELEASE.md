# AeroFTP Release Process

## Quick Reference

```bash
# After updating ALL version files and CHANGELOG.md
git add -A
git commit -m "chore(release): vX.Y.Z Description"
git tag -a vX.Y.Z -m "Release vX.Y.Z - Description"
git push origin main --tags
```

That's it! GitHub Actions handles everything else automatically.

---

## Pre-Release Checklist (12 files)

### Version Files (6 files - MUST be identical)

| # | File | Field | Notes |
|---|------|-------|-------|
| 1 | `package.json` | `"version": "X.Y.Z"` | Line ~4 |
| 2 | `src-tauri/tauri.conf.json` | `"version": "X.Y.Z"` | Line ~4 |
| 3 | `src-tauri/Cargo.toml` | `version = "X.Y.Z"` | Line ~3 |
| 4 | `src-tauri/Cargo.lock` | `name = "aeroftp"` package `version` | Re-locks on `cargo build`; commit it |
| 5 | `snap/snapcraft.yaml` | `version: 'X.Y.Z'` | Path is `snap/`, NOT root |
| 6 | `public/splash.html` | Hardcoded in `.version` div | Tauri IPC not available in splash |

> **AUR is maintained in a separate repository**, not the in-repo `aur/` folder (that copy is stale and no longer the source of truth). The AUR `PKGBUILD` / `.SRCINFO` are bumped and pushed from that separate checkout after CI publishes the `.deb` (see Post-CI: AUR Update). Do not treat the in-repo `aur/` files as a release gate.

### Metadata & Release Notes (2 files)

| # | File | What to update |
|---|------|----------------|
| 8 | `com.aeroftp.AeroFTP.metainfo.xml` | Add new `<release version="X.Y.Z" date="YYYY-MM-DD">` with `<description>` (Ubuntu Store / GNOME Software) |
| 9 | `CHANGELOG.md` | New `## [X.Y.Z] - YYYY-MM-DD` section at top (CI extracts this for GitHub Release body) |

### Documentation (2 files)

| # | File | What to update |
|---|------|----------------|
| 10 | `SECURITY.md` | Footer `*AeroFTP vX.Y.Z - DD Month YYYY*`, plus the Supported Versions table when a new minor line opens |
| 11 | `AGENTS.md` | Footer stamp `*AeroFTP CLI vX.Y.Z - ...*` when the CLI surface changed |

Also sweep `docs/PROTOCOL-FEATURES.md` and the relevant feature guide (`docs/CLI-GUIDE.md`, `docs/PROVIDERS.md`, etc.) when the release adds or changes user-visible behavior, and confirm `CHANGELOG.md` and `com.aeroftp.AeroFTP.metainfo.xml` describe the same set of changes.

### Pre-push Validation

```bash
# i18n
npm run i18n:sync        # Propagate new keys to all 47 languages
npm run i18n:validate    # Verify 100% coverage

# Rust (MUST pass - CI runs this exact command)
cd src-tauri && cargo clippy --all-targets -- -D warnings

# Frontend
npm run build
```

### Commit, Tag & Push

```bash
git add -A
git commit -m "chore(release): vX.Y.Z Short Release Title"
git tag -a vX.Y.Z -m "Release vX.Y.Z - Short Release Title"
git push origin main --tags
```

---

## Post-CI: AUR Update

The AUR package is `aeroftp-bin`, maintained in its own checkout (`ssh://aur@aur.archlinux.org/aeroftp-bin.git`), separate from this repo. The package installs the `.deb` payload (not the AppImage, which hit `EGL_BAD_PARAMETER` on some GPU drivers). After GitHub Actions publishes the release artifacts:

### 1. Compute SHA-256 for ALL 3 sources

```bash
# Download the .deb (the AUR PKGBUILD source)
curl -L -o /tmp/AeroFTP.deb \
  "https://github.com/axpdev-lab/aeroftp/releases/download/vX.Y.Z/AeroFTP_X.Y.Z_amd64.deb"

# Download icon
curl -L -o /tmp/aeroftp-icon.png \
  "https://raw.githubusercontent.com/axpdev-lab/aeroftp/main/src-tauri/icons/128x128.png"

# Compute hashes (deb, then the local aeroftp.desktop in the AUR checkout, then the icon)
sha256sum /tmp/AeroFTP.deb
sha256sum aeroftp.desktop
sha256sum /tmp/aeroftp-icon.png
```

### 2. Update PKGBUILD and .SRCINFO (in the AUR checkout)

- Bump `pkgver=X.Y.Z` and reset `pkgrel=1`
- Replace all 3 `sha256sums` (deb, .desktop, .png) - never leave `SKIP`
- Update the source URLs in both `PKGBUILD` and `.SRCINFO` (3 occurrences in `.SRCINFO`)
- Regenerate `.SRCINFO` if `makepkg` is available: `makepkg --printsrcinfo > .SRCINFO`

### 3. Push to AUR

```bash
cd /path/to/aeroftp-bin   # the separate AUR checkout
git add PKGBUILD .SRCINFO aeroftp.desktop
git commit -m "Update to X.Y.Z"
git push
```

---

## Automated CI/CD Pipeline

### Trigger
The pipeline runs automatically when a tag matching `v*` is pushed.

### Build Matrix

| Platform | Runner | Artifacts |
|----------|--------|-----------|
| Linux | `ubuntu-22.04` | `.deb`, `.rpm`, `.AppImage`, `.snap` |
| Windows | `windows-latest` | `.msi`, `.exe` (NSIS) |
| macOS | `macos-latest` | `.dmg` |

### Distribution

| Destination | Artifacts | Automation |
|-------------|-----------|------------|
| GitHub Releases | All platforms | Automatic via `softprops/action-gh-release` |
| Snap Store | `.snap` (stable channel) | Automatic via `snapcraft upload` |
| AUR | `.deb` (in `aeroftp-bin`, separate repo) | Manual post-CI (see above) |

---

## Snap Store Integration

### How It Works
1. GitHub Actions builds snap using `snapcore/action-build@v1`
2. Uploads to Snap Store with `snapcraft upload --release=stable`
3. Users with AeroFTP installed via snap get auto-updates

### Required Secret
The workflow requires `SNAPCRAFT_STORE_CREDENTIALS` in GitHub repository secrets.

**To generate credentials:**
```bash
snapcraft login
snapcraft export-login --snaps=aeroftp --acls=package_upload credentials.txt
cat credentials.txt | base64 -w 0
```

Add the base64 output as `SNAPCRAFT_STORE_CREDENTIALS` secret in:
GitHub Repo > Settings > Secrets and variables > Actions > New repository secret

### Manual Upload (Fallback)
```bash
snapcraft login
snapcraft upload aeroftp_X.Y.Z_amd64.snap --release=stable
```

---

## Monitoring Releases

### Check Workflow Status
```bash
gh run list --limit 5
gh run watch <run-id>
gh run view <run-id> --log
```

### Verify Snap Store
```bash
snap info aeroftp
snapcraft status aeroftp
```

### Verify GitHub Release
```bash
gh release view vX.Y.Z
```

### Verify AUR
```bash
yay -Si aeroftp-bin   # Check version on AUR
```

---

## Troubleshooting

### Snap Upload Fails
1. Check if `SNAPCRAFT_STORE_CREDENTIALS` secret exists
2. Verify credentials haven't expired (re-export if needed)
3. Check Snap Store review queue for manual review requirements

### Build Fails
1. Check workflow logs: `gh run view <run-id> --log`
2. Common issues: missing apt dependencies, Rust compilation errors, TypeScript type errors

### Release Not Appearing
1. Wait for all 3 platform builds to complete
2. Check if tag was pushed: `git tag -l | grep vX.Y.Z`
3. Verify workflow ran: `gh run list`

### Past Mistakes
- **v2.1.0**: `snap/snapcraft.yaml` forgotten at `2.0.11` (path confusion with root)
- **v2.2.3**: `public/splash.html` hardcoded version missed (Tauri IPC unavailable in splash)
- **v2.6.10**: `aur/.SRCINFO` not updated alongside PKGBUILD

---

## Release Channels

| Channel | Purpose | Update Frequency |
|---------|---------|------------------|
| `stable` | Production releases | On git tags |
| `edge` | Pre-release testing | Manual uploads |
| `beta` | Public beta testing | Manual uploads |

To release to edge first:
```bash
snapcraft upload aeroftp_X.Y.Z_amd64.snap --release=edge
# After testing, promote to stable:
snapcraft release aeroftp <revision> stable
```

---

*Last updated: 14 April 2026*
