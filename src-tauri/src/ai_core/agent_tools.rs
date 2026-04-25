//! Area D — agent_memory_*, rag_* unified handlers (T3 Gate 2).
//!
//! `agent_memory_write` usa l'API non-Tauri `store_memory_entry_cli`
//! (single SQLite DB, lazy lock condiviso) — funziona identicamente
//! da GUI, CLI e MCP.
//!
//! `rag_index` / `rag_search` sono filesystem-only e non hanno
//! dipendenze Tauri; le porto qui in async + spawn_blocking per
//! evitare di bloccare il runtime su tree grandi.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use serde_json::{json, Value};

use super::local_tools::{get_str, get_str_opt, resolve_local_path, validate_path};
use super::tools::{ToolCtx, ToolError};

fn map_str_err(e: String) -> ToolError {
    ToolError::Exec(e)
}

const TEXT_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "py", "json", "toml", "yaml", "yml", "md", "txt", "html",
    "css", "sh", "sql", "xml", "csv", "env", "cfg", "ini", "conf", "log", "go", "java", "c",
    "cpp", "h", "hpp", "rb", "php", "swift", "kt",
];

fn is_text_file(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| TEXT_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

pub async fn rag_index(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let path = resolve_local_path(&get_str(args, "path")?, ctx.context_local_path());
    validate_path(&path, "path").map_err(map_str_err)?;
    let recursive = args
        .get("recursive")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let max_files = args
        .get("max_files")
        .and_then(|v| v.as_u64())
        .unwrap_or(200) as u32;

    let path_clone = path.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let base_path = std::path::Path::new(&path_clone);
        if !base_path.is_dir() {
            return Err(format!("Not a directory: {}", path_clone));
        }

        fn scan_dir(
            dir: &std::path::Path,
            base: &std::path::Path,
            recursive: bool,
            files: &mut Vec<Value>,
            dirs_count: &mut u32,
            max_files: u32,
        ) {
            let entries = match std::fs::read_dir(dir) {
                Ok(e) => e,
                Err(_) => return,
            };
            for entry in entries.flatten() {
                if files.len() >= max_files as usize {
                    return;
                }
                let entry_path = entry.path();
                let meta = match entry.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if meta.is_dir() {
                    *dirs_count += 1;
                    if recursive {
                        scan_dir(&entry_path, base, recursive, files, dirs_count, max_files);
                    }
                } else if meta.is_file() {
                    let rel = entry_path
                        .strip_prefix(base)
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| entry_path.to_string_lossy().to_string());
                    let name = entry_path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let ext = entry_path
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_lowercase();
                    let size = meta.len();

                    let preview = if is_text_file(&entry_path) && size < 50_000 {
                        std::fs::read_to_string(&entry_path)
                            .ok()
                            .map(|content| content.lines().take(20).collect::<Vec<_>>().join("\n"))
                    } else {
                        None
                    };

                    let mut file_obj = json!({
                        "name": name,
                        "path": rel,
                        "size": size,
                        "ext": ext,
                    });
                    if let Some(p) = preview {
                        if let Some(obj) = file_obj.as_object_mut() {
                            obj.insert("preview".to_string(), json!(p));
                        }
                    }
                    files.push(file_obj);
                }
            }
        }

        let mut files: Vec<Value> = Vec::new();
        let mut dirs_count: u32 = 0;
        scan_dir(
            base_path,
            base_path,
            recursive,
            &mut files,
            &mut dirs_count,
            max_files,
        );

        let total_size: u64 = files
            .iter()
            .filter_map(|f| f.get("size").and_then(|s| s.as_u64()))
            .sum();

        let mut extensions: std::collections::HashMap<String, u32> =
            std::collections::HashMap::new();
        for f in &files {
            if let Some(ext) = f.get("ext").and_then(|e| e.as_str()) {
                if !ext.is_empty() {
                    *extensions.entry(ext.to_string()).or_insert(0) += 1;
                }
            }
        }

        Ok(json!({
            "files_count": files.len(),
            "dirs_count": dirs_count,
            "total_size": total_size,
            "extensions": extensions,
            "files": files,
        }))
    })
    .await
    .map_err(|e| ToolError::Exec(format!("rag_index spawn_blocking failed: {e}")))?
    .map_err(ToolError::Exec)?;

    Ok(result)
}

