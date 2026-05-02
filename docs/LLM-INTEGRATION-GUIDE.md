# AeroFTP LLM Integration Guide

> Version: 1.1
> Date: 2026-04-27
> For: LLM/AI agent developers integrating with AeroFTP CLI or MCP server

---

## Integration Surfaces

AeroFTP exposes 3 integration points for LLMs:

| Surface | Protocol | Best For |
|---------|----------|----------|
| **CLI** (`aeroftp-cli`) | stdin/stdout + exit codes | Tool use / function calling (any LLM) |
| **MCP Server** (`aeroftp-cli mcp`) | JSON-RPC over stdin/stdout | Claude Desktop, Cursor, VS Code Copilot |
| **AeroAgent** (built-in) | Tauri IPC | Desktop app users (24 AI providers) |

---

## CLI Integration

### Output Contract

| Stream | Content | Format |
|--------|---------|--------|
| **stdout** | Data only (file listings, content, JSON) | Stable, parseable |
| **stderr** | Progress, warnings, errors | Informational, not for parsing |

Always use `--json` for machine-readable output. Always redirect stderr: `2>/dev/null` or `2>log.txt`.

### Exit Codes

| Code | Meaning | Retryable | Recovery Action |
|------|---------|-----------|-----------------|
| 0 | Success | - | - |
| 1 | Connection failed | Yes | Retry with backoff |
| 2 | Not found | No | Check path exists |
| 3 | Permission denied | No | Check credentials |
| 4 | Transfer failed | Yes | Retry, check disk space |
| 5 | Invalid usage | No | Fix command syntax |
| 6 | Auth failed | No | Re-authenticate |
| 7 | Not supported | No | Use different operation |
| 8 | Timeout | Yes | Increase timeout, retry |
| 9 | Already exists / not empty | No | Skip or use `--force` |
| 10 | Server/parse error | Maybe | Check server status |
| 11 | Local I/O error | Maybe | Check disk, permissions |
| 99 | Unknown error | Maybe | Inspect error message |
| 130 | Interrupted (Ctrl+C) | - | User cancelled |

### JSON Error Structure

All errors follow this schema (emitted to **stderr**):

```json
{
  "status": "error",
  "error": "human-readable message",
  "code": 2
}
```

### JSON Success Structure

```json
{
  "status": "ok",
  "message": "Created directory: /data"
}
```

Transfer results include additional fields:

```json
{
  "status": "ok",
  "operation": "upload",
  "path": "/remote/file.txt",
  "bytes": 1048576,
  "elapsed_secs": 2.3,
  "speed_bps": 455903
}
```

---

## Safe Patterns

### Pattern 1: List-Filter-Act

```bash
# 1. List (read-only, safe)
aeroftp-cli ls --profile "Server" /data/ --json 2>/dev/null

# 2. Filter (local processing)
# (agent filters JSON result)

# 3. Act (with confirmation)
aeroftp-cli get --profile "Server" /data/target.csv ./local/ --json 2>/dev/null
```

### Pattern 2: Idempotent Directory Setup

```bash
# Create directory tree (no error if exists)
aeroftp-cli mkdir --profile "Server" /data/2026/04/ -p --json 2>/dev/null
# Exit code: always 0
```

### Pattern 3: Safe Upload (no overwrite)

```bash
# Upload only if remote doesn't exist
aeroftp-cli put --profile "Server" ./report.csv /data/ -n --json 2>/dev/null
# Exit code: 0 (uploaded) or 9 (skipped, already exists)
```

### Pattern 4: Idempotent Delete

```bash
# Delete file, no error if already gone
aeroftp-cli rm --profile "Server" /data/old-file.csv -f --json 2>/dev/null
# Exit code: always 0
```

### Pattern 5: Pre-flight Check Before Sync

```bash
# Dry-run first (no changes)
aeroftp-cli sync --profile "Server" ./local/ /remote/ --dry-run --json 2>/dev/null

# Only proceed if dry-run succeeds
aeroftp-cli sync --profile "Server" ./local/ /remote/ --json 2>/dev/null
```

### Pattern 6: Retry with Exit Code Branching

