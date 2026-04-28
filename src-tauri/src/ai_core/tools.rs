//! Core tool dispatcher condiviso GUI + CLI + MCP — **Gate 1** (infrastruttura pura).
//!
//! Questo modulo è la base del piano T3 descritto in
//! `docs/dev/roadmap/APPENDIX-C-Y-D/APPENDIX-C/tasks/2026-04-21_Piano_T3_Core_Tool_Engine.md`.
//!
//! Gate 1 crea i tipi + il dispatcher skeleton senza toccare i 3
//! dispatcher esistenti (`execute_ai_tool` GUI, `execute_cli_tool` CLI,
//! `mcp::tools::execute_tool`). Gate 2 (per-area migration) e Gate 3
//! (deprecazione vecchi dispatcher) restano aperti.
//!
//! Design note — `Surfaces` via wrapper `u8` invece di `bitflags`:
//! evitiamo una nuova dep Cargo in Gate 1. L'ergonomia (`contains`,
//! `insert`, `remove`, `|`) è replicata manualmente; quando il piano
//! approda in Gate 2 si potrà valutare se passare a `bitflags` per
//! avere `Debug` / `Display` automatici.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use serde_json::{json, Value};
use std::sync::{Arc, LazyLock};

use crate::ai_core::agent_tools;
use crate::ai_core::local_tools;
use crate::ai_core::remote_tools;
use crate::ai_core::system_tools;
use crate::ai_core::{CredentialProvider, EventSink, RemoteBackend};

/// Coarse-grained approval gate. Maps 1:1 a `mcp::tools::RateCategory`
/// (ReadOnly ↔ ReadOnly, Mutative ↔ Medium, Destructive ↔ High);
/// `Safe` aggiunge una classe nuova per i tool che non richiedono
/// neanche rate limiting (es. `clipboard_read`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DangerLevel {
    Safe,
    ReadOnly,
    Medium,
    High,
}

/// Bitmask delle surface su cui un tool è disponibile. Usiamo un
/// wrapper manuale su `u8` per evitare la dep `bitflags`. Semantica
/// identica a un bitflags normale: `contains`, `insert`, `remove`,
/// `|`, `empty`, `all`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Surfaces(u8);

impl Surfaces {
    pub const GUI: Self = Self(1 << 0);
    pub const CLI: Self = Self(1 << 1);
    pub const MCP: Self = Self(1 << 2);

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn all() -> Self {
        Self(Self::GUI.0 | Self::CLI.0 | Self::MCP.0)
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }
}

