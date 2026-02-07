# Security Policy

## Supported Versions

| Version | Supported |
| ------- | --------- |
| 1.9.x   | Yes (current) |
| 1.8.x   | Security fixes only |
| 1.7.x   | Security fixes only |
| < 1.7   | No  |

## Security Architecture

### Credential Storage

AeroFTP uses a **Universal Vault** — a single encrypted credential backend that works identically on all platforms, replacing the previous dual-mode OS Keyring + encrypted vault approach.

**Universal Vault (`vault.key` + `vault.db`)**

| Component | Details |
| --------- | ------- |
| **Storage** | `vault.db` (AES-256-GCM encrypted entries) at `~/.config/aeroftp/` |
| **Key file** | `vault.key` (76 bytes auto / 136 bytes master mode) with magic bytes and version |
| **Auto mode** (default) | 64-byte CSPRNG passphrase in `vault.key`, protected by OS file permissions (Unix `0600`, Windows ACL) |
| **Master mode** (optional) | Passphrase encrypted with Argon2id (128 MiB, t=4, p=4) + AES-256-GCM. User enters master password on app start |
| **Key derivation** | HKDF-SHA256 (RFC 5869): 512-bit passphrase → 256-bit vault key |
| **Encryption** | AES-256-GCM with per-entry random 12-byte nonces |
| **Write serialization** | `VAULT_WRITE_LOCK` Mutex prevents concurrent read-modify-write races (v1.8.9) |
| **File permissions** | `0600` (owner read/write only) on Unix; `icacls` ACL-restricted on Windows |

**Why Universal Vault (v1.8.6+)?**

The previous dual-mode system (OS Keyring primary + encrypted vault fallback) suffered from platform-specific failures: Windows Credential Manager silently lost credentials, Linux keyring required desktop environment, macOS Keychain prompted for permissions. The Universal Vault eliminates all platform dependencies while maintaining equivalent or stronger security.

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
- **Token storage** in Universal Vault (AES-256-GCM encrypted)
- **Automatic refresh** with 5-minute buffer before expiry
- **Ephemeral callback port**: OS-assigned random port (not a fixed port)

### Unified Encrypted Keystore (v1.9.0)

In v1.9.0, AeroFTP consolidates **all sensitive data** from localStorage into the encrypted vault. Previously, only server passwords and OAuth tokens were vault-encrypted; AI API keys, server profiles, and application configuration remained in browser localStorage. The Unified Keystore eliminates this gap.

| Aspect | Details |
| ------ | ------- |
| **Scope** | Server profiles, AI provider settings, OAuth credentials, application config — all moved to `vault.db` |
| **Frontend utility** | `secureStorage.ts` provides a vault-first API with automatic localStorage fallback for non-sensitive data |
| **Migration wizard** | Auto-triggers on first launch after upgrade; migrates all legacy localStorage entries to encrypted vault |
| **Data categories** | Server credentials, connection profiles, AI API keys, OAuth access/refresh tokens, config entries |
| **Encryption** | Same AES-256-GCM per-entry encryption as the Universal Vault (HKDF-SHA256 derived key) |
| **Backward compatibility** | Falls back to localStorage reads if vault entry is not found (migration may be partial on first run) |

**What changed**: Before v1.9.0, `localStorage` contained server profile metadata (host, port, username — not passwords), AI provider selection, model preferences, and UI settings. After v1.9.0, all of these are encrypted in `vault.db`. The only data remaining in localStorage is non-sensitive UI state (window size, panel layout).

### Keystore Backup and Restore (v1.9.0)

AeroFTP v1.9.0 introduces full vault export/import for disaster recovery and device migration.

| Feature | Details |
| ------- | ------- |
| **Export format** | `.aeroftp-keystore` binary file |
| **Encryption** | Argon2id (64 MB, t=3, p=4) + AES-256-GCM with user-chosen backup password |
| **Contents** | Complete vault snapshot: server credentials, connection profiles, AI API keys, OAuth tokens, config entries |
| **Category tracking** | Export summary shows count per category (e.g., "12 server credentials, 3 AI keys, 5 OAuth tokens") |
| **Import merge strategies** | **Skip existing**: only import entries not already in the vault. **Overwrite all**: replace all entries with backup data |
| **Integrity verification** | HMAC-SHA256 over the encrypted payload; import fails gracefully if file is corrupted or password is wrong |
| **UI** | Settings > Security > Backup/Restore with progress indicator and category summary |