pub async fn rag_search(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let query = get_str(args, "query")?;
    let path = resolve_local_path(
        &get_str_opt(args, "path").unwrap_or_else(|| ".".to_string()),
        ctx.context_local_path(),
    );
    validate_path(&path, "path").map_err(map_str_err)?;
    let max_results = args
        .get("max_results")
        .and_then(|v| v.as_u64())
        .unwrap_or(20) as usize;

    let path_clone = path.clone();
    let query_clone = query.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let base_path = std::path::Path::new(&path_clone);
        if !base_path.is_dir() {
            return Err(format!("Not a directory: {}", path_clone));
        }

        fn search_dir(
            dir: &std::path::Path,
            base: &std::path::Path,
            query_lower: &str,
            matches: &mut Vec<Value>,
            files_scanned: &mut u32,
            max_results: usize,
            max_files: u32,
        ) {
            let entries = match std::fs::read_dir(dir) {
                Ok(e) => e,
                Err(_) => return,
            };
            for entry in entries.flatten() {
                if matches.len() >= max_results || *files_scanned >= max_files {
                    return;
                }
                let entry_path = entry.path();
                let meta = match entry.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if meta.is_dir() {
                    search_dir(
                        &entry_path,
                        base,
                        query_lower,
                        matches,
                        files_scanned,
                        max_results,
                        max_files,
                    );
                } else if meta.is_file() && is_text_file(&entry_path) && meta.len() < 100_000 {
                    *files_scanned += 1;
                    let rel = entry_path
                        .strip_prefix(base)
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| entry_path.to_string_lossy().to_string());

                    if let Ok(content) = std::fs::read_to_string(&entry_path) {
                        for (line_num, line) in content.lines().enumerate() {
                            if matches.len() >= max_results {
                                break;
                            }
                            if line.to_lowercase().contains(query_lower) {
                                matches.push(json!({
                                    "path": rel,
                                    "line": line_num + 1,
                                    "context": line.chars().take(200).collect::<String>(),
                                }));
                            }
                        }
                    }
                }
            }
        }

        let query_lower = query_clone.to_lowercase();
        let mut matches: Vec<Value> = Vec::new();
        let mut files_scanned: u32 = 0;
        search_dir(
            base_path,
            base_path,
            &query_lower,
            &mut matches,
            &mut files_scanned,
            max_results,
            500,
        );

        Ok(json!({
            "query": query_clone,
            "files_scanned": files_scanned,
            "matches": matches,
        }))
    })
    .await
    .map_err(|e| ToolError::Exec(format!("rag_search spawn_blocking failed: {e}")))?
    .map_err(ToolError::Exec)?;

    Ok(result)
}

pub async fn agent_memory_write(_ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let entry = get_str(args, "entry")?;
    let category_raw = get_str_opt(args, "category").unwrap_or_else(|| "general".to_string());
    let project_path = get_str(args, "project_path")?;

    // Sanitize category: only alphanumeric, underscore, hyphen; max 30 chars.
    // Mirrors FIX 12 in ai_tools.rs::agent_memory_write.
    let sanitized_category: String = category_raw
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .take(30)
        .collect();

    validate_path(&project_path, "project_path").map_err(map_str_err)?;

    // Single source of truth: the CLI-style helper uses a per-process
    // memoized SQLite DB (see agent_memory_db.rs) — same DB on disk
    // regardless of surface (GUI/CLI/MCP).
    let project_path_clone = project_path.clone();
    let category_clone = sanitized_category.clone();
    let entry_clone = entry.clone();
    tokio::task::spawn_blocking(move || {
        crate::agent_memory_db::store_memory_entry_cli(
            &project_path_clone,
            &category_clone,
            &entry_clone,
            None,
        )
    })
    .await
    .map_err(|e| ToolError::Exec(format!("agent_memory_write spawn_blocking failed: {e}")))?
    .map_err(ToolError::Exec)?;

    Ok(json!({
        "success": true,
        "message": format!("Memory entry saved: [{}] {}", sanitized_category, entry)
    }))
}
