# AeroFTP CLI — User Guide

> **Version**: 3.0.8
> **Binary**: `aeroftp-cli` (ships alongside the GUI)
> **License**: GPL-3.0

---

## Overview

AeroFTP CLI is a production command-line client for multi-protocol file transfers. It shares the same Rust backend as the AeroFTP desktop app, supporting 23 protocols through a single binary with consistent behavior across all of them.

### Supported Protocols

| Protocol | URL Scheme | Auth Method |
|----------|-----------|-------------|
| FTP | `ftp://` | Password |
| FTPS | `ftps://` | Password + TLS |
| SFTP | `sftp://` | Password / SSH Key |
| WebDAV | `webdav://` / `webdavs://` | Password |
| S3 | `s3://` | Access Key + Secret |
| MEGA.nz | `mega://` | Password (E2E) |
| Azure Blob | `azure://` | HMAC / SAS Token |
| Filen | `filen://` | Password (E2E) |
| Internxt | `internxt://` | Password (E2E) |
| Jottacloud | `jottacloud://` | Bearer Token |
| FileLu | `filelu://` | API Key |
| Koofr | `koofr://` | OAuth2 Token |
| OpenDrive | `opendrive://` | Password |
| GitHub | `github://` | PAT / Device Flow |
| Yandex Disk | `yandexdisk://` | OAuth2 (via `--profile`) |
| Google Drive | — | OAuth2 (via `--profile`) |
| Dropbox | — | OAuth2 (via `--profile`) |
| OneDrive | — | OAuth2 (via `--profile`) |
| Box | — | OAuth2 (via `--profile`) |
| pCloud | — | OAuth2 (via `--profile`) |
| Zoho WorkDrive | — | OAuth2 (via `--profile`) |
| 4shared | — | OAuth1 (via `--profile`) |
| kDrive | — | OAuth2 (via `--profile`) |

> **OAuth providers** do not have URL schemes. Use `--profile` to connect via saved server profiles authorized from the AeroFTP GUI.

---

## Installation

The CLI binary (`aeroftp-cli`) is included in all AeroFTP distribution packages (.deb, .rpm, .AppImage, .snap, .msi, .dmg). After installing AeroFTP, the CLI is available system-wide.

```bash
# Verify installation
aeroftp-cli --version
# aeroftp 3.0.8

aeroftp-cli --help
```

### Building from Source

```bash
git clone https://github.com/axpdev-lab/aeroftp.git
cd aeroftp/src-tauri
cargo build --release --bin aeroftp-cli
# Binary at target/release/aeroftp-cli
```

---

## URL Format

All commands use a URL to specify the server connection:

```
protocol://user:password@host:port/path
```

### Examples

```bash
# SFTP with default port (22)
sftp://user@myserver.com

# FTP on custom port
ftp://admin@files.example.com:2121

# WebDAV over HTTPS
webdavs://user@nextcloud.example.com/remote.php/dav/files/user/

# S3 (access key as user, secret as password)
s3://AKIAIOSFODNN7EXAMPLE:secret@s3.amazonaws.com

# MEGA (email as user)
mega://user@example.com
```

### Password Handling

Passwords can be provided in several ways (in order of preference):

1. **stdin** (most secure): `echo "password" | aeroftp --password-stdin connect sftp://user@host`
2. **Environment variable**: `AEROFTP_TOKEN=mytoken aeroftp connect jottacloud://user@host`
3. **Interactive prompt**: If no password is provided, the CLI prompts on TTY
4. **URL** (least secure): `sftp://user:password@host` — a warning is displayed

> **Security**: stdin passwords are limited to 4 KB. A warning is shown when passwords appear in URLs.

---

## Server Profiles (`--profile`)

The most powerful way to use the CLI. Connect to any saved server from the AeroFTP encrypted vault — **zero credentials exposed** in the command line, shell history, or process list.

### Setup

1. Open the AeroFTP GUI
2. Connect to a server and save it (check "Save this connection")
3. Use `--profile` in the CLI to connect by name

### Usage

```bash
# List all saved profiles
aeroftp-cli profiles

# Connect using a profile name
aeroftp-cli ls --profile "My Server" /path/

# Fuzzy name matching (case-insensitive)
aeroftp-cli ls --profile "aruba" /www/

# Connect by profile index number
aeroftp-cli ls --profile 3 /

# JSON output for scripting
aeroftp-cli profiles --json
```

