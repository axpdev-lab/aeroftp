# AeroFTP CLI - User Guide

> **Binary**: `aeroftp-cli` (ships alongside the GUI)
> **Version reference**: v4.0.x series - last reviewed 22 June 2026
> **License**: GPL-3.0

---

## Overview

AeroFTP CLI is a production command-line client for multi-protocol file transfers. It shares the same Rust backend as the AeroFTP desktop app, with direct URL support for core protocols and `--profile` access for saved GUI-authorized providers. Beyond basic transfer commands, the CLI also covers cross-profile copy planning and execution, continuous bidirectional sync (`sync --watch`), reconcile/sync-doctor preflights for agents, stdin upload, remote copy/share/edit flows, batch scripting, shell completions, aliases, encrypted overlays (`crypt`), single-file AeroVault containers (`vault`), plaintext `.aerozip` archives (`archive`), local archives (`compress`/`extract`), local server bridges (`serve http/webdav/ftp/sftp`), MCP server mode for the official VS Code extension, and AI agent discovery/orchestration.

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

### Windows portable ZIP

The Windows portable build (`AeroFTP-<version>-portable-windows-x64.zip`, no
installer) ships `aeroftp-cli.exe` next to `AeroFTP.exe` (since v4.0.8). It is
not on `PATH` automatically: either call it by its full path, `cd` into the
extracted folder, or add that folder to your `PATH`.

```powershell
# From inside the extracted portable folder
.\aeroftp-cli.exe --version
.\aeroftp-cli.exe --help
```

The portable CLI shares the portable `data\` folder, so the servers and vault
you set up in the GUI are visible to the CLI and vice versa.

### Building from Source

```bash
git clone https://github.com/axpdev-lab/aeroftp.git
cd aeroftp/src-tauri
cargo build --release --bin aeroftp-cli
# Binary at target/release/aeroftp-cli
```

---

## Short Invocation

All AeroFTP packages ship a small native dispatcher so the CLI is reachable under several names without merging the GUI and CLI binaries. The dispatcher is `GUI by default`: anything ambiguous opens the GUI, so file associations and launcher entries can never be swallowed by accident.

The shipped set:

| Name | Routes to | Notes |
|---|---|---|
| `aeroftp` | GUI when called bare, with a `.aerovault` / `.aeroftp` / `.aeroftp-keystore` file, or with `-` flags | Same behavior as before |
| `aeroftp <subcommand>` | CLI | Any CLI subcommand (e.g. `aeroftp ls`, `aeroftp sync`) is routed automatically |
| `aftp` | CLI | Short 4-character built-in name, always present |
| `aeroftp-cli` | CLI | Kept for back-compat: scripts, CI, MCP, `cargo install` |

Arguments, signals and exit codes are passed through untouched.

### The `aero` opt-in alias

`aero` is officially provided as an opt-in alias you can enable with a single command. It is not shipped as a global binary because a package owning `/usr/bin/aero` would hard-fail to install on any system where another package already owns that path (a file conflict at the package manager level).

The same command both enables and disables the alias (toggle):

```bash
# Turn the alias On (creates ~/.local/bin/aero -> dispatcher)
aeroftp-cli alias-toggle aero
# The 'aero' alias is now On

# Same command turns it Off
aeroftp-cli alias-toggle aero
# The 'aero' alias is now Off
```

The command is idempotent and exits with code 0 in both directions. The default target directory is `~/.local/bin`; pass `--bin-dir <path>` to override. If the target directory is not on your `PATH`, a one-line note on stderr explains how to add it.

The toggle refuses to overwrite an existing non-symlink file at the target path, and refuses to remove a symlink that does not point at the current AeroFTP binary. This protects user-owned files and foreign installations.

JSON output for scripting:

```bash
aeroftp-cli --json alias-toggle aero
# {"alias":"aero","path":".../aero","path_in_env":false,"state":"on", ...}
```

Once `aero` is enabled, anything that works under `aeroftp` works under `aero`: `aero ls`, `aero sync`, `aero vault`, `aero mcp`, and so on.

The shorter names (`aero`, `aftp`) were added at the community's request (discussion #273) for quicker, more comfortable typing than the full `aeroftp-cli`.

### Manual recipes (alternative)

If you prefer to wire the alias yourself, the snippets below are equivalent to the toggle command above. They do not need root and they do not touch the package layout.

**bash** (`~/.bashrc`):

```bash
alias aero='aeroftp'
```

**zsh** (`~/.zshrc`):

```zsh
alias aero='aeroftp'
```

**fish** (`~/.config/fish/config.fish`):

```fish
alias aero 'aeroftp'
funcsave aero
```

**PowerShell** (`$PROFILE`):

```powershell
Set-Alias -Name aero -Value aeroftp
```

After editing the file, reopen the shell or `source` the rc file. Verify with `aero --version`.

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

Each JSON record carries a `#` field: the 1-based index of the profile, the same number accepted by `--profile <n>` and by the interactive `profiles -i` commands. Because the JSON keys are sorted, `#` is the leading field of every record, so a script can map a chosen index straight back to a `--profile` call:

```bash
# Pick the index of the first SFTP profile, then use it
idx=$(aeroftp-cli profiles --json | jq -r 'map(select(.protocol=="sftp")) | .[0]["#"]')
aeroftp-cli ls --profile "$idx" /
```

Pass `--breakdown` to wrap the array in `{ profiles, summary, breakdown }`; the per-profile records keep the same shape (including `#`).

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

### lsd / lsl / lsf - List Variants for Shell Pipelines

```bash
# Directories only
aeroftp-cli lsd --profile "My S3" /

# Long listing (permissions, size, date, name)
aeroftp-cli lsl --profile "My S3" /backups

# Flat names, one entry per line (pipe-friendly)
aeroftp-cli lsf --profile "My S3" / | grep '\.tar\.gz$'
```

Predictable list shapes for shell pipelines. `lsd` shows only directories, `lsl` is the long format, and `lsf` prints bare entries one per line. The human summary always goes to stderr, so `lsf` stdout stays clean for pipelines. All three accept the same path and `--profile` rules as `ls`.

### lsjson - Stable JSON Listing

```bash
# One JSON record per entry in a directory
aeroftp-cli lsjson --profile "My S3" /backups

# Recurse the whole subtree
aeroftp-cli lsjson --profile "My S3" / -R

# Only files, no MimeType field, piped into jq
aeroftp-cli lsjson --profile "My S3" /backups -R --files-only --no-mimetype | jq -r '.[].Path'

# A single object describing the path itself
aeroftp-cli lsjson --profile "My S3" /backups/report.pdf --stat

# Opt-in per-file hashes (downloads each file to digest it)
aeroftp-cli lsjson --profile "My S3" /backups --hash --hash-type md5
```

Emits a pretty-printed JSON **array** to stdout with a stable, machine-parsable record schema, so scripts and CI can depend on the field names. The output is always JSON regardless of the global `--json`/text flag; an empty listing is `[]`. Records are sorted by `Path` for reproducible diffs.

| Field | Type | Notes |
|---|---|---|
| `Path` | string | Relative to the listed root: leaf name non-recursively, `/`-joined subpath with `-R`. |
| `Name` | string | Leaf name only. |
| `Size` | int64 | Byte size; directories are `-1` by convention. |
| `MimeType` | string | `inode/directory` for directories, otherwise guessed from the name. Omitted with `--no-mimetype`. |
| `ModTime` | string | Provider-native modification time, passed through verbatim (RFC3339 where the provider supplies it; empty string when unknown). Not re-normalized. Omitted with `--no-modtime`. |
| `IsDir` | bool | Whether the entry is a directory. |
| `Hashes` | object | Only with `--hash`: `{ "<algo>": "<hex>" }`, server-side only (never downloads). Omitted for directories and when the backend does not expose the requested digest cheaply. |

Flags: `-R`/`--recursive`, `--files-only`, `--dirs-only` (mutually exclusive with `--files-only`), `--stat` (single object for the path itself, not its contents), `--no-modtime`, `--no-mimetype`, `--hash`, `--hash-type <md5|sha1|sha256|sha512|blake3>` (default `sha256`). The global `--max-depth` caps recursion. `--hash` is server-side only: it reports a digest the backend already exposes (S3/MinIO/R2 ETag `md5`, B2 `contentSha1`, pCloud, SFTP `sha256sum` over an exec channel) without ever downloading the file. It is off by default; the `Hashes` field is omitted for directories and for any file whose backend does not provide the requested digest cheaply (it never falls back to downloading). Exit code `0` on success, `2` on a missing path. Same path and `--profile` rules as `ls`.

### archive - Plaintext `.aerozip` Archives

