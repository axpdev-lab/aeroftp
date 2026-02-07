# Universal Credential Vault — Technical Documentation

> AeroFTP v1.9.0 — February 2026

---

## Overview

The Universal Vault is AeroFTP's credential storage system. It replaces the previous dual-mode architecture (OS Keyring + Encrypted Vault fallback) with a single, platform-independent encrypted vault that works identically on Linux, Windows, and macOS.

Since **v1.9.0**, the vault scope has been expanded into a **Unified Encrypted Keystore**: all sensitive data that was previously stored in browser localStorage (server profiles, AI provider configuration, OAuth credentials, application settings) is now encrypted in `vault.db`. A migration wizard automatically moves legacy data on first launch, and a backup/restore system enables vault portability across devices.

### Design Goals

1. **Zero-interaction default**: Credentials save and load automatically without any user prompt
2. **Cross-platform reliability**: No dependency on OS keyring services (gnome-keyring, Windows Credential Manager, macOS Keychain)
3. **Optional master password**: Users who want extra protection can enable a master password at any time
4. **Simple architecture**: One code path, no fallback chains, no health probes
5. **Complete coverage** (v1.9.0): No sensitive data in localStorage — everything encrypted in vault
6. **Portable backups** (v1.9.0): Full vault export/import for disaster recovery and device migration

---

## Architecture

### File Layout

```
~/.config/aeroftp/          (Linux/macOS)
%APPDATA%/aeroftp/          (Windows)
├── vault.key               Binary passphrase file (76 or 136 bytes)
├── vault.db                AES-256-GCM encrypted credential database (JSON)
└── ...
```

### Two Modes

| Mode | vault.key contains | User interaction | Use case |
|------|-------------------|------------------|----------|
| **Auto** (default) | Passphrase in cleartext | None — fully transparent | Most users |
| **Master** (optional) | Passphrase encrypted with user password | Enter password on app start | Security-conscious users |

### Security Model

**Auto mode** relies on OS file permissions for protection:
- Unix: `chmod 0o600` (owner read/write only)
- Windows: `icacls` ACL restricting access to current user

**Master mode** adds cryptographic protection:
- User password → Argon2id (128 MiB, t=4, p=4) → 256-bit KEK
- KEK + AES-256-GCM encrypts the 64-byte passphrase in vault.key
- Even if vault.key is exfiltrated, passphrase cannot be recovered without the user password

**Write serialization** (v1.8.9): All `store()` and `delete()` operations are serialized via `VAULT_WRITE_LOCK` Mutex. This prevents concurrent read-modify-write races when multiple credentials are saved simultaneously (e.g., OAuth settings for multiple providers).

---

## vault.key Binary Format

### Auto Mode (76 bytes)

```
Offset  Size  Field        Description
──────  ────  ─────        ───────────
0       8     MAGIC        "AEROVKEY" (ASCII)
8       1     VERSION      0x02
9       1     MODE         0x00 = auto
10      64    PASSPHRASE   64 bytes from CSPRNG (rand::thread_rng)
74      2     PADDING      Reserved (zeroes)
```

### Master Mode (136 bytes)

```
Offset  Size  Field             Description
──────  ────  ─────             ───────────
0       8     MAGIC             "AEROVKEY" (ASCII)
8       1     VERSION           0x02
9       1     MODE              0x01 = master
10      32    SALT              Argon2id salt (random)
42      12    NONCE             AES-GCM nonce (random)
54      80    ENC_PASSPHRASE    64-byte passphrase + 16-byte GCM auth tag
134     2     PADDING           Reserved (zeroes)
```

---

## Key Derivation

### Passphrase → Vault Key (both modes)

```
Passphrase (64 bytes, 512 bits entropy)
    │
    ▼
HKDF-SHA256 (RFC 5869)
    info = "aeroftp-vault-v2"
    salt = None (passphrase already high-entropy)
    │
    ▼
Vault Key (32 bytes, 256 bits)
```

HKDF is sufficient here because the input has 512 bits of entropy (CSPRNG-generated). No need for memory-hard KDF on high-entropy input.

### User Password → KEK (master mode only)

