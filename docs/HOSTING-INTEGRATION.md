# AeroFTP Hosting Provider Integration Guide

> Generate `.aeroftp` connection profiles from your control panel so customers can import pre-configured connections with a single click.

**Version**: 1.0
**Last Updated**: 4 April 2026

---

## Overview

AeroFTP uses an encrypted `.aeroftp` file format for exporting and importing server connection profiles. Hosting providers can generate these files from their control panels, allowing customers to download a ready-to-use profile instead of manually entering FTP/SFTP credentials.

**Benefits for hosting providers:**

- Customers connect in one click with no manual configuration
- Credentials are never exposed in plaintext emails
- Profiles are encrypted with AES-256-GCM + Argon2id key derivation
- Works with FTP, FTPS, SFTP, and WebDAV connections

---

## File Format

An `.aeroftp` file is a JSON document with the following structure:

```json
{
  "version": 1,
  "salt": [/* 32 random bytes */],
  "nonce": [/* 12 random bytes */],
  "encrypted_payload": [/* AES-256-GCM ciphertext */],
  "metadata": {
    "exportDate": "2026-04-04T20:00:00Z",
    "aeroftpVersion": "3.3.9",
    "serverCount": 1,
    "hasCredentials": true
  }
}
```

The `metadata` field is unencrypted and shown to the user before they enter the decryption password. The `encrypted_payload` contains the actual connection data.

---

## Encryption

### Key Derivation

The encryption password is processed through Argon2id with the following parameters:

| Parameter | Value |
|-----------|-------|
| Algorithm | Argon2id |
| Memory | 128 MiB |
| Iterations | 3 |
| Parallelism | 4 |
| Output length | 32 bytes |
| Salt | 32 random bytes |

### Encryption

| Parameter | Value |
|-----------|-------|
| Algorithm | AES-256-GCM |
| Key | 32-byte Argon2id output |
| Nonce | 12 random bytes |
| Plaintext | JSON-serialized payload (UTF-8) |

The `salt`, `nonce`, and `encrypted_payload` fields in the file are JSON arrays of unsigned byte values (0-255).

---

## Payload Schema

The decrypted payload is a JSON object containing a `servers` array:

```json
{
  "servers": [
    {
      "id": "unique-id",
      "name": "My FTP Server",
      "host": "ftp.example.com",
      "port": 21,
      "username": "customer123",
      "protocol": "ftp",
      "initialPath": "/public_html",
      "credential": "the-password",
      "options": {
        "tlsMode": "explicit",
        "verifyCert": true
      }
    }
  ]
}
```

### Server Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | string | yes | Unique identifier (UUID v4 recommended) |
| `name` | string | yes | Display name shown to the customer |
| `host` | string | yes | Server hostname or IP address |
| `port` | number | yes | Connection port |
| `username` | string | yes | Login username |
| `protocol` | string | no | `ftp`, `ftps`, `sftp`, `webdav`. Default: `ftp` |
| `initialPath` | string | no | Remote directory opened after connection |
| `localInitialPath` | string | no | Local directory paired with connection |
| `credential` | string | no | Password or passphrase (encrypted in file) |
| `color` | string | no | Hex color for the server badge (e.g. `#3B82F6`) |
| `providerId` | string | no | Provider identifier for branding |
| `options` | object | no | Protocol-specific options (see below) |

### Protocol Options

#### FTP / FTPS

| Option | Type | Values | Description |
|--------|------|--------|-------------|
| `tlsMode` | string | `explicit`, `implicit`, `explicit_if_available`, `none` | TLS encryption mode |
| `verifyCert` | boolean | `true` / `false` | Validate server certificate (default: `true`) |

- **`explicit`** (recommended): AUTH TLS on port 21
- **`implicit`**: TLS on port 990
- **`explicit_if_available`**: Try TLS, fall back to plaintext
- **`none`**: No encryption (not recommended)

#### SFTP

| Option | Type | Description |
|--------|------|-------------|
| `authMethod` | string | `password`, `key`, `key_and_password` |
| `privateKeyPath` | string | Path to SSH private key file |
| `key_passphrase` | string | Passphrase for encrypted private key |

#### WebDAV

No additional options required. Use the full server URL as `host` (e.g. `https://webdav.example.com/remote.php/dav/files/user/`).