```bash
# Create a passwordless archive with compression + recovery parity
aeroftp-cli archive create backup.aerozip ./docs ./photos

# Recovery parity is on by default; disable it for a smaller, parity-free file
aeroftp-cli archive create backup.aerozip ./docs --recovery-level 0   # also: off / none

# Tune archive-local compression; --profile remains a hidden compatibility alias
aeroftp-cli archive create backup.aerozip ./docs --compression-profile archive

# List entries; renamed files are accepted by header detection
aeroftp-cli archive list backup.aerozip

# Extract, repairing from embedded recovery parity first when possible
aeroftp-cli archive extract backup.aerozip ./restore

# Machine-readable reports
aeroftp-cli --json archive create backup.aerozip ./docs
```

`archive` is for `.aerozip` plaintext archives (`application/x-aerozip`) backed by the internal `aerovz` lane. It uses the AeroVault v3 wrapper stack without encryption: chunking, zstd compression, integrity checks, and Reed-Solomon recovery parity.

Important threat-model note: `.aerozip` is **not confidential**. It has no password and the CLI rejects `--password`; anyone who can read the archive can read the contents. Use `vault create` and `.aerovault` when you need encryption.

`archive create` writes the current product extension (`.aerozip`). `archive list` and `archive extract` identify plaintext archives from the container header, so a renamed file still opens as long as the header marks the plaintext lane. Encrypted `.aerovault` containers are rejected by `archive`, and plaintext archives are rejected by encrypted `vault` commands.

JSON reports include `encrypted: false`, `confidential: false`, file/entry/chunk counts, compression ratio, MIME, and recovery status/overhead.

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

# Delta-aware single-file download (Z.4.5 R1, SFTP only today)
aeroftp-cli get sftp://user@host /var/www/index.html ./local-copy.html --delta
```

> **Glob patterns**: Quote the remote path to prevent shell expansion. The CLI expands `*` and `?` patterns server-side.

> **`--delta`**: routes the transfer through `AerorsyncDeltaTransport` (native rsync wire protocol 31 over SSH). Falls back to the classic transfer when the provider does not expose a delta transport (everything except SFTP today), the file is too small (< 1 MiB), or the SFTP session is not delta-eligible (no host-key pin, password auth without dispatch wire-up). No-op for recursive / glob downloads (use `aeroftp sync --delta` for those). See [AeroRsync section](#aerorsync---delta-sync-engine) for the full surface.

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

# Delta-aware single-file upload (Z.4.5 R1, SFTP only today)
aeroftp-cli put sftp://user@host ./report.pdf /uploads/report.pdf --delta

# Set the uploaded file's privacy on OpenDrive (private | public | hidden)
aeroftp-cli put opendrive://user@host ./secret.pdf /docs/ --access private
```

> **`--delta`**: same semantics as on `get` (see above). On a successful delta upload the CLI prints `bytes_on_wire / total` and the rsync speedup ratio so you can confirm the saving.

