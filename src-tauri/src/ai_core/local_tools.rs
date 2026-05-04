//! Local-filesystem tool handlers condivisi GUI + CLI (T3 Gate 2 Area A).
//!
//! Ogni funzione `pub async fn local_xxx(ctx, args) -> Result<Value, ToolError>`
//! è l'unica implementazione canonica del tool: i dispatcher legacy
//! (`execute_ai_tool` in ai_tools.rs, `execute_cli_tool` in bin/aeroftp_cli.rs)
//! delegano qui via `ai_core::tools::dispatch_tool`.
//!
//! Semantica scelta (vedi handoff doc):
//! - path resolution via `ctx.context_local_path()`: CLI torna `None` e il
//!   comportamento resta equivalente al `resolve_path` pre-T3.
//! - async con `spawn_blocking` per disk_usage/find_duplicates/grep (erano
//!   sync nel CLI; la conversione è una win di non-bloccante).
//! - output shape GUI = canonica. CLI cambia forma ma in senso additivo
//!   (campi extra, non rimossi). Vedi CHANGELOG per il riepilogo.
//! - progress events via `ctx.event_sink().emit_tool_progress()`: CLI
//!   li scrive su stderr, GUI li emette via Tauri.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use serde_json::{json, Value};

use crate::ai_core::event_sink::ToolProgress;
use crate::ai_core::tools::{ToolCtx, ToolError};

// ─── Helpers (portati da ai_tools.rs, self-contained) ────────────────────

/// Risoluzione relativa. `base` = `ctx.context_local_path()`.
/// Se `path` è già assoluto, non viene modificato.
pub(crate) fn resolve_local_path(path: &str, base: Option<&str>) -> String {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return path.to_string();
    }
    if let Some(base_dir) = base {
        if !base_dir.is_empty() {
            return format!("{}/{}", base_dir.trim_end_matches('/'), path);
        }
    }
    path.to_string()
}

/// Component-aware prefix check. `/bootcamp` non matcha `/boot`,
/// `/boot/efi` sì. Risolve M-1 finding del Gate 2 audit.
pub(crate) fn path_matches_prefix(path: &str, prefix: &str) -> bool {
    path == prefix || path.starts_with(&format!("{}/", prefix))
}

/// Path validation con deny-list sistema. Ricalca `ai_tools::validate_path`
/// usando matching component-aware (vedi `path_matches_prefix`) per evitare
/// false positive su prefissi parziali (es. `/bootcamp` vs `/boot`).
pub(crate) fn validate_path(path: &str, param: &str) -> Result<(), String> {
    if path.len() > 4096 {
        return Err(format!("{}: path exceeds 4096 characters", param));
    }
    if path.contains('\0') {
        return Err(format!("{}: path contains null bytes", param));
    }
    let normalized = path.replace('\\', "/");
    for component in normalized.split('/') {
        if component == ".." {
            return Err(format!("{}: path traversal ('..') not allowed", param));
        }
    }
    let resolved = std::fs::canonicalize(path).or_else(|_| {
        std::path::Path::new(path)
            .parent()
            .map(std::fs::canonicalize)
            .unwrap_or(Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no parent",
            )))
    });
    if let Ok(canonical) = resolved {
        let s = canonical.to_string_lossy();
        let denied = [
            "/proc",
            "/sys",
            "/dev",
            "/boot",
            "/root",
            "/etc/shadow",
            "/etc/passwd",
            "/etc/ssh",
            "/etc/sudoers",
        ];
        if denied.iter().any(|d| path_matches_prefix(&s, d)) {
            return Err(format!("{}: access to system path denied: {}", param, s));
        }
        if let Ok(home) = std::env::var("HOME") {
            let home_denied = [
                ".ssh",
                ".gnupg",
                ".aws",
                ".kube",
                ".config/gcloud",
                ".docker",
                ".config/aeroftp",
                ".vault-token",
            ];
            for sensitive in &home_denied {
                if path_matches_prefix(&s, &format!("{}/{}", home, sensitive)) {
                    return Err(format!("{}: access to sensitive path denied: {}", param, s));
                }
            }
        }
        if path_matches_prefix(&s, "/run/secrets") {
            return Err(format!("{}: access to system path denied: {}", param, s));
        }
    }
    Ok(())
}

pub(crate) fn get_str(args: &Value, key: &str) -> Result<String, ToolError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| ToolError::InvalidArgs {
            tool: "<local>".to_string(),
            reason: format!("Missing required argument: {}", key),
        })
}

pub(crate) fn get_str_opt(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Estrae un array di stringhe da `args[key]`. Errore se la chiave
/// manca, non è un array, o non contiene almeno una stringa. Usato da
/// gui_tools / system_tools / handler che accettano `paths: []`,
/// `items: []`, ecc. Single source of truth per il pattern.
pub(crate) fn value_as_string_array(args: &Value, key: &str) -> Result<Vec<String>, String> {
    args.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .ok_or_else(|| format!("Missing '{}' array parameter", key))
}

fn map_err<E: std::fmt::Display>(e: E) -> ToolError {
    ToolError::Exec(e.to_string())
}

fn map_str_err(e: String) -> ToolError {
    ToolError::Exec(e)
}

fn progress(ctx: &dyn ToolCtx, tool: &str, current: u32, total: u32, item: &str) {
    ctx.event_sink().emit_tool_progress(&ToolProgress {
        tool: tool.to_string(),
        current,
        total,
        item: item.to_string(),
    });
}

fn human_bytes(size: u64) -> String {
    if size < 1024 {
        format!("{} B", size)
    } else if size < 1_048_576 {
        format!("{:.1} KB", size as f64 / 1024.0)
    } else if size < 1_073_741_824 {
        format!("{:.1} MB", size as f64 / 1_048_576.0)
    } else {
        format!("{:.2} GB", size as f64 / 1_073_741_824.0)
    }
}

fn resolve_paths_array(args: &Value, base: Option<&str>) -> Result<Vec<String>, ToolError> {
    let arr = args
        .get("paths")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ToolError::InvalidArgs {
            tool: "<local>".to_string(),
            reason: "Missing 'paths' array parameter".to_string(),
        })?;
    Ok(arr
        .iter()
        .filter_map(|v| v.as_str().map(|s| resolve_local_path(s, base)))
        .collect())
}

