// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use super::local_tools::{
    get_str, get_str_opt, protected_location_creatable_within, resolve_local_path, validate_path,
};
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

    let policy = |path: &std::path::Path| {
        let path = path.to_str().ok_or("Archive path is not valid UTF-8")?;
        validate_path(path, "path")
    };
    let result = crate::compress_archive_with_policy(
        paths,
        output_path.clone(),
        &format,
        password,
        compression_level,
        &policy,
    )
    .await;

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

/// The folders an extraction of `archive_path` may write into: the output
/// folder itself, or the per-archive subfolder under every name an extractor
/// gives it (the ZIP, 7z and RAR extractors take the file stem, the tar one
/// also trims `.tar`, the single-stream one strips the whole extension).
fn extraction_targets(archive_path: &str, output_dir: &str, create_subfolder: bool) -> Vec<String> {
    if !create_subfolder {
        return vec![output_dir.to_string()];
    }
    let path = std::path::Path::new(archive_path);
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let file_stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_default();
    let tar_stem = file_stem
        .strip_suffix(".tar")
        .unwrap_or(&file_stem)
        .to_string();
    let mut targets: Vec<String> = Vec::new();
    for stem in [
        file_stem.clone(),
        tar_stem,
        crate::archive_extract_stem(&file_name),
    ] {
        let target = std::path::Path::new(output_dir)
            .join(crate::safe_extract_folder_name(&stem))
            .to_string_lossy()
            .to_string();
        if !targets.contains(&target) {
            targets.push(target);
        }
    }
    targets
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

    // Every folder the entries may land in must pass the policy, and must not
    // be or hold a protected location, existing or not: an extraction creates
    // the folders its entries name (deep scan item 42: an archive extracted
    // into the home wrote `~/.ssh/authorized_keys`, also on a home without
    // `~/.ssh`). The extraction itself still opens the subfolder through the
    // chosen output folder, so a symlinked subfolder cannot lead it elsewhere.
    for target in extraction_targets(&archive_path, &output_dir, create_subfolder) {
        validate_path(&target, "output_dir").map_err(map_str_err)?;
        if let Some(protected) = protected_location_creatable_within(&target) {
            return Err(ToolError::Denied(format!(
                "Refusing to extract into {}: it could create the protected path {}. Extract into a subfolder instead.",
                target, protected
            )));
        }
    }

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
    } else if lower.ends_with(".gz") || lower.ends_with(".xz") || lower.ends_with(".bz2") {
        // Standalone single-stream codecs (no tar wrapper); checked after the tar
        // family so `.tar.gz` etc. stay on the tar lane.
        crate::extract_single_core(archive_path.clone(), output_dir.clone(), create_subfolder).await
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