```python
import subprocess, time

def run_with_retry(cmd, max_retries=3):
    for attempt in range(max_retries):
        result = subprocess.run(cmd, capture_output=True, text=True)
        code = result.returncode

        if code == 0:
            return json.loads(result.stdout)
        elif code in (5, 6, 7, 9):  # Non-retryable
            raise Exception(f"Fatal error (code {code}): {result.stderr}")
        elif code in (1, 4, 8, 10, 11):  # Retryable
            time.sleep(2 ** attempt)
            continue

    raise Exception(f"Max retries exceeded")
```

---

## Anti-Patterns

### DO NOT: Parse Text Output

```bash
# BAD - text format is locale-dependent and unstable
aeroftp-cli ls --profile "Server" /data/ | grep "\.csv"

# GOOD - use JSON
aeroftp-cli ls --profile "Server" /data/ --json 2>/dev/null | jq '.[] | select(.name | endswith(".csv"))'
```

### DO NOT: Chain Shell Commands via shell_execute

```bash
# BAD - shell injection risk, no error handling, mixes data and control
aeroftp-cli agent --provider ollama --message "run: ls /data && rm -rf /tmp/*"

# GOOD - use batch files for multi-step operations
cat > workflow.aeroftp << 'EOF'
CONNECT sftp://server
LS /data
EOF
aeroftp-cli batch workflow.aeroftp --json 2>/dev/null
```

### DO NOT: Embed Credentials in URLs

```bash
# BAD - password in process list, shell history, logs
aeroftp-cli ls sftp://admin:p4ssw0rd@server/data/

# GOOD - use encrypted vault profiles
aeroftp-cli ls --profile "Production Server" /data/ --json 2>/dev/null
```

### DO NOT: Assume Provider Capabilities

```bash
# BAD - not all providers support server-side copy
aeroftp-cli cp --profile "Server" /old.txt /new.txt

# GOOD - check capabilities first
capabilities=$(aeroftp-cli agent-info --json 2>/dev/null)
# Then check if server_copy is supported before calling
```

### DO NOT: Retry Non-Retryable Errors

```bash
# BAD - retrying auth failure wastes time and may trigger lockout
while ! aeroftp-cli ls --profile "Server" /data/ --json 2>/dev/null; do
    sleep 5
done

# GOOD - check exit code before retrying
aeroftp-cli ls --profile "Server" /data/ --json 2>/dev/null
code=$?
if [ $code -eq 6 ]; then
    echo "Authentication failed - fix credentials" >&2
    exit 1
fi
```

---

## MCP Server Integration

### VS Code Extension (Recommended)

