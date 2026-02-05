# AeroFTP Documentation

Welcome to the AeroFTP documentation folder. This contains all technical documentation, compatibility audits, and guides.

---

## Table of Contents

| Document | Description |
| -------- | ----------- |
| **[RELEASE.md](./RELEASE.md)** | Complete release process and CI/CD automation |
| **[TRANSLATIONS.md](./TRANSLATIONS.md)** | Internationalization (i18n) guide for adding new languages |
| **[PROTOCOL-FEATURES.md](./PROTOCOL-FEATURES.md)** | Protocol feature comparison matrix (13 protocols) |
| **[COMPETITOR-ANALYSIS.md](./COMPETITOR-ANALYSIS.md)** | Market and competitor analysis |
| **[UNIVERSAL-VAULT.md](./UNIVERSAL-VAULT.md)** | Universal Vault credential storage architecture |
| **[UBUNTU-COMPATIBILITY.md](./UBUNTU-COMPATIBILITY.md)** | Ubuntu 22.04/24.04 LTS compatibility audit |
| **[WINDOWS-COMPATIBILITY.md](./WINDOWS-COMPATIBILITY.md)** | Windows 10/11 compatibility audit |

---

## Quick Links

### Release Process
See **[RELEASE.md](./RELEASE.md)** for complete CI/CD documentation.

**Quick version:**
```bash
# Update version in 4 files, then:
git commit -m "chore(release): vX.Y.Z Description"
git tag -a vX.Y.Z -m "Release vX.Y.Z"
git push origin main --tags
# GitHub Actions handles the rest automatically!
```

### Automated Distribution
| Platform | Artifacts | Auto-published to |
|----------|-----------|-------------------|
| Linux | `.deb`, `.rpm`, `.AppImage`, `.snap` | GitHub Releases + **Snap Store** |
| Windows | `.msi`, `.exe` | GitHub Releases |
| macOS | `.dmg` | GitHub Releases |

---

## Version Files

Update version in these 4 files before release:

| File | Field |
|------|-------|
| `package.json` | `"version": "X.Y.Z"` |
| `src-tauri/tauri.conf.json` | `"version": "X.Y.Z"` |
| `src-tauri/Cargo.toml` | `version = "X.Y.Z"` |
| `snap/snapcraft.yaml` | `version: 'X.Y.Z'` |

---

## Translations

AeroFTP supports multiple languages. See [TRANSLATIONS.md](./TRANSLATIONS.md) for:
- Adding a new language
- Translation file structure
- Contributing translations

Currently supported: **51 languages** at 100% coverage

---

- **Documentation Version**: 1.8.7
- **Last Update**: 2026-02-05

---

**Maintainer**: axpnet
**Project**: [github.com/axpnet/aeroftp](https://github.com/axpnet/aeroftp)
