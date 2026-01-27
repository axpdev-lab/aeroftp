# 📚 AeroFTP Documentation

Welcome to the AeroFTP documentation folder. This contains all technical documentation, release plans, and guides.

---

## 📋 Table of Contents

| Document                                             | Description                                                |
| ---------------------------------------------------- | ---------------------------------------------------------- |
| **[RELEASE.md](./RELEASE.md)**                       | Complete release process and CI/CD automation              |
| **[TRANSLATIONS.md](./TRANSLATIONS.md)**             | Internationalization (i18n) guide for adding new languages |
| **[PROTOCOL-FEATURES.md](./PROTOCOL-FEATURES.md)**   | Protocol feature comparison matrix                         |
| **[COMPETITOR-ANALYSIS.md](./COMPETITOR-ANALYSIS.md)**| Market and competitor analysis                             |
| **[MACOS_RELEASE_PLAN.md](./MACOS_RELEASE_PLAN.md)** | Complete macOS release and distribution guide              |
| **[MACOS_QUICKSTART.md](./MACOS_QUICKSTART.md)**     | Quick start guide for macOS builds                         |
| **[FLATHUB_SUBMISSION.md](./FLATHUB_SUBMISSION.md)** | Linux Flatpak packaging and distribution                   |

---

## 🚀 Quick Links

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

### Platform-Specific Guides
- **Linux**: [FLATHUB_SUBMISSION.md](./FLATHUB_SUBMISSION.md) (Flatpak)
- **macOS**: [MACOS_RELEASE_PLAN.md](./MACOS_RELEASE_PLAN.md)
- **Snap Store**: Automatic via CI (see [RELEASE.md](./RELEASE.md))

---

## 📝 Version Files

Update version in these 4 files before release:

| File | Field |
|------|-------|
| `package.json` | `"version": "X.Y.Z"` |
| `src-tauri/tauri.conf.json` | `"version": "X.Y.Z"` |
| `src-tauri/Cargo.toml` | `version = "X.Y.Z"` |
| `snap/snapcraft.yaml` | `version: 'X.Y.Z'` |

---

## 🌍 Translations

AeroFTP supports multiple languages. See [TRANSLATIONS.md](./TRANSLATIONS.md) for:
- Adding a new language
- Translation file structure
- Contributing translations

Currently supported: **English** (base), **Italian**

---

## 📅 Last Updated

- **Documentation Version**: 1.3.0
- **Last Update**: 2026-01-28

---

**Maintainer**: axpnet  
**Project**: [github.com/axpnet/aeroftp](https://github.com/axpnet/aeroftp)
