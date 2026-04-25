# AeroAgent — AI Assistant Documentation

**Status**: Public architecture overview
**Tool catalog**: Built-in tool suite + extensible via plugins

---

## Overview

AeroAgent is an AI-powered assistant integrated into AeroFTP that can manage files, edit code, search content, interact with remote servers, and automate workflows — all through natural language conversation. It supports 19 AI providers and executes operations across local files plus the AeroFTP remote provider backends.

### Key Capabilities

- **Built-in provider-agnostic tools** spanning file management, code editing, archives, sync, and system operations
- **19 AI providers**: OpenAI, Anthropic, Gemini, xAI, OpenRouter, Ollama, Kimi, Qwen, DeepSeek, Mistral, Groq, Perplexity, Cohere, Together AI, AI21, Cerebras, SambaNova, Fireworks AI, Custom
- **Multi-step autonomous execution** with DAG-based parallel pipeline
- **3-level safety system**: safe (auto-execute), medium (confirm), high (explicit approval)
- **Vision/multimodal** support for GPT-4o, Gemini, Claude
- **RAG integration** for project-aware context
- **Persistent agent memory** across sessions
- **Plugin system** for custom tool extensions
- **Backend-agnostic architecture** — same tools work in GUI and CLI

---

## Architecture

```
┌──────────────────────────────────────────────────────┐
│                    AeroAgent                          │
│                                                      │
│  ┌───────────┐  ┌───────────┐  ┌──────────────────┐ │
│  │ GUI Mode  │  │ CLI Mode  │  │ Orchestrate/MCP  │ │
│  │ (Tauri)   │  │ (stdout)  │  │ (JSON-RPC stdio) │ │
│  └─────┬─────┘  └─────┬─────┘  └────────┬─────────┘ │
│        │               │                 │           │
│  ┌─────▼───────────────▼─────────────────▼─────────┐ │
│  │           Trait Abstraction Layer                │ │
│  │  EventSink · CredentialProvider · RemoteBackend  │ │
│  └─────────────────────┬───────────────────────────┘ │
│                        │                             │
│  ┌─────────────────────▼───────────────────────────┐ │
│  │              Tool Execution Engine               │ │
│  │  tool catalog · validation · path security · retry │ │
│  └─────────────────────┬───────────────────────────┘ │
│                        │                             │
│  ┌─────────────────────▼───────────────────────────┐ │
│  │            Shared Rust Backend                   │ │
│  │  StorageProvider backends · AeroVault            │ │
│  │  context_intelligence · shell_execute            │ │
│  └─────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────┘
```

### Core Traits (`ai_core/`)

| Trait | Purpose |
| ----- | ------- |
| `EventSink` | Abstract event emission (Tauri `app.emit()` vs CLI stdout/stderr) |
| `CredentialProvider` | Vault-based credential access without exposing passwords |
| `RemoteBackend` | Protocol-agnostic remote operations over the AeroFTP provider backends |

---

## Tool Reference

### Remote Operations (10 tools)

| Tool | Danger | Description |
| ---- | ------ | ----------- |
| `remote_list` | safe | List files in remote directory |
| `remote_read` | safe | Read remote text file (max 5 KB) |
| `remote_info` | safe | Get file/directory metadata |
| `remote_search` | safe | Search files by glob pattern |
| `remote_download` | medium | Download single file |
| `remote_upload` | medium | Upload single file |
| `remote_mkdir` | medium | Create remote directory |
| `remote_rename` | medium | Rename/move remote file |
| `remote_edit` | medium | Find and replace in remote file (download, edit, upload) |
| `remote_delete` | high | Delete remote file or directory |

### Local File Operations (15 tools)

| Tool | Danger | Description |
| ---- | ------ | ----------- |
| `local_list` | medium | List local files |
| `local_read` | medium | Read local text file (max 5 KB) |
| `local_write` | medium | Write text to local file |
| `local_mkdir` | medium | Create local directory |
| `local_rename` | medium | Rename/move local file |
| `local_edit` | medium | Find and replace in local file |
| `local_move_files` | medium | Batch move files to destination |
| `local_batch_rename` | medium | Batch rename (regex/prefix/suffix/sequential) |
| `local_copy_files` | medium | Batch copy files |
| `local_trash` | medium | Move files to system recycle bin |
| `local_file_info` | safe | Get detailed file properties |
| `local_disk_usage` | safe | Calculate directory size recursively |
| `local_find_duplicates` | safe | Find duplicate files via hash |
| `local_search` | medium | Search local files by pattern |
| `local_delete` | high | Delete local file or directory |

