# AeroFTP CLI - User Guide

> **Binary**: `aeroftp-cli` (ships alongside the GUI)
> **Version reference**: v3.7.2 (May 2026) - last reviewed 6 May 2026
> **License**: GPL-3.0

---

## Overview

AeroFTP CLI is a production command-line client for multi-protocol file transfers. It shares the same Rust backend as the AeroFTP desktop app, with direct URL support for core protocols and `--profile` access for saved GUI-authorized providers. Beyond basic transfer commands, the CLI also covers cross-profile copy planning and execution, continuous bidirectional sync (`sync --watch`), reconcile/sync-doctor preflights for agents, stdin upload, remote copy/share/edit flows, batch scripting, shell completions, aliases, encrypted overlays (`crypt`), local server bridges (`serve http/webdav/ftp/sftp`), MCP server mode for the official VS Code extension, and AI agent discovery/orchestration.

### Direct URL Protocols

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

### Profile-Backed Providers

Use `--profile` for providers authorized or configured in the GUI vault. This includes Google Drive, Dropbox, OneDrive, Box, pCloud, Zoho WorkDrive, Yandex Disk, 4shared, and Drime, and it also works for direct-auth providers when you prefer vault-backed credentials.

---

## Installation

The CLI binary (`aeroftp-cli`) is included in all AeroFTP distribution packages (.deb, .rpm, .AppImage, .snap, .msi, .dmg). After installing AeroFTP, the CLI is available system-wide.

```bash
# Verify installation
aeroftp-cli --version
# aeroftp X.Y.Z

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
2. **Environment variable**: `AEROFTP_TOKEN=mytoken aeroftp-cli connect jottacloud://user@host`
3. **Interactive prompt**: If no password is provided, the CLI prompts on TTY
4. **URL** (least secure): `sftp://user:password@host` - a warning is displayed

> **Security**: stdin passwords are limited to 4 KB. A warning is shown when passwords appear in URLs.

---

## Server Profiles (`--profile`)

The most powerful way to use the CLI. Connect to any saved server from the AeroFTP encrypted vault - **zero credentials exposed** in the command line, shell history, or process list.

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
3. **Substring match** - if only one profile matches, it connects. If multiple match, an error lists the candidates

```bash
$ aeroftp-cli ls --profile "SSH" /
Error: Ambiguous profile 'SSH'. Matches: SSH Lumo Cloud, SSH MyCloud HD. Use exact name or index number.
```

### OAuth Providers via Profile

Browser-authorized and profile-backed API providers (Google Drive, Dropbox, OneDrive, Box, pCloud, Zoho WorkDrive, Yandex Disk, 4shared, Drime) are best used through saved profiles. Authorize or configure them once in the AeroFTP GUI, then reuse them from the CLI. Note: 4shared uses OAuth 1.0 and works in CLI after completing authorization in the GUI.

```bash
# After authorizing Google Drive in the GUI:
aeroftp-cli ls --profile "My Google Drive" /

# pCloud (long-lived tokens - works immediately)
aeroftp-cli ls --profile "pCloud" /

# Dropbox
aeroftp-cli get --profile "My Dropbox" /Documents/report.pdf
```

### Master Password

If the vault is protected with a master password:

```bash
# Via environment variable (recommended)
AEROFTP_MASTER_PASSWORD=secret aeroftp-cli ls --profile "server" /

# Interactive prompt (hidden input)
aeroftp-cli ls --profile "server" /
# Master password: ********

# Via flag (visible in process list - use env var instead)
aeroftp-cli ls --profile "server" --master-password secret /
```

### AI Agent Integration

The `--profile` flag is designed for AI coding agents (Claude Code, Cursor, Codex, Devin). The agent never sees credentials:

```bash
# Agent runs this - no password anywhere
aeroftp-cli put --profile "Production" ./dist/app.js /var/www/app.js

# Agent can list, upload, download, sync - all credential-free
aeroftp-cli sync --profile "Staging" ./build/ /var/www/ --dry-run
```

---

## Commands

### connect - Test Connection

```bash
aeroftp-cli connect sftp://user@myserver.com
```

Connects to the server, displays server info (type, version, storage quota if available), and disconnects. Useful for verifying credentials and connectivity.

### ls - List Files

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

### get - Download Files

```bash
# Download a single file
aeroftp-cli get sftp://user@host /var/www/index.html

# Download to specific local path
aeroftp-cli get sftp://user@host /var/www/index.html ./local-copy.html

# Glob pattern - download all CSV files
aeroftp-cli get sftp://user@host "/data/*.csv"

# Recursive directory download
aeroftp-cli get sftp://user@host /var/www/ ./backup/ -r
```

> **Glob patterns**: Quote the remote path to prevent shell expansion. The CLI expands `*` and `?` patterns server-side.

### pget - Segmented Parallel Download

```bash
# Default: 4 parallel segments
aeroftp-cli pget --profile "AWS S3" /backup/big.tar.gz ./big.tar.gz

# Tune segment count (range 2-16)
aeroftp-cli pget --profile "AWS S3" /backup/big.tar.gz --segments 8

# JSON progress output
aeroftp-cli pget --profile "AWS S3" /backup/big.tar.gz --json
```

Alias of `get` with a parallel-segments preset. Splits a single file into N byte ranges and downloads them concurrently, then stitches them back together. Useful when latency or per-connection throughput is the bottleneck (large `.tar.gz` archives, S3 buckets from far regions). Falls back to a single sequential stream when the provider does not advertise range-request support.

### put - Upload Files