---

## Example: Generating a Profile

### Python

```python
import json
import os
import uuid
from argon2.low_level import hash_secret_raw, Type
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from datetime import datetime, timezone

def generate_aeroftp_profile(servers, password):
    # Key derivation
    salt = os.urandom(32)
    key = hash_secret_raw(
        secret=password.encode('utf-8'),
        salt=bytes(salt),
        time_cost=3,
        memory_cost=131072,  # 128 MiB
        parallelism=4,
        hash_len=32,
        type=Type.ID
    )

    # Encrypt payload
    nonce = os.urandom(12)
    payload = json.dumps({"servers": servers}).encode('utf-8')
    aesgcm = AESGCM(key)
    ciphertext = aesgcm.encrypt(nonce, payload, None)

    return {
        "version": 1,
        "salt": list(salt),
        "nonce": list(nonce),
        "encrypted_payload": list(ciphertext),
        "metadata": {
            "exportDate": datetime.now(timezone.utc).isoformat(),
            "aeroftpVersion": "3.3.9",
            "serverCount": len(servers),
            "hasCredentials": any(s.get("credential") for s in servers)
        }
    }

# Example usage
servers = [{
    "id": str(uuid.uuid4()),
    "name": "Customer - example.com",
    "host": "ftp.example.com",
    "port": 21,
    "username": "customer@example.com",
    "protocol": "ftp",
    "initialPath": "/public_html",
    "credential": "customer-password",
    "options": {
        "tlsMode": "explicit",
        "verifyCert": True
    }
}]

profile = generate_aeroftp_profile(servers, "secure-transfer-password")

with open("customer.aeroftp", "w") as f:
    json.dump(profile, f, indent=2)
```

### Node.js

```javascript
const crypto = require('crypto');
const argon2 = require('argon2');
const { v4: uuidv4 } = require('uuid');

async function generateAeroftpProfile(servers, password) {
  const salt = crypto.randomBytes(32);

  // Argon2id key derivation
  const key = await argon2.hash(password, {
    type: argon2.argon2id,
    salt: salt,
    memoryCost: 131072,  // 128 MiB
    timeCost: 3,
    parallelism: 4,
    hashLength: 32,
    raw: true
  });

  // AES-256-GCM encryption
  const nonce = crypto.randomBytes(12);
  const payload = JSON.stringify({ servers });
  const cipher = crypto.createCipheriv('aes-256-gcm', key, nonce);
  const encrypted = Buffer.concat([
    cipher.update(payload, 'utf8'),
    cipher.final(),
    cipher.getAuthTag()  // 16-byte tag appended to ciphertext
  ]);

  return {
    version: 1,
    salt: [...salt],
    nonce: [...nonce],
    encrypted_payload: [...encrypted],
    metadata: {
      exportDate: new Date().toISOString(),
      aeroftpVersion: '3.3.9',
      serverCount: servers.length,
      hasCredentials: servers.some(s => s.credential)
    }
  };
}
```

---

## Import Flow

When a customer opens an `.aeroftp` file in AeroFTP:

1. AeroFTP reads the `metadata` and shows a summary (server count, export date, whether credentials are included)
2. The customer enters the decryption password
3. AeroFTP derives the key with Argon2id using the stored `salt`
4. The `encrypted_payload` is decrypted with AES-256-GCM
5. Connections appear in the "My Servers" list, ready to use

---

## Best Practices

- **Always use FTPS or SFTP** - set `tlsMode` to `explicit` or `implicit` for FTP, or use `protocol: "sftp"`
- **Use a strong transfer password** - this protects the credentials in transit. Communicate it to the customer through a separate channel (SMS, phone, different email)
- **Set `verifyCert: true`** - ensure your server has a valid TLS certificate (Let's Encrypt works)
- **Pre-fill `initialPath`** - save customers from navigating to their web root (`/public_html`, `/httpdocs`, `/www`, etc.)
- **Use meaningful `name`** - include the domain name so customers can identify the connection (e.g. "example.com - FTP")

---

## Support

For questions about the `.aeroftp` format or integration help, contact dev@aeroftp.app.

AeroFTP is free and open source (GPL-3.0). Hosting providers are welcome to integrate without any licensing requirements.
