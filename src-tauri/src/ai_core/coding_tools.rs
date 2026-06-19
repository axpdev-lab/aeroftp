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
use crate::coding_git::{
    git_commit, git_diff, git_stage, git_status, CodingGitCommitRequest, CodingGitDiffRequest,
    CodingGitStageRequest,
};
use crate::coding_patches::{apply_coding_patch, preview_coding_patch, ApplyCodingPatchRequest};

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
    let paths = optional_string_array(args, "paths", "coding_checkpoint_restore")?;
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

pub async fn coding_apply_patch(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let workspace_root = get_str(args, "workspace_root")?;
    let patch = get_str(args, "patch")?;
    let dry_run = args
        .get("dry_run")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);

    let request = ApplyCodingPatchRequest {
        workspace_root: workspace_root.clone(),
        patch: patch.clone(),
        dry_run,
    };

    if dry_run {
        let result = preview_coding_patch(request).map_err(ToolError::Exec)?;
        return to_value(result).map_err(|e| ToolError::Exec(e.to_string()));
    }

    let preview = preview_coding_patch(ApplyCodingPatchRequest {
        workspace_root: workspace_root.clone(),
        patch: patch.clone(),
        dry_run: true,
    })
    .map_err(ToolError::Exec)?;

    if !preview.success {
        return to_value(preview).map_err(|e| ToolError::Exec(e.to_string()));
    }

    let app = ctx.tauri_app_handle().ok_or_else(|| {
        ToolError::Exec("coding_apply_patch is only available in the GUI".to_string())
    })?;
    let paths: Vec<String> = preview.files.iter().map(|file| file.path.clone()).collect();
    let checkpoint_label = get_str_opt(args, "checkpoint_label")
        .unwrap_or_else(|| "Before coding_apply_patch".to_string());

    let checkpoint = {
        let db = app.state::<crate::chat_history::ChatHistoryDb>();
        let mut conn = db.0.lock().unwrap_or_else(|e| {
            log::warn!("Chat history DB mutex was poisoned during patch checkpoint: {e}");
            e.into_inner()
        });
        create_checkpoint(
            &mut conn,
            CreateCodingCheckpointRequest {
                workspace_root: workspace_root.clone(),
                paths,
                session_id: ctx.session_id().map(str::to_string),
                anchor_message_id: get_str_opt(args, "anchor_message_id"),
                conversation_anchor: get_str_opt(args, "conversation_anchor"),
                label: Some(checkpoint_label),
            },
        )
        .map_err(ToolError::Exec)?
    };

    let mut result = apply_coding_patch(request).map_err(|error| {
        ToolError::Exec(format!(
            "{}. A checkpoint was created before applying: {}",
            error, checkpoint.id
        ))
    })?;
    result.checkpoint_id = Some(checkpoint.id);

    to_value(result).map_err(|e| ToolError::Exec(e.to_string()))
}

pub async fn coding_git_status(_ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let workspace_root = get_str(args, "workspace_root")?;
    let paths = optional_string_array(args, "paths", "coding_git_status")?;
    let result = git_status(workspace_root, paths).map_err(ToolError::Exec)?;
    to_value(result).map_err(|e| ToolError::Exec(e.to_string()))
}

pub async fn coding_git_diff(_ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let workspace_root = get_str(args, "workspace_root")?;
    let paths = optional_string_array(args, "paths", "coding_git_diff")?;
    let staged = args
        .get("staged")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let max_bytes = args
        .get("max_bytes")
        .and_then(|value| value.as_u64())
        .map(|value| value as usize);
    let result = git_diff(CodingGitDiffRequest {
        workspace_root,
        paths,
        staged,
        max_bytes,
    })
    .map_err(ToolError::Exec)?;
    to_value(result).map_err(|e| ToolError::Exec(e.to_string()))
}

pub async fn coding_git_stage(_ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let workspace_root = get_str(args, "workspace_root")?;
    let paths = value_as_string_array(args, "paths").map_err(|reason| ToolError::InvalidArgs {
        tool: "coding_git_stage".to_string(),
        reason,
    })?;
    let dry_run = args
        .get("dry_run")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let result = git_stage(CodingGitStageRequest {
        workspace_root,
        paths,
        dry_run,
    })
    .map_err(ToolError::Exec)?;
    to_value(result).map_err(|e| ToolError::Exec(e.to_string()))
}

pub async fn coding_git_commit(_ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let workspace_root = get_str(args, "workspace_root")?;
    let message = get_str(args, "message")?;
    let dry_run = args
        .get("dry_run")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let result = git_commit(CodingGitCommitRequest {
        workspace_root,
        message,
        dry_run,
    })
    .map_err(ToolError::Exec)?;
    to_value(result).map_err(|e| ToolError::Exec(e.to_string()))
}

fn optional_string_array(
    args: &Value,
    key: &str,
    tool: &str,
) -> Result<Option<Vec<String>>, ToolError> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    let Some(array) = value.as_array() else {
        return Err(ToolError::InvalidArgs {
            tool: tool.to_string(),
            reason: format!("'{key}' must be an array of strings"),
        });
    };
    let mut out = Vec::new();
    for item in array {
        let Some(s) = item.as_str() else {
            return Err(ToolError::InvalidArgs {
                tool: tool.to_string(),
                reason: format!("'{key}' must contain only strings"),
            });
        };
        out.push(s.to_string());
    }
    Ok(Some(out))
}