### Content Inspection (7 tools)

| Tool | Danger | Description |
| ---- | ------ | ----------- |
| `local_grep` | medium | Regex search in directory files |
| `local_head` | medium | Read first N lines of file (max 500) |
| `local_tail` | medium | Read last N lines of file (max 500) |
| `local_stat_batch` | medium | Metadata for multiple paths (max 100) |
| `local_diff` | safe | Unified diff between two files |
| `local_tree` | medium | Recursive directory tree (max depth 10) |
| `preview_edit` | safe | Preview find/replace without applying |

### Batch Transfer (2 tools)

| Tool | Danger | Description |
| ---- | ------ | ----------- |
| `upload_files` | medium | Upload multiple local files to remote |
| `download_files` | medium | Download multiple remote files to local |

### Archive Operations (2 tools)

| Tool | Danger | Description |
| ---- | ------ | ----------- |
| `archive_compress` | medium | Create ZIP/7z/TAR archives (optional AES-256 password) |
| `archive_decompress` | medium | Extract archives with password support |

### Context and Indexing (2 tools)

| Tool | Danger | Description |
| ---- | ------ | ----------- |
| `rag_index` | medium | Index directory files with previews (max 200 files) |
| `rag_search` | medium | Full-text search across indexed files |

### Cryptography (2 tools)

| Tool | Danger | Description |
| ---- | ------ | ----------- |
| `hash_file` | safe | Compute hash (MD5, SHA-1, SHA-256, SHA-512, BLAKE3) |
| `vault_peek` | safe | Inspect AeroVault header without password |

### Application Control (3 tools)

| Tool | Danger | Description |
| ---- | ------ | ----------- |
| `set_theme` | safe | Change app theme (light/dark/tokyo/cyber) |
| `app_info` | safe | Get app state, connection info, version |
| `sync_control` | medium | Start/stop/status AeroSync service |

### Clipboard (3 tools)

