# AeroFTP CLI — Agent Integration Guide

> This file is for AI coding agents (Claude Code, Cursor, Codex, Devin, OpenClaw).
> It describes how to use AeroFTP CLI for file transfer operations without credentials.

## Quick Start

```bash
# 1. See available servers (no credentials shown)
aeroftp profiles --json

# 2. List files on a server
aeroftp ls --profile "Server Name" /path/ --json

# 3. Upload a file
aeroftp put --profile "Server Name" ./local-file.txt /remote/path/file.txt

# 4. Download a file
aeroftp get --profile "Server Name" /remote/file.txt ./local-file.txt

# 5. Sync a directory
aeroftp sync --profile "Server Name" ./local-dir/ /remote-dir/ --dry-run
```

## How Credentials Work

You do NOT need passwords, tokens, or API keys. The user has saved their servers in an encrypted vault. Use `--profile "Name"` to connect — credentials are resolved internally by the Rust backend and never exposed to your process.

```bash
# WRONG — do not ask the user for passwords
aeroftp ls sftp://user:password@host /path/

# RIGHT — use saved profiles
aeroftp ls --profile "My Server" /path/
```

## Discovery

```bash
# List all saved servers with protocol, host, path
aeroftp profiles --json

# Get full CLI capabilities as structured JSON
aeroftp agent-info --json
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
| `ls` | `aeroftp ls --profile NAME /path/ [-l] [--json]` | List directory contents |
| `get` | `aeroftp get --profile NAME /remote/file [./local]` | Download file |
| `put` | `aeroftp put --profile NAME ./local /remote/path` | Upload file |
| `cat` | `aeroftp cat --profile NAME /remote/file` | Print file to stdout |
| `stat` | `aeroftp stat --profile NAME /remote/file [--json]` | File metadata |
| `find` | `aeroftp find --profile NAME /path/ "*.ext" [--json]` | Search files |
| `tree` | `aeroftp tree --profile NAME /path/ [-d depth] [--json]` | Directory tree |

### Modify Operations

| Command | Usage | Description |
|---------|-------|-------------|
| `mkdir` | `aeroftp mkdir --profile NAME /remote/new-dir` | Create directory |
| `rm` | `aeroftp rm --profile NAME /remote/file` | Delete file |
| `rm -rf` | `aeroftp rm --profile NAME /remote/dir/ -rf` | Delete directory recursively |
| `mv` | `aeroftp mv --profile NAME /old/path /new/path` | Move or rename |

### Bulk Operations

| Command | Usage | Description |
|---------|-------|-------------|
| `get -r` | `aeroftp get --profile NAME /remote/dir/ ./local/ -r` | Download directory |
| `put -r` | `aeroftp put --profile NAME ./local/ /remote/dir/ -r` | Upload directory |
| `get glob` | `aeroftp get --profile NAME "/path/*.csv"` | Download matching files |
| `put glob` | `aeroftp put --profile NAME "./*.json" /remote/` | Upload matching files |
| `sync` | `aeroftp sync --profile NAME ./local/ /remote/` | Bidirectional sync |
| `sync --dry-run` | `aeroftp sync --profile NAME ./local/ /remote/ --dry-run` | Preview sync |

### Info Operations

| Command | Usage | Description |
|---------|-------|-------------|
| `connect` | `aeroftp connect --profile NAME` | Test connection |
| `df` | `aeroftp df --profile NAME [--json]` | Storage quota |
| `profiles` | `aeroftp profiles [--json]` | List saved servers |
| `agent-info` | `aeroftp agent-info --json` | Full capabilities JSON |

## Output Modes

Always use `--json` when parsing output programmatically:

```bash
# Structured JSON — parse with jq or directly
aeroftp ls --profile "Server" /path/ --json

# Plain text — human-readable, for display to user
aeroftp ls --profile "Server" /path/ -l
```

**stdout** contains data only (file listings, file content, JSON).
**stderr** contains status messages, progress bars, warnings.

```bash
# Pipe file content cleanly
aeroftp cat --profile "Server" /remote/config.ini 2>/dev/null

# Parse JSON without noise
aeroftp ls --profile "Server" / --json 2>/dev/null | jq '.entries[].name'
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
aeroftp ls --profile "Production Server" /

# Risky — substring match, may be ambiguous
aeroftp ls --profile "prod" /

# Best for scripting — use profile index number
aeroftp ls --profile 1 /
```

## GitHub Integration

AeroFTP treats GitHub repositories as filesystems. Every upload creates a Git commit.

```bash
# Browse repo
aeroftp ls --profile "GitHub/myproject" /src/ -l

# Upload file → creates commit
aeroftp put --profile "GitHub/myproject" ./fix.py /src/fix.py

# Read file
aeroftp cat --profile "GitHub/myproject" /README.md

# Delete → creates commit
aeroftp rm --profile "GitHub/myproject" /old-file.txt
```

For protected branches, AeroFTP auto-creates a working branch and offers PR creation. The token never leaves the vault.

## Common Workflows

### Deploy a website
```bash
aeroftp put --profile "Production" ./dist/index.html /var/www/index.html
aeroftp put --profile "Production" ./dist/app.js /var/www/app.js
aeroftp ls --profile "Production" /var/www/ -l --json
```

### Sync a project folder
```bash
# Preview first
aeroftp sync --profile "Staging" ./build/ /var/www/ --dry-run --json
# Then execute
aeroftp sync --profile "Staging" ./build/ /var/www/
```

### Backup remote files
```bash
aeroftp get --profile "Production" /var/www/database.sql ./backups/
aeroftp get --profile "NAS" /shared/photos/ ./local-backup/ -r
```

### Check server status
```bash
aeroftp connect --profile "Production"
aeroftp df --profile "Production" --json
```

## Supported Protocols

23 protocols, all accessible via `--profile`:

**Direct auth** (work immediately): FTP, FTPS, SFTP, WebDAV, WebDAVS, S3, GitHub, MEGA, Filen, Internxt, kDrive, Koofr, Jottacloud, FileLu, OpenDrive, Yandex Disk, Azure Blob

**OAuth** (browser auth on first use, then automatic): Google Drive, Dropbox, OneDrive, Box, pCloud, Zoho WorkDrive

---

*AeroFTP CLI v2.9.9+ — [github.com/axpnet/aeroftp](https://github.com/axpnet/aeroftp)*