### Profile Matching

The CLI matches profiles in this order:
1. **Exact name** (case-insensitive)
2. **Exact ID** (internal UUID)
3. **Substring match** — if only one profile matches, it connects. If multiple match, an error lists the candidates

```bash
$ aeroftp ls --profile "SSH" /
Error: Ambiguous profile 'SSH'. Matches: SSH Lumo Cloud, SSH MyCloud HD. Use exact name or index number.
```

### OAuth Providers via Profile

OAuth providers (Google Drive, Dropbox, OneDrive, Box, pCloud, Zoho WorkDrive, Yandex Disk, 4shared) require browser authorization. Authorize once in the AeroFTP GUI, then use the CLI. Note: 4shared uses OAuth 1.0 and works in CLI after completing authorization in the GUI.

```bash
# After authorizing Google Drive in the GUI:
aeroftp-cli ls --profile "My Google Drive" /

# pCloud (long-lived tokens — works immediately)
aeroftp-cli ls --profile "pCloud" /

# Dropbox
aeroftp-cli get --profile "My Dropbox" /Documents/report.pdf
```

### Master Password

If the vault is protected with a master password:

```bash
# Via environment variable (recommended)
AEROFTP_MASTER_PASSWORD=secret aeroftp ls --profile "server" /

# Interactive prompt (hidden input)
aeroftp-cli ls --profile "server" /
# Master password: ********

# Via flag (visible in process list — use env var instead)
aeroftp-cli ls --profile "server" --master-password secret /
```

### AI Agent Integration

The `--profile` flag is designed for AI coding agents (Claude Code, Cursor, Codex, Devin). The agent never sees credentials:

```bash
# Agent runs this — no password anywhere
aeroftp-cli put --profile "Production" ./dist/app.js /var/www/app.js

# Agent can list, upload, download, sync — all credential-free
aeroftp-cli sync --profile "Staging" ./build/ /var/www/ --dry-run
```

---

## Commands

### connect — Test Connection

```bash
aeroftp-cli connect sftp://user@myserver.com
```

Connects to the server, displays server info (type, version, storage quota if available), and disconnects. Useful for verifying credentials and connectivity.

### ls — List Files

```bash
# Basic listing
aeroftp-cli ls sftp://user@host /var/www/

# Long format (permissions, size, date)
aeroftp-cli ls sftp://user@host /var/www/ -l

# Sort by size, reversed
aeroftp-cli ls sftp://user@host / -l --sort size --reverse

# Show hidden files
aeroftp-cli ls sftp://user@host / --all
```

### get — Download Files

```bash
# Download a single file
aeroftp-cli get sftp://user@host /var/www/index.html

# Download to specific local path
aeroftp-cli get sftp://user@host /var/www/index.html ./local-copy.html

# Glob pattern — download all CSV files
aeroftp-cli get sftp://user@host "/data/*.csv"

# Recursive directory download
aeroftp-cli get sftp://user@host /var/www/ ./backup/ -r
```

> **Glob patterns**: Quote the remote path to prevent shell expansion. The CLI expands `*` and `?` patterns server-side.

### put — Upload Files

```bash
# Upload a single file
aeroftp-cli put sftp://user@host ./report.pdf /uploads/

# Glob pattern — upload all JSON files
aeroftp-cli put sftp://user@host "./*.json" /data/

# Recursive upload
aeroftp-cli put sftp://user@host ./project/ /var/www/project/ -r
```

### mkdir — Create Directory

```bash
aeroftp-cli mkdir sftp://user@host /var/www/new-folder
```

### rm — Delete File or Directory

```bash
# Delete a file
aeroftp-cli rm sftp://user@host /var/www/old-file.txt

# Delete a directory recursively
aeroftp-cli rm sftp://user@host /var/www/old-folder/ -rf
```

### mv — Rename / Move

```bash
aeroftp-cli mv sftp://user@host /var/www/old-name.txt /var/www/new-name.txt
```

### cat — Print File Content

