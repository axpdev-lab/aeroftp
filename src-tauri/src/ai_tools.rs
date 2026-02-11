//! AI Tool Execution via StorageProvider trait + FTP fallback
//!
//! Provides a unified `execute_ai_tool` command that routes AI tool calls
//! through the active StorageProvider (14 protocols). When no provider is
//! connected, falls back to `AppState.ftp_manager` for FTP/FTPS sessions.

use serde_json::{json, Value};
use tauri::{Emitter, State};
use crate::provider_commands::ProviderState;
use crate::AppState;

/// Allowed tool names (whitelist)
const ALLOWED_TOOLS: &[&str] = &[
    "remote_list", "remote_read", "remote_upload", "remote_download",
    "remote_delete", "remote_rename", "remote_mkdir", "remote_search",
    "remote_info", "local_list", "local_read", "local_write",
    "local_mkdir", "local_delete", "local_rename", "local_search", "local_edit",
    "remote_edit",
    // Batch transfer tools
    "upload_files", "download_files",
    // Advanced tools
    "sync_preview", "archive_create", "archive_extract",
    // RAG tools
    "rag_index", "rag_search",
    // Preview tools
    "preview_edit",
    // Agent memory
    "agent_memory_write",
];

/// Validate a path argument — reject null bytes, traversal, excessive length
fn validate_path(path: &str, param: &str) -> Result<(), String> {
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
    // Resolve symlinks and verify canonical path is not a sensitive system path.
    // For non-existent files, check the parent directory to avoid write/read inconsistency.
    let resolved = std::fs::canonicalize(path).or_else(|_| {
        std::path::Path::new(path)
            .parent()
            .map(std::fs::canonicalize)
            .unwrap_or(Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no parent")))
    });
    if let Ok(canonical) = resolved {
        let s = canonical.to_string_lossy();
        // Block sensitive system paths (deny-list)
        let denied = [
            "/proc", "/sys", "/dev", "/boot", "/root",
            "/etc/shadow", "/etc/passwd", "/etc/ssh", "/etc/sudoers",
        ];
        if denied.iter().any(|d| s.starts_with(d)) {
            return Err(format!("{}: access to system path denied: {}", param, s));
        }
        // Block sensitive home-relative paths
        if let Ok(home) = std::env::var("HOME") {
            let home_denied = [".ssh", ".gnupg", ".aws", ".kube", ".config/gcloud"];
            for sensitive in &home_denied {
                if s.starts_with(&format!("{}/{}", home, sensitive)) {
                    return Err(format!("{}: access to sensitive path denied: {}", param, s));
                }
            }
        }
    }
    Ok(())
}

fn get_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("Missing required argument: {}", key))
}

