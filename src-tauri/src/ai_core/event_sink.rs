//! EventSink trait — abstracts event emission from Tauri AppHandle
//!
//! Replaces all `app.emit()` calls in ai_tools.rs and ai_stream.rs with
//! a trait that can be implemented for both Tauri (GUI) and CLI contexts.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use crate::ai_stream::StreamChunk;
use serde::Serialize;
use serde_json::Value;

/// Progress info for iterative tool operations (batch move, rename, copy, etc.)
#[derive(Debug, Clone, Serialize)]
pub struct ToolProgress {
    pub tool: String,
    pub current: u32,
    pub total: u32,
    pub item: String,
}

/// Abstraction over event emission — Tauri GUI or CLI stdout/stderr.
pub trait EventSink: Send + Sync {
    /// Emit a streaming chunk to a named stream channel.
    /// In Tauri: `app.emit(&format!("ai-stream-{}", stream_id), chunk)`
    /// In CLI: write JSON line to stdout or render incremental text.
    fn emit_stream_chunk(&self, stream_id: &str, chunk: &StreamChunk);

    /// Emit tool progress for batch operations.
    /// In Tauri: `app.emit("ai-tool-progress", progress)`
    /// In CLI: update progress bar on stderr.
    fn emit_tool_progress(&self, progress: &ToolProgress);

    /// Emit an app-control event (theme change, sync start/stop).
    /// In Tauri: `app.emit("ai-set-theme", payload)` etc.
    /// In CLI: no-op or log warning.
    fn emit_app_control(&self, event_name: &str, payload: &Value);
}