### Client-Side Encryption (v1.8.0)

**AeroVault v2 (.aerovault containers) — Military-Grade Encryption**

AeroVault v2 implements a state-of-the-art encryption stack:

| Component | Algorithm | Notes |
| --------- | --------- | ----- |
| **Content encryption** | AES-256-GCM-SIV (RFC 8452) | Nonce misuse-resistant — even nonce reuse doesn't compromise confidentiality |
| **Key wrapping** | AES-256-KW (RFC 3394) | Built-in integrity check on unwrap |
| **Filename encryption** | AES-256-SIV | Deterministic, hides file names |
| **Key derivation** | Argon2id | 128 MiB memory, 4 iterations, 4 parallelism |
| **Header integrity** | HMAC-SHA512 | 512-bit MAC, quantum-resistance margin |
| **Cascade mode** | ChaCha20-Poly1305 | Optional double encryption layer |
| **Chunk size** | 64 KB | Per-chunk random nonce + auth tag |
| **Container format** | Binary | 512-byte header, encrypted manifest, chunked data |

**Security advantages over Cryptomator:**

| Feature | AeroVault v2 | Cryptomator |
| ------- | ------------ | ----------- |
| Nonce misuse resistance | Yes (GCM-SIV) | No (GCM) |
| KDF memory | 128 MiB | 64-128 MiB |
| KDF algorithm | Argon2id | scrypt |
| Header integrity hash | SHA-512 | SHA-256 |
| Cascade encryption | Optional | No |

**AeroVault v1 (legacy)**

| Parameter | Value |
| --------- | ----- |
| Algorithm | AES-256-GCM |
| Key derivation | Argon2id (64 MB, 3 iterations, 4 threads) |
| Nonce | 12 bytes random per file entry |

**Cryptomator (format 8 vaults) — Legacy Support**

Accessible via folder context menu "Open as Cryptomator Vault":

| Component | Algorithm |
| --------- | --------- |
| Master key derivation | scrypt (N=2^15, r=8, p=1) |
| Key wrapping | AES Key Wrap (RFC 3394) |
| Filename encryption | AES-SIV (deterministic) |
| Content encryption | AES-GCM (32KB chunks with chunk counter nonce) |
| Directory ID hashing | SHA-256 truncated to Base32 |

### Archive Encryption

| Format | Encryption | Backend |
| ------ | ---------- | ------- |
| **ZIP** | AES-256 (read + write) | `zip` v7.2 |
| **7z** | AES-256 (read + write) | `sevenz-rust` v0.6 + p7zip sidecar |
| **RAR** | Password-protected extraction | p7zip CLI |

Archive passwords are wrapped in `secrecy::SecretString` for automatic memory zeroization on drop (SEC-001).

### Memory Safety

- `zeroize` crate clears passwords and keys from memory after use
- `secrecy` crate provides zero-on-drop containers for secrets
- Passwords are never logged or written to disk in plain text
- Rust ownership model prevents use-after-free and buffer overflows
- Archive passwords (ZIP/7z/RAR) wrapped in SecretString (v1.5.2)
- OAuth tokens wrapped in SecretString across all 5 OAuth providers (v1.5.2)

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

### AI Tool Security (v1.6.0+)

- **Tool name whitelist**: Only 27 allowed tool names accepted by the backend (25 built-in + `rag_index` + `rag_search`)
- **Path validation**: Null byte rejection, path traversal prevention (`..`), 4096-char length limit
- **Content size limits**: Remote file reads capped at 5KB, directory listings at 100 entries
- **Native function calling**: SEC-002 resolved — structured JSON tool calls replace regex parsing for OpenAI, Anthropic, Gemini
- **Danger levels**: Safe (auto-execute), Medium (user confirmation), High (explicit approval for delete operations)
- **Rate limiting**: 20 requests per minute per AI provider, frontend token bucket
- **Plugin sandboxing** (v1.9.0): Custom tools run as isolated subprocesses with 30s timeout, 1MB output limit, and plugin ID validation (alphanumeric + underscore only). Plugin execution goes through a separate `execute_plugin_tool` command, not the built-in tool whitelist

