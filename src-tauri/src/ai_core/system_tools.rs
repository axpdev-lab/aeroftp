// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use super::local_tools::{get_str, get_str_opt, resolve_local_path, validate_path};
use super::tools::{ToolCtx, ToolError};
use serde_json::{json, Value};

fn map_str_err(e: String) -> ToolError {
    ToolError::Exec(e)
}

pub async fn clipboard_read(_ctx: &dyn ToolCtx, _args: &Value) -> Result<Value, ToolError> {
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|e| ToolError::Exec(format!("Failed to access clipboard: {}", e)))?;
    let content = clipboard
        .get_text()
        .map_err(|e| ToolError::Exec(format!("Failed to read clipboard: {}", e)))?;

    Ok(json!({
        "success": true,
        "content": content,
        "length": content.len(),
    }))
}

pub async fn clipboard_write(_ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let content = get_str(args, "content")?;
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|e| ToolError::Exec(format!("Failed to access clipboard: {}", e)))?;
    clipboard
        .set_text(&content)
        .map_err(|e| ToolError::Exec(format!("Failed to write clipboard: {}", e)))?;

    Ok(json!({
        "success": true,
        "message": format!("Copied {} characters to clipboard", content.len()),
    }))
}

pub async fn shell_execute(_ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let command = get_str(args, "command")?;
    let working_dir = get_str_opt(args, "working_dir");
    let timeout_secs = args
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(30)
        .min(120);

    // Defense-in-depth: validate working_dir against system deny-list BEFORE
    // delegating to the legacy GUI helper (which only checks existence).
    // Risolve C1+M2 finding del Gate 2 audit: il legacy CLI rifiutava
    // working_dir su /etc/shadow, /root, /boot, ecc.; il fast-path
    // unificato deve mantenere la stessa garanzia su tutte le surface.
    if let Some(ref wd) = working_dir {
        if !wd.is_empty() {
            validate_path(wd, "working_dir").map_err(map_str_err)?;
        }
    }

    // Call the legacy helper from ai_tools.rs since it has all the denylist logic.
    let result = crate::ai_tools::shell_execute(command, working_dir, Some(timeout_secs))
        .await
        .map_err(ToolError::Exec)?;

    Ok(result)
}

pub async fn archive_compress(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let base = ctx.context_local_path();
    let paths: Vec<String> = args
        .get("paths")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| resolve_local_path(s, base)))
                .collect()
        })
        .ok_or_else(|| ToolError::InvalidArgs {
            tool: "archive_compress".into(),
            reason: "Missing 'paths' array parameter".into(),
        })?;

    if paths.is_empty() {
        return Err(ToolError::InvalidArgs {
            tool: "archive_compress".into(),
            reason: "'paths' array is empty".into(),
        });
    }

    let output_path = resolve_local_path(&get_str(args, "output_path")?, base);
    let format = get_str_opt(args, "format").unwrap_or_else(|| "zip".to_string());
    let password = get_str_opt(args, "password");
    let compression_level = args.get("compression_level").and_then(|v| v.as_i64());

    validate_path(&output_path, "output_path").map_err(map_str_err)?;
    for p in &paths {
        validate_path(p, "path").map_err(map_str_err)?;
    }

    let result = match format.as_str() {
        "zip" => {
            crate::compress_files_core(paths, output_path.clone(), password, compression_level)
                .await
        }
        "7z" => {
            crate::compress_7z_core(paths, output_path.clone(), password, compression_level).await
        }
        "tar" | "tar.gz" | "tar.bz2" | "tar.xz" => {
            crate::compress_tar_core(
                paths,
                output_path.clone(),
                format.clone(),
                compression_level,
            )
            .await
        }
        _ => Err(format!(
            "Unsupported format: {}. Use zip, 7z, tar, tar.gz, tar.bz2, or tar.xz",
            format
        )),
    };

    match result {
        Ok(msg) => Ok(json!({
            "success": true,
            "message": msg,
            "output_path": output_path,
            "format": format,
        })),
        Err(e) => Err(ToolError::Exec(e)),
    }
}

pub async fn archive_decompress(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let base = ctx.context_local_path();
    let archive_path = resolve_local_path(&get_str(args, "archive_path")?, base);
    let output_dir = resolve_local_path(&get_str(args, "output_dir")?, base);
    let password = get_str_opt(args, "password");
    let create_subfolder = args
        .get("create_subfolder")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    validate_path(&archive_path, "archive_path").map_err(map_str_err)?;
    validate_path(&output_dir, "output_dir").map_err(map_str_err)?;

    // Detect format from extension
    let lower = archive_path.to_lowercase();
    let result = if lower.ends_with(".zip") {
        crate::extract_archive_core(
            archive_path.clone(),
            output_dir.clone(),
            create_subfolder,
            password,
        )
        .await
    } else if lower.ends_with(".7z") {
        crate::extract_7z_core(
            archive_path.clone(),
            output_dir.clone(),
            password,
            create_subfolder,
        )
        .await
    } else if lower.ends_with(".tar")
        || lower.ends_with(".tar.gz")
        || lower.ends_with(".tgz")
        || lower.ends_with(".tar.bz2")
        || lower.ends_with(".tar.xz")
    {
        crate::extract_tar_core(archive_path.clone(), output_dir.clone(), create_subfolder).await
    } else {
        Err(format!("Unsupported archive format: {}", archive_path))
    };

    match result {
        Ok(msg) => Ok(json!({
            "success": true,
            "message": msg,
            "archive_path": archive_path,
            "output_dir": output_dir,
        })),
        Err(e) => Err(ToolError::Exec(e)),
    }
}