fn get_str_opt(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

/// Check if the StorageProvider is connected
async fn has_provider(state: &ProviderState) -> bool {
    state.provider.lock().await.is_some()
}

/// Check if FTP manager has an active connection
async fn has_ftp(app_state: &AppState) -> bool {
    app_state.ftp_manager.lock().await.is_connected()
}

/// Emit tool progress event for iterative operations
fn emit_tool_progress(app: &tauri::AppHandle, tool: &str, current: u32, total: u32, item: &str) {
    let _ = app.emit("ai-tool-progress", json!({
        "tool": tool,
        "current": current,
        "total": total,
        "item": item,
    }));
}

/// Download a remote file to bytes via StorageProvider or FTP fallback
async fn download_from_provider(
    state: &State<'_, ProviderState>,
    app_state: &State<'_, AppState>,
    path: &str,
) -> Result<Vec<u8>, String> {
    if has_provider(state).await {
        let mut provider = state.provider.lock().await;
        let provider = provider.as_mut().unwrap();
        provider.download_to_bytes(path).await.map_err(|e| e.to_string())
    } else if has_ftp(app_state).await {
        let mut manager = app_state.ftp_manager.lock().await;
        manager.download_to_bytes(path).await.map_err(|e| e.to_string())
    } else {
        Err("Not connected to any server".to_string())
    }
}

#[tauri::command]
pub async fn validate_tool_args(
    tool_name: String,
    args: Value,
) -> Result<Value, String> {
    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // Path validation for all tools with path args
    for key in &["path", "local_path", "remote_path", "from", "to"] {
        if let Some(path) = args.get(key).and_then(|v| v.as_str()) {
            if let Err(e) = validate_path(path, key) {
                errors.push(e);
            }
        }
    }

    // Tool-specific validation
    match tool_name.as_str() {
        "local_read" | "local_edit" | "local_search" | "local_list" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                let p = std::path::Path::new(path);
                if tool_name == "local_list" || tool_name == "local_search" {
                    if !p.is_dir() {
                        errors.push(format!("Directory not found: {}", path));
                    }
                } else {
                    if !p.exists() {
                        errors.push(format!("File not found: {}", path));
                    } else if p.is_dir() {
                        errors.push(format!("Path is a directory, not a file: {}", path));
                    } else if let Ok(meta) = p.metadata() {
                        let size = meta.len();
                        if size > 5_242_880 {
                            warnings.push(format!(
                                "File is large ({:.1} MB). Edit operations may be slow.",
                                size as f64 / 1_048_576.0
                            ));
                        }
                        // Check read-only for edit tools
                        if tool_name == "local_edit" && meta.permissions().readonly() {
                            errors.push(format!("File is read-only: {}", path));
                        }
                    }
                }
            }
            // Check find string for local_edit
            if tool_name == "local_edit" {
                if let Some(find) = args.get("find").and_then(|v| v.as_str()) {
                    if find.is_empty() {
                        errors.push("'find' parameter cannot be empty".to_string());
                    }
                }
            }
        }
        "local_write" | "local_mkdir" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                let p = std::path::Path::new(path);
                // Check parent exists
                if let Some(parent) = p.parent() {
                    if !parent.exists() {
                        warnings.push(format!(
                            "Parent directory does not exist: {}",
                            parent.display()
                        ));
                    }
                }
                // Check if path is read-only
                if p.exists() {
                    if let Ok(meta) = p.metadata() {
                        if meta.permissions().readonly() {
                            errors.push(format!("File is read-only: {}", path));
                        }
                    }
                }
            }
        }
        "local_delete" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                let home_dir = std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .unwrap_or_default();
                let dangerous = ["/", "~", ".", "..", home_dir.as_str()];
                let normalized = path.trim_end_matches('/');
                if dangerous
                    .iter()
                    .any(|d| normalized == *d || normalized.is_empty())
                {
                    errors.push(format!("Refusing to delete dangerous path: {}", path));
                }
                let p = std::path::Path::new(path);
                if !p.exists() {
                    warnings.push(format!(
                        "Path does not exist (nothing to delete): {}",
                        path
                    ));
                }
            }
        }
        _ => {} // Remote tools: path format already validated above
    }

    Ok(json!({
        "valid": errors.is_empty(),
        "errors": errors,
        "warnings": warnings,
    }))
}