```bash
# Upload a single file
aeroftp-cli put sftp://user@host ./report.pdf /uploads/

# Glob pattern - upload all JSON files
aeroftp-cli put sftp://user@host "./*.json" /data/

# Recursive upload
aeroftp-cli put sftp://user@host ./project/ /var/www/project/ -r
```

### mkdir - Create Directory

```bash
aeroftp-cli mkdir sftp://user@host /var/www/new-folder
```

### rm - Delete File or Directory

```bash
# Delete a file
aeroftp-cli rm sftp://user@host /var/www/old-file.txt

# Delete a directory recursively
aeroftp-cli rm sftp://user@host /var/www/old-folder/ -rf
```

### mv - Rename / Move

```bash
aeroftp-cli mv sftp://user@host /var/www/old-name.txt /var/www/new-name.txt
```

### cp - Server-Side Copy

```bash
aeroftp-cli cp --profile "server" /var/www/app.js /var/www/app.backup.js
```

Copies a remote file or object on the server side when the provider supports it. Returns exit code 7 when server-side copy is unavailable for that provider.

### link - Create Share Link

```bash
aeroftp-cli link --profile "server" /public/report.pdf
```

Creates a share link for a remote file when the provider supports share URLs. The returned link is printed to stdout or emitted as JSON with `--json`.

### edit - Remote Find/Replace

```bash
# Replace all occurrences
aeroftp-cli edit --profile "server" /var/www/index.html "Old Title" "New Title"

# Replace only the first occurrence
aeroftp-cli edit --profile "server" /var/www/index.html "Old Title" "New Title" --first
```

This is a scripted remote text edit flow, not an interactive `$EDITOR` session. The CLI downloads the remote UTF-8 file, applies a deterministic find/replace, then uploads the modified content.

### cat - Print File Content

```bash
# Print file to stdout
aeroftp-cli cat sftp://user@host /etc/config.ini

# Pipe to grep
aeroftp-cli cat sftp://user@host /etc/config.ini | grep DB_HOST

# Redirect to local file
aeroftp-cli cat sftp://user@host /data/export.csv > local.csv
```

> **Safety**: Files larger than 256 MB are rejected to prevent OOM.

### stat - File Metadata

```bash
aeroftp-cli stat sftp://user@host /var/www/index.html
```

Displays: name, path, type (file/directory), size, permissions, owner, group, modification date.

### find - Search Files

```bash
aeroftp-cli find sftp://user@host /var/www/ "*.php"
```

Searches recursively for files matching the glob pattern. Uses server-side search when available, falls back to BFS traversal.

### df - Storage Quota

```bash
aeroftp-cli df sftp://user@host
```

Displays used/total storage with a visual progress bar. Returns exit code 7 if the protocol doesn't support storage info.

### tree - Directory Tree

```bash
# Full tree
aeroftp-cli tree sftp://user@host /var/www/

# Limit depth
aeroftp-cli tree sftp://user@host /var/www/ -d 2
```

Renders a tree with Unicode connectors (├──, └──) showing the directory hierarchy. Cycle-safe with visited-path tracking.

### head - First N Lines

```bash
# Print first 20 lines (default)
aeroftp-cli head --profile "server" /var/log/app.log

# First 5 lines
aeroftp-cli head sftp://user@host /var/log/app.log -n 5

# JSON output
aeroftp-cli head --profile "server" /path/file.txt -n 3 --json
```

Prints the first N lines of a remote text file. Default: 20 lines. Files larger than 256 MB are rejected. Binary files return exit code 5.

### tail - Last N Lines

```bash
# Print last 20 lines (default)
aeroftp-cli tail --profile "server" /var/log/app.log

# Last 5 lines
aeroftp-cli tail sftp://user@host /var/log/app.log -n 5
```

Similar to `head` but prints the last N lines. Useful for viewing log files.

### touch - Create Empty File

```bash
# Create a new empty file
aeroftp-cli touch --profile "server" /remote/path/newfile.txt

# Verify file already exists
aeroftp-cli touch --profile "server" /remote/path/existing.txt
```

Creates an empty file if it doesn't exist. If the file already exists, confirms it without error (exit code 0).

### hashsum - Compute File Hash

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

### check - Verify Local/Remote Match

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

### cryptcheck - Verify Crypt/Cleartext Integrity

```bash
# Compare a local cleartext directory against a remote encrypted directory
aeroftp-cli cryptcheck --profile "server" /local/cleartext /remote/encrypted

# Use MD5 instead of SHA-256
aeroftp-cli cryptcheck --profile "server" /local/ /remote/ -a md5

# Password via flag (not recommended, use AEROFTP_RCLONE_CRYPT_PASSWORD)
aeroftp-cli cryptcheck --profile "server" /local/ /remote/ --password "secret"

# JSON output with details
aeroftp-cli cryptcheck --profile "server" /local/ /remote/ --json
```

Verifies the integrity of files stored on a remote encrypted with `rclone crypt`. Stream-decrypts the remote files (without saving to disk) and computes their hash to compare against local cleartext files. Supports `sha256` and `md5`. Reports: matches, differences, files missing on either side. Exit codes: `0` (success), `4` (differences found), `5` (invalid usage).

### about - Server Info & Storage Quota

```bash
# Detailed server info with storage quota
aeroftp-cli about --profile "server"

# JSON output
aeroftp-cli about --profile "server" --json
```

Shows provider name, type, server info, and storage quota (used/free/total) when available. More detailed than `df` - includes protocol version, server software, and connection parameters alongside quota information. Some object-storage providers do not expose quota via the upstream API, so `about` and `df` may return provider info without quota fields.