// ─── Handlers ────────────────────────────────────────────────────────────

pub async fn local_list(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let path = resolve_local_path(&get_str(args, "path")?, ctx.context_local_path());
    validate_path(&path, "path").map_err(map_str_err)?;

    let entries: Vec<Value> = std::fs::read_dir(&path)
        .map_err(|e| ToolError::Exec(format!("Failed to read directory: {}", e)))?
        .filter_map(|e| e.ok())
        .take(100)
        .map(|e| {
            let meta = e.metadata().ok();
            json!({
                "name": e.file_name().to_string_lossy(),
                "is_dir": meta.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                "size": meta.as_ref().map(|m| m.len()).unwrap_or(0),
            })
        })
        .collect();

    Ok(json!({ "entries": entries }))
}

pub async fn local_search(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let path = resolve_local_path(&get_str(args, "path")?, ctx.context_local_path());
    let pattern = get_str(args, "pattern")?;
    validate_path(&path, "path").map_err(map_str_err)?;

    let pattern_lower = pattern.to_lowercase();

    let matcher: Box<dyn Fn(&str) -> bool> = if let Some(suffix) = pattern_lower.strip_prefix('*') {
        let suffix = suffix.to_string();
        Box::new(move |name: &str| name.ends_with(&suffix))
    } else if let Some(prefix) = pattern_lower.strip_suffix('*') {
        let prefix = prefix.to_string();
        Box::new(move |name: &str| name.starts_with(&prefix))
    } else if pattern_lower.contains('*') {
        let parts: Vec<String> = pattern_lower.split('*').map(String::from).collect();
        Box::new(move |name: &str| parts.iter().all(|part| name.contains(part.as_str())))
    } else {
        let pat = pattern_lower.clone();
        Box::new(move |name: &str| name.contains(&pat))
    };

    let results: Vec<Value> = std::fs::read_dir(&path)
        .map_err(|e| ToolError::Exec(format!("Failed to read directory: {}", e)))?
        .filter_map(|e| e.ok())
        .filter(|e| matcher(&e.file_name().to_string_lossy().to_lowercase()))
        .take(100)
        .map(|e| {
            let meta = e.metadata().ok();
            json!({
                "name": e.file_name().to_string_lossy(),
                "is_dir": meta.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                "size": meta.as_ref().map(|m| m.len()).unwrap_or(0),
            })
        })
        .collect();

    let total = results.len();
    Ok(json!({
        "results": results,
        "total": total,
    }))
}

pub async fn local_read(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let path = resolve_local_path(&get_str(args, "path")?, ctx.context_local_path());
    validate_path(&path, "path").map_err(map_str_err)?;

    let meta = std::fs::metadata(&path)
        .map_err(|e| ToolError::Exec(format!("Failed to stat file: {}", e)))?;
    if meta.len() > 10_485_760 {
        return Err(ToolError::Exec(format!(
            "File too large for local_read: {:.1} MB (max 10 MB)",
            meta.len() as f64 / 1_048_576.0
        )));
    }

    let max_bytes: usize = 5120;
    let file_size = meta.len() as usize;
    let read_size = std::cmp::min(file_size, max_bytes);
    let mut file = std::fs::File::open(&path)
        .map_err(|e| ToolError::Exec(format!("Failed to open file: {}", e)))?;
    let mut buf = vec![0u8; read_size];
    use std::io::Read;
    file.read_exact(&mut buf)
        .map_err(|e| ToolError::Exec(format!("Failed to read file: {}", e)))?;

    let truncated = file_size > max_bytes;
    let content = String::from_utf8_lossy(&buf).to_string();

    Ok(json!({
        "content": content,
        "size": file_size,
        "truncated": truncated,
    }))
}

pub async fn local_write(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let path = resolve_local_path(&get_str(args, "path")?, ctx.context_local_path());
    let content = get_str(args, "content")?;
    validate_path(&path, "path").map_err(map_str_err)?;

    std::fs::write(&path, &content)
        .map_err(|e| ToolError::Exec(format!("Failed to write file: {}", e)))?;

    Ok(json!({
        "success": true,
        "message": format!("Written {} bytes to {}", content.len(), path),
    }))
}

pub async fn local_mkdir(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let path = resolve_local_path(&get_str(args, "path")?, ctx.context_local_path());
    validate_path(&path, "path").map_err(map_str_err)?;

    std::fs::create_dir_all(&path)
        .map_err(|e| ToolError::Exec(format!("Failed to create directory: {}", e)))?;

    Ok(json!({
        "success": true,
        "message": format!("Created directory {}", path),
    }))
}

