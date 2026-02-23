# AeroFTP Documentation

Welcome to the AeroFTP documentation folder. This contains all technical documentation, compatibility audits, and guides.

---

## Table of Contents

| Document | Description |
| -------- | ----------- |
| **[RELEASE.md](./RELEASE.md)** | Complete release process and CI/CD automation |
| **[TRANSLATIONS.md](./TRANSLATIONS.md)** | Internationalization (i18n) guide for adding new languages |
| **[PROTOCOL-FEATURES.md](./PROTOCOL-FEATURES.md)** | Protocol feature comparison matrix (18 protocols) |
| **[UNIVERSAL-VAULT.md](./UNIVERSAL-VAULT.md)** | Universal Vault credential storage architecture, Unified Keystore, backup/restore |
| **[SECURITY-AUDIT-SUMMARY.md](./SECURITY-AUDIT-SUMMARY.md)** | Independent security and quality audit reports (v2.5.0 + v2.6.0 provider audit) |
| **[security-evidence/README.md](./security-evidence/README.md)** | Public security evidence index and release packs |

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

## Security Audits

### v2.6.0 — Provider Security Audit (147 findings)

Post-release audit covering all 8 cloud storage providers (S3, pCloud, kDrive, Azure Blob, 4shared, Filen, Internxt, MEGA). **147 findings remediated**: URL injection, SSRF endpoint validation, path traversal, OAuth token handling, XML entity limits, E2E key derivation hardening, pagination safeguards, and more. All fixes verified via `cargo check` (0 errors).

### v2.5.0 — 6-Domain Independent Audit (86 findings)

Comprehensive 6-domain independent audit covering Security & Cryptography, Rust Code Quality, CI/CD, Documentation & OpenSSF, Performance, and Frontend Quality. All 86 findings (9 Critical, 17 High, 28 Medium, 19 Low, 13 Info) were remediated. Post-remediation composite grade: **A-**. See **[SECURITY-AUDIT-SUMMARY.md](./SECURITY-AUDIT-SUMMARY.md)** for the full report.

The **Unified Encrypted Keystore** stores all sensitive data in the AES-256-GCM encrypted vault. See [UNIVERSAL-VAULT.md](./UNIVERSAL-VAULT.md), [SECURITY.md](../SECURITY.md), and [security-evidence/README.md](./security-evidence/README.md) for full details.

## AeroVault Directory Support (v1.9.0)

AeroVault v2 encrypted containers now support **full directory hierarchies**: create nested folders inside vaults, navigate with breadcrumb UI, add files to specific directories, and recursively delete directories with all contents. Three new backend commands: `vault_v2_create_directory`, `vault_v2_delete_entries` (recursive), `vault_v2_add_files_to_dir`.

## AeroAgent (v2.6.0)

AeroAgent features **45 tools** across 10 categories, **19 AI providers** (OpenAI, Anthropic, Gemini, xAI, OpenRouter, Ollama, Kimi, Qwen, DeepSeek, Mistral, Groq, Perplexity, Cohere, Together AI, AI21 Labs, Cerebras, SambaNova, Fireworks AI, Custom), a **Command Palette** (Ctrl+Shift+P), **Plugin Registry** with GitHub-based browser, **plugin hooks**, context menu AI actions, AI status widget, and drag & drop file analysis.

---

- **Documentation Version**: 2.6.0
- **Last Update**: 2026-02-22

---

**Maintainer**: axpnet
**Project**: [github.com/axpnet/aeroftp](https://github.com/axpnet/aeroftp)