```
User Password (low entropy, human-chosen)
    │
    ▼
Argon2id (RFC 9106)
    memory  = 128 MiB (131072 KiB)
    time    = 4 iterations
    threads = 4 parallelism
    salt    = 32 random bytes
    │
    ▼
Key Encryption Key (32 bytes, 256 bits)
```

Argon2id with these parameters exceeds OWASP 2024 recommendations. The high memory cost makes GPU/ASIC attacks impractical.

---

## vault.db Format

The credential database is a JSON object encrypted per-entry with AES-256-GCM:

```json
{
  "version": 2,
  "entries": {
    "server_srv_abc123": {
      "nonce": "<base64 12 bytes>",
      "ciphertext": "<base64 encrypted password + 16-byte tag>"
    },
    "oauth_googledrive_access_token": { ... },
    "ai_apikey_openai": { ... }
  }
}
```

### Credential Key Naming Convention

| Type | Key format | Example |
|------|-----------|---------|
| Server password | `server_{id}` | `server_srv_abc123` |
| Server profile | `profile_{id}` | `profile_srv_abc123` |
| OAuth access token | `oauth_{provider}_access_token` | `oauth_googledrive_access_token` |
| OAuth refresh token | `oauth_{provider}_refresh_token` | `oauth_dropbox_refresh_token` |
| OAuth client ID | `oauth_{provider}_client_id` | `oauth_onedrive_client_id` |
| OAuth client secret | `oauth_{provider}_client_secret` | `oauth_box_client_secret` |
| AI API key | `ai_apikey_{provider}` | `ai_apikey_openai` |
| AI settings | `ai_settings_{provider}` | `ai_settings_openai` |
| App config | `config_{key}` | `config_theme_preference` |
| WebDAV password | `server_{id}` | Same as server |
| S3 secret key | `server_{id}` | Same as server |

> **v1.9.0 expansion**: `profile_*`, `ai_settings_*`, and `config_*` entries are new in v1.9.0 and correspond to data previously stored in browser localStorage.

---

## Startup Flow

### First Run (no vault.key exists)

```
App start
  → init_credential_store()
  → vault.key NOT found
  → Generate 64-byte CSPRNG passphrase
  → Write vault.key (auto mode, 76 bytes)
  → Set file permissions (0o600 / ACL)
  → Create empty vault.db
  → HKDF(passphrase) → vault key
  → Cache vault key in VAULT_CACHE
  → Return "OK"

Frontend: credentials ready, no LockScreen
Header: Shield icon → "Set Master Password" tooltip
```

### Subsequent Launch (auto mode)

```
App start
  → init_credential_store()
  → vault.key exists, mode = 0x00 (auto)
  → Read passphrase from vault.key
  → HKDF(passphrase) → vault key
  → Open vault.db, cache vault key
  → Return "OK"

Credentials available immediately.
```

### Launch with Master Password

```
App start
  → init_credential_store()
  → vault.key exists, mode = 0x01 (master)
  → Return "MASTER_PASSWORD_REQUIRED"

Frontend: show LockScreen
User enters password
  → unlock_credential_store(password)
  → Argon2id(password, salt) → KEK
  → AES-GCM decrypt(KEK, nonce, encrypted_passphrase)
  → HKDF(passphrase) → vault key
  → Open vault.db, cache vault key
  → Return OK

LockScreen dismissed, credentials available.
```

---

## Operations

### Enable Master Password

```
User clicks Shield icon → MasterPasswordSetupDialog
  → enable_master_password(password, timeout_seconds)
  → Read vault.key (auto mode) → extract passphrase
  → Generate random salt (32 bytes) + nonce (12 bytes)
  → Argon2id(password, salt) → KEK
  → AES-GCM encrypt(KEK, nonce, passphrase) → encrypted_passphrase
  → Rewrite vault.key in master mode (136 bytes)
  → Set auto-lock timeout
  → Vault key remains cached (no re-unlock needed)
```

### Disable Master Password

```
Settings → Security → Remove Master Password
  → disable_master_password(current_password)
  → Read vault.key (master mode)
  → Argon2id(password, salt) → KEK
  → Decrypt passphrase
  → Rewrite vault.key in auto mode (76 bytes, cleartext passphrase)
```

### Change Master Password

```
Settings → Security → Change Password
  → change_master_password(old_password, new_password)
  → Read vault.key → decrypt with old password
  → Generate new salt + nonce
  → Encrypt passphrase with new password
  → Rewrite vault.key
```