pub async fn local_delete(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let path = resolve_local_path(&get_str(args, "path")?, ctx.context_local_path());
    validate_path(&path, "path").map_err(map_str_err)?;

    let home_dir = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    let normalized = path.trim_end_matches('/').trim_end_matches('\\');
    if normalized.is_empty()
        || normalized == "/"
        || normalized == "~"
        || normalized == "."
        || normalized == ".."
        || normalized == home_dir
    {
        return Err(ToolError::Denied(format!(
            "Refusing to delete dangerous path: {}",
            path
        )));
    }

    let meta =
        std::fs::metadata(&path).map_err(|e| ToolError::Exec(format!("Path not found: {}", e)))?;
    if meta.is_dir() {
        std::fs::remove_dir_all(&path)
            .map_err(|e| ToolError::Exec(format!("Failed to delete directory: {}", e)))?;
    } else {
        std::fs::remove_file(&path)
            .map_err(|e| ToolError::Exec(format!("Failed to delete file: {}", e)))?;
    }

    Ok(json!({
        "success": true,
        "message": format!("Deleted {}", path),
    }))
}

pub async fn local_rename(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let base = ctx.context_local_path();
    let from = resolve_local_path(&get_str(args, "from")?, base);
    let to = resolve_local_path(&get_str(args, "to")?, base);
    validate_path(&from, "from").map_err(map_str_err)?;
    validate_path(&to, "to").map_err(map_str_err)?;

    std::fs::rename(&from, &to).map_err(|e| ToolError::Exec(format!("Failed to rename: {}", e)))?;

    Ok(json!({
        "success": true,
        "message": format!("Renamed {} to {}", from, to),
    }))
}

pub async fn local_move_files(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let base = ctx.context_local_path();
    let paths = resolve_paths_array(args, base)?;
    let destination = resolve_local_path(&get_str(args, "destination")?, base);
    validate_path(&destination, "destination").map_err(map_str_err)?;

    if paths.is_empty() {
        return Err(ToolError::InvalidArgs {
            tool: "local_move_files".into(),
            reason: "'paths' array is empty".into(),
        });
    }

    std::fs::create_dir_all(&destination)
        .map_err(|e| ToolError::Exec(format!("Failed to create destination directory: {}", e)))?;

    let mut moved = Vec::new();
    let mut errors = Vec::new();
    let total = paths.len();

    for (idx, source) in paths.iter().enumerate() {
        if let Err(e) = validate_path(source, "path") {
            errors.push(json!({ "file": source, "error": e }));
            continue;
        }
        let filename = std::path::Path::new(source)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());
        let dest_path = format!("{}/{}", destination.trim_end_matches('/'), filename);

        progress(
            ctx,
            "local_move_files",
            idx as u32 + 1,
            total as u32,
            &filename,
        );

        match std::fs::rename(source, &dest_path) {
            Ok(_) => moved.push(filename),
            Err(_) => {
                match std::fs::copy(source, &dest_path).and_then(|_| std::fs::remove_file(source)) {
                    Ok(_) => moved.push(filename),
                    Err(e) => errors.push(json!({ "file": filename, "error": e.to_string() })),
                }
            }
        }
    }

    Ok(json!({
        "moved": moved.len(),
        "failed": errors.len(),
        "total": total,
        "files": moved,
        "errors": errors,
    }))
}

pub async fn local_batch_rename(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let base = ctx.context_local_path();
    let paths = resolve_paths_array(args, base)?;
    let mode = get_str(args, "mode")?;

    if paths.is_empty() {
        return Err(ToolError::InvalidArgs {
            tool: "local_batch_rename".into(),
            reason: "'paths' array is empty".into(),
        });
    }

    fn split_name_ext(name: &str, is_dir: bool) -> (&str, &str) {
        if is_dir {
            return (name, "");
        }
        match name.rfind('.') {
            Some(pos) if pos > 0 => (&name[..pos], &name[pos..]),
            _ => (name, ""),
        }
    }

    let mut renames: Vec<(String, String)> = Vec::new();
    let mut errors = Vec::new();

    for (idx, source) in paths.iter().enumerate() {
        if let Err(e) = validate_path(source, "path") {
            errors.push(json!({ "file": source, "error": e }));
            continue;
        }
        let src_path = std::path::Path::new(source);
        let filename = src_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let is_dir = src_path.is_dir();
        let (name_no_ext, ext) = split_name_ext(&filename, is_dir);

        let new_name = match mode.as_str() {
            "find_replace" => {
                let find = get_str_opt(args, "find").unwrap_or_default();
                let replace_with = get_str_opt(args, "replace").unwrap_or_default();
                let case_sensitive = args
                    .get("case_sensitive")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if find.is_empty() {
                    filename.clone()
                } else if case_sensitive {
                    filename.replace(&find, &replace_with)
                } else {
                    let lower_find = find.to_lowercase();
                    let lower_name = filename.to_lowercase();
                    let mut result = String::new();
                    let mut start = 0;
                    while let Some(pos) = lower_name[start..].find(&lower_find) {
                        result.push_str(&filename[start..start + pos]);
                        result.push_str(&replace_with);
                        start += pos + find.len();
                    }
                    result.push_str(&filename[start..]);
                    result
                }
            }
            "add_prefix" => {
                let prefix = get_str_opt(args, "prefix").unwrap_or_default();
                format!("{}{}", prefix, filename)
            }
            "add_suffix" => {
                let suffix = get_str_opt(args, "suffix").unwrap_or_default();
                format!("{}{}{}", name_no_ext, suffix, ext)
            }
            "sequential" => {
                let base_name =
                    get_str_opt(args, "base_name").unwrap_or_else(|| "file".to_string());
                let start_number = args
                    .get("start_number")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1);
                let padding = args.get("padding").and_then(|v| v.as_u64()).unwrap_or(2) as usize;
                let num = start_number + idx as u64;
                format!("{}_{:0>width$}{}", base_name, num, ext, width = padding)
            }
            _ => {
                errors.push(
                    json!({ "file": filename, "error": format!("Unknown rename mode: {}", mode) }),
                );
                continue;
            }
        };

        if new_name != filename && !new_name.trim().is_empty() {
            let parent = src_path.parent().unwrap_or(std::path::Path::new("/"));
            let dest = parent.join(&new_name).to_string_lossy().to_string();
            renames.push((source.clone(), dest));
        }
    }

    let new_names: Vec<&str> = renames.iter().map(|(_, d)| d.as_str()).collect();
    let mut seen = std::collections::HashSet::new();
    for name in &new_names {
        if !seen.insert(*name) {
            return Err(ToolError::Exec(format!(
                "Naming conflict detected: multiple files would be renamed to '{}'",
                name
            )));
        }
    }

    let mut renamed = Vec::new();
    let total = renames.len();
    for (idx, (from, to)) in renames.iter().enumerate() {
        let filename = std::path::Path::new(from)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        progress(
            ctx,
            "local_batch_rename",
            idx as u32 + 1,
            total as u32,
            &filename,
        );
        match std::fs::rename(from, to) {
            Ok(_) => renamed.push(json!({ "from": from, "to": to })),
            Err(e) => errors.push(json!({ "file": from, "error": e.to_string() })),
        }
    }

    Ok(json!({
        "renamed": renamed.len(),
        "failed": errors.len(),
        "total": paths.len(),
        "renames": renamed,
        "errors": errors,
    }))
}

