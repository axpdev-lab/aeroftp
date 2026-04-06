# Security Policy

## Supported Versions

| Version | Supported           |
| ------- | ------------------- |
| 3.3.x   | Yes (current)       |
| 3.2.x   | Security fixes only |
| 3.1.x   | Security fixes only |
| < 3.0   | No                  |

## Security Architecture

AeroFTP follows a defense-in-depth security model across six layers. For the complete architecture with trust boundary diagrams and protocol-level details, see the [Security Overview](https://docs.aeroftp.app/security/overview) on the documentation site.

### Credential Storage

All sensitive data (server passwords, OAuth tokens, API keys, application configuration) is stored in an encrypted vault (`vault.db`) using AES-256-GCM with per-entry random nonces. The vault key is derived via HKDF-SHA256 from a 512-bit CSPRNG passphrase.

| Mode | How the passphrase is protected |
| ---- | ------------------------------- |
| **Default** | Stored in the OS keyring (GNOME Keyring, macOS Keychain, Windows Credential Manager) |
| **Master password** | Encrypted with Argon2id (128 MiB, t=4, p=4) + AES-256-GCM |
| **First launch without keyring** | Bootstraps directly into master password mode |

The vault never falls back to plaintext storage. File permissions are hardened to `0600` (Unix) / owner-only ACL (Windows).

For the complete credential lifecycle, import/export, and OS keyring integration, see [Credential Management](https://docs.aeroftp.app/security/credentials).

### Encryption

AeroFTP uses encryption at multiple layers:

| Layer | Algorithm | Purpose |
| ----- | --------- | ------- |
| AeroVault v2 containers | AES-256-GCM-SIV (RFC 8452) + Argon2id + HMAC-SHA512 | Encrypted file containers with nonce misuse resistance |
| Archive encryption | AES-256 (ZIP, 7z) | Password-protected archives |
| Credential storage | AES-256-GCM + HKDF-SHA256 | Per-entry vault encryption |
| Transport | TLS 1.2/1.3, SSH | Wire encryption for all protocols |

Key derivation parameters exceed OWASP 2024 minimums (128 MiB vs 47 MiB, 4 iterations vs 1). AeroVault v2 is available as the standalone [`aerovault`](https://crates.io/crates/aerovault) crate on crates.io.

For the full encryption architecture, cipher comparison tables, and AeroVault v2 format specification, see [Encryption](https://docs.aeroftp.app/security/encryption).

### Connection Protocols

AeroFTP supports 27 protocols with appropriate transport security:

| Category | Protocols |
| -------- | --------- |
| **End-to-end encrypted** | MEGA.nz, Filen, Internxt (client-side AES, zero-knowledge) |
| **OAuth2 with PKCE** | Google Drive, Dropbox, OneDrive, Box, Zoho WorkDrive, kDrive, Koofr, Internxt |
| **TLS/HTTPS** | S3, WebDAV, Azure Blob, pCloud, FileLu, Jottacloud, OpenDrive, Yandex Disk |
| **API Token over HTTPS** | GitHub, GitLab (PAT/Project Access Token, API v4) |
| **SSH** | SFTP with TOFU host key verification |
| **Configurable TLS** | FTP/FTPS (Explicit, Implicit, opportunistic) |

Plain FTP connections display a prominent insecure warning badge. WebDAV supports RFC 2617 Digest Authentication with automatic detection. SFTP uses Trust On First Use host key verification with visual fingerprint dialog and MITM change detection.

### AI Tool Security

AeroAgent (48 tools) operates under backend-enforced security controls:

- **Grant system**: Mutative tools require a cryptographic grant verified by the Rust backend
- **Native OS confirmation**: Grant approval triggers an operating system dialog that cannot be bypassed by web frontend compromise or prompt injection
- **Credential isolation**: AI models never receive raw credentials; the backend authenticates internally
- **Shell denylist**: 35 regex patterns block dangerous commands
- **Path validation**: Null bytes, traversal, and system paths blocked at the backend level

For the complete AI security model with grant properties, tool classification, and agent modes, see [AI Security](https://docs.aeroftp.app/security/ai-security).

### Supply Chain

All release artifacts are signed with Sigstore Cosign via GitHub Actions OIDC keyless signing:

- **Client-side verification**: The app verifies `.sigstore.json` bundles against the CI workflow identity before installing updates
- **Linux hardening**: The privileged update helper re-verifies SHA-256 before executing `dpkg`/`rpm`
- **Plugin registry**: Remote installation disabled until cryptographic registry authentication is implemented (fail-closed)

### Continuous Monitoring

- **[Aikido Security](https://aikido.dev)**: SAST, SCA, secrets detection, IaC scanning - daily automated scans
- **[Socket.dev](https://socket.dev)**: Supply chain SCA monitoring on every push - dependency risk scoring, typosquatting detection

For Sigstore verification commands and CI/CD security controls, see [Supply Chain Security](https://docs.aeroftp.app/security/supply-chain).

### Memory Safety

- `zeroize` and `secrecy` crates clear passwords, keys, and tokens from memory after use
- All provider credentials wrapped in `SecretString` across all 23 providers
- Rust ownership model prevents use-after-free and buffer overflows
- Passwords are never logged or written to disk in plain text
- Activity log and UI credential masking: usernames, emails, and access keys are masked at the source (`maskCredential`) before reaching log entries or display subtitles, preventing accidental exposure in bug reports and screenshots

### TOTP Two-Factor Authentication

Optional RFC 6238 TOTP second factor for vault access with exponential rate limiting (5 failures to 15-minute lockout cap). Setup requires initial code verification before enforcement activates.

For the complete TOTP implementation, rate limiting table, and security properties, see [TOTP 2FA](https://docs.aeroftp.app/security/totp).

## Privacy

AeroFTP collects no telemetry, sends no analytics, and makes no network requests beyond user-initiated connections. All credential storage is local. No cloud accounts or external services are involved in authentication or settings.

For the complete privacy model, data storage locations, and deletion instructions, see [Privacy](https://docs.aeroftp.app/security/privacy).

## Security Audits

| Date | Auditors | Result | Report |
| ---- | -------- | ------ | ------ |
| March 2026 | GPT 5.4 + Claude Opus 4.6 | Desktop security: 4 findings, all remediated | |
| March 2026 | Aikido Security | Top 5% benchmark, 0 open issues, OWASP/ISO/CIS/NIS2/GDPR | [PDF](docs/Security%20Audit%20Report%20axpdev-lab%20-%20March%202026.pdf) |
| February 2026 | Aikido Security | Top 5% benchmark, 0 open issues | [PDF](docs/Security%20Audit%20Report%20axpnet%20-%20February%202026.pdf) |
| v2.9.5 | Claude Opus 4.6 + GPT 5.4 | 117 findings, grade A- | |
| v2.8.7 | Claude Opus 4.6 + GPT 5.4 | 45+ findings resolved, grade A- | |
| v2.4.0 | 12 auditors, 4 phases | Provider integration audit, grade A- | |

Cumulative: 300+ findings identified across 9 audits, all critical and high findings remediated. For the complete audit history with finding details, see [Security Audits](https://docs.aeroftp.app/security/audits).

## Known Issues

| ID | Severity | Status | Details |
| -- | -------- | ------ | ------- |
| [CVE-2025-54804](https://github.com/axpdev-lab/aeroftp/security/dependabot/3) | Medium | **Resolved** | russh SFTP, fixed by upgrade to v0.57 |

## Reporting a Vulnerability

**Do not report security vulnerabilities through public GitHub issues.**

Report via [GitHub Security Advisories](https://github.com/axpdev-lab/aeroftp/security/advisories/new). We respond within 48 hours.

For the full disclosure policy, bug bounty scope, and Security Hall of Fame, see [Vulnerability Disclosure](https://docs.aeroftp.app/security/reporting).

---

*AeroFTP v3.4.3 - 6 April 2026*
