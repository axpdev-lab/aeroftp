# AeroFTP CLI — Agent Integration Guide

> This file is for AI coding agents (Claude Code, Cursor, Codex, Devin, OpenClaw).
> It describes how to use AeroFTP CLI for file transfer operations without credentials.

## Quick Start

```bash
# 1. See available servers (no credentials shown)
aeroftp-cli profiles --json

# 2. List files on a server
aeroftp-cli ls --profile "Server Name" /path/ --json

# 3. Upload a file
aeroftp-cli put --profile "Server Name" ./local-file.txt /remote/path/file.txt

# 4. Download a file
aeroftp-cli get --profile "Server Name" /remote/file.txt ./local-file.txt

# 5. Sync a directory
aeroftp-cli sync --profile "Server Name" ./local-dir/ /remote-dir/ --dry-run
```

## How Credentials Work

You do NOT need passwords, tokens, or API keys. The user has saved their servers in an encrypted vault. Use `--profile "Name"` to connect — credentials are resolved internally by the Rust backend and never exposed to your process.

```bash
# WRONG — do not ask the user for passwords
aeroftp-cli ls sftp://user:password@host /path/

# RIGHT — use saved profiles
aeroftp-cli ls --profile "My Server" /path/
```

## Discovery

```bash
# List all saved servers with protocol, host, path
aeroftp-cli profiles --json

# Get full CLI capabilities as structured JSON
aeroftp-cli agent-info --json
```

The `profiles --json` output:
```json
[
  {
    "id": "srv_123",
    "name": "Production",
    "protocol": "sftp",
    "host": "prod.example.com",
    "port": 22,
    "username": "deploy",
    "initialPath": "/var/www"
  }
]
```

## Common Commands

### File Operations

| Command | Usage | Description |
|---------|-------|-------------|
| `ls` | `aeroftp-cli ls --profile NAME /path/ [-l] [--json]` | List directory contents |
| `get` | `aeroftp-cli get --profile NAME /remote/file [./local]` | Download file |
| `put` | `aeroftp-cli put --profile NAME ./local /remote/path` | Upload file |
| `cat` | `aeroftp-cli cat --profile NAME /remote/file` | Print file to stdout |
| `stat` | `aeroftp-cli stat --profile NAME /remote/file [--json]` | File metadata |
| `find` | `aeroftp-cli find --profile NAME /path/ "*.ext" [--json]` | Search files |
| `tree` | `aeroftp-cli tree --profile NAME /path/ [-d depth] [--json]` | Directory tree |

### Modify Operations

| Command | Usage | Description |
|---------|-------|-------------|
| `mkdir` | `aeroftp-cli mkdir --profile NAME /remote/new-dir` | Create directory |
| `rm` | `aeroftp-cli rm --profile NAME /remote/file` | Delete file |
| `rm -rf` | `aeroftp-cli rm --profile NAME /remote/dir/ -rf` | Delete directory recursively |
| `mv` | `aeroftp-cli mv --profile NAME /old/path /new/path` | Move or rename |

### Bulk Operations

| Command | Usage | Description |
|---------|-------|-------------|
| `get -r` | `aeroftp-cli get --profile NAME /remote/dir/ ./local/ -r` | Download directory |
| `put -r` | `aeroftp-cli put --profile NAME ./local/ /remote/dir/ -r` | Upload directory |
| `get glob` | `aeroftp-cli get --profile NAME "/path/*.csv"` | Download matching files |
| `put glob` | `aeroftp-cli put --profile NAME "./*.json" /remote/` | Upload matching files |
| `sync` | `aeroftp-cli sync --profile NAME ./local/ /remote/` | Bidirectional sync |
| `sync --dry-run` | `aeroftp-cli sync --profile NAME ./local/ /remote/ --dry-run` | Preview sync |

### Info Operations

| Command | Usage | Description |
|---------|-------|-------------|
| `connect` | `aeroftp-cli connect --profile NAME` | Test connection |
| `df` | `aeroftp-cli df --profile NAME [--json]` | Storage quota |
| `about` | `aeroftp-cli about --profile NAME [--json]` | Provider/server info with quota when available |
| `profiles` | `aeroftp-cli profiles [--json]` | List saved servers |
| `agent-info` | `aeroftp-cli agent-info --json` | Full capabilities JSON |