pub async fn local_copy_files(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let base = ctx.context_local_path();
    let paths = resolve_paths_array(args, base)?;
    let destination = resolve_local_path(&get_str(args, "destination")?, base);
    validate_path(&destination, "destination").map_err(map_str_err)?;

    if paths.is_empty() {
        return Err(ToolError::InvalidArgs {
            tool: "local_copy_files".into(),
            reason: "'paths' array is empty".into(),
        });
    }

    let dest_path = std::path::Path::new(&destination);
    if paths.len() == 1 && dest_path.extension().is_some() && !dest_path.is_dir() {
        let source = &paths[0];
        std::fs::copy(source, &destination).map_err(|e| {
            ToolError::Exec(format!(
                "Failed to copy {} to {}: {}",
                source, destination, e
            ))
        })?;
        let filename = dest_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| destination.clone());
        return Ok(json!({
            "copied": 1,
            "failed": 0,
            "total": 1,
            "files": [filename],
            "errors": [],
        }));
    }

    std::fs::create_dir_all(&destination)
        .map_err(|e| ToolError::Exec(format!("Failed to create destination directory: {}", e)))?;

    fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<u64, String> {
        std::fs::create_dir_all(dst)
            .map_err(|e| format!("Failed to create dir {}: {}", dst.display(), e))?;
        let mut count = 0u64;
        for entry in std::fs::read_dir(src)
            .map_err(|e| format!("Failed to read dir {}: {}", src.display(), e))?
        {
            let entry = entry.map_err(|e| e.to_string())?;
            let src_path = entry.path();
            let dst_path = dst.join(entry.file_name());
            if src_path.is_dir() {
                count += copy_dir_recursive(&src_path, &dst_path)?;
            } else {
                std::fs::copy(&src_path, &dst_path)
                    .map_err(|e| format!("Failed to copy {}: {}", src_path.display(), e))?;
                count += 1;
            }
        }
        Ok(count)
    }

    let mut copied = Vec::new();
    let mut errors = Vec::new();
    let total = paths.len();

    for (idx, source) in paths.iter().enumerate() {
        if let Err(e) = validate_path(source, "path") {
            errors.push(json!({ "file": source, "error": e }));
            continue;
        }
        let src_path = std::path::Path::new(source);
        let filename = src_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());
        let dest_path_str = format!("{}/{}", destination.trim_end_matches('/'), filename);

        progress(
            ctx,
            "local_copy_files",
            idx as u32 + 1,
            total as u32,
            &filename,
        );

        if src_path.is_dir() {
            match copy_dir_recursive(src_path, std::path::Path::new(&dest_path_str)) {
                Ok(_) => copied.push(filename),
                Err(e) => errors.push(json!({ "file": filename, "error": e })),
            }
        } else {
            match std::fs::copy(source, &dest_path_str) {
                Ok(_) => copied.push(filename),
                Err(e) => errors.push(json!({ "file": filename, "error": e.to_string() })),
            }
        }
    }

    Ok(json!({
        "copied": copied.len(),
        "failed": errors.len(),
        "total": total,
        "files": copied,
        "errors": errors,
    }))
}

pub async fn local_trash(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let base = ctx.context_local_path();
    let paths = resolve_paths_array(args, base)?;

    if paths.is_empty() {
        return Err(ToolError::InvalidArgs {
            tool: "local_trash".into(),
            reason: "'paths' array is empty".into(),
        });
    }

    let mut trashed = Vec::new();
    let mut errors = Vec::new();
    let total = paths.len();

    for (idx, path) in paths.iter().enumerate() {
        if let Err(e) = validate_path(path, "path") {
            errors.push(json!({ "file": path, "error": e }));
            continue;
        }
        let filename = std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.clone());

        progress(ctx, "local_trash", idx as u32 + 1, total as u32, &filename);

        match trash::delete(path) {
            Ok(_) => trashed.push(filename),
            Err(e) => errors.push(json!({ "file": filename, "error": e.to_string() })),
        }
    }

    Ok(json!({
        "trashed": trashed.len(),
        "failed": errors.len(),
        "total": total,
        "files": trashed,
        "errors": errors,
    }))
}

