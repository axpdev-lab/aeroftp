# Credential Isolation for AI Agents

> How AeroFTP enables AI coding agents to operate on remote servers across 22 protocols without exposing credentials.

---

## The Problem

AI coding agents — whether integrated into IDEs (Claude Code, Cursor, Codex, Devin) or running autonomously via CLI — increasingly need to interact with remote servers. Deploying a website, synchronizing a build folder, downloading logs, or inspecting remote configurations are routine tasks that agents can automate effectively.

However, every such operation requires authentication. The agent needs credentials to connect to the server. In current workflows, this means one of the following:

- The developer shares the password directly in the chat or prompt
- The password is placed in an environment variable that the agent can read
- The credentials are stored in a plaintext configuration file accessible to the agent's process
- The agent is given a URL containing embedded credentials, visible in shell history and process listings

In each case, the authentication material is exposed to the agent. The agent may log it, include it in context sent to the AI model, persist it in conversation history, or inadvertently surface it in error messages. This is not a theoretical concern — it is the default behavior of every file transfer tool available today.

---

## The State of Credential Storage (March 2026)

Across the file transfer and cloud storage ecosystem, credential storage remains fundamentally unprotected against same-process or same-user access:

**Multi-cloud CLIs** store credentials in plaintext configuration files or use reversible encoding that provides no meaningful security boundary. The most widely used multi-cloud CLI stores passwords with a static encryption key that is identical across all installations and publicly documented, making the encoding trivially reversible by any process that can read the configuration file. OAuth tokens for cloud providers are stored alongside these passwords in the same unencrypted file.

**FTP/SFTP clients with command-line interfaces** either store passwords in plaintext XML, use encoding schemes for which multiple public extraction tools exist, or rely on OS keystores that are accessible to any process running under the same user account.

**S3 and cloud storage utilities** universally use plaintext credential files. This has been a known limitation since at least 2015, with multiple open issues requesting encrypted credential storage that remain unresolved.

**IDE deployment extensions** for VS Code, JetBrains, and similar editors store server credentials in workspace configuration files (typically JSON or XML) that are readable by any extension, agent, or process with access to the workspace directory. Some editors have introduced encrypted credential backends in recent versions, but none enforce an isolation boundary between the IDE's AI agent features and the stored credentials.

**Credential proxy services** that have emerged in 2025-2026 address the isolation problem for HTTP API calls by injecting real credentials at a proxy layer, keeping the agent working with opaque tokens. However, these services only support HTTP-based APIs. They cannot handle native file transfer protocols such as FTP, FTPS, SFTP, or WebDAV, which require persistent TCP connections with protocol-specific authentication handshakes.

---

## How AeroFTP Solves This

AeroFTP implements credential isolation at the architecture level, not as an add-on or workaround.

### The Vault

All server credentials — passwords, API keys, OAuth access tokens, OAuth refresh tokens, and client secrets — are stored in an encrypted vault (`vault.db`) using AES-256-GCM with keys derived via Argon2id (128 MiB memory, 4 iterations, 4 lanes). The vault is a single encrypted file in the user's configuration directory, protected by either an auto-generated 512-bit passphrase (default) or a user-chosen master password.

The vault is not a wrapper around the OS keystore. It is a self-contained encrypted database that works identically across Linux, macOS, and Windows.

### The Isolation Boundary

When an AI agent needs to operate on a remote server, it never receives the credentials. Instead:

1. The agent calls `aeroftp ls --profile "My Server" /path/` (CLI) or invokes the `server_exec` tool (AeroAgent)
2. The Rust backend receives the request with only the profile name and the operation to perform
3. The backend opens the encrypted vault, loads the credential material, and authenticates to the remote server
4. The operation executes entirely within the Rust process
5. The agent receives only the result — a directory listing, a transfer confirmation, file content — with no credential material attached

The credential material exists only inside the Rust process memory during the operation. It never appears in:

- Command-line arguments (`/proc/*/cmdline`)
- Environment variables
- Shell history
- IPC messages between backend and frontend
- AI model context or conversation history
- Log output or error messages (errors are sanitized to remove credential patterns)

### CLI: `--profile`

```bash
# List saved server profiles (names only, never credentials)
aeroftp profiles

# Connect and operate using a saved profile
aeroftp ls --profile "Production" /var/www/
aeroftp put --profile "Staging" ./dist/app.js /var/www/app.js
aeroftp sync --profile "NAS Backup" ./data/ /backups/ --dry-run

# OAuth providers work the same way (authorize once in GUI, reuse from CLI)
aeroftp ls --profile "Google Drive" /
aeroftp get --profile "Dropbox" /Documents/report.pdf
```

