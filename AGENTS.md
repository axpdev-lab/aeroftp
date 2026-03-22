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

## Commands Reference

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

## Supported Protocols

23 protocols, all accessible via `--profile`:

**Direct auth** (work immediately): FTP, FTPS, SFTP, WebDAV, WebDAVS, S3, GitHub, MEGA, Filen, Internxt, kDrive, Koofr, Jottacloud, FileLu, OpenDrive, Yandex Disk, Azure Blob

**OAuth** (browser auth on first use, then automatic): Google Drive, Dropbox, OneDrive, Box, pCloud, Zoho WorkDrive

---

*AeroFTP CLI v2.9.9+ — [github.com/axpnet/aeroftp](https://github.com/axpnet/aeroftp)*