pub async fn local_file_info(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let path = resolve_local_path(&get_str(args, "path")?, ctx.context_local_path());
    validate_path(&path, "path").map_err(map_str_err)?;

    let p = std::path::Path::new(&path);
    let meta = std::fs::symlink_metadata(&path)
        .map_err(|e| ToolError::Exec(format!("Failed to stat: {}", e)))?;

    let mut info = json!({
        "path": path,
        "name": p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        "size": meta.len(),
        "is_file": meta.is_file(),
        "is_dir": meta.is_dir(),
        "is_symlink": meta.is_symlink(),
        "readonly": meta.permissions().readonly(),
    });

    if let Ok(modified) = meta.modified() {
        if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
            info["modified"] = json!(dur.as_secs());
        }
    }
    if let Ok(created) = meta.created() {
        if let Ok(dur) = created.duration_since(std::time::UNIX_EPOCH) {
            info["created"] = json!(dur.as_secs());
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        info["permissions_octal"] = json!(format!("{:o}", meta.mode()));
        info["uid"] = json!(meta.uid());
        info["gid"] = json!(meta.gid());
    }

    if meta.is_file() {
        if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
            let mime = match ext.to_lowercase().as_str() {
                "pdf" => "application/pdf",
                "txt" => "text/plain",
                "html" | "htm" => "text/html",
                "css" => "text/css",
                "js" => "text/javascript",
                "json" => "application/json",
                "xml" => "application/xml",
                "zip" => "application/zip",
                "7z" => "application/x-7z-compressed",
                "tar" => "application/x-tar",
                "gz" => "application/gzip",
                "png" => "image/png",
                "jpg" | "jpeg" => "image/jpeg",
                "gif" => "image/gif",
                "svg" => "image/svg+xml",
                "mp3" => "audio/mpeg",
                "mp4" => "video/mp4",
                "rs" => "text/x-rust",
                "ts" | "tsx" => "text/typescript",
                "py" => "text/x-python",
                _ => "application/octet-stream",
            };
            info["mime_type"] = json!(mime);
        }
    }

    Ok(info)
}

pub async fn local_disk_usage(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let path = resolve_local_path(&get_str(args, "path")?, ctx.context_local_path());
    validate_path(&path, "path").map_err(map_str_err)?;

    let p = std::path::Path::new(&path);
    if !p.is_dir() {
        return Err(ToolError::Exec(format!(
            "Path is not a directory: {}",
            path
        )));
    }

    let path_for_walk = path.clone();
    tokio::task::spawn_blocking(move || {
        let mut total_bytes: u64 = 0;
        let mut file_count: u64 = 0;
        let mut dir_count: u64 = 0;
        const MAX_ENTRIES: u64 = 500_000;
        let mut entry_count: u64 = 0;
        let base = std::path::Path::new(&path_for_walk);

        for entry in walkdir::WalkDir::new(&path_for_walk)
            .follow_links(false)
            .max_depth(100)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            entry_count += 1;
            if entry_count > MAX_ENTRIES {
                break;
            }
            if entry.file_type().is_file() {
                total_bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
                file_count += 1;
            } else if entry.file_type().is_dir() && entry.path() != base {
                dir_count += 1;
            }
        }

        json!({
            "path": path_for_walk,
            "total_bytes": total_bytes,
            "total_human": format!("{:.1} MB", total_bytes as f64 / 1_048_576.0),
            "file_count": file_count,
            "dir_count": dir_count,
        })
    })
    .await
    .map_err(|e| ToolError::Exec(format!("local_disk_usage task failed: {}", e)))
}

pub async fn local_find_duplicates(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let path = resolve_local_path(&get_str(args, "path")?, ctx.context_local_path());
    validate_path(&path, "path").map_err(map_str_err)?;
    let min_size = args
        .get("min_size")
        .and_then(|v| v.as_u64())
        .unwrap_or(1024);

    let p = std::path::Path::new(&path);
    if !p.is_dir() {
        return Err(ToolError::Exec(format!(
            "Path is not a directory: {}",
            path
        )));
    }

    let path_for_scan = path.clone();
    tokio::task::spawn_blocking(move || -> Result<Value, ToolError> {
        let mut size_groups: std::collections::HashMap<u64, Vec<std::path::PathBuf>> =
            std::collections::HashMap::new();
        const MAX_SCAN: u64 = 50_000;
        let mut scan_count: u64 = 0;

        for entry in walkdir::WalkDir::new(&path_for_scan)
            .follow_links(false)
            .max_depth(50)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            scan_count += 1;
            if scan_count > MAX_SCAN {
                break;
            }
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            if size < min_size {
                continue;
            }
            size_groups.entry(size).or_default().push(entry.into_path());
        }

        use md5::{Digest, Md5};
        use std::io::Read;
        let mut hash_groups: std::collections::HashMap<String, (u64, Vec<String>)> =
            std::collections::HashMap::new();

        for (size, files) in &size_groups {
            if files.len() < 2 {
                continue;
            }
            for file_path in files {
                if let Ok(mut f) = std::fs::File::open(file_path) {
                    let mut hasher = Md5::new();
                    let mut buf = [0u8; 8192];
                    loop {
                        match f.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => hasher.update(&buf[..n]),
                            Err(_) => break,
                        }
                    }
                    let hash = format!("{:x}", hasher.finalize());
                    let entry = hash_groups
                        .entry(hash)
                        .or_insert_with(|| (*size, Vec::new()));
                    entry.1.push(file_path.to_string_lossy().to_string());
                }
            }
        }

        let mut duplicates: Vec<Value> = hash_groups
            .into_iter()
            .filter(|(_, (_, files))| files.len() >= 2)
            .map(|(hash, (size, files))| {
                json!({
                    "hash": hash,
                    "size": size,
                    "count": files.len(),
                    "wasted_bytes": size * (files.len() as u64 - 1),
                    "files": files,
                })
            })
            .collect();

        duplicates.sort_by(|a, b| {
            let wa = a["wasted_bytes"].as_u64().unwrap_or(0);
            let wb = b["wasted_bytes"].as_u64().unwrap_or(0);
            wb.cmp(&wa)
        });

        let total_wasted: u64 = duplicates
            .iter()
            .map(|d| d["wasted_bytes"].as_u64().unwrap_or(0))
            .sum();

        Ok(json!({
            "groups": duplicates.len(),
            "total_wasted_bytes": total_wasted,
            "total_wasted_human": format!("{:.1} MB", total_wasted as f64 / 1_048_576.0),
            "duplicates": duplicates,
        }))
    })
    .await
    .map_err(|e| ToolError::Exec(format!("local_find_duplicates task failed: {}", e)))?
}