## Output Modes

Always use `--json` when parsing output programmatically:

```bash
# Structured JSON — parse with jq or directly
aeroftp-cli ls --profile "Server" /path/ --json

# Plain text — human-readable, for display to user
aeroftp-cli ls --profile "Server" /path/ -l
```

**stdout** contains data only (file listings, file content, JSON).
**stderr** contains status messages, progress bars, warnings.

```bash
# Pipe file content cleanly
aeroftp-cli cat --profile "Server" /remote/config.ini 2>/dev/null

# Parse JSON without noise
aeroftp-cli ls --profile "Server" / --json 2>/dev/null | jq '.entries[].name'
```

## Exit Codes

| Code | Meaning | Agent Action |
|------|---------|-------------|
| 0 | Success | Continue |
| 1 | Connection error | Retry or report to user |
| 2 | Not found | Check path spelling |
| 3 | Permission denied | Report to user |
| 4 | Transfer failed | Retry once, then report |
| 5 | Invalid usage | Fix command syntax |
| 6 | Auth failed | Ask user to re-authorize |
| 7 | Not supported | Use alternative approach |
| 8 | Timeout | Retry with longer timeout |
| 99 | Unknown | Report to user |

## Safety Guidelines

### Safe operations (no confirmation needed)
- `ls`, `cat`, `stat`, `find`, `tree`, `df`, `profiles`, `connect`, `agent-info`

### Operations that modify remote state (inform user before executing)
- `put`, `mkdir`, `mv`, `sync`

### Destructive operations (always confirm with user first)
- `rm`, `rm -rf`, `sync --delete`

### Never do
- Do not ask the user for passwords — use `--profile`
- Do not pass credentials in URLs
- Do not read the vault files directly
- Do not use `--insecure` unless the user explicitly requests it

## Profile Matching

Profiles match by name (case-insensitive). Use exact names to avoid ambiguity:

```bash
# Good — exact name
aeroftp-cli ls --profile "Production Server" /

# Risky — substring match, may be ambiguous
aeroftp-cli ls --profile "prod" /

# Best for scripting — use profile index number
aeroftp-cli ls --profile 1 /
```

## GitHub Integration

AeroFTP treats GitHub repositories as filesystems. Every upload creates a Git commit.

```bash
# Browse repo
aeroftp-cli ls --profile "GitHub/myproject" /src/ -l

# Upload file → creates commit
aeroftp-cli put --profile "GitHub/myproject" ./fix.py /src/fix.py

# Read file
aeroftp-cli cat --profile "GitHub/myproject" /README.md

# Delete → creates commit
aeroftp-cli rm --profile "GitHub/myproject" /old-file.txt
```

For protected branches, AeroFTP auto-creates a working branch and offers PR creation. The token never leaves the vault.

## Common Workflows

### Deploy a website
```bash
aeroftp-cli put --profile "Production" ./dist/index.html /var/www/index.html
aeroftp-cli put --profile "Production" ./dist/app.js /var/www/app.js
aeroftp-cli ls --profile "Production" /var/www/ -l --json
```

### Sync a project folder
```bash
# Preview first
aeroftp-cli sync --profile "Staging" ./build/ /var/www/ --dry-run --json
# Then execute
aeroftp-cli sync --profile "Staging" ./build/ /var/www/
```

### Backup remote files
```bash
aeroftp-cli get --profile "Production" /var/www/database.sql ./backups/
aeroftp-cli get --profile "NAS" /shared/photos/ ./local-backup/ -r
```

### Check server status
```bash
aeroftp-cli connect --profile "Production"
aeroftp-cli df --profile "Production" --json
```

## AeroAgent Orchestration

External AI agents can invoke AeroAgent as a subprocess to perform AI-driven multi-step operations with credential isolation. AeroAgent resolves credentials from the vault, executes tool chains autonomously, and returns results — the orchestrating agent never sees passwords or tokens.