### Lock / Unlock

```
Lock (manual or auto-timeout):
  → lock_credential_store()
  → Clear VAULT_CACHE (zeroed)
  → MasterPasswordState.locked = true
  → Frontend shows LockScreen

Unlock:
  → unlock_credential_store(password)
  → Decrypt vault.key → HKDF → vault key → cache
  → MasterPasswordState.locked = false
  → LockScreen dismissed
```

### Auto-Lock

```
Every 30 seconds:
  → app_master_password_check_timeout()
  → If (now - last_activity) > timeout_seconds:
    → lock_credential_store()
    → Show LockScreen

Activity updated on:
  → Mouse move, keyboard press, scroll events
  → app_master_password_update_activity()
```

---

## Tauri Commands

| Command | Description |
|---------|-------------|
| `init_credential_store` | Initialize vault on app start. Returns "OK" or "MASTER_PASSWORD_REQUIRED" |
| `get_credential_store_status` | Returns `{ master_mode, locked, timeout_seconds }` |
| `store_credential` | Encrypt and store a credential in vault.db |
| `get_credential` | Decrypt and retrieve a credential from vault.db |
| `delete_credential` | Remove a credential from vault.db |
| `unlock_credential_store` | Unlock vault with master password |
| `lock_credential_store` | Lock vault (clear cache) |
| `enable_master_password` | Switch from auto → master mode |
| `disable_master_password` | Switch from master → auto mode |
| `change_master_password` | Re-encrypt vault.key with new password |

---

## Cryptographic Primitives

| Primitive | Library | Usage |
|-----------|---------|-------|
| AES-256-GCM | `aes-gcm 0.10` | vault.db per-entry encryption, vault.key passphrase encryption |
| Argon2id | `argon2 0.5` | Master password → KEK derivation |
| HKDF-SHA256 | `hkdf 0.12` + `sha2 0.10` | Passphrase → vault key derivation |
| CSPRNG | `rand 0.8` | Passphrase generation, nonce generation, salt generation |

---

## Comparison with Previous System

| Aspect | v1.8.x (Dual Mode) | v1.8.6 (Universal Vault) | v1.9.0 (Unified Keystore) |
|--------|--------------------|-----------------------|--------------------------|
| Backends | OS Keyring + Encrypted Vault | Single vault only | Single vault only |
| Scope | Passwords + tokens only | Passwords + tokens only | **All sensitive data** (profiles, AI config, OAuth, settings) |
| localStorage usage | Server profiles, AI settings, configs | Server profiles, AI settings, configs | **Non-sensitive UI state only** |
| Windows | Silent failures | Reliable | Reliable |
| Linux | gnome-keyring dependency | No dependency | No dependency |
| Default UX | May require master password | Zero interaction | Zero interaction + auto-migration |
| Backup/restore | None | None | `.aeroftp-keystore` (Argon2id + AES-256-GCM) |
| Migration | None | None (clean break) | Automatic wizard from localStorage |
| Fallback chain | 5 levels | None needed | Vault-first + localStorage read fallback |
| Code complexity | ~800 lines | ~450 lines | ~550 lines (+ secureStorage.ts) |
| Crate dependencies | keyring + argon2 + aes-gcm | argon2 + aes-gcm + hkdf | argon2 + aes-gcm + hkdf |

---

## Unified Keystore (v1.9.0)

### Motivation

Before v1.9.0, the Universal Vault stored only credential secrets (passwords, tokens, API keys). Server profile metadata (host, port, username, protocol), AI provider settings (model selection, temperature), and application preferences remained in browser localStorage — unencrypted and accessible to any code running in the webview.

The Unified Keystore moves **all sensitive data** into `vault.db`, eliminating the last unencrypted storage surface.

### secureStorage.ts API

The frontend uses a `secureStorage` utility that abstracts vault access:

```typescript
// Vault-first with localStorage fallback
await secureStorage.setItem('profile_srv_abc', jsonString);
const data = await secureStorage.getItem('profile_srv_abc');
await secureStorage.removeItem('profile_srv_abc');
```

- **Write**: Always writes to vault via Tauri `store_credential` command
- **Read**: Reads from vault first; falls back to localStorage if not found (migration may be partial)
- **Delete**: Removes from both vault and localStorage