Install the [AeroFTP MCP Server](https://marketplace.visualstudio.com/items?itemName=axpdev-lab.aeroftp-mcp) extension to auto-configure the MCP server for Claude Code, Claude Desktop, Cursor, and Windsurf with one click.

### Starting the MCP Server

```bash
aeroftp-cli mcp
```

The MCP server communicates via JSON-RPC 2.0 over stdin/stdout. It is multi-server by design: every tool call carries a `server` argument naming a saved profile, so a single MCP process can route operations across the whole vault. Auto-initializes the Universal Vault, or falls back to `AEROFTP_MASTER_PASSWORD` when set. Per-profile tool calls are serialized.

### Rate Limits

| Category | Limit | Examples |
|----------|-------|----------|
| ReadOnly | 60/min | `aeroftp_list_files`, `aeroftp_read_file`, `aeroftp_file_info`, `aeroftp_search_files`, `aeroftp_storage_quota`, `aeroftp_list_servers`, `aeroftp_check_tree`, `aeroftp_agent_connect`, `aeroftp_mcp_info`, `aeroftp_head`, `aeroftp_tail`, `aeroftp_tree`, `aeroftp_hashsum`, `aeroftp_sync_doctor`, `aeroftp_reconcile`, `aeroftp_dedupe` |
| Mutative | 30/min | `aeroftp_upload_file`, `aeroftp_upload_many`, `aeroftp_create_directory`, `aeroftp_rename`, `aeroftp_edit`, `aeroftp_download_file`, `aeroftp_sync_tree`, `aeroftp_close_connection`, `aeroftp_transfer`, `aeroftp_transfer_tree`, `aeroftp_touch`, `aeroftp_speed` |
| Destructive | 10/min | `aeroftp_delete`, `aeroftp_delete_many`, `aeroftp_cleanup` |

### Available Tools (39 canonical, v3.7.0)

The canonical MCP tool set uses the `aeroftp_` prefix. Each tool also ships a matching `remote_*` alias for callers that prefer the cross-profile naming convention.

| Tool | Category | Description |
|------|----------|-------------|
| `aeroftp_list_servers` | ReadOnly | List saved server profiles in the vault (filters: `name_contains`, `protocol`, `limit`, `offset`) |
| `aeroftp_agent_connect` | ReadOnly | Single-shot connect probe: connect / capabilities / quota / path in one envelope |
| `aeroftp_mcp_info` | ReadOnly | MCP process diagnostics (started_at, uptime_secs, protocol coverage) |
| `aeroftp_list_files` | ReadOnly | List files in a remote directory (glob, `name_contains`, `recursive`, `limit`) |
| `aeroftp_read_file` | ReadOnly | Read a remote text file with `preview_kb` window and soft-truncate inside the 1 MB hard cap |
| `aeroftp_file_info` | ReadOnly | Stat a remote file or directory (size, mtime, type, hash where supported) |
| `aeroftp_search_files` | ReadOnly | Search remote tree by name pattern (glob, extension filters) |
| `aeroftp_storage_quota` | ReadOnly | Storage quota / usage where the protocol exposes it |
| `aeroftp_head` / `aeroftp_tail` | ReadOnly | First / last N lines of a remote text file |
| `aeroftp_tree` | ReadOnly | Recursive directory tree, depth-capped |
| `aeroftp_hashsum` | ReadOnly | Server-side checksum (SHA-256 / SHA-1 / MD5 / BLAKE3) with provider fallback |
| `aeroftp_check_tree` | ReadOnly | Compare local vs remote tree with two-sided checksum, per-group caps (`max_match`, `max_differ`, `max_missing_local`, `max_missing_remote`), `omit_match`, `compare_method` flag per entry |
| `aeroftp_sync_doctor` | ReadOnly | Preflight risk summary with `suggested_next_command` (lighter than `sync_tree dry_run`) |
| `aeroftp_reconcile` | ReadOnly | Categorized size-only diff variant of `check_tree` with `elapsed_secs` + `suggested_next_command` |
| `aeroftp_dedupe` | ReadOnly | SHA-256 duplicate detection grouped per size — modes `newest` / `oldest` / `largest` / `smallest` / `list`, dry-run by default |
| `aeroftp_upload_file` | Mutative | Upload one local file (`create_parents`, `no_clobber`) |
| `aeroftp_upload_many` | Mutative | Batch upload from a `files: []` array |
| `aeroftp_download_file` | Mutative | Download one remote file with progress stream |
| `aeroftp_create_directory` | Mutative | Create a remote directory (idempotent with `parents`) |
| `aeroftp_rename` | Mutative | Rename / move a remote file or directory |
| `aeroftp_edit` | Mutative | Find-and-replace on a remote UTF-8 text file (no full download) |
| `aeroftp_sync_tree` | Mutative | Bidirectional sync with `plan[]` (per-file decision) and `summary.delta_files[]` (per-file delta breakdown) |
| `aeroftp_transfer` | Mutative | Cross-profile single-file copy between two saved profiles |
| `aeroftp_transfer_tree` | Mutative | Cross-profile recursive directory copy (`max_files` cap, `summary_only`, `dry_run`) |
| `aeroftp_touch` | Mutative | Create empty file or report `action: "exists"` |
| `aeroftp_speed` | Mutative | Throughput probe (random payload upload + download + SHA-256 integrity + cleanup) |
| `aeroftp_close_connection` | Mutative | Close a pooled server connection explicitly |
| `aeroftp_delete` | Destructive | Delete a remote file or directory |
| `aeroftp_delete_many` | Destructive | Batch delete with caps + configurable backoff |
| `aeroftp_cleanup` | Destructive | BFS scan for orphan `.aerotmp` partial-transfer files (dry-run by default) |

### MCP Resources

In addition to tools, the server exposes the resource URI `aeroftp://connections` so clients can introspect the active connection pool (server name, protocol, last-used timestamp) without invoking a tool. Useful for clients that surface a "running tasks" panel.

### MCP Best Practices

1. **Always specify absolute paths** - relative paths are resolved against the server's working directory, which varies by protocol.
2. **Run `aeroftp_agent_connect` first** when targeting a saved profile - one round trip returns connect / capabilities / quota / path in a single envelope.
3. **Use `aeroftp_check_tree` before `aeroftp_sync_tree`** when the agent needs to confirm what would change.
4. **Prefer batch tools** (`aeroftp_upload_many`, `aeroftp_delete_many`) over loops to amortize connection setup and let the server pace the per-item backoff.
5. **Listen for `notifications/progress`** during uploads, downloads, and sync operations when you pass a `progressToken` in the request - the server emits coalesced progress samples through the lifetime of the call.

---

## Batch Scripting for Agents

Batch files (`.aeroftp`) are safer than shell scripts for agents:

```
SET SERVER=sftp://backup-server
CONNECT {SERVER}

# Idempotent setup
MKDIR /backups/2026-04-15

# Upload with error handling
ON_ERROR CONTINUE
PUT ./report.csv /backups/2026-04-15/
PUT ./summary.pdf /backups/2026-04-15/
ON_ERROR FAIL

DISCONNECT
```

**Advantages over shell scripts**:
- Single-pass variable expansion (no shell injection)
- `ON_ERROR CONTINUE` for fault tolerance
- No shell metacharacter interpretation
- Deterministic execution order
- 1MB file limit (prevents runaway scripts)
- Max 256 variables (prevents memory exhaustion)

---

## Agent Discovery

Use `agent-info` for machine-readable capability discovery:

```bash
aeroftp-cli agent-info --json 2>/dev/null
```

Returns:
- Available commands with syntax (49 subcommands)
- Supported protocols and provider integrations (7 transport protocols + 20+ native provider integrations)
- Per-protocol `protocol_features` map (`share_links`, `resume`, `server_copy`, `versions`, `thumbnails`, `change_tracking`)
- `agent_connect_supported_protocols` array for the live-connect allowlist
- Exit code definitions
- Safety rules
- Credential model
- Saved server profiles with per-profile `auth_state` (names only, no passwords)

---

## Performance Tuning

### Chunk Size Override

```bash
# Large files on S3 (64 MB parts instead of default 5 MB)
aeroftp-cli put --profile "S3" --chunk-size 64M ./large-file.tar.gz /backups/

# SFTP with larger buffer (256 KB instead of default 32 KB)
aeroftp-cli get --profile "NAS" --buffer-size 256K /data/big-file.bin ./local/
```

### Parallel Transfers

```bash
# 8 concurrent file transfers (default: 4)
aeroftp-cli put --profile "Server" ./data/ /remote/ -r --parallel 8

# Segmented download (4 parallel chunks per file)
aeroftp-cli get --profile "S3" /data/large.bin ./local/ --segments 4
```

---

## Security Considerations for Agents

1. **Never log credentials** - use `--profile`, not URL-embedded passwords
2. **Validate paths** - the CLI validates internally, but agents should check results
3. **Use `--dry-run`** before sync operations to preview changes
4. **Confirm destructive operations** - `rm`, `rm -rf`, `sync --delete` require user confirmation unless `--force`
5. **Monitor exit codes** - distinguish retryable (1,4,8) from fatal (5,6,7,9) errors
6. **Use `--no-clobber`** for uploads where overwrite is not intended

---

*For the full threat model see [THREAT-MODEL.md](THREAT-MODEL.md). For the AeroAgent tool surface and end-to-end agent capabilities see [AEROAGENT.md](AEROAGENT.md) and [AEROAGENT-CAPABILITIES.md](AEROAGENT-CAPABILITIES.md). For the per-provider feature matrix see [PROTOCOL-FEATURES.md](PROTOCOL-FEATURES.md).*