### dedupe - Find Duplicate Files

```bash
# Scan for duplicate files (report only)
aeroftp-cli dedupe --profile "server" /data --dry-run

# Delete duplicates (keep first occurrence)
aeroftp-cli dedupe --profile "server" /data --mode skip

# JSON output with group details
aeroftp-cli dedupe --profile "server" /data --dry-run --json
```

Finds duplicate files by content hash (SHA-256). Groups files by size first (fast pre-filter), then hashes to confirm. Modes: `skip` (report only), `newest` (keep newest), `oldest` (keep oldest), `largest` (keep largest), `smallest` (keep smallest), `rename` (rename duplicates with numeric suffix), `interactive` (prompt per group), `list` (list without action).

### cleanup - Remove Orphaned Temp Files

```bash
# Dry-run: list orphaned .aerotmp files without deleting
aeroftp-cli cleanup --profile "server" /remote/path/ --json

# Delete orphaned temp files
aeroftp-cli cleanup --profile "server" /remote/path/ --force
```

Scans a remote directory recursively for `.aerotmp` files left behind by interrupted downloads. Dry-run by default. Use `--force` to delete. JSON output includes `orphans`, `bytes`, and `bytes_freed`.

### sync - Synchronize Directories

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

Sync options: `--direction` (upload/download/both), `--dry-run`, `--delete`, `--exclude`, `--max-delete`, `--backup-dir`, `--backup-suffix`, `--track-renames`, `--bwlimit`, `--conflict-mode`, `--resync`.

#### Bisync (bidirectional)

```bash
# Bidirectional sync (default --direction both)
aeroftp-cli sync --profile "server" ./local/ /remote/

# Conflict resolution: newer file wins (default)
aeroftp-cli sync --profile "server" ./local/ /remote/ --conflict-mode newer

# Other modes: older, larger, smaller, skip
aeroftp-cli sync --profile "server" ./local/ /remote/ --conflict-mode skip --dry-run

# Rename mode: keep both versions (local uploaded as .conflict-{timestamp})
aeroftp-cli sync --profile "server" ./local/ /remote/ --conflict-mode rename

# Force full resync (ignore previous snapshot)
aeroftp-cli sync --profile "server" ./local/ /remote/ --resync

# Backup overwritten files before delete
aeroftp-cli sync --profile "server" ./local/ /remote/ --delete --backup-dir /tmp/bak
```

Bisync saves a `.aeroftp-bisync.json` snapshot after each successful sync. This enables delta detection: files deleted on one side are propagated to the other with `--delete`.

#### Continuous Sync (Watch Mode)

Watch a local directory for changes and re-sync automatically. Runs in the foreground, stopped with Ctrl+C.

```bash
# Watch and sync on changes (default: native watcher, 15s cooldown, 5min rescan)
aeroftp-cli sync --profile "server" ./local/ /remote/ --watch

# Upload-only watch
aeroftp-cli sync --profile "server" ./local/ /remote/ --direction upload --watch

# Poll mode for network filesystems (NFS, SMB)
aeroftp-cli sync sftp://user@host ./local/ /remote/ --watch --watch-mode poll

# Custom cooldown and rescan interval
aeroftp-cli sync --profile "server" ./local/ /remote/ --watch --watch-cooldown 30 --watch-rescan 600

# Skip initial sync, only react to changes
aeroftp-cli sync --profile "server" ./local/ /remote/ --watch --watch-no-initial

# NDJSON output for monitoring pipelines
aeroftp-cli sync --profile "server" ./local/ /remote/ --watch --json
```

Watch options:

| Flag | Default | Description |
|------|---------|-------------|
| `--watch` | false | Enable continuous sync |
| `--watch-mode` | auto | Watcher backend: `auto`, `native`, `poll` |
| `--watch-debounce-ms` | 1500 | Debounce window for filesystem events |
| `--watch-cooldown` | 15 | Minimum seconds between consecutive syncs |
| `--watch-rescan` | 300 | Full rescan interval in seconds (0 to disable) |
| `--watch-no-initial` | false | Skip initial full sync on startup |

Output per cycle (text mode on stderr):
```
[14:32:01] Sync #1 (initial) -- 12 up, 0 down, 0 del (3.2s)
[14:35:18] Sync #2 (watcher: 2 paths) -- 2 up, 1 down, 0 del (0.8s)
[14:37:01] Sync #3 (rescan) -- no changes (0.2s)
```

With `--json`, each cycle emits one NDJSON object on stdout with `cycle`, `trigger`, `uploaded`, `downloaded`, `deleted`, `skipped`, `errors`, `elapsed_secs`, and `timestamp` fields.

Anti-loop protection: events are suppressed during active syncs and within the cooldown window. Editor temp files (`.swp`, `.tmp`, `~`, `.aerotmp`) are automatically filtered.

### reconcile - Categorized Local/Remote Diff

```bash
# Detailed diff with all 4 categories (matches / differs / missing-remote / missing-local)
aeroftp-cli reconcile --profile "server" ./local /remote --json > diff.json

# Summary mode (counts only, faster for large trees)
aeroftp-cli reconcile --profile "server" ./local /remote --reconcile-format summary

# Feed the diff into a sync command without re-scanning
aeroftp-cli sync --profile "server" ./local /remote --from-reconcile diff.json --direction upload
```

`reconcile` returns a structured diff in 4 buckets, designed for AI agents and CI pipelines that need to *plan* a sync before executing it. Pair with `sync --from-reconcile` to skip the rescan.

### sync-doctor - Pre-Sync Preflight Checks

