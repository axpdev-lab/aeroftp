# Security Policy

## Supported Versions

| Version | Supported |
| ------- | --------- |
| 1.5.x   | Yes |
| 1.4.x   | Security fixes only |
| < 1.4   | No  |

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
| FTP | None (configurable) | Plain-text by default; supports Explicit TLS, Implicit TLS, or opportunistic TLS upgrade |
| FTPS | TLS/SSL | Explicit TLS (AUTH TLS, port 21) or Implicit TLS (port 990). Certificate verification configurable. |
| SFTP | SSH | Native Rust implementation (russh 0.57) |
| WebDAV | HTTPS | TLS encrypted |
| S3 | HTTPS | SigV4 authentication with TLS |
| Google Drive | HTTPS + OAuth2 | PKCE flow with token refresh |
| Dropbox | HTTPS + OAuth2 | PKCE flow with token refresh |
| OneDrive | HTTPS + OAuth2 | PKCE flow with token refresh |
| MEGA.nz | Client-side AES | End-to-end encrypted, zero-knowledge |
| Box | HTTPS + OAuth2 | PKCE flow with token refresh |
| pCloud | HTTPS + OAuth2 | Token-based authentication |
| Azure Blob | HTTPS | Shared Key HMAC-SHA256 or SAS token |
| Filen | Client-side AES-256-GCM | E2E encrypted, PBKDF2 key derivation |

### FTPS Encryption Modes (v1.4.0)

AeroFTP supports all standard FTPS encryption modes:

| Mode | Description | Default Port |
| ---- | ----------- | ------------ |
| **Explicit TLS** | Connects plain, sends AUTH TLS to upgrade before login | 21 |
| **Implicit TLS** | Direct TLS connection from the start | 990 |
| **Explicit if available** | Attempts AUTH TLS, falls back to plain FTP if server doesn't support it | 21 |
| **None** | Plain FTP (insecure warning displayed) | 21 |

Additional options:
- **Certificate verification**: Enabled by default; can be disabled per-connection for self-signed certificates
- **TLS backend**: `native-tls` (system TLS library: OpenSSL on Linux, Secure Transport on macOS, SChannel on Windows)

### OAuth2 Security

- **PKCE** (Proof Key for Code Exchange) with SHA-256 code challenge
- **CSRF** protection via state token validation
- **Token storage** in OS keyring or encrypted vault
- **Automatic refresh** with 5-minute buffer before expiry
- **Ephemeral callback port**: OS-assigned random port (not a fixed port)

### Archive Encryption

| Format | Encryption | Backend |
| ------ | ---------- | ------- |
| **ZIP** | AES-256 (read + write) | `zip` v7.2 |
| **7z** | AES-256 (read + write) | `sevenz-rust` v0.6 + p7zip sidecar |
| **RAR** | Password-protected extraction | p7zip CLI |

### Memory Safety

- `zeroize` crate clears passwords and keys from memory after use
- `secrecy` crate provides zero-on-drop containers for secrets
- Passwords are never logged or written to disk in plain text
- Rust ownership model prevents use-after-free and buffer overflows

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

### FTP Insecure Connection Warning

When the user selects plain FTP (no TLS), AeroFTP displays:

- A red **"Insecure"** badge on the protocol selector
- A warning banner recommending FTPS or SFTP
- Fully localized (51 languages)

### OAuth Session Security (v1.5.3)

- OAuth credentials resolved from OS keyring on session switch (no plaintext fallback)
- Tokens refreshed automatically on tab switching with proper PKCE re-authentication
- Stale quota/connection state cleared before reconnection

---

## Unique Security Advantages

| Feature | Description | Why It Matters |
| ------- | ----------- | -------------- |
| **Encrypted Vault Fallback** | AES-256-GCM vault with Argon2id KDF when OS keyring is unavailable | Competitors store credentials in plaintext config files when keyring fails |
| **Ephemeral OAuth Port** | OS-assigned random port for OAuth2 callback | Fixed ports allow local processes to intercept tokens |
| **FTP Insecure Warning** | Visual red badge and warning banner on FTP selection | No competitor warns users about plaintext FTP risks |
| **Memory Zeroization** | `zeroize` and `secrecy` crates clear passwords from RAM | Rust-exclusive advantage over C++/Java competitors |
| **FTPS TLS Mode Selection** | Users choose Explicit, Implicit, or opportunistic TLS | Full control over encryption level per connection |

## Known Issues

| ID | Component | Severity | Status | Details |
| -- | --------- | -------- | ------ | ------- |
| [CVE-2025-54804](https://github.com/axpnet/aeroftp/security/dependabot/3) | russh (SFTP) | Medium | **Resolved** | Fixed by upgrading to russh v0.57. |

## Reporting a Vulnerability

**Please do not report security vulnerabilities through public GitHub issues.**

Instead, please report them via [GitHub Security Advisories](https://github.com/axpnet/aeroftp/security/advisories/new) or create a private issue.

Include:
- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (if any)

We will respond within 48 hours and work with you to address the issue.

*AeroFTP v1.5.3 - February 2026*
