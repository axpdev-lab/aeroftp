# AeroAgent - AI Assistant Documentation

> _Last updated: 2026-10-26_

**Status**: Public architecture overview
**Tool catalog**: Built-in tool suite + extensible via plugins

For the code-grounded status of the MCP client, scoped workers, GUI control and
the remaining 2.0 proposals, see the [AeroAgent roadmap](AEROAGENT-ROADMAP.md).
This overview targets AeroFTP v4.2.5; AeroAgent has its own product version (1.7).

---

## Overview

AeroAgent is an AI-powered assistant integrated into AeroFTP that can manage files, edit code, search content, interact with remote servers, and automate workflows - all through natural language conversation. It supports 26 AI providers and executes operations across local files plus the AeroFTP remote provider backends.

### Key Capabilities

- **Built-in provider-agnostic tools** spanning file management, code editing, archives, sync, and system operations
- **26 AI providers**: OpenAI, Anthropic, Gemini, xAI, OpenRouter, Ollama, Kimi, Qwen, DeepSeek, Mistral, Groq, Perplexity, Cohere, Together AI, AI21, Cerebras, SambaNova, Fireworks AI, Nvidia, Z.AI, Hyperbolic, Novita, Yi, Kilo Gateway, Amazon Bedrock, Custom
- **Multi-step autonomous execution** with DAG-based parallel pipeline
- **3-level safety system**: safe (auto-execute), medium (confirm), high (explicit approval)
- **Vision/multimodal** support for GPT-4o, Gemini, Claude
- **RAG integration** for project-aware context
- **Persistent agent memory** across sessions
- **Plugin system** for custom tool extensions
- **Shared backend architecture** with surface-specific GUI, CLI-agent and MCP catalogs

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

The desktop catalog contains **86 built-in tools** (31 safe, 42 medium, 13 high).
Use [AEROAGENT-CAPABILITIES.md](AEROAGENT-CAPABILITIES.md) for the complete current
GUI catalog, linked to `src/types/tools.ts`. The CLI agent exposes 39 tools; the
native MCP server exposes 78 names (40 primary tools plus 38 aliases), counted
separately in [COMMAND-INVENTORY.json](COMMAND-INVENTORY.json).

Coding tools, semantic GUI control, discovered MCP tools, scoped read-only
workers and plugin/macro tools have distinct context and approval requirements.
Sharing a Rust backend does not make every tool available on every surface.
The [roadmap reconciliation](AEROAGENT-ROADMAP.md) records those delivered
foundations and the remaining proposals.

---

## Safety System

### Danger Levels

| Level | Behavior | Count |
| ----- | -------- | ----- |
| **safe** | No ordinary per-tool approval prompt | 31 tools |
| **medium** | Approval/grant required under the selected mode | 42 tools |
| **high** | Higher-risk approval/grant policy | 13 tools |

These are GUI catalog counts. Safe and Normal use native confirmation for
mutating grants, Expert uses the in-app approval panel, and Extreme can
auto-approve tools. CLI auto-approval levels and MCP rate categories are separate
contracts; see [AGENTS.md](../AGENTS.md#auto-approval-levels).

### Path Validation

All file operations validate paths against:

- Null bytes and `..` traversal
- Maximum path length (4096 characters)
- Symlink resolution to sensitive locations
- System path denylist: `/proc`, `/sys`, `/dev`, `/boot`, `/root`, `/etc/shadow`, `/etc/passwd`, `/etc/ssh`, `~/.ssh`, `~/.gnupg`, `~/.aws`, `/run/secrets`

### Shell Command Denylist

`shell_execute` blocks dangerous patterns including: `rm -rf /`, `mkfs`, `dd of=/dev/`, `shutdown`, `reboot`, fork bombs, `chmod 777 /`, `sudo`, `eval`, `base64 -d` piped execution, `curl | sh`, and 20+ additional patterns. Shell meta-characters (`|`, `;`, `` ` ``, `$`, `&`) are also blocked.

### Size Guards

- Remote text previews: default 5 KiB, with a 1 MiB hard limit in the shared read path
- Local text reads: bounded previews; file transfer limits are separate from preview limits
- Remote hash tools: 256 MiB file cap; head/tail: 16 MiB file cap
- Shell output: 512 KB capture limit
- Path length: 4096 characters
- `local_stat_batch`: 100 paths maximum
- `rag_index`: default 200 files; inspect the current handler for the accepted override bounds

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

AeroAgent supports autonomous multi-step workflows. The step ceiling is driven by the selected Agent Mode (Safe 5, Normal 10, Expert 25, Extreme 50):

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
| `composer.json` | PHP | PHP |
| `*.csproj` | .NET/C# | C# |
| `CMakeLists.txt` | C/C++ | C/C++ |
| `build.gradle` | Kotlin/Android | Kotlin/Java |

### Smart Context Injection

The system prompt is dynamically composed from:

1. **Base personality** - AeroAgent identity and behavioral rules
2. **Provider profile** - Optimized for each AI provider's strengths
3. **Connection context** - Current protocol, server, path, mode (AeroCloud/Server/AeroFile)
4. **Tool definitions** - Current callable schemas selected by the registry; `tool_search` discovers additional tools without approving execution
5. **Project context** - Language, framework, dependencies
6. **RAG context** - Indexed file content matching the user's query
7. **Agent memory** - Persistent notes from previous sessions

### Token Budget Management

- Sliding window with 70% of provider's max token limit
- Automatic summarization when approaching budget
- Three budget modes: minimal, compact, full
- Priority-based context allocation (git > imports > project > memory > RAG)

---

## AI Provider Support

### 24 Providers

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
| Nvidia (NIM) | native | SSE | - | - |
| Z.AI (GLM) | native | SSE | - | - |
| Hyperbolic | native | SSE | - | - |
| Novita | native | SSE | - | - |
| Yi (01.AI) | native | SSE | - | - |
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
- **Plugin registry**: GitHub-based discovery and installation (remote registry browse/install is temporarily disabled pending a signed, client-side-verified registry; locally installed plugins continue to work)
- **Plugin hooks**: a manifest may declare hooks (file:created, transfer:complete, sync:complete), but nothing runs them: no event source emits these events, so declared hooks are inert
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
- **Token counts** per reply and per conversation, as the provider reports them, prompt cache included (no cost estimate: prices change per model and no provider returns them per request)
- **Keyboard shortcuts**: Ctrl+L (clear), Ctrl+Shift+N (new), Ctrl+Shift+E (export)

---

## Agent Modes

AeroAgent runs in one of four escalating modes, each raising the autonomous step ceiling:

| Mode | Step limit | Behavior |
| ---- | ---------- | -------- |
| Safe | 5 | Default safety, approval modals for medium/high tools |
| Normal | 10 | Standard autonomous operation |
| Expert | 25 | Extended autonomy for longer tasks |
| Extreme | 50 | Auto-approves all tool calls, fully autonomous |

Extreme Mode is gated to the Cyber and True Dark themes. When active:

- No confirmation modals
- 50-step limit
- Circuit breaker on consecutive errors
- Visual indicator in chat header

---

*AeroAgent is part of the [AeroFTP](https://github.com/axpdev-lab/aeroftp) ecosystem by axpdev-lab.*
