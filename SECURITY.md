# Security Policy

> _Last updated: 2026-10-26_

## Supported Versions

| Version | Supported           |
| ------- | ------------------- |
| 4.2.x   | Yes (current)       |
| 4.1.x   | Security fixes only |
| 4.0.x   | Security fixes only |
| 3.8.x   | Security fixes only |
| 3.7.x   | End of Life         |
| < 3.7   | No                  |

## Security Architecture

AeroFTP follows a defense-in-depth security model across six layers. For the complete architecture with trust boundary diagrams and protocol-level details, see the [Security Overview](https://docs.aeroftp.app/security/overview) on the documentation site.

### Credential Storage

All sensitive data (server passwords, OAuth tokens, API keys, application configuration) is stored in an encrypted vault (`vault.db`) using AES-256-GCM with per-entry random nonces. The vault key is derived via HKDF-SHA256 from a 512-bit CSPRNG passphrase.

| Mode | How the passphrase is protected |
| ---- | ------------------------------- |
| **Default** | Stored in the OS keyring (GNOME Keyring, macOS Keychain, Windows Credential Manager) |
| **Master password** | Encrypted with Argon2id (128 MiB, t=4, p=4) + AES-256-GCM |
| **Keyring unavailable** | Falls back to a permission-protected `vault.key` containing the random passphrase in cleartext; master-password mode remains available |

The credential database stays AES-256-GCM encrypted. In the no-keyring fallback, its unlocking passphrase is on disk beside it, so file permissions are the protection boundary until a master password is enabled. File permissions are hardened to `0600` (Unix) / owner-only ACL (Windows). See [Universal Vault](docs/UNIVERSAL-VAULT.md) for the modes and keyring account lifecycle.

For the complete credential lifecycle, import/export, and OS keyring integration, see [Credential Management](https://docs.aeroftp.app/security/credentials).

### Encryption

AeroFTP uses encryption at multiple layers:

| Layer | Algorithm | Purpose |
| ----- | --------- | ------- |
| AeroVault v2 containers | AES-256-GCM-SIV (RFC 8452) + Argon2id + HMAC-SHA512 | Encrypted file containers with nonce misuse resistance |
| AeroVault v3 containers (Published, v3.8.0) | Gear-CDC chunking + zstd per chunk + AES-256-GCM-SIV per chunk + BLAKE3-128 chunk id + BLAKE3-256 cipher hash + Argon2id KEKs (HKDF + AES-KW) + HMAC-SHA512 header | Published format, opt-in via the Archive tier; reserves an extension area for the v4 EC layer (non-critical) |
| AeroVault v4 + EC (T-AEROVAULT-ECC, shipped) | v3 + non-critical "error-correction.reed-solomon" extension (Reed-Solomon 10+2 on the ciphertext live-block stream, v2 fixed-grid ~20% overhead, per-shard 16B BLAKE3 cksums, all-or-nothing repair gate) | "v3 + EC = v4" forward-compat (pure v3 open/extract still works); EC-last wrapper per #272/#276; scrub/repair operational; telemetry in receipts. See the [public v4 format note](docs/AEROVAULT-V3-SPEC.md#11-v4-evolution-note-t-aerovault-ecc-shipped). |
| Archive encryption | AES-256 (ZIP, 7z) | Password-protected archives |
| rclone crypt interoperability | XSalsa20-Poly1305 content + EME filename decryption | Compatible access to existing rclone crypt remotes |
| Credential storage | AES-256-GCM + HKDF-SHA256 | Per-entry vault encryption |
| Transport | TLS 1.2/1.3, SSH | Encrypted transport where configured; plain FTP and HTTP WebDAV are unencrypted |

Key derivation parameters exceed OWASP 2024 minimums (128 MiB vs 47 MiB, 4 iterations vs 1). AeroVault v2 and v3 are available in the standalone [`aerovault`](https://crates.io/crates/aerovault) crate on crates.io (the crate carries the AeroVault v3 core since 0.6.0). AeroVault v3 is the published archive format and ships in the desktop binary and the bundled `aeroftp-cli` (the `vault` subcommand covers v1/v2/v3). v4 = v3 + EC (non-critical Reed-Solomon layer, T-AEROVAULT-ECC). Specifications: [v2](docs/AEROVAULT-V2-SPEC.md), [v3 + v4 note](docs/AEROVAULT-V3-SPEC.md).

### rclone crypt interoperability

AeroFTP also documents compatibility workflows for existing `rclone crypt` remotes. This is separate from AeroVault because the format and threat model are defined by rclone, not by AeroFTP.

- AeroVault and the AeroFTP crypt overlay are AeroFTP-native encryption layers
- `rclone crypt` support is about browsing and decrypting already encrypted rclone-backed storage

See the public docs for details:

- [rclone Bridge](https://docs.aeroftp.app/features/rclone)
- [rclone crypt interoperability](https://docs.aeroftp.app/features/rclone-crypt)

For the full encryption architecture, cipher comparison tables, and AeroVault v2 format specification, see [Encryption](https://docs.aeroftp.app/security/encryption).

### Connection Protocols

AeroFTP supports 7 transport protocols and 25+ native provider integrations with appropriate transport security:

| Category | Protocols |
| -------- | --------- |
| **End-to-end encrypted** | MEGA.nz, Filen, Internxt (client-side AES, zero-knowledge) |
| **Browser authorization** | Google Drive, Dropbox, OneDrive, Box, pCloud, Zoho WorkDrive, Twake Drive and 4shared use provider-specific flows; PKCE and client-secret requirements depend on the provider |
| **TLS/HTTPS** | S3, WebDAV, Azure Blob, pCloud, FileLu, Jottacloud, OpenDrive, Yandex Disk |
| **API Token over HTTPS** | GitHub, GitLab (PAT / Project Access Token, API v4) |
| **API Key over HTTPS (media)** | ImageKit (private key, HTTP Basic), Uploadcare (public + secret), Cloudinary (cloudname + key + secret), Immich (`x-api-key`) |
| **SSH** | SFTP with TOFU host key verification |
| **Configurable TLS** | FTP/FTPS (Explicit, Implicit, opportunistic) |

Plain FTP connections display a prominent insecure warning badge. WebDAV supports RFC 2617 Digest Authentication with automatic detection. SFTP uses Trust On First Use host key verification with visual fingerprint dialog and MITM change detection.

### AI Tool Security

AeroAgent (86 built-in GUI tools) operates under backend-enforced security controls:

- **Grant system**: Mutative tools require a cryptographic grant verified by the Rust backend
- **Confirmation modes**: Safe and Normal use a backend-created AeroFTP approval window for mutating grants; Expert uses the in-app approval panel and Extreme can auto-approve tools. Backend request binding and tool restrictions still apply, but the modes have different frontend-compromise boundaries
- **Credential isolation**: AI models never receive raw credentials; the backend authenticates internally
- **Shell denylist**: 34 regex patterns block dangerous commands
- **Path validation**: Null bytes, traversal, and system paths blocked at the backend level
- **Strict mode**: `--strict` (or `AEROFTP_STRICT=1`) makes any safety-relaxing CLI flag a hard error (exit 5), so unattended and agent-generated commands fail closed instead of silently downgrading TLS/host-key verification or auto-approving destructive tools

For the complete AI security model with grant properties, tool classification, and agent modes, see [AI Security](https://docs.aeroftp.app/security/ai-security).

### Supply Chain

All release artifacts are signed with Sigstore Cosign via GitHub Actions OIDC keyless signing:

- **Client-side verification**: The app verifies `.sigstore.json` bundles against the CI workflow identity before installing updates. As of v4.1.0 (sigstore 0.14) the in-app check parses GitHub's current v0.3 signing bundles and returns a real verified result; it stays a non-blocking second factor, with the artifact SHA-256 as the primary integrity check
- **Linux hardening**: The privileged update helper re-verifies SHA-256 before executing `dpkg`/`rpm`
- **Plugin registry**: Remote installation disabled until cryptographic registry authentication is implemented (fail-closed)
- **Build gates**: pushes to `main` and release tags run Clippy (`--all-targets --features test-seed -- -D warnings`) and `cargo audit --deny warnings` as hard CI gates; release artifacts are signed and published only after the lint, audit, and test jobs pass

### Continuous Monitoring

#### Self-Hosted Vulnerability Audit (primary)

AeroFTP ships a self-hosted audit pipeline that runs locally and in CI without depending on any vendor SaaS. It aggregates three independent advisory databases and cross-references findings against a documented suppression list:

```bash
npm run security:report        # generate HTML report
npm run security:report -- --json
```

- **[`cargo audit`](https://rustsec.org/)** against the RustSec advisory database (Rust dependencies)
- **`npm audit`** against the npm registry (Node production dependencies)
- **[`osv-scanner`](https://google.github.io/osv-scanner/)** against the Google OSV database (cross-references RustSec, GHSA, CVE)

Findings not yet addressed are surfaced as **open**. Findings accepted with written rationale are listed under **suppressed** with a link to [`src-tauri/.cargo/audit.toml`](src-tauri/.cargo/audit.toml) where every entry carries an inline justification reviewers can audit.

| Month | Version | Open | Suppressed (justified) | Report |
|---|---|---|---|---|
| Jun 2026 | v4.0.7 | **0** | 28 | [HTML](docs/security/security-report-latest.html) |
| May 2026 | v3.8.0 | **0** | 26 | archived |

#### Third-party tooling

- **[Socket.dev](https://socket.dev)**: Supply chain SCA monitoring on every push - dependency risk scoring, typosquatting detection
- **[Snyk](https://snyk.io)**: Continuous vulnerability scanning for npm and Cargo dependencies with automated fix PRs
- **[CodeRabbit](https://www.coderabbit.ai)**: AI-driven pull-request review on every PR - inline code suggestions and secret/PII checks complementing the SAST/SCA stack
- **GitHub Dependabot**: Native alerts and auto-PRs cross-referenced against the self-hosted audit suppression list
- **[Aikido Security](https://aikido.dev)**: Past audits (February-May 2026) archived - Top 5% benchmark, 0 open issues during the trial period

For Sigstore verification commands and CI/CD security controls, see [Supply Chain Security](https://docs.aeroftp.app/security/supply-chain).

### Memory Safety

- `zeroize` and `secrecy` crates clear passwords, keys, and tokens from memory after use
- All provider credentials wrapped in `SecretString` across every provider integration
- Rust ownership model prevents use-after-free and buffer overflows
- Passwords are never logged or written to disk in plain text
- Activity log and UI credential masking: usernames, emails, and access keys are masked at the source (`maskCredential`) before reaching log entries or display subtitles, preventing accidental exposure in bug reports and screenshots

### TOTP Two-Factor Authentication

Optional RFC 6238 TOTP second factor for vault access with exponential rate limiting (5 failures to 15-minute lockout cap). Setup requires initial code verification before enforcement activates. The 30-second codes accept a one-step clock skew; consumed steps are rejected as replays. Throttle and replay state are persisted in the encrypted vault, and auto-keyring unlock verifies TOTP before caching the usable session.

For the complete TOTP implementation, rate limiting table, and security properties, see [TOTP 2FA](https://docs.aeroftp.app/security/totp).

## Privacy

AeroFTP collects no product telemetry and sends no analytics. Credentials and settings are local. Configured background sync, update checks and enabled AeroShare discovery/receiving can communicate while the app is running, without a new connect click each time. AI and MCP operations contact the services the user enables. See [PRIVACY.md](PRIVACY.md) for the external-connection boundaries.

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

### Compliance Verification

The March 2026 Aikido Security audit verified compliance against the following frameworks with 0 open issues:

- **OWASP Top 10** - injection prevention, XSS mitigation, credential isolation, path validation
- **ISO 27001** - encryption controls, access management, credential lifecycle
- **CIS Benchmarks** - file permission hardening, transport security, supply chain controls
- **NIS2 Directive** - incident response readiness, supply chain security, encryption at rest and in transit
- **GDPR** - no telemetry, no analytics, no third-party data sharing, local-only storage, no cloud account required

These are verified compliance checks, not formal certifications.

## Known Issues

| ID | Severity | Status | Details |
| -- | -------- | ------ | ------- |
| [CVE-2025-54804](https://github.com/axpdev-lab/aeroftp/security/dependabot/3) | Medium | **Resolved** | russh SFTP, fixed by upgrade to v0.57 |
| [GHSA-wwx6-x28x-8259](https://github.com/advisories/GHSA-wwx6-x28x-8259) | High | **Resolved** | russh, resolved by upgrade to 0.61.2 (v4.0.5) |
| [GHSA-hpv4-5h6f-wqr3](https://github.com/advisories/GHSA-hpv4-5h6f-wqr3) | Medium | **Resolved** | russh, resolved by upgrade to 0.61.2 (v4.0.5) |
| [GHSA-g9hv-x236-4qp3](https://github.com/advisories/GHSA-g9hv-x236-4qp3) | Medium | **Resolved** | The July resolution is recorded in [public evidence](docs/security-evidence/RUSSH-DEPENDABOT-ADVISORIES-2026-07.md#resolution-2026-07-28); the v4.2.5 preparation snapshot pins russh 0.64.1 |
| [GHSA-5xvq-cp9x-6p6r](https://github.com/advisories/GHSA-5xvq-cp9x-6p6r) | Medium | **Resolved** | Covered by the same russh upgrade and recorded July resolution |
| [GHSA-cqjc-rmpq-xprq](https://github.com/advisories/GHSA-cqjc-rmpq-xprq) | Medium | **Resolved** | Covered by the same russh upgrade. The historical client-only reachability statement predates the optional `serve sftp` command |

## Reporting a Vulnerability

**Do not report security vulnerabilities through public GitHub issues.**

Report via [GitHub Security Advisories](https://github.com/axpdev-lab/aeroftp/security/advisories/new) or by email to the security contact **dev@aeroftp.app**. We respond within 48 hours.

For the full disclosure policy, bug bounty scope, and Security Hall of Fame, see [Vulnerability Disclosure](https://docs.aeroftp.app/security/reporting).

---

*AeroFTP v4.2.x*
