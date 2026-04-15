# AeroFTP LLM Integration Guide

> Version: 1.0
> Date: 2026-04-15
> For: LLM/AI agent developers integrating with AeroFTP CLI or MCP server

---

## Integration Surfaces

AeroFTP exposes 3 integration points for LLMs:

| Surface | Protocol | Best For |
|---------|----------|----------|
| **CLI** (`aeroftp-cli`) | stdin/stdout + exit codes | Tool use / function calling (any LLM) |
| **MCP Server** (`aeroftp-cli mcp`) | JSON-RPC over stdin/stdout | Claude Desktop, Cursor, VS Code Copilot |
| **AeroAgent** (built-in) | Tauri IPC | Desktop app users (19 AI providers) |

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
# BAD - injection risk, no error handling
aeroftp-cli agent --connect "Server" "run: ls /data && rm -rf /tmp/*"

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

### Starting the MCP Server

```bash
aeroftp-cli mcp --profile "Server"
```

The MCP server communicates via JSON-RPC over stdin/stdout.

### Rate Limits

| Category | Limit | Tools |
|----------|-------|-------|
| List/Read | 60/min | list_directory, read_file, stat, search |
| Write | 30/min | upload_file, create_directory, rename |
| Delete | 10/min | delete_file, delete_directory |

### Available Tools (16)

| Tool | Danger | Description |
|------|--------|-------------|
| list_directory | safe | List files in a directory |
| read_file | safe | Read file content (text, with size limit) |
| stat | safe | Get file/directory metadata |
| search | safe | Search for files by name pattern |
| get_quota | safe | Get storage quota info |
| download_file | medium | Download file to local path |
| upload_file | medium | Upload local file to remote |
| create_directory | medium | Create a remote directory |
| rename | medium | Rename/move a file or directory |
| copy | medium | Server-side copy (if supported) |
| delete_file | high | Delete a remote file |
| delete_directory | high | Delete a remote directory |
| list_profiles | safe | List saved server profiles |
| connect | safe | Connect to a saved profile |
| disconnect | safe | Disconnect from current server |
| server_info | safe | Get server/protocol information |

### MCP Best Practices

1. **Always specify absolute paths** - relative paths resolved against server root
2. **Use list_directory before write operations** - verify target exists
3. **Prefer stat over list_directory for single files** - lower overhead
4. **Check server_info for capabilities** - not all operations supported everywhere

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
- Available commands with syntax
- Supported protocols (28)
- Exit code definitions
- Safety rules
- Credential model
- Saved server profiles (names only, no passwords)

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

*For the full threat model, see [THREAT-MODEL.md](THREAT-MODEL.md). For AI tool schema, see [TOOL-SCHEMA.md](TOOL-SCHEMA.md).*