#[tauri::command]
pub async fn execute_ai_tool(
    app: tauri::AppHandle,
    state: State<'_, ProviderState>,
    app_state: State<'_, AppState>,
    tool_name: String,
    args: Value,
) -> Result<Value, String> {
    // Whitelist check
    if !ALLOWED_TOOLS.contains(&tool_name.as_str()) {
        return Err(format!("Unknown or disallowed tool: {}", tool_name));
    }

    match tool_name.as_str() {
        "remote_list" => {
            let path = get_str(&args, "path")?;
            validate_path(&path, "path")?;

            // Try provider first, fall back to FTP
            if has_provider(&state).await {
                let mut provider = state.provider.lock().await;
                let provider = provider.as_mut().unwrap();
                let entries = provider.list(&path).await.map_err(|e| e.to_string())?;

                let items: Vec<Value> = entries.iter().take(100).map(|e| json!({
                    "name": e.name,
                    "path": e.path,
                    "is_dir": e.is_dir,
                    "size": e.size,
                    "modified": e.modified,
                })).collect();

                Ok(json!({
                    "entries": items,
                    "total": entries.len(),
                    "truncated": entries.len() > 100,
                }))
            } else if has_ftp(&app_state).await {
                let mut manager = app_state.ftp_manager.lock().await;
                // Navigate to path, list, then return
                manager.change_dir(&path).await.map_err(|e| e.to_string())?;
                let files = manager.list_files().await.map_err(|e| e.to_string())?;

                let items: Vec<Value> = files.iter().take(100).map(|f| json!({
                    "name": f.name,
                    "path": format!("{}/{}", path.trim_end_matches('/'), f.name),
                    "is_dir": f.is_dir,
                    "size": f.size,
                    "modified": f.modified,
                })).collect();

                Ok(json!({
                    "entries": items,
                    "total": files.len(),
                    "truncated": files.len() > 100,
                }))
            } else {
                Err("Not connected to any server".to_string())
            }
        }

        "remote_read" => {
            let path = get_str(&args, "path")?;
            validate_path(&path, "path")?;

            let bytes = download_from_provider(&state, &app_state, &path).await?;

            let max_bytes = 5120;
            let truncated = bytes.len() > max_bytes;
            let content = String::from_utf8_lossy(&bytes[..bytes.len().min(max_bytes)]).to_string();

            Ok(json!({ "content": content, "size": bytes.len(), "truncated": truncated }))
        }

        "remote_upload" => {
            let local_path = get_str(&args, "local_path")?;
            let remote_path = get_str(&args, "remote_path")?;
            validate_path(&local_path, "local_path")?;
            validate_path(&remote_path, "remote_path")?;

            if has_provider(&state).await {
                let mut provider = state.provider.lock().await;
                let provider = provider.as_mut().unwrap();
                provider.upload(&local_path, &remote_path, None).await.map_err(|e| e.to_string())?;
            } else if has_ftp(&app_state).await {
                let mut manager = app_state.ftp_manager.lock().await;
                manager.upload_file(&local_path, &remote_path).await.map_err(|e| e.to_string())?;
            } else {
                return Err("Not connected to any server".to_string());
            }

            Ok(json!({ "success": true, "message": format!("Uploaded {} to {}", local_path, remote_path) }))
        }

        "remote_download" => {
            let remote_path = get_str(&args, "remote_path")?;
            let local_path = get_str(&args, "local_path")?;
            validate_path(&remote_path, "remote_path")?;
            validate_path(&local_path, "local_path")?;

            if has_provider(&state).await {
                let mut provider = state.provider.lock().await;
                let provider = provider.as_mut().unwrap();
                provider.download(&remote_path, &local_path, None).await.map_err(|e| e.to_string())?;
            } else if has_ftp(&app_state).await {
                let mut manager = app_state.ftp_manager.lock().await;
                manager.download_file(&remote_path, &local_path).await.map_err(|e| e.to_string())?;
            } else {
                return Err("Not connected to any server".to_string());
            }

            Ok(json!({ "success": true, "message": format!("Downloaded {} to {}", remote_path, local_path) }))
        }

        "remote_delete" => {
            let path = get_str(&args, "path")?;
            validate_path(&path, "path")?;

            if has_provider(&state).await {
                let mut provider = state.provider.lock().await;
                let provider = provider.as_mut().unwrap();
                provider.delete(&path).await.map_err(|e| e.to_string())?;
            } else if has_ftp(&app_state).await {
                let mut manager = app_state.ftp_manager.lock().await;
                manager.remove(&path).await.map_err(|e| e.to_string())?;
            } else {
                return Err("Not connected to any server".to_string());
            }

            Ok(json!({ "success": true, "message": format!("Deleted {}", path) }))
        }

        "remote_rename" => {
            let from = get_str(&args, "from")?;
            let to = get_str(&args, "to")?;
            validate_path(&from, "from")?;
            validate_path(&to, "to")?;

            if has_provider(&state).await {
                let mut provider = state.provider.lock().await;
                let provider = provider.as_mut().unwrap();
                provider.rename(&from, &to).await.map_err(|e| e.to_string())?;
            } else if has_ftp(&app_state).await {
                let mut manager = app_state.ftp_manager.lock().await;
                manager.rename(&from, &to).await.map_err(|e| e.to_string())?;
            } else {
                return Err("Not connected to any server".to_string());
            }

            Ok(json!({ "success": true, "message": format!("Renamed {} to {}", from, to) }))
        }

        "remote_mkdir" => {
            let path = get_str(&args, "path")?;
            validate_path(&path, "path")?;

            if has_provider(&state).await {
                let mut provider = state.provider.lock().await;
                let provider = provider.as_mut().unwrap();
                provider.mkdir(&path).await.map_err(|e| e.to_string())?;
            } else if has_ftp(&app_state).await {
                let mut manager = app_state.ftp_manager.lock().await;
                manager.mkdir(&path).await.map_err(|e| e.to_string())?;
            } else {
                return Err("Not connected to any server".to_string());
            }

            Ok(json!({ "success": true, "message": format!("Created directory {}", path) }))
        }

        "remote_search" => {
            let path = get_str(&args, "path")?;
            let pattern = get_str(&args, "pattern")?;
            validate_path(&path, "path")?;

            if has_provider(&state).await {
                let mut provider = state.provider.lock().await;
                let provider = provider.as_mut().unwrap();
                let results = provider.find(&path, &pattern).await.map_err(|e| e.to_string())?;

                let items: Vec<Value> = results.iter().take(100).map(|e| json!({
                    "name": e.name,
                    "path": e.path,
                    "is_dir": e.is_dir,
                    "size": e.size,
                })).collect();

                Ok(json!({
                    "results": items,
                    "total": results.len(),
                    "truncated": results.len() > 100,
                }))
            } else if has_ftp(&app_state).await {
                // FTP doesn't have native search — list directory and filter client-side
                let mut manager = app_state.ftp_manager.lock().await;
                manager.change_dir(&path).await.map_err(|e| e.to_string())?;
                let files = manager.list_files().await.map_err(|e| e.to_string())?;

                let pattern_lower = pattern.to_lowercase();
                let results: Vec<Value> = files.iter()
                    .filter(|f| f.name.to_lowercase().contains(&pattern_lower))
                    .take(100)
                    .map(|f| json!({
                        "name": f.name,
                        "path": format!("{}/{}", path.trim_end_matches('/'), f.name),
                        "is_dir": f.is_dir,
                        "size": f.size,
                    }))
                    .collect();

                let total = results.len();
                Ok(json!({
                    "results": results,
                    "total": total,
                    "truncated": false,
                    "note": "FTP search is limited to current directory listing with name filter",
                }))
            } else {
                Err("Not connected to any server".to_string())
            }
        }

        "remote_info" => {
            let path = get_str(&args, "path")?;
            validate_path(&path, "path")?;

            if has_provider(&state).await {
                let mut provider = state.provider.lock().await;
                let provider = provider.as_mut().unwrap();
                let entry = provider.stat(&path).await.map_err(|e| e.to_string())?;

                Ok(json!({
                    "name": entry.name,
                    "path": entry.path,
                    "is_dir": entry.is_dir,
                    "size": entry.size,
                    "modified": entry.modified,
                    "permissions": entry.permissions,
                    "owner": entry.owner,
                }))
            } else if has_ftp(&app_state).await {
                // FTP: list parent dir and find the entry
                let file_name = path.rsplit(|c| c == '/' || c == '\\').next().unwrap_or(&path);
                let parent = if let Some(pos) = path.rfind(|c: char| c == '/' || c == '\\') {
                    let p = &path[..pos];
                    if p.is_empty() { "/" } else { p }
                } else {
                    "/"
                };

                let mut manager = app_state.ftp_manager.lock().await;
                manager.change_dir(parent).await.map_err(|e| e.to_string())?;
                let files = manager.list_files().await.map_err(|e| e.to_string())?;

                let entry = files.iter().find(|f| f.name == file_name)
                    .ok_or_else(|| format!("File not found: {}", path))?;

                Ok(json!({
                    "name": entry.name,
                    "path": path,
                    "is_dir": entry.is_dir,
                    "size": entry.size,
                    "modified": entry.modified,
                }))
            } else {
                Err("Not connected to any server".to_string())
            }
        }

        "local_list" => {
            let path = get_str(&args, "path")?;
            validate_path(&path, "path")?;

            let entries: Vec<Value> = std::fs::read_dir(&path)
                .map_err(|e| format!("Failed to read directory: {}", e))?
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

        "local_search" => {
            let path = get_str(&args, "path")?;
            let pattern = get_str(&args, "pattern")?;
            validate_path(&path, "path")?;

            let pattern_lower = pattern.to_lowercase();
            let results: Vec<Value> = std::fs::read_dir(&path)
                .map_err(|e| format!("Failed to read directory: {}", e))?
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().to_lowercase().contains(&pattern_lower))
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

            Ok(json!({
                "results": results,
                "total": results.len(),
            }))
        }

        "local_read" => {
            let path = get_str(&args, "path")?;
            validate_path(&path, "path")?;

            let meta = std::fs::metadata(&path)
                .map_err(|e| format!("Failed to stat file: {}", e))?;
            if meta.len() > 10_485_760 {
                return Err(format!("File too large for local_read: {:.1} MB (max 10 MB)", meta.len() as f64 / 1_048_576.0));
            }

            // Only read the first 5KB instead of the entire file
            let max_bytes: usize = 5120;
            let file_size = meta.len() as usize;
            let read_size = std::cmp::min(file_size, max_bytes);
            let mut file = std::fs::File::open(&path)
                .map_err(|e| format!("Failed to open file: {}", e))?;
            let mut buf = vec![0u8; read_size];
            use std::io::Read;
            file.read_exact(&mut buf)
                .map_err(|e| format!("Failed to read file: {}", e))?;

            let truncated = file_size > max_bytes;
            let content = String::from_utf8_lossy(&buf).to_string();

            Ok(json!({
                "content": content,
                "size": file_size,
                "truncated": truncated,
            }))
        }

        "local_write" => {
            let path = get_str(&args, "path")?;
            let content = get_str(&args, "content")?;
            validate_path(&path, "path")?;

            std::fs::write(&path, &content)
                .map_err(|e| format!("Failed to write file: {}", e))?;

            Ok(json!({ "success": true, "message": format!("Written {} bytes to {}", content.len(), path) }))
        }

        "local_mkdir" => {
            let path = get_str(&args, "path")?;
            validate_path(&path, "path")?;

            std::fs::create_dir_all(&path)
                .map_err(|e| format!("Failed to create directory: {}", e))?;

            Ok(json!({ "success": true, "message": format!("Created directory {}", path) }))
        }

        "local_delete" => {
            let path = get_str(&args, "path")?;
            validate_path(&path, "path")?;

            // Dangerous path protection (defense-in-depth)
            let home_dir = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_default();
            let normalized = path.trim_end_matches('/').trim_end_matches('\\');
            if normalized.is_empty() || normalized == "/" || normalized == "~" || normalized == "." || normalized == ".." || normalized == home_dir {
                return Err(format!("Refusing to delete dangerous path: {}", path));
            }

            let meta = std::fs::metadata(&path)
                .map_err(|e| format!("Path not found: {}", e))?;
            if meta.is_dir() {
                std::fs::remove_dir_all(&path)
                    .map_err(|e| format!("Failed to delete directory: {}", e))?;
            } else {
                std::fs::remove_file(&path)
                    .map_err(|e| format!("Failed to delete file: {}", e))?;
            }

            Ok(json!({ "success": true, "message": format!("Deleted {}", path) }))
        }

        "local_rename" => {
            let from = get_str(&args, "from")?;
            let to = get_str(&args, "to")?;
            validate_path(&from, "from")?;
            validate_path(&to, "to")?;

            std::fs::rename(&from, &to)
                .map_err(|e| format!("Failed to rename: {}", e))?;

            Ok(json!({ "success": true, "message": format!("Renamed {} to {}", from, to) }))
        }

        "local_edit" => {
            let path = get_str(&args, "path")?;
            let find = get_str(&args, "find")?;
            let replace = get_str(&args, "replace")?;
            let replace_all = args.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(true);
            validate_path(&path, "path")?;

            let mut content = std::fs::read_to_string(&path)
                .map_err(|e| format!("Failed to read file: {}", e))?;

            // Strip UTF-8 BOM if present (common in Windows-created files)
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

            std::fs::write(&path, &new_content)
                .map_err(|e| format!("Failed to write file: {}", e))?;

            let replaced = if replace_all { occurrences } else { 1 };
            Ok(json!({
                "success": true,
                "message": format!("Replaced {} occurrence(s) in {}", replaced, path),
                "occurrences": occurrences,
                "replaced": replaced,
            }))
        }

        "remote_edit" => {
            let path = get_str(&args, "path")?;
            let find = get_str(&args, "find")?;
            let replace = get_str(&args, "replace")?;
            let replace_all = args.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(true);
            validate_path(&path, "path")?;

            // Download file content
            let bytes = download_from_provider(&state, &app_state, &path).await?;

            let mut content = String::from_utf8(bytes)
                .map_err(|_| "File is not valid UTF-8 text".to_string())?;

            // Strip UTF-8 BOM if present
            content = content.strip_prefix('\u{FEFF}').unwrap_or(&content).to_string();

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

            // Write back via temp file + upload
            let tmp_path = std::env::temp_dir()
                .join(format!("aeroftp_{}", uuid::Uuid::new_v4()))
                .to_string_lossy()
                .to_string();
            std::fs::write(&tmp_path, &new_content)
                .map_err(|e| format!("Failed to write temp file: {}", e))?;

            let upload_result = if has_provider(&state).await {
                let mut provider = state.provider.lock().await;
                let provider = provider.as_mut().unwrap();
                provider.upload(&tmp_path, &path, None).await.map_err(|e| e.to_string())
            } else if has_ftp(&app_state).await {
                let mut manager = app_state.ftp_manager.lock().await;
                manager.upload_file(&tmp_path, &path).await.map_err(|e| e.to_string())
            } else {
                Err("Not connected".to_string())
            };

            let _ = std::fs::remove_file(&tmp_path);
            upload_result?;

            let replaced = if replace_all { occurrences } else { 1 };
            Ok(json!({
                "success": true,
                "message": format!("Replaced {} occurrence(s) in {}", replaced, path),
                "occurrences": occurrences,
                "replaced": replaced,
            }))
        }

        "upload_files" => {
            let local_paths: Vec<String> = args.get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .ok_or("Missing 'paths' array parameter")?;
            let remote_dir = get_str(&args, "remote_dir")?;
            validate_path(&remote_dir, "remote_dir")?;

            let mut uploaded = Vec::new();
            let mut errors = Vec::new();
            let total = local_paths.len();

            for (idx, local_path) in local_paths.iter().enumerate() {
                validate_path(local_path, "path").map_err(|e| e.to_string())?;
                let filename = std::path::Path::new(local_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "file".to_string());
                let remote_path = format!("{}/{}", remote_dir.trim_end_matches('/'), filename);

                emit_tool_progress(&app, "upload_files", idx as u32 + 1, total as u32, &filename);

                let result = if has_provider(&state).await {
                    let mut provider = state.provider.lock().await;
                    let provider = provider.as_mut().unwrap();
                    provider.upload(local_path, &remote_path, None).await.map_err(|e| e.to_string())
                } else if has_ftp(&app_state).await {
                    let mut manager = app_state.ftp_manager.lock().await;
                    manager.upload_file(local_path, &remote_path).await.map_err(|e| e.to_string())
                } else {
                    Err("Not connected to any server".to_string())
                };

                match result {
                    Ok(_) => uploaded.push(filename),
                    Err(e) => errors.push(json!({ "file": filename, "error": e })),
                }
            }

            Ok(json!({
                "uploaded": uploaded.len(),
                "failed": errors.len(),
                "files": uploaded,
                "errors": errors,
            }))
        }

        "download_files" => {
            let remote_paths: Vec<String> = args.get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .ok_or("Missing 'paths' array parameter")?;
            let local_dir = get_str(&args, "local_dir")?;
            validate_path(&local_dir, "local_dir")?;

            // Ensure local dir exists
            std::fs::create_dir_all(&local_dir)
                .map_err(|e| format!("Failed to create local directory: {}", e))?;

            let mut downloaded = Vec::new();
            let mut errors = Vec::new();
            let total = remote_paths.len();

            for (idx, remote_path) in remote_paths.iter().enumerate() {
                validate_path(remote_path, "path").map_err(|e| e.to_string())?;
                let filename = std::path::Path::new(remote_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "file".to_string());
                let local_path = format!("{}/{}", local_dir.trim_end_matches('/'), filename);

                emit_tool_progress(&app, "download_files", idx as u32 + 1, total as u32, &filename);

                let result = if has_provider(&state).await {
                    let mut provider = state.provider.lock().await;
                    let provider = provider.as_mut().unwrap();
                    provider.download(remote_path, &local_path, None).await.map_err(|e| e.to_string())
                } else if has_ftp(&app_state).await {
                    let mut manager = app_state.ftp_manager.lock().await;
                    manager.download_file(remote_path, &local_path).await.map_err(|e| e.to_string())
                } else {
                    Err("Not connected to any server".to_string())
                };

                match result {
                    Ok(_) => downloaded.push(filename),
                    Err(e) => errors.push(json!({ "file": filename, "error": e })),
                }
            }

            Ok(json!({
                "downloaded": downloaded.len(),
                "failed": errors.len(),
                "files": downloaded,
                "errors": errors,
            }))
        }

        "sync_preview" => {
            let local_path = get_str(&args, "local_path")?;
            let remote_path = get_str(&args, "remote_path")?;
            validate_path(&local_path, "local_path")?;
            validate_path(&remote_path, "remote_path")?;

            // Collect local files
            let local_files: std::collections::HashMap<String, u64> = std::fs::read_dir(&local_path)
                .map_err(|e| format!("Failed to read local directory: {}", e))?
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let meta = e.metadata().ok()?;
                    if meta.is_file() {
                        Some((e.file_name().to_string_lossy().to_string(), meta.len()))
                    } else {
                        None
                    }
                })
                .collect();

            // Collect remote files
            let remote_files: std::collections::HashMap<String, u64> = if has_provider(&state).await {
                let mut provider = state.provider.lock().await;
                let provider = provider.as_mut().unwrap();
                let entries = provider.list(&remote_path).await.map_err(|e| e.to_string())?;
                entries.iter().filter(|e| !e.is_dir).map(|e| (e.name.clone(), e.size)).collect()
            } else if has_ftp(&app_state).await {
                let mut manager = app_state.ftp_manager.lock().await;
                manager.change_dir(&remote_path).await.map_err(|e| e.to_string())?;
                let files = manager.list_files().await.map_err(|e| e.to_string())?;
                files.iter().filter(|f| !f.is_dir).map(|f| (f.name.clone(), f.size.unwrap_or(0))).collect()
            } else {
                return Err("Not connected to any server".to_string());
            };

            // Compare
            let mut only_local: Vec<Value> = Vec::new();
            let mut only_remote: Vec<Value> = Vec::new();
            let mut size_diff: Vec<Value> = Vec::new();
            let mut identical: Vec<String> = Vec::new();

            for (name, local_size) in &local_files {
                match remote_files.get(name) {
                    Some(&remote_size) if *local_size == remote_size => {
                        identical.push(name.clone());
                    }
                    Some(&remote_size) => {
                        size_diff.push(json!({
                            "name": name,
                            "local_size": local_size,
                            "remote_size": remote_size,
                        }));
                    }
                    None => {
                        only_local.push(json!({ "name": name, "size": local_size }));
                    }
                }
            }
            for (name, remote_size) in &remote_files {
                if !local_files.contains_key(name) {
                    only_remote.push(json!({ "name": name, "size": remote_size }));
                }
            }

            Ok(json!({
                "local_path": local_path,
                "remote_path": remote_path,
                "local_files": local_files.len(),
                "remote_files": remote_files.len(),
                "identical": identical.len(),
                "only_local": only_local,
                "only_remote": only_remote,
                "size_different": size_diff,
                "synced": only_local.is_empty() && only_remote.is_empty() && size_diff.is_empty(),
            }))
        }

        "archive_create" | "archive_extract" => {
            let _path = get_str(&args, "path")?;
            let _format = get_str_opt(&args, "format").unwrap_or_else(|| "zip".to_string());
            Ok(json!({
                "message": "Archive operations should be performed via the context menu on files. Right-click a file to compress/extract.",
            }))
        }

        "rag_index" => {
            let path = get_str(&args, "path")?;
            validate_path(&path, "path")?;
            let recursive = args.get("recursive").and_then(|v| v.as_bool()).unwrap_or(true);
            let max_files = args.get("max_files").and_then(|v| v.as_u64()).unwrap_or(200) as u32;

            const TEXT_EXTENSIONS: &[&str] = &[
                "rs", "ts", "tsx", "js", "jsx", "py", "json", "toml", "yaml", "yml",
                "md", "txt", "html", "css", "sh", "sql", "xml", "csv", "env", "cfg",
                "ini", "conf", "log", "go", "java", "c", "cpp", "h", "hpp", "rb",
                "php", "swift", "kt",
            ];

            fn is_text_file(path: &std::path::Path) -> bool {
                path.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| TEXT_EXTENSIONS.contains(&e.to_lowercase().as_str()))
                    .unwrap_or(false)
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
                        let rel = entry_path.strip_prefix(base)
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_else(|_| entry_path.to_string_lossy().to_string());
                        let name = entry_path.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let ext = entry_path.extension()
                            .and_then(|e| e.to_str())
                            .unwrap_or("")
                            .to_lowercase();
                        let size = meta.len();

                        let preview = if is_text_file(&entry_path) && size < 50_000 {
                            std::fs::read_to_string(&entry_path)
                                .ok()
                                .map(|content| {
                                    content.lines().take(20).collect::<Vec<_>>().join("\n")
                                })
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
                            file_obj.as_object_mut().unwrap().insert("preview".to_string(), json!(p));
                        }
                        files.push(file_obj);
                    }
                }
            }

            let base_path = std::path::Path::new(&path);
            if !base_path.is_dir() {
                return Err(format!("Not a directory: {}", path));
            }

            let mut files: Vec<Value> = Vec::new();
            let mut dirs_count: u32 = 0;
            scan_dir(base_path, base_path, recursive, &mut files, &mut dirs_count, max_files);

            // Emit progress after scan completes
            emit_tool_progress(&app, "rag_index", files.len() as u32, files.len() as u32, "scan complete");

            let total_size: u64 = files.iter()
                .filter_map(|f| f.get("size").and_then(|s| s.as_u64()))
                .sum();

            let mut extensions: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
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
        }

        "rag_search" => {
            let query = get_str(&args, "query")?;
            let path = get_str_opt(&args, "path").unwrap_or_else(|| ".".to_string());
            validate_path(&path, "path")?;
            let max_results = args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

            const SEARCH_EXTENSIONS: &[&str] = &[
                "rs", "ts", "tsx", "js", "jsx", "py", "json", "toml", "yaml", "yml",
                "md", "txt", "html", "css", "sh", "sql", "xml", "csv", "env", "cfg",
                "ini", "conf", "log", "go", "java", "c", "cpp", "h", "hpp", "rb",
                "php", "swift", "kt",
            ];

            fn is_searchable(path: &std::path::Path) -> bool {
                path.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| SEARCH_EXTENSIONS.contains(&e.to_lowercase().as_str()))
                    .unwrap_or(false)
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
                        search_dir(&entry_path, base, query_lower, matches, files_scanned, max_results, max_files);
                    } else if meta.is_file() && is_searchable(&entry_path) && meta.len() < 100_000 {
                        *files_scanned += 1;
                        let rel = entry_path.strip_prefix(base)
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

            let base_path = std::path::Path::new(&path);
            if !base_path.is_dir() {
                return Err(format!("Not a directory: {}", path));
            }

            let query_lower = query.to_lowercase();
            let mut matches: Vec<Value> = Vec::new();
            let mut files_scanned: u32 = 0;
            search_dir(base_path, base_path, &query_lower, &mut matches, &mut files_scanned, max_results, 500);

            Ok(json!({
                "query": query,
                "files_scanned": files_scanned,
                "matches": matches,
            }))
        }

        "preview_edit" => {
            let path = get_str(&args, "path")?;
            let find = get_str(&args, "find")?;
            let replace = get_str(&args, "replace")?;
            let replace_all = args.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(true);
            let remote = args.get("remote").and_then(|v| v.as_bool()).unwrap_or(false);
            validate_path(&path, "path")?;

            const MAX_PREVIEW_BYTES: usize = 100 * 1024; // 100KB

            let mut content = if remote {
                let bytes = download_from_provider(&state, &app_state, &path).await?;
                if bytes.len() > MAX_PREVIEW_BYTES {
                    return Ok(json!({
                        "success": false,
                        "message": "File too large for preview (max 100KB)",
                    }));
                }
                String::from_utf8(bytes)
                    .map_err(|_| "File is not valid UTF-8 text".to_string())?
            } else {
                let meta = std::fs::metadata(&path)
                    .map_err(|e| format!("Failed to stat file: {}", e))?;
                if meta.len() as usize > MAX_PREVIEW_BYTES {
                    return Ok(json!({
                        "success": false,
                        "message": "File too large for preview (max 100KB)",
                    }));
                }
                std::fs::read_to_string(&path)
                    .map_err(|e| format!("Failed to read file: {}", e))?
            };

            // Strip UTF-8 BOM if present
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

            let modified_content = if replace_all {
                content.replace(&find, &replace)
            } else {
                content.replacen(&find, &replace, 1)
            };

            let replaced = if replace_all { occurrences } else { 1 };
            Ok(json!({
                "success": true,
                "original": content,
                "modified": modified_content,
                "occurrences": occurrences,
                "replaced": replaced,
            }))
        }

        "agent_memory_write" => {
            let entry = args.get("entry")
                .and_then(|v| v.as_str())
                .ok_or("Missing 'entry' parameter")?;
            let category = args.get("category")
                .and_then(|v| v.as_str())
                .unwrap_or("general");

            // FIX 12: Sanitize category — only alphanumeric, underscore, hyphen; max 30 chars
            let sanitized_category: String = category.chars()
                .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                .take(30)
                .collect();

            // FIX 11: Require explicit project_path and validate it
            let project_path = args.get("project_path")
                .and_then(|v| v.as_str())
                .ok_or("Missing 'project_path' parameter")?;
            validate_path(project_path, "project_path")?;

            let formatted = format!("\n[{}] [{}] {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M"),
                sanitized_category,
                entry
            );

            crate::context_intelligence::write_agent_memory(
                project_path.to_string(),
                formatted
            ).await.map_err(|e| e.to_string())?;

            Ok(json!({
                "success": true,
                "message": format!("Memory entry saved: [{}] {}", sanitized_category, entry)
            }))
        }

        _ => Err(format!("Tool not implemented: {}", tool_name)),
    }
}