impl std::ops::BitOr for Surfaces {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for Surfaces {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// Definizione uniforme di un tool. Single source of truth che
/// sostituirà `ALLOWED_TOOLS` (GUI), `tool_definitions()` (CLI) e
/// `McpToolDef` (MCP) in Gate 2.
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub danger: DangerLevel,
    pub surfaces: Surfaces,
}

/// Contesto per-surface passato al dispatcher. Ogni surface fornisce
/// una propria impl (`TauriToolCtx`, `CliToolCtx`, `McpToolCtx`);
/// il dispatcher non conosce Tauri, libc o ssh2 — vede solo i trait.
///
/// I default method mantengono `context_local_path` / `approval_grant_id`
/// / `extreme_mode` opzionali: il CLI oggi non propaga alcuni di questi
/// campi, e restituire `None` / `false` conserva il comportamento pre-T3.
#[async_trait::async_trait]
pub trait ToolCtx: Send + Sync {
    fn event_sink(&self) -> &dyn EventSink;
    fn credentials(&self) -> &dyn CredentialProvider;
    async fn remote_backend(&self, server_id: &str) -> Result<Arc<dyn RemoteBackend>, String>;
    fn context_local_path(&self) -> Option<&str> {
        None
    }
    fn approval_grant_id(&self) -> Option<&str> {
        None
    }
    fn tauri_app_handle(&self) -> Option<tauri::AppHandle> {
        None
    }
    fn extreme_mode(&self) -> bool {
        false
    }
    /// La surface di questo contesto. Usata dal dispatcher per filtrare
    /// tool non disponibili su questa surface (es. `aeroftp_*` MCP-only
    /// chiamato da GUI → `ToolError::NotOnSurface`).
    fn surface(&self) -> Surfaces;
}

/// Errori del dispatcher. `thiserror` già presente in Cargo.
#[derive(thiserror::Error, Debug)]
pub enum ToolError {
    #[error("unknown tool: {0}")]
    Unknown(String),
    #[error("tool {tool} not available on surface {surface:?}")]
    NotOnSurface { tool: String, surface: Surfaces },
    #[error("invalid args for {tool}: {reason}")]
    InvalidArgs { tool: String, reason: String },
    #[error("execution failed: {0}")]
    Exec(String),
    #[error("denied: {0}")]
    Denied(String),
    /// Gate 1 marker: la migrazione Gate 2 non ha ancora fornito un
    /// handler per questo tool. Quando arriva un tool in questo ramo,
    /// i dispatcher legacy (GUI/CLI/MCP) restano autoritative.
    #[error("gate1 bridge not yet wired for tool {0}")]
    NotMigrated(String),
}

/// Registry canonico. Popolato in Gate 2 per aree (local_*, clipboard_/
/// shell_/archive_, remote_, rag_/memory_). I tool non ancora migrati
/// non compaiono qui e restano gestiti dai dispatcher legacy (arriva
/// `ToolError::Unknown` al call site, che fa fallback).
pub static TOOL_DEFINITIONS: LazyLock<Vec<ToolDef>> = LazyLock::new(|| {
    let local_surfaces = Surfaces::GUI | Surfaces::CLI;
    let remote_surfaces = Surfaces::GUI | Surfaces::CLI | Surfaces::MCP;
    vec![
        // ─── Area A: local_* (T3 Gate 2) ─────────────────────────────
        ToolDef {
            name: "local_list",
            description: "List entries in a local directory (max 100).",
            input_schema: json!({
                "type": "object",
                "properties": { "path": {"type": "string"} },
                "required": ["path"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "local_read",
            description: "Read first 5 KB of a local text file (max 10 MB).",
            input_schema: json!({
                "type": "object",
                "properties": { "path": {"type": "string"} },
                "required": ["path"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "local_write",
            description: "Write text content to a local file, overwriting if present.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"},
                },
                "required": ["path", "content"],
            }),
            danger: DangerLevel::Medium,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "local_mkdir",
            description: "Create a directory (and missing parents) on the local filesystem.",
            input_schema: json!({
                "type": "object",
                "properties": { "path": {"type": "string"} },
                "required": ["path"],
            }),
            danger: DangerLevel::Medium,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "local_delete",
            description: "Delete a local file or directory (recursive). Refuses dangerous paths.",
            input_schema: json!({
                "type": "object",
                "properties": { "path": {"type": "string"} },
                "required": ["path"],
            }),
            danger: DangerLevel::High,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "local_rename",
            description: "Rename or move a single local file/directory.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "from": {"type": "string"},
                    "to": {"type": "string"},
                },
                "required": ["from", "to"],
            }),
            danger: DangerLevel::Medium,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "local_search",
            description: "Search a local directory for files matching a glob-like pattern.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "pattern": {"type": "string"},
                },
                "required": ["path", "pattern"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "local_edit",
            description: "Find & replace inside a local text file (BOM-aware).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "find": {"type": "string"},
                    "replace": {"type": "string"},
                    "replace_all": {"type": "boolean"},
                },
                "required": ["path", "find", "replace"],
            }),
            danger: DangerLevel::Medium,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "local_move_files",
            description: "Move multiple local files/directories into a destination directory.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "paths": {"type": "array", "items": {"type": "string"}},
                    "destination": {"type": "string"},
                },
                "required": ["paths", "destination"],
            }),
            danger: DangerLevel::Medium,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "local_batch_rename",
            description:
                "Batch-rename files via find_replace / add_prefix / add_suffix / sequential modes.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "paths": {"type": "array", "items": {"type": "string"}},
                    "mode": {"type": "string"},
                    "find": {"type": "string"},
                    "replace": {"type": "string"},
                    "case_sensitive": {"type": "boolean"},
                    "prefix": {"type": "string"},
                    "suffix": {"type": "string"},
                    "base_name": {"type": "string"},
                    "start_number": {"type": "integer"},
                    "padding": {"type": "integer"},
                },
                "required": ["paths", "mode"],
            }),
            danger: DangerLevel::Medium,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "local_copy_files",
            description: "Copy multiple local files/directories (recursive) into a destination.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "paths": {"type": "array", "items": {"type": "string"}},
                    "destination": {"type": "string"},
                },
                "required": ["paths", "destination"],
            }),
            danger: DangerLevel::Medium,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "local_trash",
            description: "Move files/directories to the OS recycle bin (recoverable).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "paths": {"type": "array", "items": {"type": "string"}},
                },
                "required": ["paths"],
            }),
            danger: DangerLevel::Medium,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "local_file_info",
            description: "Full metadata for a local file or directory (size, perms, MIME, times).",
            input_schema: json!({
                "type": "object",
                "properties": { "path": {"type": "string"} },
                "required": ["path"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "local_disk_usage",
            description: "Recursive disk usage for a local directory (capped at 500k entries).",
            input_schema: json!({
                "type": "object",
                "properties": { "path": {"type": "string"} },
                "required": ["path"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "local_find_duplicates",
            description:
                "Find duplicate files by MD5 hash, grouped by size (min_size default 1024).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "min_size": {"type": "integer"},
                },
                "required": ["path"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "local_grep",
            description: "Regex search within text files in a directory tree.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "pattern": {"type": "string"},
                    "glob": {"type": "string"},
                    "max_results": {"type": "integer"},
                    "context_lines": {"type": "integer"},
                    "case_sensitive": {"type": "boolean"},
                },
                "required": ["path", "pattern"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "local_head",
            description: "First N lines of a local text file.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "lines": {"type": "integer"},
                },
                "required": ["path"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "local_tail",
            description: "Last N lines of a local text file.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "lines": {"type": "integer"},
                },
                "required": ["path"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "local_stat_batch",
            description: "Batch metadata for up to 100 local paths.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "paths": {"type": "array", "items": {"type": "string"}},
                },
                "required": ["paths"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "local_diff",
            description: "Unified diff between two local text files (max 5 MB each).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path_a": {"type": "string"},
                    "path_b": {"type": "string"},
                    "context_lines": {"type": "integer"},
                },
                "required": ["path_a", "path_b"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "local_tree",
            description: "Unicode tree view of a local directory (max_depth default 3, capped).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "max_depth": {"type": "integer"},
                    "show_hidden": {"type": "boolean"},
                    "glob": {"type": "string"},
                },
                "required": ["path"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: local_surfaces,
        },
        // ─── Area B: system_* (clipboard/shell/archive) (T3 Gate 2) ──────────
        ToolDef {
            name: "clipboard_read",
            description: "Read text from the system clipboard.",
            input_schema: json!({
                "type": "object",
                "properties": {},
            }),
            danger: DangerLevel::Safe,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "clipboard_write",
            description: "Write text to the system clipboard.",
            input_schema: json!({
                "type": "object",
                "properties": { "content": {"type": "string"} },
                "required": ["content"],
            }),
            danger: DangerLevel::Safe,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "shell_execute",
            description: "Execute a shell command and capture output (stderr/stdout merged).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "working_dir": {"type": "string"},
                    "timeout_secs": {"type": "integer"},
                },
                "required": ["command"],
            }),
            danger: DangerLevel::High,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "archive_compress",
            description: "Compress files into an archive (ZIP, 7z, TAR, etc.).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "paths": {"type": "array", "items": {"type": "string"}},
                    "output_path": {"type": "string"},
                    "format": {"type": "string"},
                    "password": {"type": "string"},
                    "compression_level": {"type": "integer"},
                },
                "required": ["paths", "output_path"],
            }),
            danger: DangerLevel::Medium,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "archive_decompress",
            description: "Extract an archive file to a local directory.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "archive_path": {"type": "string"},
                    "output_dir": {"type": "string"},
                    "password": {"type": "string"},
                    "create_subfolder": {"type": "boolean"},
                },
                "required": ["archive_path", "output_dir"],
            }),
            danger: DangerLevel::Medium,
            surfaces: local_surfaces,
        },
        // ─── Area C: remote_* + aeroftp_* aliases (T3 Gate 2) ────────────────
        ToolDef {
            name: "aeroftp_list_servers",
            description:
                "List saved server profiles from the encrypted vault. Passwords are never exposed.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name_contains": {"type": "string"},
                    "protocol": {"type": "string"},
                    "limit": {"type": "integer"},
                    "offset": {"type": "integer"},
                },
                "required": [],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "remote_list_servers",
            description: "Alias of aeroftp_list_servers.",
            input_schema: json!({"type": "object", "properties": {}, "required": []}),
            danger: DangerLevel::ReadOnly,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "server_list_saved",
            description: "Alias of aeroftp_list_servers (legacy GUI/CLI name).",
            input_schema: json!({"type": "object", "properties": {}, "required": []}),
            danger: DangerLevel::ReadOnly,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "aeroftp_list_files",
            description: "List files and directories on a remote server.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "path": {"type": "string"},
                },
                "required": ["server"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "remote_list",
            description: "Legacy alias of aeroftp_list_files.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "path": {"type": "string"},
                },
                "required": ["server"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "remote_list_files",
            description: "Alias of aeroftp_list_files.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "path": {"type": "string"},
                },
                "required": ["server"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "aeroftp_read_file",
            description: "Read a remote text file preview.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "path": {"type": "string"},
                    "preview_kb": {"type": "integer"},
                },
                "required": ["server", "path"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "remote_read",
            description: "Legacy alias of aeroftp_read_file.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "path": {"type": "string"},
                    "preview_kb": {"type": "integer"},
                },
                "required": ["server", "path"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "remote_read_file",
            description: "Alias of aeroftp_read_file.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "path": {"type": "string"},
                    "preview_kb": {"type": "integer"},
                },
                "required": ["server", "path"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "aeroftp_file_info",
            description: "Get metadata for a remote file or directory.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "path": {"type": "string"},
                },
                "required": ["server", "path"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "remote_info",
            description: "Legacy alias of aeroftp_file_info.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "path": {"type": "string"},
                },
                "required": ["server", "path"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "remote_file_info",
            description: "Alias of aeroftp_file_info.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "path": {"type": "string"},
                },
                "required": ["server", "path"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "remote_stat",
            description: "Alias of aeroftp_file_info.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "path": {"type": "string"},
                },
                "required": ["server", "path"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "aeroftp_search_files",
            description: "Search for files matching a pattern on a remote server.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "path": {"type": "string"},
                    "pattern": {"type": "string"},
                },
                "required": ["server", "pattern"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "remote_search",
            description: "Legacy alias of aeroftp_search_files.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "path": {"type": "string"},
                    "pattern": {"type": "string"},
                },
                "required": ["server", "pattern"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "remote_search_files",
            description: "Alias of aeroftp_search_files.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "path": {"type": "string"},
                    "pattern": {"type": "string"},
                },
                "required": ["server", "pattern"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "aeroftp_upload_file",
            description: "Upload a local file or inline text content to a remote server.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "remote_path": {"type": "string"},
                    "local_path": {"type": "string"},
                    "content": {"type": "string"},
                    "create_parents": {"type": "boolean"},
                    "no_clobber": {"type": "boolean"},
                },
                "required": ["server", "remote_path"],
            }),
            danger: DangerLevel::Medium,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "remote_upload",
            description: "Legacy alias of aeroftp_upload_file.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "remote_path": {"type": "string"},
                    "local_path": {"type": "string"},
                    "content": {"type": "string"},
                },
                "required": ["server", "remote_path"],
            }),
            danger: DangerLevel::Medium,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "remote_upload_file",
            description: "Alias of aeroftp_upload_file.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "remote_path": {"type": "string"},
                    "local_path": {"type": "string"},
                    "content": {"type": "string"},
                },
                "required": ["server", "remote_path"],
            }),
            danger: DangerLevel::Medium,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "aeroftp_upload_many",
            description: "Upload multiple local files to a remote server.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "items": {"type": "array"},
                    "continue_on_error": {"type": "boolean"},
                },
                "required": ["server", "items"],
            }),
            danger: DangerLevel::Medium,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "remote_upload_many",
            description: "Alias of aeroftp_upload_many.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "items": {"type": "array"},
                    "continue_on_error": {"type": "boolean"},
                },
                "required": ["server", "items"],
            }),
            danger: DangerLevel::Medium,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "aeroftp_download_file",
            description: "Download a remote file to a local path.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "remote_path": {"type": "string"},
                    "local_path": {"type": "string"},
                },
                "required": ["server", "remote_path", "local_path"],
            }),
            danger: DangerLevel::Medium,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "remote_download",
            description: "Legacy alias of aeroftp_download_file.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "remote_path": {"type": "string"},
                    "local_path": {"type": "string"},
                },
                "required": ["server", "remote_path", "local_path"],
            }),
            danger: DangerLevel::Medium,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "remote_download_file",
            description: "Alias of aeroftp_download_file.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "remote_path": {"type": "string"},
                    "local_path": {"type": "string"},
                },
                "required": ["server", "remote_path", "local_path"],
            }),
            danger: DangerLevel::Medium,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "aeroftp_create_directory",
            description: "Create a directory on a remote server.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "path": {"type": "string"},
                },
                "required": ["server", "path"],
            }),
            danger: DangerLevel::Medium,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "remote_mkdir",
            description: "Legacy alias of aeroftp_create_directory.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "path": {"type": "string"},
                },
                "required": ["server", "path"],
            }),
            danger: DangerLevel::Medium,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "remote_create_directory",
            description: "Alias of aeroftp_create_directory.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "path": {"type": "string"},
                },
                "required": ["server", "path"],
            }),
            danger: DangerLevel::Medium,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "aeroftp_delete",
            description: "Delete one or more remote entries.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "path": {"type": "string"},
                    "paths": {"type": "array", "items": {"type": "string"}},
                    "recursive": {"type": "boolean"},
                    "continue_on_error": {"type": "boolean"},
                },
                "required": ["server"],
            }),
            danger: DangerLevel::High,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "remote_delete_many",
            description: "Alias of aeroftp_delete_many.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "paths": {"type": "array", "items": {"type": "string"}},
                    "recursive": {"type": "boolean"},
                    "continue_on_error": {"type": "boolean"},
                },
                "required": ["server", "paths"],
            }),
            danger: DangerLevel::High,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "remote_delete",
            description: "Legacy alias of aeroftp_delete.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "path": {"type": "string"},
                },
                "required": ["server", "path"],
            }),
            danger: DangerLevel::High,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "aeroftp_delete_many",
            description: "Batch delete remote entries.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "paths": {"type": "array", "items": {"type": "string"}},
                    "recursive": {"type": "boolean"},
                    "continue_on_error": {"type": "boolean"},
                },
                "required": ["server", "paths"],
            }),
            danger: DangerLevel::High,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "aeroftp_rename",
            description: "Rename or move a file/directory on a remote server.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "from": {"type": "string"},
                    "to": {"type": "string"},
                },
                "required": ["server", "from", "to"],
            }),
            danger: DangerLevel::Medium,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "remote_rename",
            description: "Legacy alias of aeroftp_rename.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "from": {"type": "string"},
                    "to": {"type": "string"},
                },
                "required": ["server", "from", "to"],
            }),
            danger: DangerLevel::Medium,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "aeroftp_storage_quota",
            description: "Get storage usage and quota information for a remote server.",
            input_schema: json!({
                "type": "object",
                "properties": { "server": {"type": "string"} },
                "required": ["server"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "remote_storage_quota",
            description: "Alias of aeroftp_storage_quota.",
            input_schema: json!({
                "type": "object",
                "properties": { "server": {"type": "string"} },
                "required": ["server"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "aeroftp_agent_connect",
            description: "Single-shot agent connect surface. Returns one JSON payload with per-block status (`connect`, `capabilities`, `quota`, `path`) so the agent can decide go/no-go and gracefully degrade. Replaces the boilerplate sequence of `connect → about → df → ls /`. `connect.status` is the critical signal; `unsupported`/`unavailable`/`error` on other blocks are non-fatal.",
            input_schema: json!({
                "type": "object",
                "properties": { "server": {"type": "string", "description": "Server name or ID (exact match preferred; unique substring also accepted)"} },
                "required": ["server"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "agent_connect",
            description: "Alias of aeroftp_agent_connect.",
            input_schema: json!({
                "type": "object",
                "properties": { "server": {"type": "string"} },
                "required": ["server"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: remote_surfaces,
        },
        ToolDef {
            name: "server_exec",
            description: "Execute a vault-backed server operation without exposing credentials.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "operation": {"type": "string"},
                    "path": {"type": "string"},
                    "pattern": {"type": "string"},
                    "local_path": {"type": "string"},
                    "remote_path": {"type": "string"},
                    "destination": {"type": "string"},
                },
                "required": ["server", "operation"],
            }),
            danger: DangerLevel::High,
            surfaces: remote_surfaces,
        },
        // ─── Area D: rag_*, agent_memory_* (T3 Gate 2) ──────────────
        ToolDef {
            name: "rag_index",
            description: "Index a local directory for retrieval (file list with optional preview).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "recursive": {"type": "boolean"},
                    "max_files": {"type": "integer"},
                },
                "required": ["path"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "rag_search",
            description: "Full-text search inside a local directory (case-insensitive substring).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "path": {"type": "string"},
                    "max_results": {"type": "integer"},
                },
                "required": ["query"],
            }),
            danger: DangerLevel::ReadOnly,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "agent_memory_write",
            description: "Persist a categorized memory note for a project (SQLite-backed).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "entry": {"type": "string"},
                    "category": {"type": "string"},
                    "project_path": {"type": "string"},
                },
                "required": ["entry", "project_path"],
            }),
            danger: DangerLevel::Medium,
            surfaces: local_surfaces,
        },
        ToolDef {
            name: "set_theme",
            description: "Change the application theme.",
            input_schema: json!({ "type": "object", "properties": { "theme": {"type": "string"} }, "required": ["theme"] }),
            danger: DangerLevel::Safe,
            surfaces: Surfaces::GUI,
        },
        ToolDef {
            name: "app_info",
            description: "Get application version, OS, and connection status.",
            input_schema: json!({ "type": "object" }),
            danger: DangerLevel::Safe,
            surfaces: Surfaces::GUI,
        },
        ToolDef {
            name: "sync_control",
            description: "Control background sync process.",
            input_schema: json!({ "type": "object", "properties": { "action": {"type": "string"} }, "required": ["action"] }),
            danger: DangerLevel::Safe,
            surfaces: Surfaces::GUI,
        },
        ToolDef {
            name: "vault_peek",
            description: "Check the number of credentials in the vault without unlocking it.",
            input_schema: json!({ "type": "object", "properties": { "path": {"type": "string"} }, "required": ["path"] }),
            danger: DangerLevel::Safe,
            surfaces: Surfaces::GUI,
        },
        ToolDef {
            name: "cross_profile_transfer",
            description: "Transfer files between two saved remote profiles without downloading locally.",
            input_schema: json!({ "type": "object" }),
            danger: DangerLevel::Medium,
            surfaces: Surfaces::GUI,
        },
        ToolDef {
            name: "preview_edit",
            description: "Preview a find and replace edit.",
            input_schema: json!({ "type": "object" }),
            danger: DangerLevel::Safe,
            surfaces: Surfaces::GUI,
        },
        ToolDef {
            name: "hash_file",
            description: "Compute the hash of a local file.",
            input_schema: json!({ "type": "object" }),
            danger: DangerLevel::Safe,
            surfaces: Surfaces::GUI,
        },
        ToolDef {
            name: "generate_transfer_plan",
            description: "Plan a bulk upload or download operation.",
            input_schema: json!({ "type": "object" }),
            danger: DangerLevel::Safe,
            surfaces: Surfaces::GUI,
        },
        ToolDef {
            name: "upload_files",
            description: "Upload multiple local files to a remote directory.",
            input_schema: json!({ "type": "object" }),
            danger: DangerLevel::Medium,
            surfaces: Surfaces::GUI,
        },
        ToolDef {
            name: "download_files",
            description: "Download multiple remote files to a local directory.",
            input_schema: json!({ "type": "object" }),
            danger: DangerLevel::Safe,
            surfaces: Surfaces::GUI,
        },
        ToolDef {
            name: "sync_preview",
            description: "Preview a directory sync operation.",
            input_schema: json!({ "type": "object" }),
            danger: DangerLevel::Safe,
            surfaces: Surfaces::GUI,
        },
        ToolDef {
            name: "remote_edit",
            description: "Edit a remote file by finding and replacing a string.",
            input_schema: json!({ "type": "object" }),
            danger: DangerLevel::Medium,
            surfaces: Surfaces::GUI,
        },
    ]
});