### OAuth Session Security (v1.5.3)

- OAuth credentials resolved from OS keyring on session switch (no plaintext fallback)
- Tokens refreshed automatically on tab switching with proper PKCE re-authentication
- Stale quota/connection state cleared before reconnection

---

## Unique Security Advantages

| Feature | Description | Why It Matters |
| ------- | ----------- | -------------- |
| **AeroVault v2** | Military-grade containers with AES-256-GCM-SIV (nonce misuse-resistant), AES-KW key wrapping, AES-SIV filename encryption, Argon2id 128 MiB, HMAC-SHA512 integrity, optional ChaCha20 cascade | Advanced encryption with nonce misuse resistance and cascade mode |
| **Cryptomator Support** | Format 8 vault compatibility with scrypt + AES-SIV + AES-GCM (context menu) | Only Cyberduck also supports this; FileZilla, WinSCP do not |
| **Universal Vault** | Single AES-256-GCM vault with HKDF-SHA256, Argon2id master mode, no OS keyring dependency (v1.8.6) | Competitors store credentials in plaintext config files or depend on OS keyring |
| **Unified Keystore** | ALL sensitive data (server profiles, AI config, OAuth tokens) migrated from localStorage to encrypted vault (v1.9.0) | No credentials exposed in browser storage; complete encryption coverage |
| **Keystore Backup/Restore** | Full vault export/import as `.aeroftp-keystore` with Argon2id + AES-256-GCM protection (v1.9.0) | Secure disaster recovery and device migration; competitors offer no encrypted backup |
| **Ephemeral OAuth Port** | OS-assigned random port for OAuth2 callback | Fixed ports allow local processes to intercept tokens |
| **FTP Insecure Warning** | Visual red badge and warning banner on FTP selection | No competitor warns users about plaintext FTP risks |
| **Memory Zeroization** | `zeroize` and `secrecy` crates clear passwords from RAM | Rust-exclusive advantage over C++/Java competitors |
| **Archive Password Zeroization** | ZIP/7z/RAR passwords wrapped in SecretString | Prevents password leakage in memory dumps |
| **AI Tool Sandboxing** | 27-tool whitelist + path validation + danger levels + rate limiting + plugin subprocess isolation | AI cannot execute arbitrary commands or access restricted paths |
| **FTPS TLS Mode Selection** | Users choose Explicit, Implicit, or opportunistic TLS | Full control over encryption level per connection |

## Known Issues

| ID | Component | Severity | Status | Details |
| -- | --------- | -------- | ------ | ------- |
| [CVE-2025-54804](https://github.com/axpnet/aeroftp/security/dependabot/3) | russh (SFTP) | Medium | **Resolved** | Fixed by upgrading to russh v0.57. |
| SEC-001 | Archive passwords | Medium | **Resolved (v1.5.2)** | ZIP/7z/RAR passwords now wrapped in SecretString |
| SEC-002 | AI tool parsing | Medium | **Resolved (v1.6.0)** | Native function calling replaces regex-based parsing |
| SEC-003 | Keep-alive routing | Low | **Resolved (v1.5.1)** | Keep-alive now routes correctly per protocol |
| SEC-004 | OAuth token exposure | Medium | **Resolved (v1.5.2)** | OAuth tokens wrapped in SecretString |

## Reporting a Vulnerability

**Please do not report security vulnerabilities through public GitHub issues.**

Instead, please report them via [GitHub Security Advisories](https://github.com/axpnet/aeroftp/security/advisories/new) or create a private issue.

Include:
- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (if any)

We will respond within 48 hours and work with you to address the issue.

*AeroFTP v1.9.0 - 6 February 2026*