```bash
# Print file to stdout
aeroftp-cli cat sftp://user@host /etc/config.ini

# Pipe to grep
aeroftp-cli cat sftp://user@host /etc/config.ini | grep DB_HOST

# Redirect to local file
aeroftp-cli cat sftp://user@host /data/export.csv > local.csv
```

> **Safety**: Files larger than 256 MB are rejected to prevent OOM.

### stat — File Metadata

```bash
aeroftp-cli stat sftp://user@host /var/www/index.html
```

Displays: name, path, type (file/directory), size, permissions, owner, group, modification date.

### find — Search Files

```bash
aeroftp-cli find sftp://user@host /var/www/ "*.php"
```

Searches recursively for files matching the glob pattern. Uses server-side search when available, falls back to BFS traversal.

### df — Storage Quota

```bash
aeroftp-cli df sftp://user@host
```

Displays used/total storage with a visual progress bar. Returns exit code 7 if the protocol doesn't support storage info.

### tree — Directory Tree

```bash
# Full tree
aeroftp-cli tree sftp://user@host /var/www/

# Limit depth
aeroftp-cli tree sftp://user@host /var/www/ -d 2
```

Renders a tree with Unicode connectors (├──, └──) showing the directory hierarchy. Cycle-safe with visited-path tracking.

### head — First N Lines

```bash
# Print first 20 lines (default)
aeroftp-cli head --profile "server" /var/log/app.log

# First 5 lines
aeroftp-cli head sftp://user@host /var/log/app.log -n 5

# JSON output
aeroftp-cli head --profile "server" /path/file.txt -n 3 --json
```

Prints the first N lines of a remote text file. Default: 20 lines. Files larger than 256 MB are rejected. Binary files return exit code 5.

### tail — Last N Lines

```bash
# Print last 20 lines (default)
aeroftp-cli tail --profile "server" /var/log/app.log

# Last 5 lines
aeroftp-cli tail sftp://user@host /var/log/app.log -n 5
```

Similar to `head` but prints the last N lines. Useful for viewing log files.

### touch — Create Empty File

```bash
# Create a new empty file
aeroftp-cli touch --profile "server" /remote/path/newfile.txt

# Verify file already exists
aeroftp-cli touch --profile "server" /remote/path/existing.txt
```

Creates an empty file if it doesn't exist. If the file already exists, confirms it without error (exit code 0).

### hashsum — Compute File Hash

```bash
# SHA-256 hash
aeroftp-cli hashsum --profile "server" sha256 /data/file.bin

# MD5 hash
aeroftp-cli hashsum sftp://user@host md5 /data/file.iso

# BLAKE3 hash
aeroftp-cli hashsum --profile "server" blake3 /path/file.dat

# JSON output
aeroftp-cli hashsum --profile "server" sha256 /file.txt --json
```

Supported algorithms: `md5`, `sha1`, `sha256`, `sha512`, `blake3`. Output format matches standard `sha256sum` format: `<hash>  <path>`.

### check — Verify Local/Remote Match

```bash
# Compare local and remote directories
aeroftp-cli check --profile "server" /local/dir /remote/dir

# Use SHA-256 checksums (slower but more accurate)
aeroftp-cli check --profile "server" /local/ /remote/ --checksum

# One-way: only check files that exist locally
aeroftp-cli check --profile "server" /local/ /remote/ --one-way

# JSON output with details
aeroftp-cli check --profile "server" /local/ /remote/ --json
```

Verifies that a local directory and remote directory are identical. Compares by file size (default) or SHA-256 checksum (`--checksum`). Reports: matches, differences, files missing on either side.

### about — Server Info & Storage Quota

```bash
# Detailed server info with storage quota
aeroftp-cli about --profile "server"

# JSON output
aeroftp-cli about --profile "server" --json
```

Shows provider name, type, server info, and storage quota (used/free/total) in a single command. More detailed than `df` — includes protocol version, server software, and connection parameters alongside quota information.

### dedupe — Find Duplicate Files

```bash
# Scan for duplicate files (report only)
aeroftp-cli dedupe --profile "server" /data --dry-run

# Delete duplicates (keep first occurrence)
aeroftp-cli dedupe --profile "server" /data --mode skip

# JSON output with group details
aeroftp-cli dedupe --profile "server" /data --dry-run --json
```

