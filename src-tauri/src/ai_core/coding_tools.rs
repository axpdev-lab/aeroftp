// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Coding-agent specialized tool handlers.

use serde_json::{to_value, Value};
use tauri::Manager;

use crate::ai_core::local_tools::{get_str, get_str_opt, value_as_string_array};
use crate::ai_core::tools::{ToolCtx, ToolError};
use crate::coding_checkpoints::{
    create_checkpoint, restore_checkpoint, CreateCodingCheckpointRequest,
    RestoreCodingCheckpointRequest,
};

pub async fn coding_checkpoint_create(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let app = ctx.tauri_app_handle().ok_or_else(|| {
        ToolError::Exec("coding_checkpoint_create is only available in the GUI".to_string())
    })?;
    let workspace_root = get_str(args, "workspace_root")?;
    let paths = value_as_string_array(args, "paths").map_err(|reason| ToolError::InvalidArgs {
        tool: "coding_checkpoint_create".to_string(),
        reason,
    })?;
    if paths.is_empty() {
        return Err(ToolError::InvalidArgs {
            tool: "coding_checkpoint_create".to_string(),
            reason: "paths cannot be empty".to_string(),
        });
    }

    let db = app.state::<crate::chat_history::ChatHistoryDb>();
    let mut conn = db.0.lock().unwrap_or_else(|e| {
        log::warn!("Chat history DB mutex was poisoned during checkpoint create: {e}");
        e.into_inner()
    });

    let checkpoint = create_checkpoint(
        &mut conn,
        CreateCodingCheckpointRequest {
            workspace_root,
            paths,
            session_id: ctx
                .session_id()
                .map(str::to_string)
                .or_else(|| get_str_opt(args, "session_id")),
            anchor_message_id: get_str_opt(args, "anchor_message_id"),
            conversation_anchor: get_str_opt(args, "conversation_anchor"),
            label: get_str_opt(args, "label"),
        },
    )
    .map_err(ToolError::Exec)?;

    to_value(checkpoint).map_err(|e| ToolError::Exec(e.to_string()))
}

pub async fn coding_checkpoint_restore(
    ctx: &dyn ToolCtx,
    args: &Value,
) -> Result<Value, ToolError> {
    let app = ctx.tauri_app_handle().ok_or_else(|| {
        ToolError::Exec("coding_checkpoint_restore is only available in the GUI".to_string())
    })?;
    let checkpoint_id = get_str(args, "checkpoint_id")?;
    let paths = optional_string_array(args, "paths")?;
    let dry_run = args
        .get("dry_run")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);

    let db = app.state::<crate::chat_history::ChatHistoryDb>();
    let conn = db.0.lock().unwrap_or_else(|e| {
        log::warn!("Chat history DB mutex was poisoned during checkpoint restore: {e}");
        e.into_inner()
    });

    let result = restore_checkpoint(
        &conn,
        RestoreCodingCheckpointRequest {
            checkpoint_id,
            paths,
            dry_run,
        },
    )
    .map_err(ToolError::Exec)?;

    to_value(result).map_err(|e| ToolError::Exec(e.to_string()))
}

fn optional_string_array(args: &Value, key: &str) -> Result<Option<Vec<String>>, ToolError> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    let Some(array) = value.as_array() else {
        return Err(ToolError::InvalidArgs {
            tool: "<coding_checkpoint>".to_string(),
            reason: format!("'{key}' must be an array of strings"),
        });
    };
    let mut out = Vec::new();
    for item in array {
        let Some(s) = item.as_str() else {
            return Err(ToolError::InvalidArgs {
                tool: "<coding_checkpoint>".to_string(),
                reason: format!("'{key}' must contain only strings"),
            });
        };
        out.push(s.to_string());
    }
    Ok(Some(out))
}
