# AeroAgent - AI-Powered File Management

AeroAgent is AeroFTP's integrated AI assistant with **68 tools** across 8 categories. It can create, read, edit, and manage files locally and on remote servers using natural language commands.

> Full test results, prompt examples, and provider compatibility matrix available at [docs.aeroftp.app/aeroagent](https://docs.aeroftp.app/aeroagent)

## Supported AI Providers

AeroAgent works with **24 AI providers** - choose your preferred model:

| Provider | Tool Calling | Streaming | Vision | Thinking |
|----------|:---:|:---:|:---:|:---:|
| OpenAI (GPT-4o, o3) | Yes | Yes | Yes | Yes |
| Anthropic (Claude) | Yes | Yes | Yes | Yes |
| Google Gemini | Yes | Yes | Yes | Yes |
| xAI (Grok) | Yes | Yes | Yes | Yes |
| Cohere (Command A) | Yes | Yes | Yes | Yes |
| DeepSeek | Yes | Yes | - | Yes |
| Mistral | Yes | Yes | - | - |
| Groq | Yes | Yes | - | - |
| Qwen (Alibaba) | Yes | Yes | Yes | Yes |
| Kimi (Moonshot) | Yes | Yes | - | - |
| Ollama (Local) | Yes | Yes | Varies | Varies |
| OpenRouter | Yes | Yes | Varies | Varies |
| Together AI | Yes | Yes | - | - |
| Perplexity | - | Yes | - | - |
| AI21 Labs | Yes | Yes | - | - |
| Cerebras | Yes | Yes | - | - |
| SambaNova | Yes | Yes | - | - |
| Fireworks AI | Yes | Yes | - | - |
| Nvidia (NIM) | Yes | Yes | - | - |
| Z.AI (GLM) | Yes | Yes | - | Yes |
| Hyperbolic | Yes | Yes | - | - |
| Novita | Yes | Yes | - | - |
| Yi (01.AI) | Yes | Yes | - | - |
| Custom (OpenAI-compatible) | Yes | Yes | - | - |

## Tool Categories

The 68 tools span eight categories. Each tool carries a danger level (read-only / medium / high); every medium or high tool requires explicit user approval before it runs.

### Remote operations (21 tools)
List, read, stat, search, upload, download, create directories, rename, edit, delete, hash, head/tail, tree, and check quota on any saved server. These route through the `StorageProvider` trait, so they behave identically across all 7 transport protocols and 25+ native provider integrations.

```
> Connect to my-server.com and list the contents of /var/www/html/
```
```
AeroAgent: Connected to my-server.com via FTP/TLS
  / css/           FOLDER   2026-03-15
  / js/            FOLDER   2026-03-15
  / index.html     4.2 KB   2026-03-18
  / favicon.ico    1.1 KB   2026-03-10
```

### Local file operations (21 tools)
Create, read, write, edit, move, copy, rename, trash, and inspect files on the local filesystem, plus grep, head, tail, diff, tree, batch stat, disk usage, and duplicate detection.

```
> Create three files in the project folder: index.html with a dark theme,
  style.css with custom colors, and app.js with a console.log.
```
```
AeroAgent: Created 3 files:
  + index.html (496 bytes)
  + style.css (286 bytes)
  + app.js (22 bytes)
```

```
> Search for "TODO" in all files, then show the directory tree
```
```
AeroAgent: 2 match(es) in 3 files for "TODO"
  app.js:15 - // TODO: add error handling
  config.json:3 - // TODO: update API key

  project/
  ├── app.js (1.2 KB)
  ├── config.json (256 B)
  └── styles/
      └── main.css (890 B)
```

### Cross-profile transfer (4 tools)
Upload and download in bulk, and copy files directly between two saved profiles without staging them locally.

```
> Upload the entire build/ folder to the remote server
```
```
AeroAgent: Uploaded 12 file(s) to /www/build/
  + build/index.html
  + build/css/style.css
  + build/js/app.js
  ...
```

### Sync & verification (7 tools)
Preview and run directory syncs, preflight risk checks, reconcile and dedupe remote trees, clean up orphan temp files, control background sync, and benchmark throughput.

### Archives (2 tools)
Compress and extract ZIP, 7z, TAR, GZ, XZ, BZ2 archives.

```
> Compress all files in the project/ folder into project.zip
```
```
AeroAgent: Created /project/project.zip (357 bytes)
```

### System & shell (4 tools)
Execute system commands with captured stdout/stderr and exit codes, read/write the clipboard, and hash local files.

```
> Run "uname -a" to check the system info
```
```
AeroAgent: uname -a - exit code 0
  Linux server 6.17.0-19-generic #19~24.04.2-Ubuntu SMP x86_64
```

### Knowledge & memory (3 tools)
RAG indexing, full-text search across your workspace, and persistent per-project memory notes.

```
> Index the current project and search for authentication-related code
```
```
AeroAgent: Indexed 45 files. Found 3 matches for "authentication":
  - auth.ts:12 - export function validateToken()
  - middleware.ts:8 - const authGuard = ...
  - config.ts:25 - AUTH_ENDPOINT: "https://..."
```

### App & server control (6 tools)
Inspect the app and vault state, switch theme, preview an edit before applying, list saved profiles, and run vault-backed operations on any saved server (`server_exec`) without exposing credentials to the model.

## Multi-Step Workflows

AeroAgent executes complex tasks autonomously with tool chaining:

| Workflow | Tools Used | Steps |
|----------|-----------|:---:|
| Create website + deploy | `local_mkdir` → `local_write` x4 → `upload_files` | 6 |
| Read + edit + upload | `local_read` → `local_edit` → `upload_files` | 3 |
| Backup + compress | `local_copy_files` → `archive_compress` | 2 |
| Audit remote server | `server_list_saved` → `server_exec(ls)` → `server_exec(df)` | 3 |
| Extract + analyze | `archive_decompress` → `local_tree` → `local_grep` | 3 |

## Safety Features

- **Tool approval**: All file modifications require explicit user approval (Allow/Reject)
- **Diff preview**: See exactly what changes will be made before approving
- **Danger levels**: Tools classified as safe/medium/high with appropriate warnings
- **Password isolation**: Server credentials resolved in Rust backend, never exposed to AI model
- **Command denylist**: Dangerous shell commands blocked at backend level

## Tested Providers

Validated with real-world file operations (create, read, edit, upload, server connect):

| Provider | Model | Tool Calling | Multi-Step | Server Exec |
|----------|-------|:---:|:---:|:---:|
| Cohere | Command A Reasoning 08 2025 | Yes | Yes | Yes |
| Google | Gemini 3.1 Flash Lite Preview | Yes | Yes | Yes |
| Google | Gemini 2.5 Flash | Yes | Yes | Yes |

> Full provider compatibility matrix and test results: [docs.aeroftp.app/aeroagent/providers](https://docs.aeroftp.app/aeroagent/providers)

## Complete Tool List (68 tools)

<details>
<summary>Click to expand</summary>

Counts distinct capabilities. Many remote tools ship under both an `aeroftp_*` (MCP) and a `remote_*` (GUI / cross-profile) alias for the same capability; 42 of these capabilities are exposed over the MCP server.

**Remote operations (21)**

| Tool | Danger | Tool | Danger |
|------|--------|------|--------|
| `remote_list` | Read-only | `remote_upload` | Medium |
| `remote_read` | Read-only | `upload_files` | Medium |
| `remote_info` | Read-only | `upload_many` | Medium |
| `remote_search` | Read-only | `remote_download` | Medium |
| `remote_head` | Read-only | `download_files` | Safe |
| `remote_tail` | Read-only | `remote_mkdir` | Medium |
| `remote_tree` | Read-only | `remote_rename` | Medium |
| `remote_storage_quota` | Read-only | `remote_edit` | Medium |
| `remote_hashsum` | Read-only | `remote_touch` | Medium |
| `list_servers` | Read-only | `remote_delete` | High |
| | | `remote_delete_many` | High |

**Local file operations (21)**

| Tool | Danger | Tool | Danger |
|------|--------|------|--------|
| `local_list` | Read-only | `local_write` | Medium |
| `local_read` | Read-only | `local_mkdir` | Medium |
| `local_search` | Read-only | `local_rename` | Medium |
| `local_file_info` | Read-only | `local_edit` | Medium |
| `local_disk_usage` | Read-only | `local_move_files` | Medium |
| `local_find_duplicates` | Read-only | `local_copy_files` | Medium |
| `local_diff` | Read-only | `local_batch_rename` | Medium |
| `local_tree` | Read-only | `local_trash` | Medium |
| `local_grep` | Read-only | `local_delete` | High |
| `local_head` | Read-only | `local_stat_batch` | Read-only |
| `local_tail` | Read-only | | |

**Cross-profile transfer (4)**

| Tool | Danger |
|------|--------|
| `transfer` | Medium |
| `transfer_tree` | Medium |
| `cross_profile_transfer` | Medium |
| `generate_transfer_plan` | Safe |

**Sync & verification (7)**

| Tool | Danger | Tool | Danger |
|------|--------|------|--------|
| `sync_doctor` | Read-only | `cleanup` | High |
| `reconcile` | Read-only | `sync_control` | Safe |
| `dedupe` | High | `sync_preview` | Safe |
| `speed` | Medium | | |

**Archives (2)**

| Tool | Danger |
|------|--------|
| `archive_compress` | Medium |
| `archive_decompress` | Medium |

**System & shell (4)**

| Tool | Danger |
|------|--------|
| `shell_execute` | High |
| `clipboard_read` | Safe |
| `clipboard_write` | Safe |
| `hash_file` | Safe |

**Knowledge & memory (3)**

| Tool | Danger |
|------|--------|
| `rag_index` | Read-only |
| `rag_search` | Read-only |
| `agent_memory_write` | Medium |

**App & server control (6)**

| Tool | Danger |
|------|--------|
| `app_info` | Safe |
| `set_theme` | Safe |
| `vault_peek` | Safe |
| `preview_edit` | Safe |
| `agent_connect` | Read-only |
| `server_exec` | High |

</details>
