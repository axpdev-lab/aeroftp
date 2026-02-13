# AeroFTP Documentation

Welcome to the AeroFTP documentation folder. This contains all technical documentation, compatibility audits, and guides.

---

## Table of Contents

| Document | Description |
| -------- | ----------- |
| **[RELEASE.md](./RELEASE.md)** | Complete release process and CI/CD automation |
| **[TRANSLATIONS.md](./TRANSLATIONS.md)** | Internationalization (i18n) guide for adding new languages |
| **[PROTOCOL-FEATURES.md](./PROTOCOL-FEATURES.md)** | Protocol feature comparison matrix (14 protocols) |
| **[COMPETITOR-ANALYSIS.md](./dev/COMPETITOR-ANALYSIS.md)** | Market and competitor analysis (internal) |
| **[UNIVERSAL-VAULT.md](./UNIVERSAL-VAULT.md)** | Universal Vault credential storage architecture, Unified Keystore, backup/restore |
| **[security-evidence/README.md](./security-evidence/README.md)** | Public security evidence index and release packs |
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

### Security Evidence
See **[security-evidence/README.md](./security-evidence/README.md)** for the public release-by-release security evidence index and template.

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

Currently supported: **47 languages** at 100% coverage

---

## Security (v2.0.6)

AeroFTP v2.0.5 adds **WebDAV HTTP Digest Authentication (RFC 2617)** with auto-detection — the password is never transmitted, only MD5 challenge-response hashes. CloudMe is the only cloud service requiring Digest auth, and AeroFTP is one of the few clients that support it correctly. The **Unified Encrypted Keystore** (v1.9.0) stores all sensitive data in the AES-256-GCM encrypted vault. See [UNIVERSAL-VAULT.md](./UNIVERSAL-VAULT.md), [SECURITY.md](../SECURITY.md), and [security-evidence/README.md](./security-evidence/README.md) for full details.

## AeroVault Directory Support (v1.9.0)

AeroVault v2 encrypted containers now support **full directory hierarchies**: create nested folders inside vaults, navigate with breadcrumb UI, add files to specific directories, and recursively delete directories with all contents. Three new backend commands: `vault_v2_create_directory`, `vault_v2_delete_entries` (recursive), `vault_v2_add_files_to_dir`.

## AeroAgent Intelligence (v1.9.0)

AeroAgent now features **27 tools** (up from 25), including **RAG integration** (`rag_index` + `rag_search` for workspace-aware AI context) and a **plugin system** for custom tools via JSON manifest + shell scripts. See the [README](../README.md) AeroAgent section for details.

---

- **Documentation Version**: 2.0.9
- **Last Update**: 2026-02-13

---

**Maintainer**: axpnet
**Project**: [github.com/axpnet/aeroftp](https://github.com/axpnet/aeroftp)