pub async fn local_edit(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let path = resolve_local_path(&get_str(args, "path")?, ctx.context_local_path());
    let find = get_str(args, "find")?;
    let replace = get_str(args, "replace")?;
    let replace_all = args
        .get("replace_all")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    validate_path(&path, "path").map_err(map_str_err)?;

    let mut content = std::fs::read_to_string(&path)
        .map_err(|e| ToolError::Exec(format!("Failed to read file: {}", e)))?;

    if content.starts_with('\u{FEFF}') {
        content = content.strip_prefix('\u{FEFF}').unwrap().to_string();
    }

    let occurrences = content.matches(&find).count();
    if occurrences == 0 {
        return Ok(json!({
            "success": false,
            "message": "String not found in file",
            "occurrences": 0,
        }));
    }

    let new_content = if replace_all {
        content.replace(&find, &replace)
    } else {
        content.replacen(&find, &replace, 1)
    };

    std::fs::write(&path, &new_content).map_err(map_err)?;

    let replaced = if replace_all { occurrences } else { 1 };
    Ok(json!({
        "success": true,
        "message": format!("Replaced {} occurrence(s) in {}", replaced, path),
        "occurrences": occurrences,
        "replaced": replaced,
    }))
}

pub async fn local_grep(_ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let path = get_str(args, "path")?;
    let pattern = get_str(args, "pattern")?;
    validate_path(&path, "path").map_err(map_str_err)?;

    let glob_filter = get_str_opt(args, "glob");
    let max_results = args
        .get("max_results")
        .and_then(|v| v.as_u64())
        .unwrap_or(50) as usize;
    let context_lines = args
        .get("context_lines")
        .and_then(|v| v.as_u64())
        .unwrap_or(2) as usize;
    let case_sensitive = args
        .get("case_sensitive")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let re = if case_sensitive {
        regex::Regex::new(&pattern)
    } else {
        regex::RegexBuilder::new(&pattern)
            .case_insensitive(true)
            .build()
    }
    .map_err(|e| ToolError::Exec(format!("Invalid regex: {}", e)))?;

    let base_path = std::path::Path::new(&path);
    if !base_path.is_dir() {
        return Err(ToolError::Exec(format!("Not a directory: {}", path)));
    }

    let glob_re = if let Some(ref g) = glob_filter {
        let glob_pattern = g.replace('.', "\\.").replace('*', ".*").replace('?', ".");
        regex::RegexBuilder::new(&format!("^{}$", glob_pattern))
            .case_insensitive(true)
            .build()
            .ok()
    } else {
        None
    };

    let path_for_grep = path.clone();
    let pattern_for_result = pattern.clone();
    tokio::task::spawn_blocking(move || -> Result<Value, ToolError> {
        let base_path = std::path::Path::new(&path_for_grep);
        let mut matches: Vec<Value> = Vec::new();
        let mut files_searched: u32 = 0;
        const MAX_FILE_SIZE: u64 = 10_485_760;

        for entry in walkdir::WalkDir::new(&path_for_grep)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if matches.len() >= max_results {
                break;
            }
            let entry_path = entry.path();
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if !meta.is_file() || meta.len() > MAX_FILE_SIZE {
                continue;
            }

            if let Some(ref gre) = glob_re {
                if let Some(name) = entry_path.file_name().and_then(|n| n.to_str()) {
                    if !gre.is_match(name) {
                        continue;
                    }
                }
            }

            let bytes = match std::fs::read(entry_path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let check_len = bytes.len().min(8192);
            if bytes[..check_len].contains(&0) {
                continue;
            }

            let content = match String::from_utf8(bytes) {
                Ok(s) => s,
                Err(_) => continue,
            };
            files_searched += 1;

            let lines: Vec<&str> = content.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                if matches.len() >= max_results {
                    break;
                }
                if re.is_match(line) {
                    let rel = entry_path
                        .strip_prefix(base_path)
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| entry_path.to_string_lossy().to_string());

                    let ctx_before: Vec<&str> = lines[i.saturating_sub(context_lines)..i].to_vec();
                    let ctx_after: Vec<&str> =
                        lines[(i + 1)..lines.len().min(i + 1 + context_lines)].to_vec();

                    matches.push(json!({
                        "file": rel,
                        "line_number": i + 1,
                        "line": line.chars().take(500).collect::<String>(),
                        "context_before": ctx_before,
                        "context_after": ctx_after,
                    }));
                }
            }
        }

        Ok(json!({
            "success": true,
            "pattern": pattern_for_result,
            "total_matches": matches.len(),
            "files_searched": files_searched,
            "matches": matches,
        }))
    })
    .await
    .map_err(|e| ToolError::Exec(format!("local_grep task failed: {}", e)))?
}