### Discover AI Providers

```bash
# List configured AI providers (from vault and environment)
aeroftp-cli ai-models --json
```

The output includes provider name, active model, and source (`vault`, `env`, or `vault+env`). API keys are never included.

### Agent One-Shot Mode

```bash
# Run a single instruction and exit
aeroftp-cli agent --provider xai --model grok-3-mini \
  -m "List files on the Production server at /var/www/" \
  --auto-approve all -y --json
```

If `--provider` is omitted, the CLI auto-detects from environment variables or vault-stored API keys.

### Server Operations via Agent

The agent has two tools for vault-backed server operations:

**`server_list_saved`** — Lists all 57 saved server profiles (names, protocols, hosts). No credentials exposed.

**`server_exec`** — Executes operations on any saved server. Credentials resolved internally.

| Operation | Description |
|-----------|-------------|
| `ls` | List directory contents |
| `cat` | Read file content (5 KB cap) |
| `stat` | File metadata |
| `find` | Search by pattern |
| `df` | Storage quota |

Example orchestration flow:
```bash
# Step 1: Discover servers
aeroftp-cli agent -p xai -m "List all saved servers" -y --json

# Step 2: Check a specific server
aeroftp-cli agent -p xai -m "List files on axpdev.it at /www.axpdev.it/" -y --json

# Step 3: Verify file integrity
aeroftp-cli agent -p xai -m "Compute SHA-256 of /var/www/app.js" -y --json
```

### Auto-Approval Levels

| Level | Behavior |
|-------|----------|
| `--auto-approve safe` | Read-only tools only (default) |
| `--auto-approve medium` | Read + local writes |
| `--auto-approve high` | Read + writes + uploads |
| `--auto-approve all` or `-y` | Everything including shell and delete |

### Security

- Credentials are never in command arguments, environment, stdout, or AI model context
- Shell denylist blocks 35 dangerous command patterns even with `--auto-approve all`
- Path validation blocks traversal, null bytes, and sensitive paths (`~/.ssh/`, vault files)
- The agent cannot read the vault database directly — path validator blocks it

### Native MCP Server

External MCP clients can now connect directly through AeroFTP without wrapping CLI text output:

```bash
aeroftp-cli agent --mcp
```

Current MCP mode exposes:
- 16 curated remote tools only (no local shell/file tools)
- resources for saved profiles, status, capabilities, and pooled connections
- prompt templates for deploy, backup, sync, and clean workflows
- async stdio transport, connection pooling, request cancellation, rate limiting, and audit logging

Use this mode for Claude Desktop, Cursor, VS Code, or any other MCP client that can spawn a stdio server.

### Coming Soon

- **Mutative server_exec**: `put`, `rm`, `mv`, `mkdir` via CLI agent with headless grant model
- **JSON-RPC orchestration**: `aeroftp-cli agent --orchestrate` for programmatic agent-to-agent integration
- **Cross-server operations**: `server_diff`, `server_sync` between two remote servers
- **Agent session tokens**: Pre-authorized scoped sessions for headless automation

Full orchestration documentation with a verified field test report: **[Agent Orchestration](https://docs.aeroftp.app/features/agent-orchestration)**

## Supported Protocols

Saved profiles cover both direct-auth and browser-authorized providers.

**Direct auth / token auth**: FTP, FTPS, SFTP, WebDAV, WebDAVS, S3, GitHub, MEGA, Filen, Internxt, kDrive, Koofr, Jottacloud, FileLu, OpenDrive, Yandex Disk, Azure Blob

**Browser-authorized or profile-backed API providers**: Google Drive, Dropbox, OneDrive, Box, pCloud, Zoho WorkDrive, 4shared, Drime

`df` and quota fields are provider-dependent. For several object-storage providers, `about` and `df` may omit quota data because the upstream API does not expose `storage_info`.

---

*AeroFTP CLI v3.2.0 — [github.com/axpdev-lab/aeroftp](https://github.com/axpdev-lab/aeroftp)*