/// Lookup O(N) nel registry. N è limitato a ~110 tool: è più
/// pratico di una HashMap statica per il Gate 1 (zero setup, facile
/// estendere). Se diventa un hot path si può passare a `phf` o
/// `HashMap<&'static str, &'static ToolDef>` wrappato in `LazyLock`.
pub fn find_tool(name: &str) -> Option<&'static ToolDef> {
    TOOL_DEFINITIONS.iter().find(|t| t.name == name)
}

/// Entry point unico. Gate 1 scheletro:
/// 1. lookup del tool in `TOOL_DEFINITIONS`
/// 2. validazione surface
/// 3. validazione `required` JSON Schema
/// 4. dispatch — in Gate 1 ritorna `ToolError::NotMigrated` per forzare
///    i call site legacy (GUI/CLI/MCP) a continuare a gestire i loro
///    tool fino a Gate 2.
pub async fn dispatch_tool(
    ctx: &dyn ToolCtx,
    tool_name: &str,
    args: &Value,
) -> Result<Value, ToolError> {
    let def = find_tool(tool_name).ok_or_else(|| ToolError::Unknown(tool_name.to_string()))?;
    let surface = ctx.surface();
    if !def.surfaces.contains(surface) {
        return Err(ToolError::NotOnSurface {
            tool: tool_name.to_string(),
            surface,
        });
    }
    validate_required_fields(def, args).map_err(|reason| ToolError::InvalidArgs {
        tool: tool_name.to_string(),
        reason,
    })?;
    match tool_name {
        // ─── Area A: local_* (T3 Gate 2) ─────────────────────────────
        "local_list" => local_tools::local_list(ctx, args).await,
        "local_read" => local_tools::local_read(ctx, args).await,
        "local_write" => local_tools::local_write(ctx, args).await,
        "local_mkdir" => local_tools::local_mkdir(ctx, args).await,
        "local_delete" => local_tools::local_delete(ctx, args).await,
        "local_rename" => local_tools::local_rename(ctx, args).await,
        "local_search" => local_tools::local_search(ctx, args).await,
        "local_edit" => local_tools::local_edit(ctx, args).await,
        "local_move_files" => local_tools::local_move_files(ctx, args).await,
        "local_batch_rename" => local_tools::local_batch_rename(ctx, args).await,
        "local_copy_files" => local_tools::local_copy_files(ctx, args).await,
        "local_trash" => local_tools::local_trash(ctx, args).await,
        "local_file_info" => local_tools::local_file_info(ctx, args).await,
        "local_disk_usage" => local_tools::local_disk_usage(ctx, args).await,
        "local_find_duplicates" => local_tools::local_find_duplicates(ctx, args).await,
        "local_grep" => local_tools::local_grep(ctx, args).await,
        "local_head" => local_tools::local_head(ctx, args).await,
        "local_tail" => local_tools::local_tail(ctx, args).await,
        "local_stat_batch" => local_tools::local_stat_batch(ctx, args).await,
        "local_diff" => local_tools::local_diff(ctx, args).await,
        "local_tree" => local_tools::local_tree(ctx, args).await,
        // ─── Area B: system_* (clipboard/shell/archive) ──────────────────────
        "clipboard_read" => system_tools::clipboard_read(ctx, args).await,
        "clipboard_write" => system_tools::clipboard_write(ctx, args).await,
        "shell_execute" => system_tools::shell_execute(ctx, args).await,
        "archive_compress" => system_tools::archive_compress(ctx, args).await,
        "archive_decompress" => system_tools::archive_decompress(ctx, args).await,
        // ─── Area C: remote_* + aeroftp_* aliases ───────────────────────────
        "aeroftp_list_servers"
        | "remote_list_servers"
        | "server_list_saved"
        | "aeroftp_list_files"
        | "remote_list"
        | "remote_list_files"
        | "aeroftp_read_file"
        | "remote_read"
        | "remote_read_file"
        | "aeroftp_file_info"
        | "remote_info"
        | "remote_file_info"
        | "remote_stat"
        | "aeroftp_search_files"
        | "remote_search"
        | "remote_search_files"
        | "aeroftp_upload_file"
        | "remote_upload"
        | "remote_upload_file"
        | "aeroftp_upload_many"
        | "remote_upload_many"
        | "aeroftp_download_file"
        | "remote_download"
        | "remote_download_file"
        | "aeroftp_create_directory"
        | "remote_mkdir"
        | "remote_create_directory"
        | "aeroftp_delete"
        | "remote_delete"
        | "aeroftp_delete_many"
        | "remote_delete_many"
        | "aeroftp_rename"
        | "remote_rename"
        | "aeroftp_storage_quota"
        | "remote_storage_quota"
        | "aeroftp_agent_connect"
        | "agent_connect"
        | "remote_agent_connect"
        | "server_exec" => remote_tools::dispatch_remote_tool(ctx, tool_name, args).await,
        // ─── Area D: rag_*, agent_memory_* ──────────────────────────────────
        "rag_index" => agent_tools::rag_index(ctx, args).await,
        "rag_search" => agent_tools::rag_search(ctx, args).await,
        "agent_memory_write" => agent_tools::agent_memory_write(ctx, args).await,
        // GUI-specific legacy tools
        "set_theme"
        | "app_info"
        | "sync_control"
        | "vault_peek"
        | "cross_profile_transfer"
        | "preview_edit"
        | "hash_file"
        | "generate_transfer_plan"
        | "upload_files"
        | "download_files"
        | "sync_preview"
        | "remote_edit" => crate::ai_core::gui_tools::dispatch_gui_tool(ctx, tool_name, args).await,

        // Tool presente nel registry ma non ancora wired nel dispatcher.
        _ => Err(ToolError::NotMigrated(tool_name.to_string())),
    }
}