Profile matching supports exact names, IDs, and disambiguated substring matching. If a query matches multiple profiles, the CLI reports the candidates and asks for clarification rather than guessing.

### AeroAgent: `server_exec`

The built-in AI assistant uses the same isolation architecture through two dedicated tools:

- **`server_list_saved`**: Returns profile names, protocols, hosts, and paths. Never includes passwords or tokens.
- **`server_exec`**: Accepts a server name and an operation (ls, cat, get, put, mkdir, rm, mv, stat, find, df). The Rust backend resolves credentials from the vault and executes the operation. The AI model sees only the result.

This means a developer can instruct AeroAgent: *"Upload the build folder to the staging server and verify the deployment"* — and the agent completes the entire workflow without any credential material entering the AI context.

### Protocol Coverage

The credential isolation architecture is not limited to a single protocol or service type. It works across all 22 protocols supported by AeroFTP:

**Direct authentication**: FTP, FTPS, SFTP, WebDAV/WebDAVS, S3 (AWS, Backblaze, Wasabi, Cloudflare R2, MinIO, and other compatible services), Azure Blob Storage, MEGA, Filen, Internxt Drive, kDrive, Jottacloud, FileLu, Koofr, OpenDrive, Yandex Disk

**OAuth-authenticated providers**: Google Drive, Dropbox, OneDrive, Box, pCloud, Zoho WorkDrive (authorize once in the GUI; the CLI reuses the stored tokens with automatic refresh)

This breadth is significant because credential proxy services that have emerged for AI agent workflows only support HTTP-based APIs. They cannot authenticate to an FTP server, negotiate an SSH handshake, or manage an OAuth token refresh cycle for a cloud storage provider. AeroFTP handles all of these natively.

---

## Practical Workflows

### Web Development with AI Agent

A developer using Claude Code, Cursor, or a similar AI coding agent can set up a workflow where the agent modifies code locally and deploys to a remote server in a single step:

```bash
# Agent edits files locally, then deploys
aeroftp put --profile "axpdev.it" ./dist/app.js /var/www/app.js
aeroftp put --profile "axpdev.it" ./dist/styles.css /var/www/styles.css

# Agent verifies deployment
aeroftp ls --profile "axpdev.it" /var/www/ -l
```

The developer saved the server once in the AeroFTP GUI. The agent uses it indefinitely without ever seeing the FTP password.

### CI/CD Pipeline

```bash
# The vault master password is the only secret the pipeline needs
AEROFTP_MASTER_PASSWORD=${{ secrets.VAULT_KEY }} \
  aeroftp sync --profile "Production" ./build/ /var/www/ --delete
```

A single secret (the vault master password) unlocks access to all configured servers. Individual server credentials never appear in CI/CD configuration, logs, or environment dumps.

### Multi-Server Operations

```bash
# Batch script: deploy to staging, verify, then deploy to production
aeroftp batch deploy.aeroftp --json
```

```
# deploy.aeroftp
SET ON_ERROR=stop
CONNECT staging-server
PUT ./build/app.js /var/www/app.js
STAT /var/www/app.js
DISCONNECT
CONNECT production-server
PUT ./build/app.js /var/www/app.js
ECHO Deployment complete
DISCONNECT
```

---

## Implementation

AeroFTP is open source under the GPL-3.0 license. The credential isolation architecture is fully auditable:

| Component | File | Purpose |
|-----------|------|---------|
| Encrypted vault | [`src-tauri/src/credential_store.rs`](../src-tauri/src/credential_store.rs) | AES-256-GCM + Argon2id vault with atomic writes |
| CLI `--profile` | [`src-tauri/src/bin/aeroftp_cli.rs`](../src-tauri/src/bin/aeroftp_cli.rs) | Profile resolution and credential loading |
| AeroAgent tools | [`src-tauri/src/ai_tools.rs`](../src-tauri/src/ai_tools.rs) | `server_list_saved` and `server_exec` |
| Provider factory | [`src-tauri/src/cloud_provider_factory.rs`](../src-tauri/src/cloud_provider_factory.rs) | Multi-protocol provider creation |
| OAuth2 manager | [`src-tauri/src/providers/oauth2.rs`](../src-tauri/src/providers/oauth2.rs) | Token storage, refresh, and lifecycle |

The architecture has been reviewed by 10 independent auditors (5 Claude Opus 4.6 + 5 GPT 5.4) with 83+ findings identified and resolved across security, performance, code quality, and OAuth correctness.

---

*AeroFTP — [github.com/axpnet/aeroftp](https://github.com/axpnet/aeroftp) — GPL-3.0*