```bash
# Preflight: risks + suggested next command
aeroftp-cli sync-doctor --profile "server" ./local /remote --json
```

Emits a JSON report with planned upload/download/delete counts, bandwidth estimate, top-level diff buckets, and a `next_command` field with the exact `aeroftp-cli sync ...` invocation that matches the preflight. **Recommended discovery surface for AI coding agents** before they execute mutating sync.

### transfer - Cross-Profile Transfer

Copy files directly between two saved profiles without exposing credentials in the shell.

```bash
# Preview plan, checks, and risks
aeroftp-cli transfer-doctor "FTP Aruba" "AWS S3" /www.site.it /backup/site --json

# Execute a recursive copy
aeroftp-cli transfer "FTP Aruba" "AWS S3" /www.site.it /backup/site --recursive

# Skip files that already exist on destination
aeroftp-cli transfer "Cloudflare R2" "Wasabi" /logs /archive/logs --recursive --skip-existing
```

`transfer` uses two vault-backed profiles, one for the source and one for the destination.

### transfer-doctor - Cross-Profile Preflight

```bash
# JSON preflight before transfer (recommended for automation/agents)
aeroftp-cli transfer-doctor "FTP Aruba" "AWS S3" /www.site.it /backup/site --json

# Recursive preflight
aeroftp-cli transfer-doctor "Cloudflare R2" "Wasabi" /logs /archive/logs --recursive --json
```

Same arguments as `transfer`, no side effects. Returns the planned copy (file list and total bytes), a risk summary (overwrites, missing source paths, destination quota pressure), and a suggested next command. Recommended preflight for automation pipelines and AI agent workflows because the output is structured and the exit code maps to risk severity.

### mount - FUSE Virtual Filesystem

Mount any remote as a local directory. Any application can then access remote files with standard tools.

```bash
# Mount S3 as local directory
mkdir /mnt/cloud
aeroftp-cli --profile "S3 Storj" mount /mnt/cloud

# Mount with read-only access
aeroftp-cli --profile "server" mount /mnt/remote --read-only

# Custom cache TTL (seconds)
aeroftp-cli --profile "NAS" mount /mnt/nas --cache-ttl 60

# Windows: mount as drive letter
aeroftp-cli --profile "server" mount Z:
```

Supported operations: ls, cat, cp, vim, grep, mkdir, rm, touch, mv, df. All file managers (Nautilus, Dolphin, Explorer) can browse the mount natively.

- **Linux/macOS**: FUSE (requires libfuse3-dev on Linux, macFUSE on macOS)
- **Windows**: WebDAV bridge mapped as network drive (zero extra software)
- Unmount: `fusermount -u /mnt/cloud` or Ctrl+C

### ncdu - Interactive Disk Usage Explorer

```bash
# Interactive TUI (navigate with keyboard)
aeroftp-cli --profile "server" ncdu /

# Scan depth limit
aeroftp-cli --profile "server" ncdu / -d 5

# Export to JSON (non-interactive)
aeroftp-cli --profile "server" ncdu / --export /tmp/usage.json

# JSON to stdout
aeroftp-cli --profile "server" ncdu / --json
```

TUI controls: Up/Down navigate, Enter opens directory, Backspace goes back, q quits.

### serve - Expose Remote as Local Server

#### serve http (read-only)

```bash
aeroftp-cli --profile "server" serve http _ / --addr 127.0.0.1:8080
# Browse at http://127.0.0.1:8080
```

#### serve webdav (read-write)

```bash
aeroftp-cli --profile "server" serve webdav _ / --addr 127.0.0.1:8080
```

#### serve ftp (read-write)

```bash
aeroftp-cli --profile "server" serve ftp _ / --addr 0.0.0.0:2121 --passive-ports 49152-49200
# Connect with any FTP client: curl ftp://localhost:2121/
```

#### serve sftp (read-write)

```bash
aeroftp-cli --profile "server" serve sftp _ / --addr 0.0.0.0:2222
# Connect with: sftp -P 2222 anon@localhost
# Or: curl sftp://localhost:2222/
```

All serve modes expose any AeroFTP provider (S3, MEGA, WebDAV, FTP, etc.) as a local server of the chosen protocol. Anonymous access, Ctrl+C to stop.

### daemon - Background Service

```bash
# Start daemon (HTTP API on port 14320)
aeroftp-cli daemon start

# Check status
aeroftp-cli daemon status

# Stop daemon
aeroftp-cli daemon stop
```

The daemon enables persistent mounts, scheduled transfers, and job management via HTTP RC API at `http://localhost:14320`.

### jobs - Background Transfer Jobs

```bash
# Add a job (requires daemon running)
aeroftp-cli jobs add get --profile "S3" /backups/db.sql ./

# List jobs
aeroftp-cli jobs list

# Check job status
aeroftp-cli jobs status <id>

# Cancel a job
aeroftp-cli jobs cancel <id>
```

Jobs are persisted in SQLite (`~/.config/aeroftp/jobs.db`).

### crypt - Zero-Knowledge Encrypted Storage

```bash
# Initialize encrypted overlay on a remote directory
AEROFTP_CRYPT_PASSWORD=MySecret aeroftp-cli --profile "S3" crypt init _ /encrypted

# Upload with encryption (content + filename encrypted)
AEROFTP_CRYPT_PASSWORD=MySecret aeroftp-cli --profile "S3" crypt put ./secret.pdf _ /encrypted

# List (shows decrypted names)
AEROFTP_CRYPT_PASSWORD=MySecret aeroftp-cli --profile "S3" crypt ls _ /encrypted

# Download with decryption
AEROFTP_CRYPT_PASSWORD=MySecret aeroftp-cli --profile "S3" crypt get secret.pdf _ /encrypted ./decrypted.pdf

# Password via environment variable
AEROFTP_CRYPT_PASSWORD=MySecret aeroftp-cli --profile "S3" crypt ls _ /encrypted
```