> **`--access <private|public|hidden>`** (issue #252): on providers that model a three-level access scheme (OpenDrive today), sets the uploaded file's privacy. When omitted on an OpenDrive target the upload defaults to **private** (max-privacy, opt out with `--access public`), mirroring rclone's `--opendrive-access`. Applies to single-file uploads; for recursive/glob uploads set the destination folder privacy with `mkdir --access`, which cascades to children. Ignored, with a note, on providers that do not model access.

### mkdir - Create Directory

```bash
aeroftp-cli mkdir sftp://user@host /var/www/new-folder

# Create a private OpenDrive folder (privacy cascades to its children)
aeroftp-cli mkdir opendrive://user@host /private-docs --access private
```

> **`--access <private|public|hidden>`** (issue #252): on OpenDrive, sets the created folder's privacy, which cascades to existing children server-side. Defaults to **private** when omitted on an OpenDrive target.

### rm - Delete File or Directory

```bash
# Delete a file
aeroftp-cli rm sftp://user@host /var/www/old-file.txt

# Delete a directory recursively
aeroftp-cli rm sftp://user@host /var/www/old-folder/ -rf
```

### rmdir - Remove an Empty Directory

```bash
aeroftp-cli rmdir --profile "My S3" /backups/2024-archived
```

Removes a single directory only when it is empty. A non-empty target is refused with exit code 9 (the emptiness check counts every entry, including dotfiles), so `rmdir` is safe to script against where `rm -r` / `purge` would wipe the whole subtree. Returns exit 2 if the path does not exist.

### rmdirs - Recursively Prune Empty Directories

```bash
# Dry-run (default): list which empty directories would be removed
aeroftp-cli rmdirs --profile "My S3" /staging

# Actually remove them
aeroftp-cli rmdirs --profile "My S3" /staging --force --json
```

Recursively removes **only empty** directories under the path. Files and any directory that still contains entries are left untouched, and the target path itself is never removed. Pruning is bottom-up, so a parent that becomes empty once its empty children are removed is cleaned up in the same pass. Dry-run by default (prints what would be removed); pass `--force` to delete. `--json` reports `removed[]`, `removed_count`, `kept_nonempty`, and `scan_errors`/`delete_errors`; the run is idempotent (a second pass over a clean tree exits `0` with nothing removed). Honors the shared scan-depth/entry caps and path-traversal validation.

### purge - Recursive Delete of a Path

```bash
# Confirms interactively unless --force
aeroftp-cli purge --profile "My S3" /tmp/old-namespace
aeroftp-cli purge --profile "My S3" /tmp/old-namespace --force
```

Removes a path and everything under it. It routes through the same engine as `rm -r`, so it inherits the root guard (refuses to purge `/`), the confirmation prompt, and the shared error / exit-code mapping.

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

# Recursive used-storage scan for no-quota backends (FTP/FTPS/SFTP/S3/WebDAV).
# Scans the profile initialPath subtree; --full scans from the account root.
aeroftp-cli df --profile "My S3" --scan
aeroftp-cli df --profile "My S3" --scan --full
```

Displays used/total storage with a visual progress bar. Returns exit code 7 if the protocol doesn't support storage info.

`--scan` computes `used` recursively for backends with no quota API (S3 list-recursive, WebDAV `Depth:infinity` with a BFS fallback, generic BFS elsewhere) and caches it on the profile so a later `df` / `profiles` shows it without rescanning. A user-set manual total cap (`options.manualTotalBytes` on the profile, also accepted via `--manual-total "50 GB"` on `profile-add` / `profile-duplicate`) is a TRUE override: it wins even over an API-reported total, because SFTP `statfs` reports the whole server disk rather than the user's allotment. The effective used/total/% follows the single `resolveEffectiveQuota` rule shared with the GUI, with a source marker (api / manual / scan).

### tree - Directory Tree

```bash
# Full tree
aeroftp-cli tree sftp://user@host /var/www/

# Limit depth
aeroftp-cli tree sftp://user@host /var/www/ -d 2
```

Renders a tree with Unicode connectors (├──, └──) showing the directory hierarchy. Cycle-safe with visited-path tracking.

### size - Total Size and Object Count

```bash
# Scan the profile initialPath subtree
aeroftp-cli size --profile "My S3" /

# A specific subtree, JSON output
aeroftp-cli --json size --profile "My S3" /backups
```

Reports the recursive object count and total bytes under a path. It reuses the same scan engine as `df --scan`: S3 flat list-recursive, WebDAV `Depth:infinity` with a BFS fallback, generic BFS elsewhere, and the same depth/entry caps. Files and directories are reported separately because SFTP/FTP/WebDAV model real directories that object stores do not. A capped scan sets `truncated: true` (JSON) or prints a `TRUNCATED` marker, so the figure is a clear lower bound. Never issues a per-file `stat()`.

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

When the backend can supply the requested digest without transferring the file, `hashsum` takes a server-side fast-path and never downloads it: S3/MinIO/R2 `md5` from the object ETag, Backblaze B2 `sha1` from `contentSha1`, pCloud via its checksum API, Google Drive / OneDrive / Box / Dropbox from their listing metadata, SFTP by running `sha256sum` on the server over an exec channel, ownCloud/Nextcloud WebDAV from the `oc:checksums` property, and FTP/FTPS when the server advertises `HASH`/`XSHA256`/`XMD5`/`XCRC` in `FEAT`. `sha512`/`blake3`, multipart/SSE S3 objects, and any backend without a cheap digest fall back to download-and-digest automatically (same result, just slower).

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

### audit - Autonomous Server Audits

```bash
# Storage quota: alert when used/total exceeds 80%
aeroftp-cli audit storage-quota --profile-glob '*' --threshold 0.80 --json

# Backup freshness: newest file under /backups must be < 7 days old
aeroftp-cli audit backup-freshness --profile "NAS" --path /backups --max-age 7d --json

# File presence: confirm /www/index.html exists and is at least 1KB
aeroftp-cli audit file-exists --profile "Production" --path /www/index.html --min-size 1024
```

Predefined audit checks that run deterministic, parallel inspections across one or more saved profiles selected by glob. Designed for monitoring pipelines and on-demand spot-checks. Exit codes signal severity: `0` ok, `1` warning (e.g. quota close to threshold), `2` critical (threshold exceeded, file missing), `5` invalid usage. Output is JSON or markdown. Use `aeroftp-cli audit help` to list every check; new checks are added incrementally.

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

# Report only, no action (default mode)
aeroftp-cli dedupe --profile "server" /data --mode skip

# JSON output with group details
aeroftp-cli dedupe --profile "server" /data --dry-run --json

# Destructive mode in a script: --force is required without a TTY
aeroftp-cli dedupe --profile "server" /data --mode newest --force --max-delete 50
```

Finds duplicate files by content hash (SHA-256). Groups files by size first (fast pre-filter), then hashes to confirm. Modes: `skip` (report only), `newest` (keep newest), `oldest` (keep oldest), `largest` (keep largest), `smallest` (keep smallest), `rename` (rename duplicates with numeric suffix), `interactive` (prompt per group), `list` (list without action).

The destructive modes (`delete`, `newest`, `oldest`, `largest`, `smallest`, `rename`) are gated like `rm -r` and `sync --delete`: in an interactive terminal they prompt for confirmation, and in a non-interactive (non-TTY/JSON/cron) context they **fail closed (exit 5)** unless `--force` is passed. A safety cap bounds how many files one run may delete/rename: default **100**, raised or lowered with `--max-delete N` (or `N%` of the scanned files); exceeding the cap aborts with exit 4. Use `--dry-run` to preview without acting.

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

# Generate Reed-Solomon recovery sidecars after successful uploads
aeroftp-cli sync --profile "Backup" ./photos/ /backup/photos/ --direction upload --error-correction

# Pick an overhead level: low=7, medium=15, quartile=25, high=30, or 5-50
aeroftp-cli sync --profile "Backup" ./photos/ /backup/photos/ --direction upload --ec=quartile
```

Sync options: `--direction` (upload/download/both), `--dry-run`, `--delete`, `--exclude`, `--error-correction[=LEVEL]` / `--ec[=LEVEL]`, `--max-delete`, `--backup-dir`, `--backup-suffix`, `--track-renames`, `--bwlimit`, `--conflict-mode`, `--resync`.

`--error-correction` is opt-in for CLI sync and protects uploaded remote files at rest by writing a sibling `<remote>.aerorec` sidecar after each successful upload. If no level is supplied the CLI uses `medium` (15% target overhead). When enabled, sync automatically excludes `*.aerorec` from comparisons so parity sidecars are not mirrored back as user data or deleted as orphans. Remote deletes best-effort remove the protected file's companion sidecar after the primary delete succeeds; missing sidecars are ignored and sidecar delete failures do not fail the file delete. Phase 1 sidecar generation is capped at 256 MiB per source file; larger files are uploaded normally and counted as `ec_skipped_too_large`. JSON sync reports include `ec_generated`, `ec_skipped_too_large`, `ec_generate_failed`, `ec_sidecar_deleted`, and `ec_sidecar_delete_failed` when EC is enabled. Local-to-local sync ignores this flag.

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

# Include an AeroSync EC sidecar cost estimate without generating parity
aeroftp-cli sync-doctor --profile "server" ./local /remote --ec=medium --json
```

Emits a JSON report with planned upload/download/delete counts, bandwidth estimate, top-level diff buckets, and a `next_command` field with the exact `aeroftp-cli sync ...` invocation that matches the preflight. With `--error-correction[=LEVEL]` / `--ec[=LEVEL]`, sync-doctor automatically excludes `*.aerorec` from scans and adds estimate fields (`ec_enabled`, `ec_level_pct`, `ec_estimated_sidecars`, `ec_estimated_overhead_bytes`, `ec_skipped_too_large`, `ec_phase1_max_file_size`) without computing or uploading parity. **Recommended discovery surface for AI coding agents** before they execute mutating sync.

### AEROSYNC-EC - Sync Error Correction Sidecars

AEROSYNC-EC extends AeroSync backups with Reed-Solomon parity stored next to each protected remote file. It reuses the same AVEC codec family as AeroVault error correction, but wraps sync parity in an `AERC1` sidecar:

```text
<remote-file>.aerorec
magic: "AERC1\0\0\0"
binding: BLAKE3("aerosync-recovery-binding-v1" || relative_path || file_size || content_sha256)
segments: Phase 1 emits one whole-file AVEC segment
checksum: trailing BLAKE3 over the sidecar body
```

The binding prevents stale or mismatched sidecars from being trusted: if the file path, size, or content hash changes, the sidecar no longer matches that file. Backup-class sync profiles use Medium EC by default in the app; the CLI command surface is explicit per run via `sync --error-correction` or `sync --ec`.

Honest limits: EC protects against localized at-rest corruption after upload, not TCP/SSH/TLS wire corruption, logical deletes, ransomware, whole-file loss, or truncation beyond the Reed-Solomon parity budget. Each protected file creates one extra remote object and consumes the selected storage overhead. Phase 1 skips files larger than 256 MiB until the later windowed multi-segment generator lands.

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

# Re-initialize over an existing overlay (DESTRUCTIVE: orphans existing files)
AEROFTP_CRYPT_PASSWORD=MySecret aeroftp-cli --profile "S3" crypt init _ /encrypted --force

# Upload a file with encryption (content + filename encrypted)
AEROFTP_CRYPT_PASSWORD=MySecret aeroftp-cli --profile "S3" crypt put ./secret.pdf _ /encrypted

# Upload a whole directory tree (-r): every level gets an encrypted name
AEROFTP_CRYPT_PASSWORD=MySecret aeroftp-cli --profile "S3" crypt put ./photos _ /encrypted -r

# List one level (shows decrypted names)
AEROFTP_CRYPT_PASSWORD=MySecret aeroftp-cli --profile "S3" crypt ls _ /encrypted

# List the whole tree (-R): decrypted relative paths
AEROFTP_CRYPT_PASSWORD=MySecret aeroftp-cli --profile "S3" crypt ls _ /encrypted -R

# Download and decrypt a single file
AEROFTP_CRYPT_PASSWORD=MySecret aeroftp-cli --profile "S3" crypt get secret.pdf _ /encrypted ./decrypted.pdf

# Download and decrypt a directory tree (-r) into ./photos
AEROFTP_CRYPT_PASSWORD=MySecret aeroftp-cli --profile "S3" crypt get photos _ /encrypted . -r
```

`crypt init` refuses to run when an overlay config (`.aeroftp-crypt.json`) already exists at the target, because re-init rotates the salt and would permanently orphan every file already encrypted under the old overlay. It exits with code 9 ("already exists") and prints a hint. Pass `--force` to overwrite intentionally (DESTRUCTIVE).

Encryption: new overlays are written as **AECR v3**. Content is AES-256-GCM-SIV (RFC 8452, 64KB blocks) under a per-file random data key wrapped with AES-256-KW (RFC 3394); filenames are encrypted with AES-256-SIV; the master key is derived with Argon2id (128 MiB / t=4 / p=4) and subkeys via HKDF-SHA256. Each content block binds its index and the total block count (length-binding), and the overlay config carries a key-bound HKDF-SHA256 MAC so its parameters cannot be tampered with. Legacy overlays (v2 GCM-SIV, v1 plain AES-256-GCM) stay readable but are never written. The cloud provider never sees file names or content.

### vault - AeroVault Encrypted Container

Create and manage a single-file `.aerovault` container directly from the
CLI, for every AeroVault format: v3 (the wrapper-stack default), v2
(AES-256-GCM-SIV) and the legacy v1 WinZip-AES container. This calls the
exact backend the desktop app uses, so a CLI round-trip proves the format
end to end.

```bash
# Create an empty v3 vault (compression profile: fast | balanced | archive)
AEROFTP_VAULT_PASSWORD=secret aeroftp-cli vault create my.aerovault --profile balanced

# Create a v2 vault (--cascade enables the ChaCha20-Poly1305 paranoid mode)
AEROFTP_VAULT_PASSWORD=secret aeroftp-cli vault create v2.aerovault --vault-version v2 --cascade

# Create a legacy v1 vault
AEROFTP_VAULT_PASSWORD=secret aeroftp-cli vault create v1.aerovault --vault-version v1

# Add files (v3 batches sub-256KB files into shared packs before chunking).
# The version is auto-detected from the file header; --vault-version forces it.
AEROFTP_VAULT_PASSWORD=secret aeroftp-cli vault add my.aerovault ./a.txt ./b.bin

# Add files and export the behind-the-scenes technical receipt
AEROFTP_VAULT_PASSWORD=secret aeroftp-cli vault add my.aerovault ./*.csv --receipt receipt.json

# Show vault info (auto-detects v1/v2/v3) as JSON
AEROFTP_VAULT_PASSWORD=secret aeroftp-cli vault info my.aerovault

# Extract an entry to a file or a directory destination (all versions)
AEROFTP_VAULT_PASSWORD=secret aeroftp-cli vault extract my.aerovault a.txt ./out/
```

`--vault-version` accepts `v1` | `v2` | `v3` (and `auto`, the default for
`info`/`add`/`extract`, which reads the file header: v3 magic, v2
self-describing, else v1). `create` defaults to v3. v3 pipeline:
small-file batching, Gear-CDC chunking, keyed BLAKE3-128 chunk ids
(dedup), per-chunk zstd, AES-256-GCM-SIV, BLAKE3-256 cipher-block hashes;
the `archive` compression profile widens the CDC bounds for a better
ratio at the cost of finer-grained dedup. Password resolves from

### Error Correction (v4 track): Reed-Solomon error correction

v4 = v3 + non-critical "error-correction.reed-solomon" (Error Correction last wrapper). Opt-in at create:

```bash
# Create v3 + Error Correction (real RS shards on every seal; ~20% overhead, v2 grid)
# Flag spelling confirmed with Ehud Kirsh (#276): --error-correction, short alias --ec.
AEROFTP_VAULT_PASSWORD=secret aeroftp-cli vault create myvault.aerovault --error-correction
AEROFTP_VAULT_PASSWORD=secret aeroftp-cli vault create myvault.aerovault --ec   # short alias

# Info advertises it
aeroftp-cli vault info myvault.aerovault --json   # has_error_correction: true, error_correction: {enabled,algorithm:"reed-solomon",...}

# Scrub (returns checked + damaged list + the parity source repair would use)
aeroftp-cli vault scrub myvault.aerovault

# Repair (honest: "repaired R of D" or "vault left untouched"; --dry-run preview)
aeroftp-cli vault repair myvault.aerovault --dry-run
aeroftp-cli vault repair myvault.aerovault
```

**Recovery placement (the SIDECAR slice, #276).** Parity can live inside the
container (`embedded`, the default), in a sibling `.aerovault.rec` recovery file
(`detached`), or `both`. Detached keeps the encrypted container byte-identical to
a plain vault (so a remote storage/browse view stays stable) and lets you add or
refresh parity *after* the fact, the concrete win over Kopia (which can only
enable ECC at creation). par2-style: the recovery file carries a binding id to its
vault, so a wrong file is refused early; staleness after edits is caught by the
payload's own per-shard checksums.

```bash
# Create with a detached sidecar instead of embedded parity
AEROFTP_VAULT_PASSWORD=secret aeroftp-cli vault create myvault.aerovault --ec --recovery-placement detached

# Pick the QR-style overhead level (#276): named low|medium|quartile|high, or a number 5-50.
# Overhead is P/K; low ~7%, medium ~15%, quartile ~25%, high ~30%; default 20% (K=10/P=2).
AEROFTP_VAULT_PASSWORD=secret aeroftp-cli vault create myvault.aerovault --ec --recovery-level low
AEROFTP_VAULT_PASSWORD=secret aeroftp-cli vault create myvault.aerovault --ec --recovery-level 12

# Add parity later to a vault created without it (reads the container, never rewrites it)
AEROFTP_VAULT_PASSWORD=secret aeroftp-cli vault export-parity myvault.aerovault            # -> myvault.aerovault.rec
AEROFTP_VAULT_PASSWORD=secret aeroftp-cli vault export-parity myvault.aerovault -o out.rec # custom path

# Repair from an explicit recovery file (binding id verified against the vault)
aeroftp-cli vault repair myvault.aerovault --parity myvault.aerovault.rec
aeroftp-cli vault scrub  myvault.aerovault --parity myvault.aerovault.rec   # reports parity_source

# Drop the embedded copy once a sidecar exists (refused without one unless --force)
AEROFTP_VAULT_PASSWORD=secret aeroftp-cli vault strip-parity myvault.aerovault
```

Repair resolves the parity source in priority order: explicit `--parity` ->
`<vault>.aerovault.rec` sidecar -> embedded extension, and reports which it used
(`parity_source` in `--json`). Detached parity tracks a fixed data set, so re-run
`export-parity` after editing a detached/both vault.

See `aeroftp-cli vault --help`, the APPENDIX-AEROVAULT-V4-ECC, AEROVAULT-V3-SPEC §11, and AGENTS.md (use direct paths for vault; --profile for remote servers). Receipts (`--receipt` or GUI) now include error_correction_* fields when present (P3-03). All via the shared engine (22 tests, live verified 20.1% overhead + full corrupt→repair→extract SHA-256 match). 

(Closes T-AEROVAULT-ECC.)
`--password` / `-p`, `AEROFTP_VAULT_PASSWORD`, or a TTY prompt. `vault
add --receipt <path>` writes a machine-readable JSON receipt and prints a
human-readable summary to stderr (stdout stays clean JSON for piping).
Exit codes: 5 create, 4 add, 1 open/info, 2 extract.

When the global `--profile <name>` is set, `vault add` also records a
per-profile compression aggregate on that saved profile (plaintext vs
compressed bytes and ratio). It surfaces in the optional, default-hidden
`Saved` / `Saved%` columns: `aeroftp-cli profiles --show saved,saved%`
(text and `--json`) and the My Servers table column picker in the GUI.

### rclone-crypt - rclone-Compatible Encrypted Upload

```bash
# Encrypt + upload a file using the rclone crypt format (Standard mode)
AEROFTP_CRYPT_PASSWORD=MySecret \
    aeroftp-cli --profile "S3" rclone-crypt put ./report.pdf /backups/report.pdf

# Obfuscate filename mode (for providers with case-folding issues)
AEROFTP_CRYPT_PASSWORD=MySecret AEROFTP_CRYPT_PASSWORD2=Salt \
    aeroftp-cli --profile "S3" rclone-crypt put ./report.pdf /backups/report.pdf --filename-encryption obfuscate
```

Drop-in compatible with the format produced by [`rclone crypt`](https://rclone.org/crypt/), so a file uploaded here can be read back with `rclone` and vice versa. Separate from the `crypt` subcommand above (which is the AeroFTP-native overlay): use `rclone-crypt` when the bucket must remain interoperable with the rclone toolchain, use `crypt` for AeroFTP-only flows.

### compress / extract - Local Archives

```bash
# Create an archive (format inferred from the output extension)
aeroftp-cli compress backup.zip ./docs ./photos

# Force a format and tune the compression level (0-9)
aeroftp-cli compress backup.tar.gz ./src --archive-format tar.gz -l 9

# AES-256 encrypted zip / 7z (also AEROFTP_ARCHIVE_PASSWORD)
aeroftp-cli compress secret.7z ./private -p "secret"

# Extract an archive into the current directory (or a target dir)
aeroftp-cli extract backup.zip ./restored

# Extract an encrypted archive
aeroftp-cli extract secret.7z ./out -p "secret"
```

Local-only archive operations (no connection). Supported formats: `zip`, `7z`, `tar`, `tar.gz` (alias `tgz`), `tar.xz` (alias `txz`), `tar.bz2` (alias `tbz2`), and `rar` (extract only). The format is inferred from the file extension unless `--archive-format` is given. Only `zip` and `7z` accept an AES-256 `--password`; the tar family is unencrypted. `--level`/`-l` (0-9, default 6; 0 = store) applies to zip and the tar.gz/tar.xz/tar.bz2 families; 7z uses a fixed LZMA2 preset and ignores it. `extract` honors `--subfolder` to unpack into a directory named after the archive.

> **ZIP store-if-larger**: when deflate would not shrink a given file (already-compressed payloads such as JPEG, MP4, or zip), `compress` stores that ZIP entry uncompressed instead of inflating it. This is a per-entry decision inside the ZIP container and does not change the framing of the tar.gz / tar.xz / tar.bz2 formats.

### batch - Execute Script

```bash
aeroftp-cli batch deploy.aeroftp-script
```

Executes a `.aeroftp-script` file containing a sequence of commands. See [Batch Scripting](#batch-scripting) below. The `.aeroftp` extension is reserved for profile-export backups (the GUI Settings -> Backup format) and is rejected here with a rename hint; pipe through stdin with `-` if you genuinely want to script directly from a pipe.

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

### aerorsync - Delta Sync Engine

`aerorsync` is the configuration + diagnostics surface for the AeroFTP native delta sync engine (rsync wire protocol 31 implemented in Rust via `russh` and `xxhash`). It mirrors the GUI **Settings → Native Rsync** toggle so headless deployments and AI agents have the same controls available.

#### `aerorsync mode`: get / set the engine mode

```bash
# Print the current mode (auto / classic / native)
aeroftp-cli aerorsync mode get

# Persist a new mode (writes the same TOML the GUI uses)
aeroftp-cli aerorsync mode set native
aeroftp-cli aerorsync mode set classic
aeroftp-cli aerorsync mode set auto       # default

# JSON output for scripts
aeroftp-cli --json aerorsync mode get
# {"mode":"auto"}
```

**Modes**:

| Mode | Behaviour |
|---|---|
| `auto` | Try the native russh transport first; transparently fall back to the classic `rsync` binary path when not delta-eligible. Default. |
| `classic` | Always use the classic `rsync -e ssh` subprocess path (Unix only). Use when troubleshooting native protocol regressions. |
| `native` | Always use the native russh path; refuse the classic fallback. Use to enforce the cross-OS path on Windows or to validate native-only regressions in CI. |

The mode is process-global (persisted in `~/.config/aeroftp/native-rsync.toml`) so a `set` here is immediately picked up by the GUI on its next launch and by every subsequent CLI invocation.

#### `aerorsync probe`: live test of the russh password transport

Z.4.5 R1 dispatch step (2026-05-14): smoke-test that an SSH endpoint accepts the AeroFTP russh password authentication and runs the requested remote command. Useful to validate an `rsync.<provider>.com:2222` endpoint **before** committing to a full profile. Does NOT consult the vault; credentials are passed explicitly so the probe is a pure diagnostics tool.

```bash
# Probe the password fixture (tests/fixtures/sftp-rsync/docker-compose.password.yml)
docker compose -f src-tauri/tests/fixtures/sftp-rsync/docker-compose.password.yml up -d
AEROFTP_PROBE_PW="testpass" aeroftp-cli aerorsync probe \
    127.0.0.1 testuser \
    --port 2223 \
    --password-env AEROFTP_PROBE_PW \
    --accept-any-host-key

# Probe a real rsync-as-a-service endpoint (FileLu, Hetzner Storage Box, etc.)
read -s -p "Password: " FILELU_RSYNC_PW && export FILELU_RSYNC_PW
aeroftp-cli aerorsync probe \
    rsync.filelu.com aleimob \
    --port 2222 \
    --password-env FILELU_RSYNC_PW \
    --accept-any-host-key

# Custom remote command + JSON output
aeroftp-cli --json aerorsync probe 127.0.0.1 testuser \
    --port 2223 \
    --password-env AEROFTP_PROBE_PW \
    --accept-any-host-key \
    --remote-command "whoami"
# {"exit_code":0,"host":"127.0.0.1","port":2223,"remote_command":"whoami","stderr":"","stdout":"testuser\n","user":"testuser"}
```

**Password sources** (resolution order, first match wins):

1. `--password-env <VAR>` reads from the named environment variable
2. `--password-stdin` reads one line from stdin (`echo "pass" | aeroftp-cli aerorsync probe ... --password-stdin`)
3. Interactive `rpassword` prompt on stderr (when stdin is a TTY)

The password is **never** echoed, **never** logged, **never** included in error messages. The remote banner IS printed on success: it is public information from the server.

**`--accept-any-host-key`**: the probe is intentionally lighter than the production provider and does NOT pin host keys to a TOFU file. The flag is required to acknowledge that the operator is opting into a one-shot smoke; production providers always pin the SHA-256 fingerprint after the first connection.

**Exit codes**:

| Code | Meaning |
|---|---|
| 0 | Connect + remote exec succeeded with exit 0 |
| 4 | Endpoint reachable + auth OK, but the remote command exited non-zero (e.g. `rsync` not installed on the server) |
| 5 | Configuration error (missing password env var, empty password, invalid mode, conflicting flags) |
| 6 | SSH transport failure: connection refused, timeout, or password rejected by server |
| 7 | `aerorsync` cargo feature is not compiled in this binary |

**Build requirement**: the probe requires the binary to be compiled with `--features aerorsync`. The release `.deb`/`.rpm`/`.AppImage` bundles ship with the feature on by default; a custom build that omits it surfaces exit 7 with a clear message.

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

### benchmark - Community Benchmark Suite

```bash
# Default level (light): a few representative ops
aeroftp-cli benchmark --profile "server"

# Heavier levels stress more operations
aeroftp-cli benchmark --profile "server" medium
aeroftp-cli benchmark --profile "server" heavy

# JSON for scripts / community submission
aeroftp-cli benchmark --profile "server" --json
```

Runs a deterministic benchmark across upload, download, list, and stat operations and emits a sanitized, opt-in report shaped after the public schema `aeroftp.benchmark.v1`. **The output is intentionally credential-free**: hostnames, paths, bucket names, and credentials are NOT recorded; only protocol family, region (when known), file sizes, and timings. Use this command to compare providers honestly, to file regression reports against AeroFTP, or to contribute results to the community benchmark.

#### Many small files (`--file-count`)

The single-file size sweep measures raw throughput on one large transfer. A *many small files* workload measures something different and complementary: per-file overhead (connection handshake, request signing, metadata round-trips), which is where S3 typically pulls ahead of WebDAV and FTP. This is a **separate axis** and is never averaged into the single-file numbers.

```bash
# 1000 files of 64 KB each, alongside the default size sweep
aeroftp-cli benchmark --profile "server" --file-count 1000

# Tune the per-file size (e.g. 4 KB to stress metadata cost)
aeroftp-cli benchmark --profile "server" --file-count 5000 --file-size 4K --json
```

When `--file-count N` is set (`--file-size` defaults to `64K`), the run adds five dedicated operations: **`upload-all`**, **`list-dir`** (one listing of the N-file directory), **`stat-all`**, **`download-all`**, and **`delete-all`**. Each reports **`files_per_second`** and a per-file latency distribution (`latency_ms`, whose p50 is the mean per-file time) alongside raw `throughput_mbps` for the transfer ops. The aggregate payload (`file-count` x `file-size`) is capped at 5 GiB and `--file-count` at 100000.

### import - Import Server Profiles

`import` ingests server profiles from external tools and stores them in the AeroFTP encrypted vault. **15 sources** are supported: `rclone`, `winscp`, `filezilla`, `aws`, `ssh`, `mc`, `cyberduck`, `s3cmd`, `lftp`, `putty`, `mobaxterm`, `dreamweaver`, `kopia`, `duplicacy`, `restic`. Use `--json` on any subcommand for scripting; secrets are decoded from each tool's native obfuscation and re-wrapped in AES-256-GCM by the vault on commit. The three original sources (rclone, winscp, filezilla) have dedicated examples below; the other twelve share the same `aeroftp import <tool> [path]` interface (see [import (other tools)](#import-other-tools)).

#### import rclone

```bash
# Auto-detect rclone.conf and list importable remotes
aeroftp-cli import rclone

# Specify config path explicitly
aeroftp-cli import rclone /path/to/rclone.conf

# JSON output for scripting
aeroftp-cli import rclone --json
```

Supports 17 rclone backend types (FTP, SFTP, S3, WebDAV, Google Drive, Dropbox, OneDrive, MEGA, Box, pCloud, Azure Blob, Swift, Yandex Disk, Koofr, Jottacloud, Backblaze B2, OpenDrive). Passwords are de-obfuscated from rclone's reversible AES-256-CTR scheme. See the full [rclone Integration Guide](https://docs.aeroftp.app/features/rclone) for the complete backend mapping table. For existing `rclone crypt` remotes (full read/write interop with transparent encrypted overlay session), see [rclone crypt interoperability](https://docs.aeroftp.app/features/rclone-crypt).

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

#### import (other tools)

The remaining twelve sources use the same `aeroftp import <tool> [path]` interface and `--json` envelope. When no path is given, AeroFTP auto-detects each tool's conventional config location.

```bash
aeroftp-cli import kopia                       # auto-detect repository.config
aeroftp-cli import s3cmd ~/.s3cfg --json       # explicit path + JSON
```

| Tool | Config / file | Protocols | Credentials |
|---|---|---|---|
| `aws` | `credentials` / `config` (INI) | S3 | Full |
| `mc` | `config.json` (JSON) | S3 | Full |
| `s3cmd` | `.s3cfg` (INI) | S3, SFTP, WebDAV | Full |
| `ssh` | `ssh config` | SFTP | Metadata only (key / agent) |
| `putty` | registry `.reg` export | SFTP | Metadata only |
| `mobaxterm` | `MobaXterm.ini` | FTP, FTPS, SFTP, WebDAV, S3 | Limited (host-bound) |
| `lftp` | `rc` / bookmarks | FTP, FTPS, SFTP, WebDAV | Limited |
| `cyberduck` | `.duck` bookmark (plist/XML) | FTP, FTPS, SFTP, WebDAV, S3 | Metadata only (keychain) |
| `dreamweaver` | `.ste` site export (XML) | FTP, FTPS, SFTP, WebDAV, S3 | Full |
| `kopia` | `repository.config` (JSON) | S3, SFTP, WebDAV | Full |
| `restic` | env script (`RESTIC_REPOSITORY` + AWS env) | S3, SFTP, WebDAV | Full |
| `duplicacy` | `preferences` (JSON) | S3, SFTP, WebDAV | Limited |

> **Credentials:** *Full* = the secret is recovered and upgraded into the vault; *Limited* = only part of the secret material (host-bound or optional); *Metadata only* = connection metadata is imported but the secret stays in the OS keychain / SSH agent / an interactive prompt.

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

### export - Export Server Profiles

The reverse direction of `import`: takes the AeroFTP vault and emits a configuration file in the dialect of an external tool, so you can hand off a connection bundle to operators or migrate away. All 15 bridge tools are valid export targets (`rclone`, `winscp`, `filezilla`, `aws`, `ssh`, `mc`, `cyberduck`, `s3cmd`, `lftp`, `putty`, `mobaxterm`, `dreamweaver`, `kopia`, `duplicacy`, `restic`):

```bash
# rclone.conf format (S3, SFTP, FTP, WebDAV, MEGA)
aeroftp-cli export rclone --output ./rclone.conf

# WinSCP.ini format (FTP/FTPS/SFTP)
aeroftp-cli export winscp --output ./WinSCP.ini

# FileZilla sitemanager.xml format
aeroftp-cli export filezilla --output ./sitemanager.xml

# Any other bridge target, e.g. restic env script or s3cmd config
aeroftp-cli export restic --output ./restic-env.sh
aeroftp-cli export s3cmd  --output ./.s3cfg
```

OAuth-based providers (pCloud, Dropbox, Google Drive, Box, OneDrive, Yandex, Zoho, Koofr, Internxt, kDrive) **cannot be exported to rclone** because rclone uses its own OAuth flow with provider-issued client IDs; those entries are emitted as `# manual setup required` comments instead. Passwords for non-OAuth profiles are re-encoded in the target tool's native obfuscation (rclone reversible obscure, WinSCP password mask, FileZilla base64). Use `--json` on any subcommand for a machine-readable summary of what was exported and what was skipped.

### keystore - Encrypted Full-State Backup

The keystore is AeroFTP's portable backup format: a single encrypted `.aeroftp-keystore` file that bundles the entire installation state (vault entries, SQLite databases, plugins, sync snapshots, UI preferences) under a single password. The same envelope is produced and consumed by the GUI **Settings → Backup** flow, so a CLI export can be imported through the GUI and vice versa.

```bash
# Export full state to an encrypted backup
AEROFTP_KEYSTORE_PASSWORD=MyBackupPassword \
    aeroftp-cli keystore export --output ./aeroftp-2026-05-14.aeroftp-keystore

# Vault-only export (skip SQLite + files)
aeroftp-cli keystore export --output ./vault-only.aeroftp-keystore --mode vault-only \
    --password-stdin <<< "$BACKUP_PW"

# Inspect a backup WITHOUT decrypting (cleartext envelope metadata only)
aeroftp-cli keystore info --input ./aeroftp-2026-05-14.aeroftp-keystore --json

# Import a backup, skipping a section if you don't want to overwrite it
AEROFTP_KEYSTORE_PASSWORD=MyBackupPassword \
    aeroftp-cli keystore import --input ./aeroftp-2026-05-14.aeroftp-keystore \
    --merge skip --skip-local-storage
```

**Password resolution chain** for both `export` and `import`: `AEROFTP_KEYSTORE_PASSWORD` env var, then `--password-stdin`, then `--password <pw>` (visible in `ps`, prints a warning), then an interactive `rpassword` prompt on stderr when stdin is a TTY. Backend enforces a minimum password length of 8 characters.

**Modes**: `full` (everything), `vault-only` (server profiles + AI keys + OAuth tokens), `sqlite-only` (chat history, agent memory, file tags, vault history), `files-only` (plugins, sync snapshots).

**Merge strategies on import**: `skip` (default, never overwrite existing vault entries), `overwrite` (force replace), `keep-newer` (compare timestamps). The `--skip-*` flags let you opt out of an entire section per import (vault, sqlite, files, local-storage).

After a successful import that touched SQLite or files, the CLI prints `requires_restart=true` on stdout and exits 0. The AeroFTP GUI must be restarted before the restored databases become visible.

### completions - Generate Shell Completion Scripts

```bash
aeroftp-cli completions bash
aeroftp-cli completions zsh
```

Generates completion scripts for `bash`, `zsh`, `fish`, `elvish`, and `powershell`.

### users - Multi-User Accounts

Since v4.0.0 the vault can be split into per-user encrypted partitions (Argon2id-derived keys, AES partition encryption). The feature is optional: single-user installs are unchanged and an older single-user keystore is migrated automatically. See [MULTI-USER.md](./MULTI-USER.md) for the full model.

```bash
# Manage local users
aeroftp-cli users list
aeroftp-cli users add alice
aeroftp-cli users switch alice            # persists the active user
aeroftp-cli users set-passphrase alice    # set/change; --remove returns to device-wrapped access
aeroftp-cli users rename alice alicia
aeroftp-cli users sort alice bob carol    # order for the GUI dropdown / lock screen
aeroftp-cli users delete alice
aeroftp-cli users lock                    # lock the in-memory session

# Interactive prompt (TTY): re-index (#), Rename (R), Copy (C), Delete (D),
# Fav (F, marks the default user auto-unlocked on launch), List (L), Tree (T)
aeroftp-cli users -i
```

Each user keeps an isolated set of server profiles and AeroSync settings inside its own partition. An opt-in admin role gates user management, with a last-admin guard so an installation cannot lock itself out.

The global `--user <name>` flag runs a single command as a specific user **for that invocation only**, without changing the persistent active user:

```bash
aeroftp-cli --user alice ls --profile "Backup" /
aeroftp-cli --user alice sync --profile "Backup" /local /remote
```

When the selected partition is passphrase-protected, supply it with `--user-passphrase`, `--passphrase-file`, or the `AEROFTP_USER_PASSPHRASE` environment variable; otherwise the CLI prompts on a TTY. `--user` is optional everywhere and defaults to the active user, so existing scripts keep working unchanged.

### groups - Server-Profile Groups

`groups` manages the named group labels on saved profiles (the My Servers group chips). Membership lives in the vault under `config_server_groups`, shared with the GUI, so a change made in the CLI shows up in My Servers and vice versa. The GUI list is drag-reorderable; the CLI exposes the same order via re-index.

```bash
# List the group chips (optionally as JSON)
aeroftp-cli groups
aeroftp-cli groups --json

# Interactive prompt (TTY): re-index (#), Rename (R), Copy (C), Delete (D), List (L)
aeroftp-cli groups -i
```

The interactive prompt shares the same `-i` engine as `profiles -i` and `users -i`: select a target by index or name, `.` refreshes the screen, `h` shows help, and only an explicit Quit exits the sticky loop.

### profiles - List Saved Profiles

```bash
# Print saved profiles (text format)
aeroftp-cli profiles

# Same payload as JSON for scripting
aeroftp-cli profiles --json

# Show the optional, default-hidden compression columns
aeroftp-cli profiles --show name,saved,saved%
```

Lists every server profile in the encrypted vault: display name, protocol, host, optional saved path, and a credential indicator (passwords are never printed). The JSON form is the canonical input for AI agents that need to discover what's available before running anything else.

`--show` is an exclusive allowlist (the pinned `#`/`Name` plus the listed columns), `--hide` is subtractive, and both accept `*`/`all`. Column aliases include `used`, `total`, `pct`, `saved`, `saved%` (alias `savedpct`), `path`, `last`, `fav`. `saved` and `saved%` are the compression telemetry columns (Ehud [#162](https://github.com/axpdev-lab/aeroftp/issues/162)), hidden by default and populated by `aeroftp-cli --profile <name> vault add`; they also appear in `--json` as `lastCompression`.

The bare `profiles` command also opens an **interactive shell** when stdin is a TTY: it prints the list and accepts single-letter actions like `l <selector>` (list root), `t <selector>` (tree depth 2), `d <selector>` (delete with tombstone), `f <selector>` (toggle favourite), `c <selector>` (duplicate), `r <selector>` (rename), `e <selector>` (inline edit). It also accepts `# <selector> <N>` to move a profile to a new 1-based position (`# 3 1` moves profile 3 to the top); the target is clamped to the profile count and the new order is persisted to the vault. After a move the table is reprinted with a visual diff: the moved profile shows as a struck-through ghost at its old slot and a live row at its new slot joined by a left-gutter arrow, and every row whose index shifted is marked `old -> new`. Reordering needs manual order, so when a sort is active (the displayed order differs from the saved order) the move is refused with a hint to re-run with `--sort manual`. On a multi-user vault, `u` lists the users and `u <selector>` switches the active user; the compact forms `u3` and `3u` are accepted as well, matching the other actions' attached-selector tokens (use the spaced `u <name>` form for names containing spaces). Type `?` for the full reference. Selectors can be 1-based indices, exact names, exact ids, or unique substrings.

### profile-add - Create a New Profile

```bash
# SFTP with explicit host
aeroftp-cli profile-add \
    --name "My NAS" \
    --protocol sftp \
    --host nas.local \
    --port 22 \
    --username admin \
    --initial-path /volume1/backups

# Cloud preset (no host required)
aeroftp-cli profile-add \
    --name "My Koofr" \
    --protocol koofr \
    --username myuser
```

Scriptable equivalent of the GUI **New Server** flow and of the interactive shell's `n` action. Writes only the configuration entry; credentials live in a separate vault key set with `profile-set-password` (below) or via the GUI Edit modal.

`--protocol` accepts the lowercase protocol identifier: `ftp`, `ftps`, `sftp`, `webdav`, `webdavs`, `s3`, `mega`, `dropbox`, `googledrive`, `onedrive`, `box`, `pcloud`, `zohoworkdrive`, `fourshared`, `filen`, `internxt`, `kdrive`, `koofr`, `drime`, `filelu`, `yandexdisk`, `opendrive`, `jottacloud`, `azure`, `b2`, `backblaze`, `imagekit`, `uploadcare`, `cloudinary`, `tabdigital`, `felicloud`, `github`, `gitlab`, `immich`, `pixelunion`, `blomp`. Cloud providers that resolve their endpoint from the protocol alone do not require `--host`.

### profile-duplicate - Duplicate a Saved Profile

```bash
# Duplicate "Production" as "Production (copy)"
aeroftp-cli profile-duplicate "Production"

# Duplicate with a custom name
aeroftp-cli profile-duplicate 4 --name "Production - sandbox"
```

Mirrors the **Duplicate** action in the GUI server context menu and the `c` action in the interactive shell. Copies both the configuration entry and any stored credential (`server_<id>` vault key) under a fresh `srv_<ms>_<rand>` id so the copy is identical, including the password, until you change it.

### profile-delete - Delete a Saved Profile

```bash
# Interactive: prompts for y/N on stderr
aeroftp-cli profile-delete "Production - sandbox"

# Skip the prompt (CI / scripts)
aeroftp-cli profile-delete "Production - sandbox" --yes

# JSON output for pipelines
aeroftp-cli --json profile-delete 4 --yes
```

Headless equivalent of the GUI **MyServersPanel** confirmDelete action and of the interactive shell's `d` action. Removes three pieces of state in one atomic step: the profile config entry in `config_server_profiles`, the per-profile credential blob (`server_<id>`), and any membership in `config_favorite_servers`.

Without `--yes` and with stdin attached to a TTY, the command prompts for `y/N` on stderr. **In non-TTY contexts** (CI, pipes, daemons) the absence of `--yes` is a hard error (exit 5) so a script never silently consumes stdin. Exit codes: 0 success or user cancellation, 2 selector not found, 5 vault read/write failure or missing `--yes` in a non-TTY shell.

### profile-set-password - Write Credential into the Vault

```bash
# Single-secret shape: read from env var (recommended)
read -s -p "SSH password: " SSH_PW && export SSH_PW
aeroftp-cli profile-set-password "My NAS" --password-env SSH_PW

# Read from stdin (good for piping from a secret manager)
op read "op://AeroFTP/My NAS/password" \
    | aeroftp-cli profile-set-password "My NAS" --password-stdin

# Inline (not recommended: visible in `ps` and shell history)
aeroftp-cli profile-set-password "Test Server" --password "weak-test-pw"

# JSON credential blob (OAuth tokens, API keys, structured creds)
aeroftp-cli profile-set-password "Backblaze B2" \
    --credential-json '{"applicationKeyId":"K001...","applicationKey":"K001...secret"}'

# JSON blob from a file
aeroftp-cli profile-set-password "Google Drive" \
    --credential-json-file ./oauth-tokens.json
```

Writes the per-protocol credential blob into the vault key `server_<id>`, **the exact same place** the GUI Edit modal writes to. A credential written via the CLI is read back by the GUI on the next reload. Two write modes:

- **Single-secret** (`--password*`): stores the raw secret as the credential value. Matches the FTP / SFTP / WebDAV `password` field on the GUI side.
- **JSON blob** (`--credential-json*`): stores an arbitrary JSON object verbatim. Use this for OAuth tokens, multi-field API key envelopes, or any structured credential. Shape is validated upfront so a typo in the JSON surfaces as exit 5 before the vault write.

The two modes are **mutually exclusive**: pass exactly one source. Inline `--password` emits a "visible in ps" warning; prefer `--password-env` or `--password-stdin` for production.

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

Replaces the boilerplate `connect -> about -> df -> ls /` sequence agents would otherwise run. Returns a single JSON envelope with four blocks: `connect`, `capabilities`, `quota`, `path`. Each block carries one of `ok` / `unsupported` / `unavailable` / `error`. The `capabilities` block also carries a `transfer_capabilities` sub-block; on the discovery path it reports `source: "protocol_defaults"`, and when the backend is actually opened it is upgraded in place to `source: "live_provider"` from the live provider instance (no credentials are exposed). See [Transfer Capabilities by Protocol](#transfer-capabilities-by-protocol). Live-connect allowlist: FTP, FTPS, SFTP, WebDAV, S3, GitHub, GitLab. For other providers (pCloud, Dropbox, OneDrive, Box, Filen, MEGA, Koofr, kDrive, Jottacloud, Drime, FileLu, Yandex, 4shared, Internxt, Swift, Azure, Google Drive, Zoho WorkDrive, Immich) the `connect` block returns `status: "unsupported"` but `capabilities`, `path`, and `profile` stay actionable, and the CLI exits 0. Exit codes: `0` ok or unsupported with valid capabilities, `1` connect attempted and failed, `2` profile lookup failed.

### agent-info - AI Agent Discovery Metadata

```bash
aeroftp-cli agent-info --json
```

Prints structured JSON describing safe/modify/destructive command groups, credential model, output hygiene, saved profile inventory with per-profile auth state, and a `protocol_features` map keyed by protocol (share_links, resume, server_copy, versions, thumbnails, change_tracking) plus an `agent_connect_supported_protocols` array. This is the recommended discovery surface for AI coding agents and the canonical input for capability-aware tool routing.

It also emits the transfer-scheduler surface: a `protocol_transfer_capabilities` map (one block per protocol, `source: "protocol_defaults"`) and a per-profile `transfer_capabilities` block (`source: "profile_defaults"`) under `profiles.servers[]`, alongside the safe profile fields `id`, `name`, `protocol`, `host`, `port`, `username`, `initialPath`, `providerId`. Each block is `{ status, source, capabilities }`, where `capabilities` is the `TransferCapabilities` object documented in [Transfer Capabilities by Protocol](#transfer-capabilities-by-protocol). These fields are additive: consumers reading only `protocol_features` keep working unchanged.

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

## Transfer Capabilities by Protocol

Not every protocol can honor every transfer flag. `--parallel`, `--chunk-size`, `--partial`, and the segmented/delta paths each depend on what the underlying backend actually supports. The CLI does not guess: each protocol advertises a `TransferCapabilities` block that the scheduler reads before deciding how to move bytes. This is the same block exposed as machine-readable JSON by [`agent-info`](#agent-info---ai-agent-discovery-metadata) (`protocol_transfer_capabilities` and per-profile `transfer_capabilities`) and by [`agent-connect`](#agent-connect---single-shot-agent-connect).

The table below is the provider-generic default (no live connection). It is generated from the same source of truth the runtime uses, so a backend never claims a capability the transfer engine will not actually exercise.

| Protocol(s) | Parallel files | Session pool | Concurrent range GET | Multipart upload | Resume DL / UL | Server checksum | API rate-limited |
|---|---|---|---|---|---|---|---|
| FTP, FTPS | yes (8 slots) | yes | no | no | yes / yes | no | no |
| SFTP | no (1 slot) | no | no | no | no / no | no | no |
| S3 | no (1 slot) | no | yes | yes (4 parts, 5 MiB) | yes / no | yes (ETag) | no |
| Azure Blob | no (1 slot) | no | yes | no | yes / no | no | no |
| OpenStack Swift | no (1 slot) | no | no | no | yes / no | no | no |
| WebDAV, Koofr | no (1 slot) | no | after probe | no | yes / no | no | no |
| Backblaze B2 | no (1 slot) | no | no | yes (4 parts, 100 MiB) | no / no | no | no |
| Google Drive, Google Photos, Dropbox, OneDrive, Box, pCloud, Zoho WorkDrive, 4shared, Yandex Disk, kDrive, Jottacloud, Drime, FileLu, OpenDrive | no (1 slot) | no | no | no | no / no | no | yes |
| GitHub, GitLab | no (1 slot) | no | no | no | no / no | no | yes |
| AeroCloud, MEGA, Filen, Internxt, Immich, ImageKit, Uploadcare, Cloudinary | no (1 slot) | no | no | no | no / no | no | no |

Legend: **yes** = `supported`, **no** = `unsupported`, **after probe** = `supported_after_probe` (the capability is exercised only after a runtime range probe confirms the server honors `Range:` correctly). "Parallel files" is `file_parallel` with its `max_file_slots`; "Multipart upload" lists `max_chunk_slots` and `preferred_chunk_size` when the backend sets one. The JSON block also carries `max_checker_slots` (listing/verify fan-out) and the remaining `TransferCapabilities` fields (`offset_upload`, `upload_session`, `server_side_copy`, `list_parallel`, `batch_list`, `atomic_rename`); per-protocol `server_side_copy` and the feature tokens (`share_links`, `versions`, ...) live in the `protocol_features` map.

Notes on honesty of the matrix:

- **FTP/FTPS** advertise file-level parallelism through the session pool (up to 8 concurrent file slots) rather than strict concurrent range GET on a single file; the capability surface deliberately does not overclaim single-file range parallelism for FTP.
- **SFTP** reports a single-lease provider-generic profile by design (the shared SFTP pool is not the provider-generic path). SFTP-specific acceleration is still available and is documented separately: byte-level delta via [`--delta`](#get---download-files) and segmented single-file download via [`pget`](#pget---segmented-parallel-download).
- The discovery surfaces report `source: "profile_defaults"` (`agent-info` per-profile) or `source: "protocol_defaults"` (`agent-info` `protocol_transfer_capabilities`, `agent-connect` discovery path). Only a real connection upgrades a block to `source: "live_provider"`, which can differ from the defaults above when a specific server exposes more or fewer primitives.

Per-file size limits are intentionally not a column here. A maximum single-file size is a provider plan or account policy, not a transfer-scheduler capability, so it is not part of the generated `TransferCapabilities` block this table is built from. For those limits see [wrapper-stack: Free-tier max single file](architecture/wrapper-stack.md#chunking-in-depth) (the largest single file a provider's free plan accepts, which AeroFTP's chunking is built to bypass) and [PROVIDER-INTEGRATION-GUIDE: Upload Pattern Summary](PROVIDER-INTEGRATION-GUIDE.md#upload-pattern-summary) (the API single-request ceiling before a chunked upload strategy is needed).

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

## Additional subcommands

These commands round out discovery, file preview, and profile management.

### `peek` - smart file preview

```bash
aeroftp-cli peek --profile NAME /path/file [-n N] [--bytes B] [--tail]
```

Smart preview of a remote file: by default the first 20 lines, `--tail` for the
last lines, `--bytes B` for a binary-safe byte slice (base64 in JSON when the
slice is not valid UTF-8).

### `catalog` - provider catalog (SSOT)

```bash
aeroftp-cli catalog [--json] [--free] [--paid]
```

Prints the built-in provider catalog (the same single source of truth that drives
the GUI Discover Services view): protocol, auth model, free/paid tier, storage
region, and website. `--free` / `--paid` filter by pricing tier.

### `profile-duplicate-mode` / `profile-convert-mode` - switch a profile's mode

```bash
aeroftp-cli profile-duplicate-mode SELECTOR --mode <api|webdav|s3|ftp> [--name NAME] [--username USER] [--password-stdin]
aeroftp-cli profile-convert-mode   SELECTOR --mode <api|webdav|s3|ftp> [--name NAME] [--username USER] [--password-stdin] [--yes]
```

`profile-duplicate-mode` creates a second profile for the same account in a
different access mode (e.g. FileLu API key vs WebDAV), cloning the source
credential when the modes share one. `profile-convert-mode` does the same but
**replaces** the original (keeping its slot), like the GUI's "Convert to ..."
button; it prompts for confirmation on a TTY unless `--yes` is given. Prefer
`--password-env VAR` or `--password-stdin` over inline `--password` so the secret
never lands in argv or shell history.

### `profile-copy-user` / `profile-move-user` - relocate a profile between users

```bash
aeroftp-cli profile-copy-user SELECTOR --to-user NAME
aeroftp-cli profile-move-user SELECTOR --to-user NAME [--yes]
```

Multi-user installs only: copy (or move) a saved profile from the active user's
encrypted partition into another user's partition. `profile-move-user` removes the
source after the copy and prompts for confirmation on a TTY unless `--yes` is
given. See [MULTI-USER.md](./MULTI-USER.md) for the partition model.

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
| 130 | Interrupted by SIGINT (Ctrl+C); second Ctrl+C force-exits |

```bash
aeroftp-cli connect sftp://user@host
echo "Exit code: $?"
```

---

## Batch Scripting

Create `.aeroftp-script` files for automated workflows:

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
aeroftp-cli batch deploy.aeroftp-script
aeroftp-cli batch deploy.aeroftp-script --json    # JSON output for all commands
aeroftp-cli batch deploy.aeroftp-script --quiet   # Errors only
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
# deploy.aeroftp-script
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

- **v4.0.x - AeroVault from the CLI + multi-user**: single-file `.aerovault` create/add/extract/info for the v1/v2/v3 families, Reed-Solomon error correction (`vault create --error-correction` / `--ec`, `vault info` with an `ecc` object, `vault scrub <path>`, `vault repair <path> [--dry-run]`, all with `--json` output) shipped in v4.0.5, and per-user encrypted vault partitions (Argon2id-derived keys, the `users` subcommand and `--user` global flag) shipped since v4.0.0.
- **v3.7.2 - CLI security hardening + community polish**: Codex CLI external audit closes 17 paired findings (CLI-AUDIT-01..17). Highlights: GUI tool execution now enforces backend approval, MCP / AI core remote dispatcher path validation (null bytes, traversal, control chars, option-like forms, length cap), `server_exec` strictly read-only (rejects get/put/mkdir/rm/mv with explicit-use error), MCP profile lookup requires exact id/name or unique substring (no silent first-match), `local_copy_files` and `local_stat_batch` validate every path including symlink rejection, SFTP packet parser bounds-checked end to end, `.aerotmp` writes use `create_new` and refuse symlinked temp paths, inline upload temp files use `tempfile::Builder::tempfile()`, daemon auth token created with `O_NOFOLLOW` + mode 0600, `sync --direction <invalid>` fails before connecting with exit code 5, `sync-doctor` resolves remote paths the same way `sync` does, `sync-doctor --checksum` no longer suggests the non-existent flag, `transfer` checks cancellation between plan and execution returning exit code 130, `agent-info --json` treats missing profile list as empty, CLI help footer documents the extended exit-code contract (9, 10, 11, 130). CLI `profiles` dynamic terminal-width-aware layout (Ehud, #161), unified `--breakdown` table folding TOTAL into the breakdown rows, `--hide=fav` / `favorite` / `favourite` / `favs` alias surface documented. Direct `rsa = "0.9"` dependency dropped, `jsonwebtoken` switched to `aws-lc-rs`. `audit.toml` documents the two remaining transitive RSA paths (sigstore, russh) with written threat-model justifications.
- **v3.7.1 - Mount Manager + community polish**: GUI Mount Manager dialog wraps `aeroftp-cli mount` with persistent configs, sidecar JSON or vault-backed storage, cross-platform autostart (systemd-user / Task Scheduler ONLOGON), and an "Open mount in file manager" shortcut. CLI `profiles -i` interactive prompt loop with compact `1l` / `2t` / `3d` tokens. Filen Desktop local WebDAV / S3 bridges connect on the first try thanks to the layered WebDAV scheme detection rewrite.
- **v3.7.0 - AeroRsync session-cached batch + crypto overlay**: new `AerorsyncBatch` trait amortizes one SSH session across many delta transfers; `SyncReport` exposes `delta_files[]` and `bytes_on_wire`. Cross-profile transfer (`aeroftp_transfer`, `aeroftp_transfer_tree`) and six new ops tools (`aeroftp_touch`, `aeroftp_cleanup`, `aeroftp_speed`, `aeroftp_sync_doctor`, `aeroftp_dedupe`, `aeroftp_reconcile`) bring MCP to 39 tools. rclone crypt becomes full read/write through transparent overlay session; AeroVault gets matching overlay-session model.
- **v3.6.1 - Windows first-class delta sync**: native rsync protocol 31 in pure Rust (`aerorsync`), no `rsync.exe` bundle, no WSL requirement. The Windows binary now performs delta uploads byte-identical to stock rsync 3.4.1 in CI.
- **v3.5.4 - MCP hardening**: `aeroftp-cli mcp` top-level alias, vault auto-init in MCP, per-profile serialization, schema validation, S3 bucket fix, FTP/SFTP/WebDAV/Filen/FileLu/Drime/Immich error message hardening.
- **v3.5.3 - Continuous bidirectional `sync --watch`**: native filesystem watcher (inotify/FSEvents/ReadDirectoryChangesW) with anti-loop cooldown, periodic rescan, NDJSON output. No external sync daemon required.
- **v3.5.3 - Agent-friendly flags**: `--files-from`, `--immutable`, `--no-check-dest`, `--max-depth`, `--inplace`, `--fast-list` (S3), `--compare-dest`/`--copy-dest`, `cleanup` for orphan `.aerotmp`.
- **v3.5.2 - Determinism & threat model**: 12 structured exit codes mapping all `ProviderError` variants, `mkdir --parents`, `rm --force`, `put --no-clobber`, `--chunk-size`/`--buffer-size` overrides, [`docs/THREAT-MODEL.md`](THREAT-MODEL.md), [`docs/LLM-INTEGRATION-GUIDE.md`](LLM-INTEGRATION-GUIDE.md).
- **v3.3.4 - Local server bridges**: `serve http` (read-only HTTP, Range requests), `serve webdav` (read-write, axum-based), `serve ftp` / `serve sftp` (anonymous read-write).

---

*AeroFTP CLI is part of the [AeroFTP](https://github.com/axpdev-lab/aeroftp) project - GPL-3.0*