pub async fn local_head(_ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let path = get_str(args, "path")?;
    validate_path(&path, "path").map_err(map_str_err)?;
    let num_lines = args
        .get("lines")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .min(500) as usize;

    let p = std::path::Path::new(&path);
    if !p.is_file() {
        return Err(ToolError::Exec(format!("Not a file: {}", path)));
    }
    let meta =
        std::fs::metadata(&path).map_err(|e| ToolError::Exec(format!("Failed to stat: {}", e)))?;
    if meta.len() > 52_428_800 {
        return Err(ToolError::Exec("File too large (max 50MB)".to_string()));
    }

    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(&path)
        .map_err(|e| ToolError::Exec(format!("Failed to open: {}", e)))?;
    let reader = BufReader::new(file);
    let mut result_lines: Vec<String> = Vec::new();
    let mut total_lines: usize = 0;

    for line in reader.lines() {
        total_lines += 1;
        if result_lines.len() < num_lines {
            match line {
                Ok(l) => result_lines.push(l),
                Err(_) => result_lines.push("[binary data]".to_string()),
            }
        } else if line.is_err() {
            continue;
        }
    }

    let content = result_lines.join("\n");
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    Ok(json!({
        "success": true,
        "content": content,
        "lines_read": result_lines.len(),
        "total_lines": total_lines,
        "file_name": name,
    }))
}

pub async fn local_tail(_ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let path = get_str(args, "path")?;
    validate_path(&path, "path").map_err(map_str_err)?;
    let num_lines = args
        .get("lines")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .min(500) as usize;

    let p = std::path::Path::new(&path);
    if !p.is_file() {
        return Err(ToolError::Exec(format!("Not a file: {}", path)));
    }
    let meta =
        std::fs::metadata(&path).map_err(|e| ToolError::Exec(format!("Failed to stat: {}", e)))?;
    if meta.len() > 52_428_800 {
        return Err(ToolError::Exec("File too large (max 50MB)".to_string()));
    }

    let content = std::fs::read_to_string(&path)
        .map_err(|e| ToolError::Exec(format!("Failed to read: {}", e)))?;
    let all_lines: Vec<&str> = content.lines().collect();
    let total_lines = all_lines.len();
    let start = total_lines.saturating_sub(num_lines);
    let result_lines: Vec<&str> = all_lines[start..].to_vec();
    let result_content = result_lines.join("\n");
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    Ok(json!({
        "success": true,
        "content": result_content,
        "lines_read": result_lines.len(),
        "total_lines": total_lines,
        "file_name": name,
    }))
}

pub async fn local_stat_batch(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let paths = args
        .get("paths")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ToolError::InvalidArgs {
            tool: "local_stat_batch".into(),
            reason: "Missing 'paths' array".into(),
        })?;

    if paths.len() > 100 {
        return Err(ToolError::InvalidArgs {
            tool: "local_stat_batch".into(),
            reason: format!("Too many paths: {} (max 100)", paths.len()),
        });
    }

    let base = ctx.context_local_path();
    let mut files: Vec<Value> = Vec::new();

    for p in paths.iter().filter_map(|v| v.as_str()) {
        let resolved = resolve_local_path(p, base);
        let path = std::path::Path::new(&resolved);
        if let Ok(meta) = std::fs::metadata(path) {
            let size = meta.len();
            let modified = meta.modified().ok().map(|t| {
                let datetime: chrono::DateTime<chrono::Local> = t.into();
                datetime.format("%Y-%m-%d %H:%M:%S").to_string()
            });
            let is_file = meta.is_file();
            let is_dir = meta.is_dir();
            #[cfg(unix)]
            let permissions = {
                use std::os::unix::fs::PermissionsExt;
                format!("{:o}", meta.permissions().mode() & 0o777)
            };
            #[cfg(not(unix))]
            let permissions = if meta.permissions().readonly() {
                "r--"
            } else {
                "rw-"
            }
            .to_string();

            let size_human = human_bytes(size);

            files.push(json!({
                "path": resolved,
                "name": path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                "size": size,
                "size_human": size_human,
                "modified": modified,
                "is_file": is_file,
                "is_dir": is_dir,
                "permissions": permissions,
                "exists": true,
            }));
        } else {
            files.push(json!({
                "path": resolved,
                "name": path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                "exists": false,
            }));
        }
    }

    let total = files.len();
    Ok(json!({
        "success": true,
        "files": files,
        "total": total,
    }))
}

