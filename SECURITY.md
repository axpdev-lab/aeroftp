# Security Policy

## Supported Versions

| Version | Supported |
| ------- | --------- |
| 1.3.x   | Yes |
| < 1.3   | No  |

## Security Architecture

### Credential Storage

AeroFTP uses a dual-mode credential storage system with the OS native keyring as primary backend and an encrypted vault as fallback.

**OS Keyring (primary)**

| Platform | Backend |
| -------- | ------- |
| Linux | gnome-keyring / Secret Service |
| macOS | Keychain |
| Windows | Credential Manager |

**Encrypted Vault (fallback)**

When the OS keyring is unavailable, credentials are stored in a local encrypted vault at `~/.config/aeroftp/vault.db`:

- **Key derivation**: Argon2id (64 MB memory, 3 iterations, 4 threads) producing a 256-bit key
- **Encryption**: AES-256-GCM with per-entry random 12-byte nonces
- **File permissions**: `0600` (owner read/write only)

### Connection Protocols

| Protocol | Encryption | Details |
| -------- | ---------- | ------- |
| FTP | None | Plain-text, use only on trusted networks |
| FTPS | TLS/SSL | Implicit TLS on port 990 |
| SFTP | SSH | Native Rust implementation (russh) |
| WebDAV | HTTPS | TLS encrypted |
| S3 | HTTPS | AWS SDK with TLS |
| Google Drive | HTTPS + OAuth2 | PKCE flow with token refresh |
| Dropbox | HTTPS + OAuth2 | PKCE flow with token refresh |
| OneDrive | HTTPS + OAuth2 | PKCE flow with token refresh |
| MEGA.nz | Client-side AES | End-to-end encrypted, zero-knowledge |

### OAuth2 Security

- **PKCE** (Proof Key for Code Exchange) with SHA-256 code challenge
- **CSRF** protection via state token validation
- **Token storage** in OS keyring or encrypted vault
- **Automatic refresh** with 5-minute buffer before expiry

### Archive Encryption

- **7z**: AES-256 encryption/decryption support
- **ZIP**: Deflate compression (level 6)
- **TAR**: .tar, .tar.gz, .tar.xz, .tar.bz2 support

### Memory Safety

- `zeroize` crate clears passwords and keys from memory after use
- `secrecy` crate provides zero-on-drop containers for secrets
- Passwords are never logged or written to disk in plain text

### File System Hardening

- Config directory (`~/.config/aeroftp/`): permissions `0700`
- Vault and token files: permissions `0600`
- Applied recursively on startup

### SFTP Host Key Verification

AeroFTP implements Trust On First Use (TOFU) for SFTP connections:

- On first connection, the server's public key is saved to `~/.ssh/known_hosts`
- On subsequent connections, the stored key is compared against the server's key
- **Key mismatch = connection rejected** (MITM protection)
- Supports `[host]:port` format for non-standard ports
- Creates `~/.ssh/` directory with `0700` and `known_hosts` with `0600` permissions automatically

### OAuth2 Ephemeral Port

OAuth2 callback server binds to port `0`, letting the OS assign a random available port:

- Eliminates fixed-port predictability (previously port 17548)
- Prevents local token interception by other processes listening on the known port
- The redirect URI is constructed dynamically with the assigned port
- No competitor uses ephemeral ports for OAuth2 callbacks

### FTP Insecure Connection Warning

When the user selects FTP (unencrypted), AeroFTP displays:

- A red **"Insecure"** badge on the protocol selector
- A warning banner recommending FTPS or SFTP as secure alternatives
- Fully localized via i18n (51 languages)
- No competitor visually warns users about plain-text FTP risks

### Privacy and Analytics

- **Aptabase** integration: opt-in only, disabled by default
- No PII collected (only protocol types, feature usage, transfer size ranges)
- EU data residency (GDPR compliant)

---

## Unique Security Advantages

AeroFTP implements security measures not found in any competing FTP client:

| Feature | Description | Why It Matters |
| ------- | ----------- | -------------- |
| **Encrypted Vault Fallback** | AES-256-GCM vault with Argon2id KDF when OS keyring is unavailable | Competitors store credentials in plaintext config files when keyring fails. AeroFTP never has plaintext credentials on disk. |
| **Ephemeral OAuth Port** | OS-assigned random port for OAuth2 callback instead of fixed port | A fixed port (e.g. 17548) allows any local process to bind first and intercept tokens. Ephemeral ports are unpredictable. |
| **FTP Insecure Warning** | Visual red badge and warning banner on FTP protocol selection | No competitor warns users that FTP transmits passwords in cleartext. AeroFTP educates users to choose FTPS/SFTP. |
| **Memory Zeroization** | `zeroize` and `secrecy` crates clear passwords from RAM after use | Rust-exclusive advantage. C++/Java competitors leave passwords in memory until garbage collected or process exit, vulnerable to memory dumps. |

## Known Issues

| ID | Component | Severity | Status | Details |
| -- | --------- | -------- | ------ | ------- |
| [CVE-2025-54804](https://github.com/axpnet/aeroftp/security/dependabot/3) | russh v0.48 (SFTP) | Medium | **Resolved** | Fixed by upgrading to russh v0.54.5. Removed russh-keys dependency. |
| - | SFTP host key verification | Low | Resolved | Trust On First Use (TOFU) via russh built-in `known_hosts` module. Rejects connections on key mismatch (MITM protection). |

## Reporting a Vulnerability

**Please do not report security vulnerabilities through public GitHub issues.**

Instead, please report them via [GitHub Security Advisories](https://github.com/axpnet/aeroftp/security/advisories/new) or create a private issue.

Include:
- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (if any)

We will respond within 48 hours and work with you to address the issue.

*AeroFTP v1.3.4 - January 2026*