Finds duplicate files by content hash (SHA-256). Groups files by size first (fast pre-filter), then hashes to confirm. Modes: `skip` (report only), `newest` (keep newest), `oldest` (keep oldest), `largest` (keep largest), `smallest` (keep smallest).

### sync — Synchronize Directories

```bash
# Preview what would be synced
aeroftp-cli sync --profile "server" ./local/ /remote/ --dry-run

# Upload only
aeroftp-cli sync --profile "server" ./local/ /remote/ --direction upload

# Download only
aeroftp-cli sync --profile "server" ./local/ /remote/ --direction download

# Sync with delete (mirror mode)
aeroftp-cli sync sftp://user@host ./local/ /remote/ --delete

# Exclude patterns
aeroftp-cli sync --profile "server" ./local/ /remote/ --exclude "*.tmp" --exclude ".git"

# Safety limit: abort if more than 50 files would be deleted
aeroftp-cli sync --profile "server" ./local/ /remote/ --delete --max-delete 50

# Safety limit: abort if more than 25% of files would be deleted
aeroftp-cli sync --profile "server" ./local/ /remote/ --delete --max-delete 25%

# Detect renamed files to avoid re-upload
aeroftp-cli sync --profile "server" ./local/ /remote/ --delete --track-renames --dry-run
# Without --track-renames: 1 upload + 1 delete
# With --track-renames: 1 server-side rename (no data transfer)

# Time-based bandwidth schedule
aeroftp-cli sync --profile "server" ./local/ /remote/ --bwlimit "08:00,512k 12:00,10M 18:00,512k 23:00,off"

# Simple bandwidth limit (alternative to --limit-rate with schedule syntax)
aeroftp-cli sync --profile "server" ./local/ /remote/ --bwlimit "1M"
```

Sync options: `--direction` (upload/download/both), `--dry-run`, `--delete`, `--exclude`, `--max-delete`, `--backup-dir`, `--backup-suffix`, `--track-renames`, `--bwlimit`.

### batch — Execute Script

```bash
aeroftp-cli batch deploy.aeroftp
```