/// Valida la presenza dei campi `required` dichiarati nel JSON Schema
/// del tool. Non valida i tipi (MCP lo fa già più in profondità via
/// `mcp::tools`). Gate 2 estenderà questo a type-check completo.
pub fn validate_required_fields(def: &ToolDef, args: &Value) -> Result<(), String> {
    let Some(req) = def.input_schema.get("required").and_then(|v| v.as_array()) else {
        return Ok(());
    };
    let Some(obj) = args.as_object() else {
        return Err("args must be a JSON object".to_string());
    };
    for r in req {
        if let Some(k) = r.as_str() {
            if !obj.contains_key(k) {
                return Err(format!("missing required field: {k}"));
            }
        }
    }
    Ok(())
}

// Re-exports convenienti per i call site. Non usati internamente qui
// ma servono quando Gate 2 inizia a scrivere tool handlers che vogliono
// costruire Value direttamente.
pub use serde_json::json as _reexport_json;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Helper: ToolDef minimale per i test del validator.
    fn make_def(required: &[&str], surfaces: Surfaces) -> ToolDef {
        let req_val = Value::Array(required.iter().map(|s| json!(*s)).collect());
        ToolDef {
            name: "t",
            description: "",
            input_schema: json!({
                "type": "object",
                "required": req_val,
            }),
            danger: DangerLevel::Safe,
            surfaces,
        }
    }

    #[test]
    fn surfaces_contains_and_combine() {
        let s = Surfaces::GUI | Surfaces::CLI;
        assert!(s.contains(Surfaces::GUI));
        assert!(s.contains(Surfaces::CLI));
        assert!(!s.contains(Surfaces::MCP));
        assert!(!s.contains(Surfaces::GUI | Surfaces::MCP));
        assert!(Surfaces::all().contains(Surfaces::GUI | Surfaces::CLI | Surfaces::MCP));
        assert_eq!(Surfaces::empty().bits(), 0);
        assert_eq!(Surfaces::all().bits(), 0b111);
    }

    #[test]
    fn surfaces_insert_and_remove() {
        let mut s = Surfaces::empty();
        s.insert(Surfaces::GUI);
        s.insert(Surfaces::MCP);
        assert!(s.contains(Surfaces::GUI));
        assert!(s.contains(Surfaces::MCP));
        assert!(!s.contains(Surfaces::CLI));
        s.remove(Surfaces::GUI);
        assert!(!s.contains(Surfaces::GUI));
        assert!(s.contains(Surfaces::MCP));
    }

    #[test]
    fn validate_required_fields_passes_when_all_present() {
        let def = make_def(&["path"], Surfaces::all());
        assert!(validate_required_fields(&def, &json!({"path": "/tmp/x"})).is_ok());
    }

    #[test]
    fn validate_required_fields_rejects_missing_field() {
        let def = make_def(&["path"], Surfaces::all());
        let err = validate_required_fields(&def, &json!({})).unwrap_err();
        assert!(err.contains("missing required field: path"), "got: {err}");
    }

    #[test]
    fn validate_required_fields_rejects_non_object_args() {
        let def = make_def(&["path"], Surfaces::all());
        let err = validate_required_fields(&def, &json!("/tmp/x")).unwrap_err();
        assert!(err.contains("must be a JSON object"), "got: {err}");
    }

    #[test]
    fn validate_required_fields_empty_schema_always_passes() {
        let def = make_def(&[], Surfaces::all());
        assert!(validate_required_fields(&def, &json!({})).is_ok());
        assert!(validate_required_fields(&def, &json!({"any": 1})).is_ok());
    }

    #[test]
    fn find_tool_returns_none_on_empty_registry() {
        // Gate 1: registry is intentionally empty. Once Gate 2 starts
        // populating `TOOL_DEFINITIONS`, this test may need to be
        // adapted to use an injected registry or rely on at least one
        // well-known tool name.
        assert!(find_tool("nonexistent_tool_123").is_none());
    }

    // Minimal ToolCtx mock for dispatcher shape tests. Inline no-op
    // EventSink — we could grow this into a shared helper in Gate 2
    // once multiple test suites need a silent sink.
    struct NoopSink;
    impl EventSink for NoopSink {
        fn emit_stream_chunk(&self, _stream_id: &str, _chunk: &crate::ai_stream::StreamChunk) {}
        fn emit_tool_progress(&self, _progress: &crate::ai_core::ToolProgress) {}
        fn emit_app_control(&self, _event_name: &str, _payload: &Value) {}
    }

    struct MockCtx {
        surface: Surfaces,
        sink: NoopSink,
        creds: MockCreds,
    }

    struct MockCreds;

    impl CredentialProvider for MockCreds {
        fn list_servers(&self) -> Result<Vec<crate::ai_core::ServerProfile>, String> {
            Ok(Vec::new())
        }
        fn get_credentials(
            &self,
            _server_id: &str,
        ) -> Result<crate::ai_core::ServerCredentials, String> {
            Err("mock: no credentials".into())
        }
        fn get_extra_options(
            &self,
            _server_id: &str,
        ) -> Result<std::collections::HashMap<String, String>, String> {
            Ok(std::collections::HashMap::new())
        }
    }

    #[async_trait::async_trait]
    impl ToolCtx for MockCtx {
        fn event_sink(&self) -> &dyn EventSink {
            &self.sink
        }
        fn credentials(&self) -> &dyn CredentialProvider {
            &self.creds
        }
        async fn remote_backend(&self, _server_id: &str) -> Result<Arc<dyn RemoteBackend>, String> {
            Err("mock: no remote backend".into())
        }
        fn surface(&self) -> Surfaces {
            self.surface
        }
    }

    fn mock_ctx(surface: Surfaces) -> MockCtx {
        MockCtx {
            surface,
            sink: NoopSink,
            creds: MockCreds,
        }
    }

    #[tokio::test]
    async fn dispatch_unknown_tool_returns_unknown_error() {
        let ctx = mock_ctx(Surfaces::GUI);
        let err = dispatch_tool(&ctx, "nope_nope", &json!({}))
            .await
            .unwrap_err();
        match err {
            ToolError::Unknown(name) => assert_eq!(name, "nope_nope"),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    // NOTE: dispatch_on_wrong_surface / dispatch_missing_required /
    // dispatch_returns_not_migrated are intentionally deferred to
    // Gate 2 because they require at least one populated ToolDef.
    // The validator tests above cover the surface+required paths at
    // the unit level; Gate 2 will add integration coverage through
    // actual migrated tools.
}