Encryption: AES-256-GCM (content, 64KB blocks) + AES-256-SIV (filenames) + Argon2id (key derivation). The cloud provider never sees file names or content.

### batch - Execute Script

```bash
aeroftp-cli batch deploy.aeroftp
```

Executes a `.aeroftp` script file containing a sequence of commands. See [Batch Scripting](#batch-scripting) below.

### rcat - Upload stdin Directly

```bash
printf 'hello from stdin\n' | aeroftp-cli rcat --profile "server" /remote/path/message.txt
```

Reads stdin and uploads it directly to a remote file. Useful for pipelines, generated content, and agent workflows where creating a temporary local file would be unnecessary.

### alias - Manage Command Aliases

```bash
# Create or update an alias
aeroftp-cli alias set prod-ls ls --profile Production /var/www/ -l

# Show one alias
aeroftp-cli alias show prod-ls

# List all aliases
aeroftp-cli alias list

# Remove an alias
aeroftp-cli alias remove prod-ls
```

Aliases are stored in the CLI `config.toml` file and expanded before command parsing. Alias expansion is cycle-protected.

### agent - AeroAgent from the CLI

```bash
# One-shot prompt
aeroftp-cli agent --provider ollama --message "list the saved servers"

# Orchestration mode over stdin/stdout
aeroftp-cli agent --orchestrate

# MCP server mode (full alias)
aeroftp-cli agent --mcp
```

Runs AeroAgent through the shared Rust backend. It supports one-shot prompts, interactive runs, orchestration mode, and MCP server mode for external agent clients.

### mcp - MCP Server (top-level shortcut)

```bash
# Start the MCP server on stdin/stdout (used by the official VS Code extension)
aeroftp-cli mcp
```

`aeroftp-cli mcp` is a top-level alias for `aeroftp-cli agent --mcp` (added in v3.5.4). The official VS Code extension `axpdev-lab.aeroftp-mcp` registers exactly this argv, so the shortcut keeps the extension self-contained without nested subcommands. The server initializes the Universal Vault automatically (or falls back to `AEROFTP_MASTER_PASSWORD` when set) so saved profiles work out-of-the-box, serializes per-profile tool calls, and validates `inputSchema.required` before dispatch.

### speed - Bandwidth Benchmark

```bash
# Default test (10 MB synthetic file)
aeroftp-cli speed --profile "server"

# Custom size (alias --size / -s)
aeroftp-cli speed --profile "server" --size 100M

# JSON output
aeroftp-cli speed --profile "server" --size 50M --json
```

Uploads a synthetic file, downloads it back, and reports upload/download MB/s, latency, and round-trip time. Useful for diagnosing slow connections or comparing providers.

### speed-compare - Multi-Server Benchmark

```bash
# Rank two or more servers side-by-side
aeroftp-cli speed-compare sftp://user@host1 sftp://user@host2 sftp://user@host3

# Custom test size and parallelism
aeroftp-cli speed-compare --size 100M --parallel 4 sftp://user@host1 sftp://user@host2

# Persist reports to disk (JSON v1 + CSV + Markdown)
aeroftp-cli speed-compare sftp://user@host1 sftp://user@host2 \
    --json-out report.json --csv-out report.csv --md-out report.md
```

Benchmarks two or more servers in parallel and emits a ranked comparison table with download/upload throughput, TTFB, and SHA-256 integrity status. Up to 4 parallel runs (`--parallel`, default 2). Schema `aeroftp.speedtest.v1` stable across single (`speed`) and multi-server (`speed-compare`) reports. Passwords are redacted from every output path; CSV cells are spreadsheet-formula-safe; Markdown cells escape pipes/newlines/backslashes.

### import - Import Server Profiles

`import` ingests server profiles from external tools and stores them in the AeroFTP encrypted vault. Three sources today: `rclone`, `winscp`, `filezilla`. Use `--json` on any subcommand for scripting; passwords are decoded from each tool's native obfuscation and re-wrapped in AES-256-GCM by the vault on commit (commit happens through the GUI import flow today).

#### import rclone

```bash
# Auto-detect rclone.conf and list importable remotes
aeroftp-cli import rclone

# Specify config path explicitly
aeroftp-cli import rclone /path/to/rclone.conf

# JSON output for scripting
aeroftp-cli import rclone --json
```

Supports 17 rclone backend types (FTP, SFTP, S3, WebDAV, Google Drive, Dropbox, OneDrive, MEGA, Box, pCloud, Azure Blob, Swift, Yandex Disk, Koofr, Jottacloud, Backblaze B2, OpenDrive). Passwords are de-obfuscated from rclone's reversible AES-256-CTR scheme. See the full [rclone Integration Guide](https://docs.aeroftp.app/features/rclone) for the complete backend mapping table and security comparison. For existing `rclone crypt` remotes (full read/write interop with transparent encrypted overlay session), see [rclone crypt interoperability](https://docs.aeroftp.app/features/rclone-crypt).

#### import winscp

```bash
# Auto-detect WinSCP.ini on Windows
aeroftp-cli import winscp

# Specify path explicitly
aeroftp-cli import winscp /path/to/WinSCP.ini

# JSON output
aeroftp-cli import winscp --json
```

Imports saved sessions from WinSCP. Supports SFTP, SCP, FTP, FTPS (implicit and explicit TLS), WebDAV (HTTP/HTTPS), and S3. Passwords are decoded from WinSCP's XOR obfuscation. See the [WinSCP Bridge guide](https://docs.aeroftp.app/features/winscp).

#### import filezilla

```bash
# Auto-detect sitemanager.xml
aeroftp-cli import filezilla

# Specify path explicitly
aeroftp-cli import filezilla /path/to/sitemanager.xml

# JSON output
aeroftp-cli import filezilla --json
```

Imports sites from FileZilla's `sitemanager.xml`. Supports FTP, SFTP, FTPS (implicit and explicit), and S3. Passwords are decoded from base64 and upgraded to AES-256-GCM on commit. See the [FileZilla Bridge guide](https://docs.aeroftp.app/features/filezilla).

#### import rclone-filter

```bash
# Convert an rclone filter file to .aeroignore on stdout
aeroftp-cli import rclone-filter ~/.config/rclone/filter.txt

# Write directly to a file (refuses to overwrite without --force)
aeroftp-cli import rclone-filter filter.txt -o /project/.aeroignore

# Read filter rules from stdin (e.g. piping from another tool)
cat filter.txt | aeroftp-cli import rclone-filter -

# JSON envelope (status, input path, generated text, warnings)
aeroftp-cli import rclone-filter filter.txt --json
```

Converts an rclone `--filter-from` file (`+ pattern` / `- pattern` / `# comments` / `! ` reset) into an `.aeroignore` (gitignore syntax). Because rclone uses **first-match-wins** and gitignore uses **last-match-wins**, the generated rules are emitted in **reversed order** to preserve precedence: the highest-priority rclone rule ends up last in the output. Includes (`+ pattern`) become re-include rules (`!pattern`), excludes pass through unchanged.

Two non-fatal warnings are surfaced (text on stderr, JSON in the `warnings` array):

- `! ` reset directive: gitignore has no direct equivalent. The rules above the reset are kept (rclone discards them at runtime) so you can review and prune manually.
- `{a,b}` brace alternation: gitignore does not support brace expansion natively. The pattern is passed through unchanged; expand manually if the rule must apply.

Exit codes: `0` ok, `2` input file not found, `9` output file exists without `--force`, `11` I/O error reading stdin or writing output.

### completions - Generate Shell Completion Scripts

```bash
aeroftp-cli completions bash
aeroftp-cli completions zsh
```

Generates completion scripts for `bash`, `zsh`, `fish`, `elvish`, and `powershell`.

### profiles - List Saved Profiles

```bash
# Print saved profiles (text format)
aeroftp-cli profiles

# Same payload as JSON for scripting
aeroftp-cli profiles --json
```

Lists every server profile in the encrypted vault: display name, protocol, host, optional saved path, and a credential indicator (passwords are never printed). The JSON form is the canonical input for AI agents that need to discover what's available before running anything else.

### ai-models - List Configured AI Providers

```bash
aeroftp-cli ai-models
aeroftp-cli ai-models --json
```

Lists every AI provider configured in the encrypted vault with its associated models. API keys are never printed. Useful for verifying which providers are wired up before invoking `agent --provider <id>`.

### agent-bootstrap - Canonical Quick-Start for AI Agents

```bash
# General intro payload
aeroftp-cli agent-bootstrap --json

# Task-tailored variants
aeroftp-cli agent-bootstrap --task explore --path /var/www --json
aeroftp-cli agent-bootstrap --task verify-file --remote-path /a.txt --local-path ./a.txt --json
aeroftp-cli agent-bootstrap --task transfer \
    --source-profile "FTP Aruba" --dest-profile "AWS S3" \
    --source-path /www --dest-path /backup/www --json
```

Returns the canonical task-oriented quick-start that agents should follow before issuing real commands. The payload includes the recommended workflow, profile inventory with per-profile auth state (`valid` / `expired` / `needs_refresh` / `no_credentials` / `unknown`), and ready-to-run command templates for each supported task. Tasks: `explore`, `verify-file`, `transfer`, `backup`, `reconcile`.

### agent-connect - Single-Shot Agent Connect

```bash
# One JSON payload covering connect, capabilities, quota, and root listing
aeroftp-cli agent-connect "AWS S3" --json
```

Replaces the boilerplate `connect -> about -> df -> ls /` sequence agents would otherwise run. Returns a single JSON envelope with four blocks: `connect`, `capabilities`, `quota`, `path`. Each block carries one of `ok` / `unsupported` / `unavailable` / `error`. Live-connect allowlist: FTP, FTPS, SFTP, WebDAV, S3, GitHub, GitLab. For other providers (pCloud, Dropbox, OneDrive, Box, Filen, MEGA, Koofr, kDrive, Jottacloud, Drime, FileLu, Yandex, 4shared, Internxt, Swift, Azure, Google Drive, Zoho WorkDrive, Immich) the `connect` block returns `status: "unsupported"` but `capabilities`, `path`, and `profile` stay actionable, and the CLI exits 0. Exit codes: `0` ok or unsupported with valid capabilities, `1` connect attempted and failed, `2` profile lookup failed.

### agent-info - AI Agent Discovery Metadata

```bash
aeroftp-cli agent-info --json
```

Prints structured JSON describing safe/modify/destructive command groups, credential model, output hygiene, saved profile inventory with per-profile auth state, and a `protocol_features` map keyed by protocol (share_links, resume, server_copy, versions, thumbnails, change_tracking) plus an `agent_connect_supported_protocols` array. This is the recommended discovery surface for AI coding agents and the canonical input for capability-aware tool routing.

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
| `--parallel <n>` | Number of parallel transfer workers for recursive/bulk operations |
| `--partial` | Resume interrupted transfers when the provider supports partial files or remote offsets |
| `--include <pattern>` | Include only files matching glob pattern (repeatable) |
| `--exclude-global <pattern>` | Exclude files matching glob pattern (repeatable) |
| `--include-from <file>` | Read include patterns from file |
| `--exclude-from <file>` | Read exclude patterns from file |
| `--min-size <size>` | Minimum file size filter (e.g., `100k`, `1M`) |
| `--max-size <size>` | Maximum file size filter (e.g., `1G`) |
| `--min-age <duration>` | Skip files newer than duration (e.g., `7d`, `24h`) |
| `--max-age <duration>` | Skip files older than duration (e.g., `30d`) |
| `--max-transfer <size>` | Abort session after transferring N bytes (e.g., `10G`). Exit code 8 |
| `--retries <n>` | Retry failed transfers N times (default: 3). Auth/usage errors not retried |
| `--retries-sleep <dur>` | Delay between retries (e.g., `5s`, `1m`, `500ms`). Default: 1s |
| `--max-backlog <n>` | Max queued transfer tasks for parallel operations (default: 10000) |
| `--files-from <file>` | Transfer only files listed in file (one per line, `#` comments). Works with get -r, put -r, sync |
| `--files-from-raw <file>` | Like `--files-from` but preserves whitespace and empty lines |
| `--immutable` | Never overwrite existing files on destination (append-only mode) |
| `--no-check-dest` | Skip remote directory listing during sync (assume destination is empty) |
| `--max-depth <n>` | Maximum recursion depth for ls -R, find, sync, get -r, put -r |
| `--default-time <ts>` | Fallback mtime when backend returns None. Accepts ISO 8601, RFC 3339, or `now` |
| `--fast-list` | S3 only: recursive listing in a single API call (fewer API calls for large buckets) |
| `--inplace` | Write downloads directly to final path (no .aerotmp temp file) |
| `--chunk-size <size>` | Override upload chunk size (e.g., `64M`). Min 5M for S3 multipart |
| `--buffer-size <size>` | Override download buffer size (e.g., `256K`, `1M`) |
| `--dump <kinds>` | Debug: `headers`, `bodies`, `auth` (comma-separated). Prints to stderr |

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

## CLI Configuration

The CLI reads defaults and aliases from `config.toml` under the user config directory:

- Linux: `~/.config/aeroftp/config.toml`
- macOS: `~/Library/Application Support/aeroftp/config.toml`
- Windows: `%APPDATA%/aeroftp/config.toml`

Example:

```toml
[defaults]
profile = "Production"
json = true
parallel = 8
partial = true
limit_rate = "5M"

[aliases]
prod-ls = ["ls", "--profile", "Production", "/var/www/", "-l"]
```

Supported defaults include `profile`, `format`, `json`, `parallel`, `partial`, `quiet`, `verbose`, `limit_rate`, `bwlimit`, `max_transfer`, `max_backlog`, `retries`, and `retries_sleep`.

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
| 9 | Already exists / directory not empty (--immutable, --no-clobber) |
| 10 | Server error / parse error |
| 11 | I/O error |
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

# Using saved profile (recommended - no token exposed)
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

# Skip certificate verification (invalid, self-signed, or hostname-mismatched cert)
aeroftp-cli connect ftp://user@host --tls explicit --insecure
```

When certificate verification is enabled, FTPS connections fail closed on invalid certificates, including hostname mismatch. Use `--insecure` only when you intentionally trust that server despite certificate validation failure.

The same rule applies to saved `--profile` connections. If a saved FTPS profile points to a host whose certificate does not match the configured hostname, AeroFTP CLI fails immediately and does not retry automatically with verification disabled. For example, a saved Aruba profile like `aeroftp.app` fails closed on `hostname mismatch` until you explicitly allow invalid/self-signed certificates in the saved profile or use `--insecure` for a direct URL connection.

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
AEROFTP_TOKEN=mytoken aeroftp-cli ls jottacloud://user@jottacloud.com /

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
- **Output hygiene**: Data on stdout, messages on stderr - safe for piping
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
USAGE=$(aeroftp-cli df sftp://user@host --json 2>/dev/null | jq '.used_pct')
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

If a saved FTPS profile fails with `certificate verify failed` or `hostname mismatch`, that is now the expected secure behavior unless the profile explicitly allows invalid or self-signed certificates.

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

## Live Test Results

The following providers have been tested live via CLI with `--profile`:

| Provider | Protocol | connect | ls | put/get | head/tail | hashsum | check | about | dedupe | track-renames | bwlimit | touch | tree | df |
|----------|----------|---------|----|---------|-----------|---------|-------|-------|--------|---------------|---------|-------|------|------|
| WD MyCloud NAS | SFTP | PASS | PASS | PASS | PASS | PASS | PASS | PASS | PASS | PASS | PASS | PASS | PASS | PASS |
| axpdev.it | FTP | PASS | PASS | - | PASS | PASS | - | PASS | - | - | PASS | - | - | - |
| Playground | GitHub | PASS | PASS | PASS | PASS | PASS | - | PASS | - | - | - | PASS | PASS | - |
| MEGA.nz | MEGA | PASS | PASS | - | - | - | - | PASS | - | - | - | - | - | - |
| OpenDrive | OpenDrive | PASS | PASS | - | - | - | - | PASS | - | - | - | - | - | PASS |
| Filen | Filen (E2E) | PASS | PASS | - | - | - | - | PASS | - | - | - | - | - | PASS |
| Koofr | WebDAV | PASS | PASS | - | - | - | - | PASS | - | - | - | - | - | - |
| Koofr | Native API | PASS | PASS | - | - | - | - | PASS | - | - | - | - | - | PASS |
| WD MyCloud NAS | WebDAV | PASS | PASS | - | - | - | - | PASS | - | - | - | - | - | - |
| Backblaze B2 | S3 | PASS | PASS | - | - | - | - | PASS | - | - | - | - | - | - |
| Azure | Azure Blob | PASS | PASS | - | - | - | - | PASS | - | - | - | - | - | - |
| 4shared | OAuth 1.0 | PASS | PASS | - | - | - | - | PASS | - | - | - | - | - | PASS |

**12 providers tested**, all core operations verified. `about` tested on all 12 providers. `dedupe`, `track-renames`, and `bwlimit` tested on SFTP.

---

## Recent Highlights

- **v3.7.2 - CLI security hardening + community polish**: Codex CLI external audit closes 17 paired findings (CLI-AUDIT-01..17). Highlights: GUI tool execution now enforces backend approval, MCP / AI core remote dispatcher path validation (null bytes, traversal, control chars, option-like forms, length cap), `server_exec` strictly read-only (rejects get/put/mkdir/rm/mv with explicit-use error), MCP profile lookup requires exact id/name or unique substring (no silent first-match), `local_copy_files` and `local_stat_batch` validate every path including symlink rejection, SFTP packet parser bounds-checked end to end, `.aerotmp` writes use `create_new` and refuse symlinked temp paths, inline upload temp files use `tempfile::Builder::tempfile()`, daemon auth token created with `O_NOFOLLOW` + mode 0600, `sync --direction <invalid>` fails before connecting with exit code 5, `sync-doctor` resolves remote paths the same way `sync` does, `sync-doctor --checksum` no longer suggests the non-existent flag, `transfer` checks cancellation between plan and execution returning exit code 130, `agent-info --json` treats missing profile list as empty, CLI help footer documents the extended exit-code contract (9, 10, 11, 130). CLI `profiles` dynamic terminal-width-aware layout (Ehud, #161), unified `--breakdown` table folding TOTAL into the breakdown rows, `--hide=fav` / `favorite` / `favourite` / `favs` alias surface documented. Direct `rsa = "0.9"` dependency dropped, `jsonwebtoken` switched to `aws-lc-rs`. `audit.toml` documents the two remaining transitive RSA paths (sigstore, russh) with written threat-model justifications.
- **v3.7.1 - Mount Manager + community polish**: GUI Mount Manager dialog wraps `aeroftp-cli mount` with persistent configs, sidecar JSON or vault-backed storage, cross-platform autostart (systemd-user / Task Scheduler ONLOGON), and an "Open mount in file manager" shortcut. CLI `profiles -i` interactive prompt loop with compact `1l` / `2t` / `3d` tokens. Filen Desktop local WebDAV / S3 bridges connect on the first try thanks to the layered WebDAV scheme detection rewrite.
- **v3.7.0 - AeroRsync session-cached batch + crypto overlay**: new `AerorsyncBatch` trait amortizes one SSH session across many delta transfers; `SyncReport` exposes `delta_files[]` and `bytes_on_wire`. Cross-profile transfer (`aeroftp_transfer`, `aeroftp_transfer_tree`) and six new ops tools (`aeroftp_touch`, `aeroftp_cleanup`, `aeroftp_speed`, `aeroftp_sync_doctor`, `aeroftp_dedupe`, `aeroftp_reconcile`) bring MCP to 39 tools. rclone crypt becomes full read/write through transparent overlay session; AeroVault gets matching overlay-session model.
- **v3.6.1 - Windows first-class delta sync**: native rsync protocol 31 in pure Rust (`aerorsync`), no `rsync.exe` bundle, no WSL requirement. The Windows binary now performs delta uploads byte-identical to stock rsync 3.4.1 in CI.
- **v3.5.4 - MCP hardening**: `aeroftp-cli mcp` top-level alias, vault auto-init in MCP, per-profile serialization, schema validation, S3 bucket fix, FTP/SFTP/WebDAV/Filen/FileLu/Drime/Immich error message hardening.
- **v3.5.3 - Continuous bidirectional `sync --watch`**: native filesystem watcher (inotify/FSEvents/ReadDirectoryChangesW) with anti-loop cooldown, periodic rescan, NDJSON output. First CLI on the market with this feature natively (rclone doesn't ship it).
- **v3.5.3 - Agent-friendly flags**: `--files-from`, `--immutable`, `--no-check-dest`, `--max-depth`, `--inplace`, `--fast-list` (S3), `--compare-dest`/`--copy-dest`, `cleanup` for orphan `.aerotmp`.
- **v3.5.2 - Determinism & threat model**: 12 structured exit codes mapping all `ProviderError` variants, `mkdir --parents`, `rm --force`, `put --no-clobber`, `--chunk-size`/`--buffer-size` overrides, [`docs/THREAT-MODEL.md`](THREAT-MODEL.md), [`docs/LLM-INTEGRATION-GUIDE.md`](LLM-INTEGRATION-GUIDE.md).
- **v3.3.4 - Local server bridges**: `serve http` (read-only HTTP, Range requests), `serve webdav` (read-write, axum-based), `serve ftp` / `serve sftp` (anonymous read-write).

---

*AeroFTP CLI is part of the [AeroFTP](https://github.com/axpdev-lab/aeroftp) project - GPL-3.0*
