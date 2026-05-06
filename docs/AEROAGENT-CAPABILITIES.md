# AeroAgent - AI-Powered File Management

AeroAgent is AeroFTP's integrated AI assistant with **39 tools** across 8 categories. It can create, read, edit, and manage files locally and on remote servers using natural language commands.

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

### Local File Operations (17 tools)
Create, read, edit, move, copy, delete, search, and manage files on your local filesystem.

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

### Remote Server Operations (2 tools)
Connect to any saved server and execute file operations without switching connections.

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

### File Transfer (2 tools)
Upload and download files between local and remote, including recursive directory upload.

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

### Code Editing (1 tool)
Find and replace text in files with diff preview before applying changes.

```
> In index.html, replace "Welcome" with "Hello World"
```
```
AeroAgent: Showing diff preview...
  - <h1>Welcome</h1>
  + <h1>Hello World</h1>
  [Allow] [Reject]

Replaced 1 occurrence(s) in index.html
```

### Shell Commands (1 tool)
Execute system commands with captured stdout/stderr and exit codes.

```
> Run "uname -a" to check the system info
```
```
AeroAgent: uname -a - exit code 0
  Linux server 6.17.0-19-generic #19~24.04.2-Ubuntu SMP x86_64
```

### Archive Operations (2 tools)
Compress and extract ZIP, 7z, TAR, GZ, XZ, BZ2 archives.

```
> Compress all files in the project/ folder into project.zip
```
```
AeroAgent: Created /project/project.zip (357 bytes)
```

### Search & Analysis (8 tools)
Grep, head, tail, diff, tree, file info, disk usage, find duplicates.

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

### Context & Intelligence (2 tools)
RAG indexing and semantic search across your workspace.

```
> Index the current project and search for authentication-related code
```
```
AeroAgent: Indexed 45 files. Found 3 matches for "authentication":
  - auth.ts:12 - export function validateToken()
  - middleware.ts:8 - const authGuard = ...
  - config.ts:25 - AUTH_ENDPOINT: "https://..."
```

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

## Complete Tool List (39 tools)

<details>
<summary>Click to expand</summary>

| # | Tool | Category | Danger |
|---|------|----------|--------|
| 1 | `local_list` | Files | Safe |
| 2 | `local_read` | Files | Safe |
| 3 | `local_write` | Files | Medium |
| 4 | `local_edit` | Files | Medium |
| 5 | `local_mkdir` | Files | Safe |
| 6 | `local_delete` | Files | High |
| 7 | `local_rename` | Files | Medium |
| 8 | `local_move_files` | Files | Medium |
| 9 | `local_copy_files` | Files | Safe |
| 10 | `local_trash` | Files | Medium |
| 11 | `local_file_info` | Files | Safe |
| 12 | `local_disk_usage` | Files | Safe |
| 13 | `local_find_duplicates` | Files | Safe |
| 14 | `local_batch_rename` | Files | Medium |
| 15 | `local_search` | Search | Safe |
| 16 | `local_grep` | Search | Safe |
| 17 | `local_head` | Search | Safe |
| 18 | `local_tail` | Search | Safe |
| 19 | `local_stat_batch` | Search | Safe |
| 20 | `local_diff` | Search | Safe |
| 21 | `local_tree` | Search | Safe |
| 22 | `remote_list` | Remote | Safe |
| 23 | `remote_read` | Remote | Safe |
| 24 | `remote_info` | Remote | Safe |
| 25 | `remote_edit` | Remote | Medium |
| 26 | `upload_files` | Transfer | Medium |
| 27 | `download_files` | Transfer | Safe |
| 28 | `archive_compress` | Archives | Safe |
| 29 | `archive_decompress` | Archives | Safe |
| 30 | `shell_execute` | System | High |
| 31 | `clipboard_read` | System | Safe |
| 32 | `clipboard_write` | System | Safe |
| 33 | `clipboard_read_image` | System | Safe |
| 34 | `rag_index` | Context | Safe |
| 35 | `rag_search` | Context | Safe |
| 36 | `agent_memory_write` | Context | Safe |
| 37 | `server_list_saved` | Server | Safe |
| 38 | `server_exec` | Server | High |
| 39 | `vault_v2_create` | Vault | Medium |
| 40 | `vault_v2_open` | Vault | Safe |
| 41 | `vault_v2_add_files` | Vault | Medium |
| 42 | `vault_v2_extract` | Vault | Safe |
| 43 | `vault_v2_list` | Vault | Safe |
| 44 | `preview_edit` | Editor | Safe |
| 45 | `context_detect_project` | Context | Safe |
| 46 | `context_scan_imports` | Context | Safe |
| 47 | `context_file_summary` | Context | Safe |

</details>