| Tool | Danger | Description |
| ---- | ------ | ----------- |
| `clipboard_read` | medium | Read text from system clipboard |
| `clipboard_write` | medium | Write text to system clipboard |
| `clipboard_read_image` | medium | Read clipboard image as RGBA (used as multimodal-paste fallback on WebKitGTK Linux where the standard `clipboardData.items` doesn't expose images) |

### Agent Memory (1 tool)

| Tool | Danger | Description |
| ---- | ------ | ----------- |
| `agent_memory_write` | medium | Save persistent note (convention/preference/issue/pattern) |

### Server Management (2 tools)

| Tool | Danger | Description |
| ---- | ------ | ----------- |
| `server_list_saved` | safe | List saved server profiles (no passwords exposed) |
| `server_exec` | high | Execute operation on saved server (ls/cat/get/put/mkdir/rm/mv/stat/find/df) |

### Shell Execution (1 tool)

| Tool | Danger | Description |
| ---- | ------ | ----------- |
| `shell_execute` | high | Execute shell command (30s timeout, 1 MB output limit) |

---

## Safety System

### Danger Levels

| Level | Behavior | Count |
| ----- | -------- | ----- |
| **safe** | Auto-execute without user confirmation | 14 tools |
| **medium** | Show approval modal, user must confirm | 28 tools |
| **high** | Explicit confirmation with danger warning | 6 tools |

### Path Validation

All file operations validate paths against:

- Null bytes and `..` traversal
- Maximum path length (4096 characters)
- Symlink resolution to sensitive locations
- System path denylist: `/proc`, `/sys`, `/dev`, `/boot`, `/root`, `/etc/shadow`, `/etc/passwd`, `/etc/ssh`, `~/.ssh`, `~/.gnupg`, `~/.aws`, `/run/secrets`

### Shell Command Denylist

`shell_execute` blocks dangerous patterns including: `rm -rf /`, `mkfs`, `dd of=/dev/`, `shutdown`, `reboot`, fork bombs, `chmod 777 /`, `sudo`, `eval`, `base64 -d` piped execution, `curl | sh`, and 20+ additional patterns. Shell meta-characters (`|`, `;`, `` ` ``, `$`, `&`) are also blocked.

### Size Guards

- AI file downloads: 50 MB maximum
- `cat`/`read` operations: 5 KB for AI context
- Shell output: 1 MB capture limit
- Path length: 4096 characters
- `local_stat_batch`: 100 paths maximum
- `rag_index`: 200 files maximum

---

## Execution Pipeline

### DAG-Based Parallel Execution

When the AI model requests multiple tool calls in a single response, AeroAgent builds a Directed Acyclic Graph based on path dependencies:

1. **Extract paths** from tool parameters (`path`, `local_path`, `remote_path`, `from`, `to`)
2. **Classify tools** as mutating (write/delete/rename) or read-only
3. **Build dependency edges**: mutating tools on shared paths are serialized
4. **Topological sort** (Kahn's algorithm) into execution levels
5. **Execute levels in parallel**, sequential between levels

This means `local_read("a.txt")` and `local_read("b.txt")` execute simultaneously, but `local_write("a.txt")` waits for `local_read("a.txt")` to complete first.

### Multi-Step Execution

AeroAgent supports autonomous multi-step workflows (up to 10 steps by default, 50 in Extreme Mode):

1. User sends prompt
2. AI responds with tool calls
3. Tools execute (with approval if needed)
4. Results fed back to AI
5. AI decides: respond to user or call more tools
6. Repeat until AI gives final response or step limit reached

### Error Recovery

8 retry strategies with automatic analysis:

- **Not found**: Suggests `rag_search` to locate correct content
- **Permission denied**: Suggests listing parent directory
- **Rate limit (429/503)**: Auto-retry with exponential backoff (3 attempts)
- **Timeout**: Suggests smaller scope or different approach
- **Connection lost**: Prompts reconnection
- **File too large**: Suggests chunked approach

---

## Context Intelligence

### Project Detection

AeroAgent automatically detects project type and injects relevant context:

| Marker File | Detected As | Languages |
| ----------- | ----------- | --------- |
| `Cargo.toml` | Rust project | Rust |
| `package.json` | Node.js/React/Vue | JS/TS |
| `pom.xml` | Java/Maven | Java |
| `requirements.txt` | Python | Python |
| `go.mod` | Go | Go |
| `Gemfile` | Ruby/Rails | Ruby |
| `composer.json` | PHP | PHP |
| `*.csproj` | .NET/C# | C# |
| `CMakeLists.txt` | C/C++ | C/C++ |
| `build.gradle` | Kotlin/Android | Kotlin/Java |

### Smart Context Injection

The system prompt is dynamically composed from:

1. **Base personality** — AeroAgent identity and behavioral rules
2. **Provider profile** — Optimized for each AI provider's strengths
3. **Connection context** — Current protocol, server, path, mode (AeroCloud/Server/AeroFile)
4. **Tool definitions** — Full tool list with JSON Schema parameters
5. **Project context** — Language, framework, dependencies
6. **RAG context** — Indexed file content matching the user's query
7. **Agent memory** — Persistent notes from previous sessions

### Token Budget Management

- Sliding window with 70% of provider's max token limit
- Automatic summarization when approaching budget
- Three budget modes: minimal, compact, full
- Priority-based context allocation (git > imports > project > memory > RAG)

---

## AI Provider Support

### 19 Providers

| Provider | Tool Format | Streaming | Vision | Thinking |
| -------- | ----------- | --------- | ------ | -------- |
| OpenAI | native | SSE | GPT-4o | o3 |
| Anthropic | native | SSE | Claude 3.5+ | Claude 3.5+ |
| Gemini | native | SSE | Gemini 2.0 | - |
| xAI (Grok) | native | SSE | Grok Vision | - |
| OpenRouter | native | SSE | varies | varies |
| Ollama | native | NDJSON | llava | - |
| Mistral | native | SSE | Pixtral | - |
| Groq | native | SSE | - | - |
| Perplexity | text | SSE | - | - |
| Cohere | native | SSE | - | - |
| Together AI | native | SSE | - | - |
| AI21 Labs | native | SSE | - | - |
| Cerebras | native | SSE | - | - |
| SambaNova | native | SSE | - | - |
| Fireworks AI | native | SSE | - | - |
| Kimi | native | SSE | - | - |
| Qwen | native | SSE | - | - |
| DeepSeek | native | SSE | - | DeepSeek-R1 |
| Custom | native/text | SSE | configurable | configurable |

### Ollama Model Families

Auto-detected with optimized prompting:

| Family | Models | Best For |
| ------ | ------ | -------- |
| llama3 | Llama 3, 3.1, 3.2, 3.3 | General, code, analysis |
| codellama | Code Llama | Code generation |
| deepseek-coder | DeepSeek Coder, V2, R1 | Code, reasoning |
| qwen | Qwen 2, 2.5 | Code, multilingual |
| mistral | Mistral, Mixtral, Codestral | General, code |
| phi | Phi 3, 4 | Code, quick tasks |
| gemma | Gemma 2, CodeGemma | General, analysis |
| starcoder | StarCoder 2 | Code completion |

---

## Plugin System

AeroAgent supports runtime plugins for custom tool extensions:

- **Plugin manifest**: JSON file defining name, version, tools, hooks
- **Plugin scripts**: Shell scripts executed by the tool engine
- **Plugin registry**: GitHub-based discovery and installation
- **Plugin hooks**: Event-driven execution (file:created, transfer:complete, sync:complete)
- **SHA-256 integrity**: Verified at install and before each execution

### Plugin Management

Available in AI Settings > Plugins tab:
- Browse and install from registry
- View installed plugins with update status
- Enable/disable individual plugins
- Plugin tools appear in AeroAgent with their own danger levels

---

## Macro System

Composite tool macros allow chaining multiple tools into reusable workflows:

```json
{
  "name": "safe_edit",
  "description": "Read file, show content, then edit",
  "steps": [
    { "toolName": "local_read", "args": { "path": "{{file}}" } },
    { "toolName": "preview_edit", "args": { "path": "{{file}}", "find": "{{find}}", "replace": "{{replace}}" } },
    { "toolName": "local_edit", "args": { "path": "{{file}}", "find": "{{find}}", "replace": "{{replace}}" } }
  ]
}
```

- `{{variable}}` templates resolved from user parameters
- Maximum 20 total steps (including nested macros)
- Single-pass variable expansion (injection-safe)
- Configurable in AI Settings > Macros tab

---

## CLI Integration

AeroAgent tools are available in the AeroFTP CLI via the `ai_core/` trait abstraction:

```bash
# One-shot prompt
aeroftp-cli agent --provider ollama --message "list saved servers"

# Orchestration mode (JSON-RPC over stdin/stdout)
aeroftp-cli agent --orchestrate

# MCP server mode (full alias)
aeroftp-cli agent --mcp

# MCP server mode (top-level shortcut, used by the VS Code extension)
aeroftp-cli mcp
```

### Orchestration Protocol

JSON-RPC 2.0 over stdin/stdout enabling external agents (Claude Code, CI pipelines) to drive AeroAgent as a sub-process. Run `aeroftp-cli agent --orchestrate` and pipe newline-delimited JSON requests on stdin; responses and stream notifications come back on stdout, stderr carries diagnostic logs. The current surface covers `agent/ready`, `agent/chat`, `session/status`, `session/clear`, `session/close`, and `tool/list`; use `aeroftp-cli agent --help` and `aeroftp-cli agent-info --json` for the live method catalog.

### MCP Compatibility

AeroAgent's architecture maps naturally to the [Model Context Protocol](https://modelcontextprotocol.io/):

| MCP Primitive | AeroAgent Equivalent |
| ------------- | -------------------- |
| Tools | Built-in tools with JSON Schema |
| Resources | RAG index, vault peek, server profiles |
| Prompts | Macro system + prompt template library |
| Sampling | Multi-step execution loop |

MCP server mode exposes the AeroAgent tool catalog as standard MCP endpoints, enabling integration with Claude Desktop, ChatGPT, Cursor, and other MCP-compatible clients.

---

## Chat Features

- **Streaming markdown renderer** with finalized/streaming segments
- **Code block actions**: Copy, Apply, Diff, Run
- **Thinking visualization** with token count and duration
- **Prompt template library** (15 built-in, `/` prefix activation)
- **Chat search** (Ctrl+F) with role filter
- **Conversation branching** (fork/switch/delete)
- **Chat history** in SQLite with FTS5 full-text search
- **Export** to Markdown or JSON
- **Cost tracking** per message and monthly budget
- **Keyboard shortcuts**: Ctrl+L (clear), Shift+N (new), Shift+E (export)

---

## Extreme Mode

Available only in Cyber theme. Auto-approves all tool calls for fully autonomous execution:

- No confirmation modals
- 50-step limit (vs 10 default)
- Circuit breaker on consecutive errors
- Visual indicator in chat header

---

*AeroAgent is part of the [AeroFTP](https://github.com/axpdev-lab/aeroftp) ecosystem by AXP Development.*
