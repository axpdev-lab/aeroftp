# AeroAgent capabilities and built-in tools

> AeroFTP v4.2.5 documentation, reviewed 26 October 2026.

AeroAgent 1.7 is the desktop app's optional AI assistant. It supports **26 AI
provider presets** and **86 built-in GUI tools**. The tool names and approval
levels below come from [`AGENT_TOOLS`](../src/types/tools.ts); provider presets
come from [`PROVIDER_PRESETS`](../src/types/ai.ts). Model capabilities depend on
the selected model and transport, rather than every model of a provider
supporting the same features. Unknown-model metadata uses conservative limits.

The GUI, CLI agent and MCP server share Rust operations but expose different
catalogs. The CLI agent has 39 tools. The native MCP server has **78 callable
names: 40 primary tools plus 38 compatibility aliases**, as recorded in the
[command inventory](COMMAND-INVENTORY.json). Do not add those names to the GUI
count or assume an alias exists on every surface. Coding and semantic GUI
control require the desktop context and are not exposed by the native MCP
server. External MCP tools, plugins, macros and the `tool_search` discovery
helper are additional to the 86 built-in GUI entries.

## What the assistant can do

AeroAgent can inspect and manage local files, operate on active or saved remote
profiles, prepare transfers, run checks, review and apply coding patches, create
checkpoints, and use approved MCP servers. Remote operations consume each
provider's actual capabilities: sharing, server-side copy, checksums, trash,
versions and quota are provider-dependent. Cross-profile copies can stage data
locally; they are not a universal direct server-to-server protocol.

The [AeroAgent roadmap](AEROAGENT-ROADMAP.md) documents the delivered Responses
adapter, tool discovery, local/S3 read-only workers, MCP client and semantic GUI
control, plus the boundaries of remaining 2.0 proposals. Workers do not provide
arbitrary provider writes or durable background jobs. The
[MCP managed-install guide](MCP-MANAGED-INSTALL.md) describes Linux STDIO
isolation; production local STDIO launch remains refused on Windows and macOS.
Remote Streamable HTTP has a separate transport and OAuth lifecycle.

## Approval levels

The catalog contains 31 safe, 42 medium and 13 high entries. These are the GUI
catalog classifications, not the CLI's auto-approval policy or MCP rate-limit
categories. Backend grants, path validation, cancellation, account context and
agent-mode policy still apply. Discovery never approves execution. Native
confirmation depends on mode; Expert uses the in-app approval panel and Extreme
can auto-approve tools. Choose a mode appropriate for the workspace and data.

The semantic GUI controller uses bounded intents and redacted state, with a
visible control banner and Stop. It does not grant arbitrary clicks, scripts,
master-password access or secret editing. See [AGENTS.md](../AGENTS.md#controlling-the-desktop-gui)
for the closed public action contract.

## Built-in GUI catalog

### Remote files and provider maintenance

| Tool | GUI approval level |
|---|---|
| `remote_list` | safe |
| `remote_read` | safe |
| `remote_info` | safe |
| `remote_search` | safe |
| `remote_edit` | medium |
| `remote_download` | medium |
| `remote_upload` | medium |
| `remote_mkdir` | medium |
| `remote_rename` | medium |
| `remote_delete` | high |
| `server_list_saved` | safe |
| `server_exec` | high |
| `remote_hashsum` | safe |
| `remote_head` | safe |
| `remote_tail` | safe |
| `remote_tree` | safe |
| `remote_storage_quota` | safe |
| `remote_sync_doctor` | safe |
| `remote_reconcile` | safe |
| `remote_speed` | medium |
| `remote_touch` | medium |
| `remote_cleanup` | high |
| `remote_dedupe` | high |
| `remote_versions` | high |
| `remote_trash` | high |

### Transfers, sync and telemetry

| Tool | GUI approval level |
|---|---|
| `generate_transfer_plan` | safe |
| `upload_files` | medium |
| `download_files` | medium |
| `sync_preview` | medium |
| `sync_control` | medium |
| `cross_profile_transfer` | high |
| `aeroftp_benchmark` | medium |
| `aeroftp_transfer_stats` | safe |

### Local files and inspection

| Tool | GUI approval level |
|---|---|
| `preview_edit` | safe |
| `local_list` | medium |
| `local_read` | medium |
| `local_search` | medium |
| `local_mkdir` | medium |
| `local_write` | medium |
| `local_rename` | medium |
| `local_move_files` | medium |
| `local_edit` | medium |
| `local_batch_rename` | medium |
| `local_copy_files` | medium |
| `local_trash` | medium |
| `local_grep` | medium |
| `local_head` | medium |
| `local_tail` | medium |
| `local_stat_batch` | medium |
| `local_tree` | medium |
| `local_diff` | medium |
| `local_file_info` | safe |
| `local_disk_usage` | safe |
| `local_find_duplicates` | safe |
| `hash_file` | safe |
| `local_delete` | high |

### Coding foundation

| Tool | GUI approval level |
|---|---|
| `coding_checkpoint_create` | medium |
| `coding_checkpoint_restore` | high |
| `coding_apply_patch` | high |
| `coding_git_status` | safe |
| `coding_git_diff` | safe |
| `coding_git_stage` | high |
| `coding_git_commit` | high |
| `coding_git_log` | safe |
| `coding_git_show` | safe |
| `coding_run_checks` | medium |
| `coding_verify` | medium |
| `coding_diagnostics` | safe |
| `coding_search` | safe |

### Archives and recovery

| Tool | GUI approval level |
|---|---|
| `archive_compress` | medium |
| `archive_decompress` | medium |
| `vault_peek` | safe |
| `aeroftp_correct_gen` | medium |
| `aeroftp_correct_verify` | safe |
| `aeroftp_correct_repair` | medium |

### Context, system and app control

| Tool | GUI approval level |
|---|---|
| `clipboard_read` | medium |
| `clipboard_write` | medium |
| `shell_execute` | high |
| `rag_index` | medium |
| `rag_search` | medium |
| `agent_memory_write` | medium |
| `gui_state` | safe |
| `gui_wait` | safe |
| `gui_run` | medium |
| `set_theme` | safe |
| `app_info` | safe |

## Example workflows

- Inspect a saved deployment profile with `server_list_saved`, then
  `remote_list` / `remote_info` using its exact name or ID. Credentials are
  resolved internally; use an approved transfer tool for writes.
- Search a coding workspace with `coding_search`, inspect diagnostics, review
  a proposed patch, then approve `coding_apply_patch` and run the relevant checks.
- Preview a transfer with `generate_transfer_plan` or `sync_preview`, review its
  destination and conflicts, then approve execution. A preview does not authorize
  a later destructive operation.
- Ask for visible work in AeroFTP with `gui_state`, a supported `gui_run` intent
  and `gui_wait`. Headless CLI operations do not substitute for visible GUI work.

Use [AEROAGENT.md](AEROAGENT.md) for the architecture and
[LLM-INTEGRATION-GUIDE.md](LLM-INTEGRATION-GUIDE.md) for native CLI/MCP integration.