pub async fn local_diff(_ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let path_a = get_str(args, "path_a")?;
    let path_b = get_str(args, "path_b")?;
    validate_path(&path_a, "path_a").map_err(map_str_err)?;
    validate_path(&path_b, "path_b").map_err(map_str_err)?;
    let context_lines = args
        .get("context_lines")
        .and_then(|v| v.as_u64())
        .unwrap_or(3) as usize;

    const MAX_DIFF_SIZE: u64 = 5_242_880;

    let meta_a =
        std::fs::metadata(&path_a).map_err(|e| ToolError::Exec(format!("path_a: {}", e)))?;
    let meta_b =
        std::fs::metadata(&path_b).map_err(|e| ToolError::Exec(format!("path_b: {}", e)))?;

    if !meta_a.is_file() {
        return Err(ToolError::Exec(format!("Not a file: {}", path_a)));
    }
    if !meta_b.is_file() {
        return Err(ToolError::Exec(format!("Not a file: {}", path_b)));
    }
    if meta_a.len() > MAX_DIFF_SIZE {
        return Err(ToolError::Exec(format!(
            "File A too large: {:.1} MB (max 5MB)",
            meta_a.len() as f64 / 1_048_576.0
        )));
    }
    if meta_b.len() > MAX_DIFF_SIZE {
        return Err(ToolError::Exec(format!(
            "File B too large: {:.1} MB (max 5MB)",
            meta_b.len() as f64 / 1_048_576.0
        )));
    }

    let content_a = std::fs::read_to_string(&path_a)
        .map_err(|e| ToolError::Exec(format!("Failed to read file A: {}", e)))?;
    let content_b = std::fs::read_to_string(&path_b)
        .map_err(|e| ToolError::Exec(format!("Failed to read file B: {}", e)))?;

    let diff = similar::TextDiff::from_lines(&content_a, &content_b);
    let unified = diff
        .unified_diff()
        .context_radius(context_lines)
        .header(&path_a, &path_b)
        .to_string();

    let mut additions: usize = 0;
    let mut deletions: usize = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Insert => additions += 1,
            similar::ChangeTag::Delete => deletions += 1,
            _ => {}
        }
    }

    let name_a = std::path::Path::new(&path_a)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let name_b = std::path::Path::new(&path_b)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    Ok(json!({
        "success": true,
        "diff": unified,
        "identical": additions == 0 && deletions == 0,
        "stats": {
            "additions": additions,
            "deletions": deletions,
            "file_a": name_a,
            "file_b": name_b,
        },
    }))
}

pub async fn local_tree(_ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let path = get_str(args, "path")?;
    validate_path(&path, "path").map_err(map_str_err)?;
    let max_depth = args
        .get("max_depth")
        .and_then(|v| v.as_u64())
        .unwrap_or(3)
        .min(10) as usize;
    let show_hidden = args
        .get("show_hidden")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let glob_filter = get_str_opt(args, "glob");

    let base_path = std::path::Path::new(&path);
    if !base_path.is_dir() {
        return Err(ToolError::Exec(format!("Not a directory: {}", path)));
    }

    let glob_re = if let Some(ref g) = glob_filter {
        let glob_pattern = g.replace('.', "\\.").replace('*', ".*").replace('?', ".");
        regex::RegexBuilder::new(&format!("^{}$", glob_pattern))
            .case_insensitive(true)
            .build()
            .ok()
    } else {
        None
    };

    const MAX_ENTRIES: usize = 1000;
    let mut tree_lines: Vec<String> = Vec::new();
    let mut file_count: u32 = 0;
    let mut dir_count: u32 = 0;
    let mut total_size: u64 = 0;

    let root_name = base_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.clone());
    tree_lines.push(format!("{}/", root_name));

    #[allow(clippy::too_many_arguments)]
    fn build_tree(
        dir: &std::path::Path,
        prefix: &str,
        depth: usize,
        max_depth: usize,
        show_hidden: bool,
        glob_re: &Option<regex::Regex>,
        lines: &mut Vec<String>,
        file_count: &mut u32,
        dir_count: &mut u32,
        total_size: &mut u64,
        max_entries: usize,
    ) {
        if depth >= max_depth || lines.len() >= max_entries {
            return;
        }
        let mut entries: Vec<_> = match std::fs::read_dir(dir) {
            Ok(e) => e.filter_map(|e| e.ok()).collect(),
            Err(_) => return,
        };
        entries.sort_by_key(|e| {
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            (!is_dir, e.file_name().to_string_lossy().to_lowercase())
        });

        let count = entries.len();
        for (i, entry) in entries.iter().enumerate() {
            if lines.len() >= max_entries {
                lines.push(format!(
                    "{}... (truncated at {} entries)",
                    prefix, max_entries
                ));
                return;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if !show_hidden && name.starts_with('.') {
                continue;
            }
            let is_last = i == count - 1;
            let connector = if is_last {
                "\u{2514}\u{2500}\u{2500} "
            } else {
                "\u{251C}\u{2500}\u{2500} "
            };
            let child_prefix = if is_last {
                format!("{}    ", prefix)
            } else {
                format!("{}\u{2502}   ", prefix)
            };

            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };

            if meta.is_dir() {
                *dir_count += 1;
                lines.push(format!("{}{}{}/", prefix, connector, name));
                build_tree(
                    &entry.path(),
                    &child_prefix,
                    depth + 1,
                    max_depth,
                    show_hidden,
                    glob_re,
                    lines,
                    file_count,
                    dir_count,
                    total_size,
                    max_entries,
                );
            } else if meta.is_file() {
                if let Some(ref gre) = glob_re {
                    if !gre.is_match(&name) {
                        continue;
                    }
                }
                *file_count += 1;
                *total_size += meta.len();
                let size_str = if meta.len() < 1024 {
                    format!("{} B", meta.len())
                } else if meta.len() < 1_048_576 {
                    format!("{:.1} KB", meta.len() as f64 / 1024.0)
                } else {
                    format!("{:.1} MB", meta.len() as f64 / 1_048_576.0)
                };
                lines.push(format!("{}{}{} ({})", prefix, connector, name, size_str));
            }
        }
    }

    build_tree(
        base_path,
        "",
        0,
        max_depth,
        show_hidden,
        &glob_re,
        &mut tree_lines,
        &mut file_count,
        &mut dir_count,
        &mut total_size,
        MAX_ENTRIES,
    );

    let total_human = human_bytes(total_size);

    Ok(json!({
        "success": true,
        "tree": tree_lines.join("\n"),
        "stats": {
            "files": file_count,
            "dirs": dir_count,
            "total_size": total_size,
            "total_size_human": total_human,
        },
        "truncated": tree_lines.len() >= MAX_ENTRIES,
    }))
}
