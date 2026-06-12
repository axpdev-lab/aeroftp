//! Error-correction tool handlers (`aeroftp_correct_*`), MCP surface.
//!
//! These expose the standalone `.aerocorrect` sidecar lifecycle (the same one the
//! CLI `correct` subcommand drives) to an MCP agent: generate a detached
//! Reed-Solomon recovery file for a local file, verify it read-only, or repair the
//! file in place. They wrap the canonical `crate::error_correction::correct_*` API
//! so the wire format and the fail-closed repair contract stay identical across the
//! CLI and MCP.
//!
//! Paths are resolved relative to `ctx.context_local_path()` and validated with the
//! shared local-tools deny-list (no traversal, no system directories), exactly like
//! the `local_*` handlers. The codec itself is synchronous and can stream a file up
//! to the 1 GiB EC cap, so each call runs on `spawn_blocking`.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use serde_json::Value;

use crate::ai_core::local_tools::{get_str, get_str_opt, resolve_local_path, validate_path};
use crate::ai_core::tools::{ToolCtx, ToolError};

/// Generate a detached `.aerocorrect` sidecar for a local file.
///
/// Args: `file` (required), `level` (optional 5-50 overhead percent, default 15),
/// `out` (optional sidecar path override, default `<file>.aerocorrect`).
pub async fn correct_gen(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let base = ctx.context_local_path();
    let file = resolve_local_path(&get_str(args, "file")?, base);
    validate_path(&file, "file").map_err(ToolError::Denied)?;
    let level = args.get("level").and_then(|v| v.as_u64()).unwrap_or(15) as u32;
    let out = match get_str_opt(args, "out") {
        Some(o) => {
            let resolved = resolve_local_path(&o, base);
            validate_path(&resolved, "out").map_err(ToolError::Denied)?;
            Some(resolved)
        }
        None => None,
    };

    let report = tokio::task::spawn_blocking(move || {
        crate::error_correction::correct_generate(&file, level, out.as_deref())
    })
    .await
    .map_err(|e| ToolError::Exec(e.to_string()))?
    .map_err(ToolError::Exec)?;

    serde_json::to_value(report).map_err(|e| ToolError::Exec(e.to_string()))
}

/// Verify a local file against its `.aerocorrect` sidecar (read-only, never mutates).
///
/// Args: `file` (required), `parity` (optional sidecar path override).
pub async fn correct_verify(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let base = ctx.context_local_path();
    let file = resolve_local_path(&get_str(args, "file")?, base);
    validate_path(&file, "file").map_err(ToolError::Denied)?;
    let parity = match get_str_opt(args, "parity") {
        Some(p) => {
            let resolved = resolve_local_path(&p, base);
            validate_path(&resolved, "parity").map_err(ToolError::Denied)?;
            Some(resolved)
        }
        None => None,
    };

    let report = tokio::task::spawn_blocking(move || {
        crate::error_correction::correct_verify(&file, parity.as_deref())
    })
    .await
    .map_err(|e| ToolError::Exec(e.to_string()))?
    .map_err(ToolError::Exec)?;

    serde_json::to_value(report).map_err(|e| ToolError::Exec(e.to_string()))
}

/// Repair a local file in place from its `.aerocorrect` sidecar (atomic,
/// all-or-nothing; an already-intact file is reported `verified` with no write).
///
/// Args: `file` (required), `parity` (optional sidecar path override).
pub async fn correct_repair(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let base = ctx.context_local_path();
    let file = resolve_local_path(&get_str(args, "file")?, base);
    validate_path(&file, "file").map_err(ToolError::Denied)?;
    let parity = match get_str_opt(args, "parity") {
        Some(p) => {
            let resolved = resolve_local_path(&p, base);
            validate_path(&resolved, "parity").map_err(ToolError::Denied)?;
            Some(resolved)
        }
        None => None,
    };

    let report = tokio::task::spawn_blocking(move || {
        crate::error_correction::correct_repair(&file, parity.as_deref())
    })
    .await
    .map_err(|e| ToolError::Exec(e.to_string()))?
    .map_err(ToolError::Exec)?;

    serde_json::to_value(report).map_err(|e| ToolError::Exec(e.to_string()))
}
