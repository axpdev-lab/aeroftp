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
| **[UNIVERSAL-VAULT.md](./UNIVERSAL-VAULT.md)** | Universal Vault credential storage architecture, Unified Keystore, backup/restore |
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

## Security (v1.9.0)

AeroFTP v1.9.0 introduces the **Unified Encrypted Keystore**: all sensitive data (server profiles, AI configuration, OAuth credentials) is now stored in the AES-256-GCM encrypted vault. The release also adds **keystore backup/restore** (`.aeroftp-keystore` format with Argon2id + AES-256-GCM) and a **migration wizard** that automatically moves legacy localStorage data to the encrypted vault on first launch. Dual security audit (Claude Opus 4.6 + GPT-5.2-Codex) with all findings resolved. See [UNIVERSAL-VAULT.md](./UNIVERSAL-VAULT.md) and [SECURITY.md](../SECURITY.md) for full details.

## AeroVault Directory Support (v1.9.0)

AeroVault v2 encrypted containers now support **full directory hierarchies**: create nested folders inside vaults, navigate with breadcrumb UI, add files to specific directories, and recursively delete directories with all contents. Three new backend commands: `vault_v2_create_directory`, `vault_v2_delete_entries` (recursive), `vault_v2_add_files_to_dir`.

## AeroAgent Intelligence (v1.9.0)

AeroAgent now features **27 tools** (up from 25), including **RAG integration** (`rag_index` + `rag_search` for workspace-aware AI context) and a **plugin system** for custom tools via JSON manifest + shell scripts. See the [README](../README.md) AeroAgent section for details.

---

- **Documentation Version**: 2.0.4
- **Last Update**: 2026-02-10

---

**Maintainer**: axpnet
**Project**: [github.com/axpnet/aeroftp](https://github.com/axpnet/aeroftp)