### Data Categories

| Category | Key prefix | Count tracked | Example data |
|----------|-----------|---------------|--------------|
| Server credentials | `server_` | Yes | Passwords, SSH keys |
| Connection profiles | `profile_` | Yes | Host, port, username, protocol config |
| AI API keys | `ai_apikey_` | Yes | OpenAI, Anthropic, Gemini keys |
| AI settings | `ai_settings_` | Yes | Model, provider, temperature |
| OAuth tokens | `oauth_` | Yes | Access/refresh tokens for 5 OAuth providers |
| Config entries | `config_` | Yes | Theme, locale, UI preferences |

---

## Migration Wizard (v1.9.0)

### Trigger

The migration wizard runs automatically on first launch after upgrading to v1.9.0. It detects localStorage entries matching known key patterns and migrates them to the encrypted vault.

### Migration Flow

```
App start (v1.9.0 first launch)
  → detect_legacy_data()
  → Found: server profiles, AI settings, OAuth tokens in localStorage
  → Show migration dialog with category summary
  → User confirms (or auto-migrate in background)
  → For each entry:
    → Read from localStorage
    → store_credential(key, value) → vault.db
    → Verify: get_credential(key) matches original
    → Mark localStorage entry as migrated
  → Show completion summary
  → Remove migrated localStorage entries
```

### Safety

- Migration is **non-destructive**: localStorage entries are only removed after successful vault write + verification
- If migration is interrupted, it resumes on next launch (idempotent)
- Entries that fail to migrate remain in localStorage and continue to work via the fallback read path

---

## Keystore Backup and Restore (v1.9.0)

### Export (.aeroftp-keystore)

The backup file contains a complete snapshot of all vault entries, encrypted with a user-chosen backup password.

```
Export flow:
  → User enters backup password
  → Read all entries from vault.db
  → Serialize to JSON with category metadata
  → Argon2id(password, random_salt) → backup KEK
  → AES-256-GCM encrypt(KEK, payload) → ciphertext
  → HMAC-SHA256(KEK, ciphertext) → integrity tag
  → Write .aeroftp-keystore file:
    [magic: "AEROBKP\0"] [version: 1] [salt: 32B]
    [nonce: 12B] [hmac: 32B] [ciphertext]
```

### Import

```
Import flow:
  → User selects .aeroftp-keystore file
  → Verify magic bytes and version
  → User enters backup password
  → Argon2id(password, salt) → backup KEK
  → Verify HMAC-SHA256 integrity
  → AES-256-GCM decrypt → JSON payload
  → Parse categories and entry count summary
  → User selects merge strategy:
    → Skip existing: only import entries not in current vault
    → Overwrite all: replace all entries with backup data
  → Import entries to vault.db
  → Show completion summary with per-category counts
```

### Backup Encryption Parameters

| Parameter | Value |
|-----------|-------|
| KDF | Argon2id |
| Memory | 64 MB (65536 KiB) |
| Iterations | 3 |
| Parallelism | 4 |
| Salt | 32 random bytes |
| Encryption | AES-256-GCM (12-byte nonce) |
| Integrity | HMAC-SHA256 over ciphertext |
| File extension | `.aeroftp-keystore` |

---

## Security Considerations

### Auto Mode Threat Model

- **Physical access**: vault.key is readable only by the current OS user (file permissions). An attacker with same-user access can read credentials.
- **Root access**: Root can read any file. This is the same threat model as OS keyring on Linux (gnome-keyring is also accessible to root).
- **Mitigation**: Enable master password mode for stronger protection.

### Master Mode Threat Model

- **Brute force**: Argon2id with 128 MiB makes each guess take ~1 second on modern hardware. A 12-character password with mixed case/digits/symbols is practically unbreakable.
- **Memory dump**: Vault key is in memory while app is unlocked. Auto-lock timeout limits exposure window.
- **vault.key exfiltration**: Without the user password, the encrypted passphrase cannot be recovered.

### What We Don't Protect Against

- Keyloggers capturing the master password as it's typed
- Memory forensics on a running, unlocked application
- A compromised Tauri frontend sending credentials to a malicious endpoint

These threats are out of scope for any client-side credential manager, including OS keyrings.

---

*This document is maintained as part of AeroFTP development documentation.*