Executes a `.aeroftp` script file containing a sequence of commands. See [Batch Scripting](#batch-scripting) below.

---

## Global Flags

| Flag | Description |
|------|-------------|
| `--profile <name>` / `-P` | Use a saved server profile from the encrypted vault |
| `--master-password <pw>` | Unlock vault master password (env: `AEROFTP_MASTER_PASSWORD`) |
| `--json` / `--format json` | Machine-readable JSON output |
| `--quiet` / `-q` | Suppress info messages (errors only) |
| `--verbose` / `-v` | Debug output (`-vv` for trace) |
| `--password-stdin` | Read password from stdin pipe |
| `--key <path>` | SSH private key file for SFTP |
| `--key-passphrase <pass>` | Passphrase for encrypted SSH key |
| `--bucket <name>` | S3 bucket name |
| `--region <region>` | S3/Azure region |
| `--container <name>` | Azure container name |
| `--token <token>` | Bearer/API token (env: `AEROFTP_TOKEN`) |
| `--tls <mode>` | FTP TLS mode: `none`, `explicit`, `implicit`, `explicit_if_available` |
| `--insecure` | Skip TLS certificate verification |
| `--trust-host-key` | Trust unknown SSH host keys |
| `--two-factor <code>` | 2FA code for Filen/Internxt (env: `AEROFTP_2FA`) |
| `--limit-rate <speed>` | Speed limit (e.g., `1M`, `500K`) |
| `--bwlimit <schedule>` | Bandwidth schedule (e.g., `"08:00,512k 18:00,off"` or `"1M"`) |
| `--include <pattern>` | Include only files matching glob pattern (repeatable) |
| `--exclude-global <pattern>` | Exclude files matching glob pattern (repeatable) |
| `--include-from <file>` | Read include patterns from file |
| `--exclude-from <file>` | Read exclude patterns from file |
| `--min-size <size>` | Minimum file size filter (e.g., `100k`, `1M`) |
| `--max-size <size>` | Maximum file size filter (e.g., `1G`) |
| `--min-age <duration>` | Skip files newer than duration (e.g., `7d`, `24h`) |
| `--max-age <duration>` | Skip files older than duration (e.g., `30d`) |

---

## JSON Output

All commands support `--json` for structured machine-readable output:

```bash
# JSON directory listing
aeroftp-cli ls sftp://user@host / --json

# JSON file metadata
aeroftp-cli stat sftp://user@host /file.txt --json

# JSON tree
aeroftp-cli tree sftp://user@host / --json
```

### JSON Structure

```json
{
  "status": "ok",
  "entries": [
    {
      "name": "index.html",
      "path": "/var/www/index.html",
      "is_dir": false,
      "size": 4096,
      "permissions": "-rw-r--r--",
      "modified": "2026-03-10 14:30"
    }
  ],
  "summary": {
    "total": 5,
    "dirs": 1,
    "files": 4,
    "total_size": 102400
  }
}
```

Error responses:

```json
{
  "status": "error",
  "error": "Authentication failed",
  "code": 6
}
```

---

## Output Hygiene

The CLI follows Unix conventions for clean pipeline integration:

- **stdout**: Data output only (file listings, file content, JSON)
- **stderr**: Info messages, progress bars, connection status, summaries
- **`--quiet`**: Suppresses all non-error stderr output
- **`NO_COLOR`**: Disables ANSI colors (also `CLICOLOR=0`)
- **`CLICOLOR_FORCE=1`**: Forces colors even when not a TTY

```bash
# Pipe file content without noise
aeroftp-cli cat sftp://user@host /data.csv > output.csv 2>/dev/null

# Parse JSON programmatically
aeroftp-cli ls sftp://user@host / --json 2>/dev/null | jq '.entries[].name'
```

---

## Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Connection / network error |
| 2 | Not found |
| 3 | Permission denied |
| 4 | Transfer failed / partial |
| 5 | Invalid config / usage error |
| 6 | Authentication failed |
| 7 | Not supported |
| 8 | Timeout |
| 99 | Unknown error |

```bash
aeroftp-cli connect sftp://user@host
echo "Exit code: $?"
```

---

## Batch Scripting

Create `.aeroftp` script files for automated workflows:

### Script Format

```
# Comment lines start with #
SET SERVER=sftp://deploy@prod.example.com

CONNECT ${SERVER}
LS /var/www/ -l
PUT ./dist/index.html /var/www/index.html
PUT ./dist/app.js /var/www/app.js
ECHO Deployment complete
DISCONNECT
```

### Commands

| Command | Description |
|---------|-------------|
| `SET KEY=VALUE` | Define a variable |
| `CONNECT <url>` | Connect to server |
| `DISCONNECT` | Disconnect from server |
| `LS <path> [flags]` | List directory |
| `GET <remote> [local]` | Download file |
| `PUT <local> [remote]` | Upload file |
| `MKDIR <path>` | Create directory |
| `RM <path>` | Delete file |
| `MV <from> <to>` | Move/rename |
| `CAT <path>` | Print file |
| `STAT <path>` | File info |
| `FIND <path> <pattern>` | Search files |
| `TREE <path> [flags]` | Directory tree |
| `DF` | Storage quota |
| `SYNC <local> <remote>` | Synchronize directories |
| `ECHO <message>` | Print message |
| `ON_ERROR CONTINUE\|FAIL` | Set error handling policy |

### Variable Substitution

```
SET ENV=production
SET VERSION=2.9.1
ECHO Deploying ${ENV} v${VERSION}
# Use $$ to produce a literal $
ECHO Price: $$$VERSION  # → Price: $2.9.1
```

### Error Handling

```
# Continue on error (default: FAIL)
ON_ERROR CONTINUE

# Stop on first error
ON_ERROR FAIL
```

### Running Batch Scripts

```bash
aeroftp-cli batch deploy.aeroftp
aeroftp-cli batch deploy.aeroftp --json    # JSON output for all commands
aeroftp-cli batch deploy.aeroftp --quiet   # Errors only
```

---

## Protocol-Specific Notes

### GitHub

```bash
# Browse repository as filesystem
aeroftp-cli ls github://token:YOUR_PAT@owner/repo /src/ -l

# Specific branch
aeroftp-cli ls github://token:YOUR_PAT@owner/repo@develop /

# Upload file → creates Git commit
aeroftp-cli put github://token:YOUR_PAT@owner/repo ./fix.py /src/fix.py

# Read file to stdout
aeroftp-cli cat github://token:YOUR_PAT@owner/repo /README.md

# Delete file → creates Git commit
aeroftp-cli rm github://token:YOUR_PAT@owner/repo /old-file.txt

# Using saved profile (recommended — no token exposed)
aeroftp-cli ls --profile "My GitHub Repo" /src/ -l
aeroftp-cli put --profile "My GitHub Repo" ./app.js /dist/app.js

# Connection info (shows branch, write mode, rate limit)
aeroftp-cli connect --profile "My GitHub Repo"
```

Every upload and delete creates a real Git commit. For protected branches, AeroFTP automatically creates a working branch (`aeroftp/{user}/{base}`) and offers PR creation.

Generate a Fine-grained PAT at: https://github.com/settings/personal-access-tokens/new
Required permissions: `Contents: Read and write`, `Metadata: Read`.

### SFTP

```bash
# Password authentication
aeroftp-cli connect sftp://user@host

# SSH key authentication
aeroftp-cli connect sftp://user@host --key ~/.ssh/id_ed25519

# Non-standard port
aeroftp-cli connect sftp://user@host:2222

# Trust unknown host keys (first connection)
aeroftp-cli connect sftp://user@host --trust-host-key
```

### FTP / FTPS

```bash
# Plain FTP
aeroftp-cli connect ftp://user@host

# Explicit TLS (recommended)
aeroftp-cli connect ftp://user@host --tls explicit

# Implicit TLS (port 990)
aeroftp-cli connect ftps://user@host

# Skip certificate verification (self-signed)
aeroftp-cli connect ftp://user@host --tls explicit --insecure
```

### S3

```bash
# Backblaze B2
aeroftp-cli ls s3://keyId:appKey@s3.eu-central-003.backblazeb2.com \
  --bucket my-bucket --region eu-central-003 /

# AWS S3
aeroftp-cli ls s3://AKID:SECRET@s3.amazonaws.com \
  --bucket my-bucket --region us-east-1 /
```

### WebDAV

```bash
# HTTPS (webdavs://)
aeroftp-cli ls webdavs://user@nextcloud.example.com/remote.php/dav/files/user/ /

# HTTP (webdav://)
aeroftp-cli ls webdav://user@webdav.example.com /
```

### Token-Based Providers

```bash
# Jottacloud (token via env)
AEROFTP_TOKEN=mytoken aeroftp ls jottacloud://user@jottacloud.com /

# FileLu (API key as token)
aeroftp-cli ls filelu://user@filelu.com --token my-api-key /

# 2FA (Filen)
aeroftp-cli connect filen://user@filen.io --two-factor 123456
```

---

## Security

The CLI implements the same security standards as the GUI:

- **Path traversal prevention**: All remote paths validated against `..` components and null bytes
- **Password protection**: stdin limit (4 KB), URL password warnings, env var hiding (`hide_env_values`)
- **ANSI sanitization**: Filenames from servers are stripped of ANSI escape sequences and control characters
- **OOM protection**: `cat` limited to 256 MB, `tree`/`find` limited to 500,000 entries
- **BFS cycle detection**: `tree` and `find` use visited-path tracking to prevent infinite loops
- **Output hygiene**: Data on stdout, messages on stderr — safe for piping
- **NO_COLOR compliance**: Respects `NO_COLOR`, `CLICOLOR`, `CLICOLOR_FORCE` environment variables

---

## Examples

### Automated Deployment

```bash
#!/bin/bash
set -e

SERVER="sftp://deploy@prod.example.com"
echo "$DEPLOY_PASSWORD" | aeroftp --password-stdin put $SERVER \
  ./dist/app.js /var/www/app.js

echo "$DEPLOY_PASSWORD" | aeroftp --password-stdin put $SERVER \
  ./dist/index.html /var/www/index.html
```

### Backup Script

```bash
#!/bin/bash
DATE=$(date +%Y%m%d)
aeroftp-cli get sftp://backup@nas:2222 /data/database.sql \
  "./backups/db-${DATE}.sql" --key ~/.ssh/backup_key
```

### CI/CD Integration

```yaml
# GitHub Actions example
- name: Deploy to server
  env:
    DEPLOY_PASS: ${{ secrets.DEPLOY_PASSWORD }}
  run: |
    echo "$DEPLOY_PASS" | aeroftp --password-stdin put \
      sftp://deploy@prod.example.com ./dist/ /var/www/ -r
```

### Monitoring with JSON

```bash
# Check storage quota and alert
USAGE=$(aeroftp df sftp://user@host --json 2>/dev/null | jq '.used_pct')
if (( $(echo "$USAGE > 90" | bc -l) )); then
  echo "WARNING: Storage at ${USAGE}%"
fi
```

### Batch Deployment

```
# deploy.aeroftp
SET SERVER=sftp://deploy@prod.example.com:2222
ON_ERROR FAIL

CONNECT ${SERVER}
ECHO Starting deployment...

# Upload new build
PUT ./dist/app.js /var/www/app.js
PUT ./dist/styles.css /var/www/styles.css
PUT ./dist/index.html /var/www/index.html

# Verify
STAT /var/www/index.html
ECHO Deployment successful!
DISCONNECT
```

---

## Troubleshooting

### Connection Issues

```bash
# Verbose output for debugging
aeroftp-cli connect sftp://user@host -vv

# Test with --insecure for certificate issues
aeroftp-cli connect ftp://user@host --tls explicit --insecure
```

### FTP Passive Mode

If FTP downloads hang, the server may have passive mode issues. Try SFTP or WebDAV instead.

### Large File Transfers

Use `--limit-rate` for a fixed cap, or `--bwlimit` for time-based scheduling:

```bash
# Fixed speed cap
aeroftp-cli get sftp://user@host /large-file.iso --limit-rate 5M

# Scheduled bandwidth: slow during business hours, unlimited at night
aeroftp-cli get sftp://user@host /large-file.iso --bwlimit "08:00,512k 18:00,off"
```

### Encoding Issues

The CLI sanitizes filenames with ANSI escape sequences. If filenames appear truncated, the server is sending control characters in directory listings.

---

## Live Test Results (v3.0.8)

The following providers have been tested live via CLI with `--profile`:

| Provider | Protocol | connect | ls | put/get | head/tail | hashsum | check | about | dedupe | track-renames | bwlimit | touch | tree | df |
|----------|----------|---------|----|---------|-----------|---------||-------|--------|---------------|---------|-------|------|------|
| WD MyCloud NAS | SFTP | PASS | PASS | PASS | PASS | PASS | PASS | PASS | PASS | PASS | PASS | PASS | PASS | PASS |
| axpdev.it | FTP | PASS | PASS | — | PASS | PASS | — | PASS | — | — | PASS | — | — | — |
| Playground | GitHub | PASS | PASS | PASS | PASS | PASS | — | PASS | — | — | — | PASS | PASS | — |
| MEGA.nz | MEGA | PASS | PASS | — | — | — | — | PASS | — | — | — | — | — | — |
| OpenDrive | OpenDrive | PASS | PASS | — | — | — | — | PASS | — | — | — | — | — | PASS |
| Filen | Filen (E2E) | PASS | PASS | — | — | — | — | PASS | — | — | — | — | — | PASS |
| Koofr | WebDAV | PASS | PASS | — | — | — | — | PASS | — | — | — | — | — | — |
| Koofr | Native API | PASS | PASS | — | — | — | — | PASS | — | — | — | — | — | PASS |
| WD MyCloud NAS | WebDAV | PASS | PASS | — | — | — | — | PASS | — | — | — | — | — | — |
| Backblaze B2 | S3 | PASS | PASS | — | — | — | — | PASS | — | — | — | — | — | — |
| Azure | Azure Blob | PASS | PASS | — | — | — | — | PASS | — | — | — | — | — | — |
| 4shared | OAuth 1.0 | PASS | PASS | — | — | — | — | PASS | — | — | — | — | — | PASS |

**12 providers tested**, all core operations verified. `about` tested on all 12 providers. `dedupe`, `track-renames`, and `bwlimit` tested on SFTP.

---

*AeroFTP CLI is part of the [AeroFTP](https://github.com/axpdev-lab/aeroftp) project — GPL-3.0*
