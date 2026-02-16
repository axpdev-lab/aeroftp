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
    "local_move_files", "local_batch_rename", "local_copy_files", "local_trash",
    "local_file_info", "local_disk_usage", "local_find_duplicates",
    "remote_edit",
    // Batch transfer tools
    "upload_files", "download_files",
    // Advanced tools
    "sync_preview", "archive_compress", "archive_decompress",
    // RAG tools
    "rag_index", "rag_search",
    // Preview tools
    "preview_edit",
    // Agent memory
    "agent_memory_write",
    // Cyber tools
    "hash_file",
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
        "local_move_files" => {
            let paths = args.get("paths").and_then(|v| v.as_array());
            if paths.is_none() || paths.is_some_and(|a| a.is_empty()) {
                errors.push("'paths' array is missing or empty".to_string());
            } else if let Some(arr) = paths {
                for p in arr.iter().filter_map(|v| v.as_str()) {
                    let path = std::path::Path::new(p);
                    if !path.exists() {
                        warnings.push(format!("Source file not found: {}", p));
                    }
                }
            }
            if let Some(dest) = args.get("destination").and_then(|v| v.as_str()) {
                if let Err(e) = validate_path(dest, "destination") {
                    errors.push(e);
                }
            }
        }
        "local_batch_rename" | "local_copy_files" | "local_trash" => {
            let paths = args.get("paths").and_then(|v| v.as_array());
            if paths.is_none() || paths.is_some_and(|a| a.is_empty()) {
                errors.push("'paths' array is missing or empty".to_string());
            } else if let Some(arr) = paths {
                for p in arr.iter().filter_map(|v| v.as_str()) {
                    let path = std::path::Path::new(p);
                    if !path.exists() {
                        warnings.push(format!("Source not found: {}", p));
                    }
                }
            }
            if tool_name == "local_batch_rename" {
                if let Some(mode) = args.get("mode").and_then(|v| v.as_str()) {
                    if !["find_replace", "add_prefix", "add_suffix", "sequential"].contains(&mode) {
                        errors.push(format!("Invalid rename mode: {}. Use find_replace, add_prefix, add_suffix, or sequential", mode));
                    }
                } else {
                    errors.push("Missing 'mode' parameter".to_string());
                }
            }
            if tool_name == "local_copy_files" {
                if let Some(dest) = args.get("destination").and_then(|v| v.as_str()) {
                    if let Err(e) = validate_path(dest, "destination") {
                        errors.push(e);
                    }
                }
            }
        }
        "archive_compress" => {
            let paths = args.get("paths").and_then(|v| v.as_array());
            if paths.is_none() || paths.is_some_and(|a| a.is_empty()) {
                errors.push("'paths' array is missing or empty".to_string());
            }
            if let Some(output) = args.get("output_path").and_then(|v| v.as_str()) {
                if let Err(e) = validate_path(output, "output_path") {
                    errors.push(e);
                }
            } else {
                errors.push("Missing 'output_path' parameter".to_string());
            }
            if let Some(fmt) = args.get("format").and_then(|v| v.as_str()) {
                if !["zip", "7z", "tar", "tar.gz", "tar.bz2", "tar.xz"].contains(&fmt) {
                    errors.push(format!("Unsupported format: {}. Use zip, 7z, tar, tar.gz, tar.bz2, or tar.xz", fmt));
                }
            }
        }
        "archive_decompress" => {
            if let Some(path) = args.get("archive_path").and_then(|v| v.as_str()) {
                if let Err(e) = validate_path(path, "archive_path") {
                    errors.push(e);
                }
                let p = std::path::Path::new(path);
                if !p.exists() {
                    errors.push(format!("Archive not found: {}", path));
                }
            } else {
                errors.push("Missing 'archive_path' parameter".to_string());
            }
            if let Some(dir) = args.get("output_dir").and_then(|v| v.as_str()) {
                if let Err(e) = validate_path(dir, "output_dir") {
                    errors.push(e);
                }
            }
        }
        "hash_file" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                let p = std::path::Path::new(path);
                if !p.exists() {
                    errors.push(format!("File not found: {}", path));
                } else if p.is_dir() {
                    errors.push(format!("Path is a directory, not a file: {}", path));
                }
            }
            if let Some(algo) = args.get("algorithm").and_then(|v| v.as_str()) {
                if !["md5", "sha1", "sha256", "sha512", "blake3"].contains(&algo) {
                    errors.push(format!("Unsupported algorithm: {}. Use md5, sha1, sha256, sha512, or blake3", algo));
                }
            }
        }
        "local_file_info" | "local_disk_usage" | "local_find_duplicates" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                let p = std::path::Path::new(path);
                if !p.exists() {
                    errors.push(format!("Path not found: {}", path));
                }
                if (tool_name == "local_disk_usage" || tool_name == "local_find_duplicates") && !p.is_dir() {
                    errors.push(format!("Path is not a directory: {}", path));
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

/// Resolve a path against an optional base directory.
/// If path is already absolute, returns it unchanged.
/// If path is relative (just a filename) and base is Some, joins them.
fn resolve_local_path(path: &str, base: Option<&str>) -> String {
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

#[tauri::command]
pub async fn execute_ai_tool(
    app: tauri::AppHandle,
    state: State<'_, ProviderState>,
    app_state: State<'_, AppState>,
    tool_name: String,
    args: Value,
    context_local_path: Option<String>,
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
            let path = resolve_local_path(&get_str(&args, "path")?, context_local_path.as_deref());
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
            let path = resolve_local_path(&get_str(&args, "path")?, context_local_path.as_deref());
            let pattern = get_str(&args, "pattern")?;
            validate_path(&path, "path")?;

            let pattern_lower = pattern.to_lowercase();

            // Simple glob support: *.pdf → ends_with(".pdf"), test* → starts_with("test")
            let matcher: Box<dyn Fn(&str) -> bool> = if let Some(suffix) = pattern_lower.strip_prefix('*') {
                let suffix = suffix.to_string();
                Box::new(move |name: &str| name.ends_with(&suffix))
            } else if let Some(prefix) = pattern_lower.strip_suffix('*') {
                let prefix = prefix.to_string();
                Box::new(move |name: &str| name.starts_with(&prefix))
            } else if pattern_lower.contains('*') {
                let parts: Vec<String> = pattern_lower.split('*').map(String::from).collect();
                Box::new(move |name: &str| {
                    parts.iter().all(|part| name.contains(part.as_str()))
                })
            } else {
                let pat = pattern_lower.clone();
                Box::new(move |name: &str| name.contains(&pat))
            };

            let results: Vec<Value> = std::fs::read_dir(&path)
                .map_err(|e| format!("Failed to read directory: {}", e))?
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

            Ok(json!({
                "results": results,
                "total": results.len(),
            }))
        }

        "local_read" => {
            let path = resolve_local_path(&get_str(&args, "path")?, context_local_path.as_deref());
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
            let path = resolve_local_path(&get_str(&args, "path")?, context_local_path.as_deref());
            let content = get_str(&args, "content")?;
            validate_path(&path, "path")?;

            std::fs::write(&path, &content)
                .map_err(|e| format!("Failed to write file: {}", e))?;

            Ok(json!({ "success": true, "message": format!("Written {} bytes to {}", content.len(), path) }))
        }

        "local_mkdir" => {
            let path = resolve_local_path(&get_str(&args, "path")?, context_local_path.as_deref());
            validate_path(&path, "path")?;

            std::fs::create_dir_all(&path)
                .map_err(|e| format!("Failed to create directory: {}", e))?;

            Ok(json!({ "success": true, "message": format!("Created directory {}", path) }))
        }

        "local_delete" => {
            let path = resolve_local_path(&get_str(&args, "path")?, context_local_path.as_deref());
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
            let base = context_local_path.as_deref();
            let from = resolve_local_path(&get_str(&args, "from")?, base);
            let to = resolve_local_path(&get_str(&args, "to")?, base);
            validate_path(&from, "from")?;
            validate_path(&to, "to")?;

            std::fs::rename(&from, &to)
                .map_err(|e| format!("Failed to rename: {}", e))?;

            Ok(json!({ "success": true, "message": format!("Renamed {} to {}", from, to) }))
        }

        "local_move_files" => {
            let base = context_local_path.as_deref();
            let paths: Vec<String> = args.get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| resolve_local_path(s, base))).collect())
                .ok_or("Missing 'paths' array parameter")?;
            let destination = resolve_local_path(&get_str(&args, "destination")?, base);
            validate_path(&destination, "destination")?;

            if paths.is_empty() {
                return Err("'paths' array is empty".to_string());
            }

            // Ensure destination directory exists
            std::fs::create_dir_all(&destination)
                .map_err(|e| format!("Failed to create destination directory: {}", e))?;

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

                emit_tool_progress(&app, "local_move_files", idx as u32 + 1, total as u32, &filename);

                // Try rename first (fast, same-device move)
                match std::fs::rename(source, &dest_path) {
                    Ok(_) => moved.push(filename),
                    Err(_) => {
                        // Cross-device fallback: copy + delete
                        match std::fs::copy(source, &dest_path)
                            .and_then(|_| std::fs::remove_file(source))
                        {
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

        "local_batch_rename" => {
            let base = context_local_path.as_deref();
            let paths: Vec<String> = args.get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| resolve_local_path(s, base))).collect())
                .ok_or("Missing 'paths' array parameter")?;
            let mode = get_str(&args, "mode")?;

            if paths.is_empty() {
                return Err("'paths' array is empty".to_string());
            }

            // Helper: split name and extension (preserve extension for files)
            fn split_name_ext(name: &str, is_dir: bool) -> (&str, &str) {
                if is_dir { return (name, ""); }
                match name.rfind('.') {
                    Some(pos) if pos > 0 => (&name[..pos], &name[pos..]),
                    _ => (name, ""),
                }
            }

            // Compute new names
            let mut renames: Vec<(String, String)> = Vec::new();
            let mut errors = Vec::new();

            for (idx, source) in paths.iter().enumerate() {
                if let Err(e) = validate_path(source, "path") {
                    errors.push(json!({ "file": source, "error": e }));
                    continue;
                }
                let src_path = std::path::Path::new(source);
                let filename = src_path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let is_dir = src_path.is_dir();
                let (name_no_ext, ext) = split_name_ext(&filename, is_dir);

                let new_name = match mode.as_str() {
                    "find_replace" => {
                        let find = get_str_opt(&args, "find").unwrap_or_default();
                        let replace_with = get_str_opt(&args, "replace").unwrap_or_default();
                        let case_sensitive = args.get("case_sensitive").and_then(|v| v.as_bool()).unwrap_or(false);
                        if find.is_empty() {
                            filename.clone()
                        } else if case_sensitive {
                            filename.replace(&find, &replace_with)
                        } else {
                            // Case-insensitive replace
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
                        let prefix = get_str_opt(&args, "prefix").unwrap_or_default();
                        format!("{}{}", prefix, filename)
                    }
                    "add_suffix" => {
                        let suffix = get_str_opt(&args, "suffix").unwrap_or_default();
                        format!("{}{}{}", name_no_ext, suffix, ext)
                    }
                    "sequential" => {
                        let base_name = get_str_opt(&args, "base_name").unwrap_or_else(|| "file".to_string());
                        let start_number = args.get("start_number").and_then(|v| v.as_u64()).unwrap_or(1);
                        let padding = args.get("padding").and_then(|v| v.as_u64()).unwrap_or(2) as usize;
                        let num = start_number + idx as u64;
                        format!("{}_{:0>width$}{}", base_name, num, ext, width = padding)
                    }
                    _ => {
                        errors.push(json!({ "file": filename, "error": format!("Unknown rename mode: {}", mode) }));
                        continue;
                    }
                };

                if new_name != filename && !new_name.trim().is_empty() {
                    let parent = src_path.parent().unwrap_or(std::path::Path::new("/"));
                    let dest = parent.join(&new_name).to_string_lossy().to_string();
                    renames.push((source.clone(), dest));
                }
            }

            // Conflict detection
            let new_names: Vec<&str> = renames.iter().map(|(_, d)| d.as_str()).collect();
            let mut seen = std::collections::HashSet::new();
            for name in &new_names {
                if !seen.insert(*name) {
                    return Err(format!("Naming conflict detected: multiple files would be renamed to '{}'", name));
                }
            }

            // Execute renames
            let mut renamed = Vec::new();
            let total = renames.len();
            for (idx, (from, to)) in renames.iter().enumerate() {
                let filename = std::path::Path::new(from).file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                emit_tool_progress(&app, "local_batch_rename", idx as u32 + 1, total as u32, &filename);
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

        "local_copy_files" => {
            let base = context_local_path.as_deref();
            let paths: Vec<String> = args.get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| resolve_local_path(s, base))).collect())
                .ok_or("Missing 'paths' array parameter")?;
            let destination = resolve_local_path(&get_str(&args, "destination")?, base);
            validate_path(&destination, "destination")?;

            if paths.is_empty() {
                return Err("'paths' array is empty".to_string());
            }

            std::fs::create_dir_all(&destination)
                .map_err(|e| format!("Failed to create destination directory: {}", e))?;

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
                let filename = src_path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "file".to_string());
                let dest_path = format!("{}/{}", destination.trim_end_matches('/'), filename);

                emit_tool_progress(&app, "local_copy_files", idx as u32 + 1, total as u32, &filename);

                if src_path.is_dir() {
                    match copy_dir_recursive(src_path, std::path::Path::new(&dest_path)) {
                        Ok(_) => copied.push(filename),
                        Err(e) => errors.push(json!({ "file": filename, "error": e })),
                    }
                } else {
                    match std::fs::copy(source, &dest_path) {
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

        "local_trash" => {
            let base = context_local_path.as_deref();
            let paths: Vec<String> = args.get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| resolve_local_path(s, base))).collect())
                .ok_or("Missing 'paths' array parameter")?;

            if paths.is_empty() {
                return Err("'paths' array is empty".to_string());
            }

            let mut trashed = Vec::new();
            let mut errors = Vec::new();
            let total = paths.len();

            for (idx, path) in paths.iter().enumerate() {
                if let Err(e) = validate_path(path, "path") {
                    errors.push(json!({ "file": path, "error": e }));
                    continue;
                }
                let filename = std::path::Path::new(path).file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.clone());

                emit_tool_progress(&app, "local_trash", idx as u32 + 1, total as u32, &filename);

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

        "local_file_info" => {
            let path = resolve_local_path(&get_str(&args, "path")?, context_local_path.as_deref());
            validate_path(&path, "path")?;

            let p = std::path::Path::new(&path);
            let meta = std::fs::symlink_metadata(&path)
                .map_err(|e| format!("Failed to stat: {}", e))?;

            let mut info = json!({
                "path": path,
                "name": p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                "size": meta.len(),
                "is_file": meta.is_file(),
                "is_dir": meta.is_dir(),
                "is_symlink": meta.is_symlink(),
                "readonly": meta.permissions().readonly(),
            });

            // Timestamps
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

            // Unix-specific: permissions octal, uid, gid
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                info["permissions_octal"] = json!(format!("{:o}", meta.mode()));
                info["uid"] = json!(meta.uid());
                info["gid"] = json!(meta.gid());
            }

            // MIME type from extension
            if meta.is_file() {
                if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                    let mime = match ext.to_lowercase().as_str() {
                        "pdf" => "application/pdf", "txt" => "text/plain",
                        "html" | "htm" => "text/html", "css" => "text/css",
                        "js" => "text/javascript", "json" => "application/json",
                        "xml" => "application/xml", "zip" => "application/zip",
                        "7z" => "application/x-7z-compressed", "tar" => "application/x-tar",
                        "gz" => "application/gzip", "png" => "image/png",
                        "jpg" | "jpeg" => "image/jpeg", "gif" => "image/gif",
                        "svg" => "image/svg+xml", "mp3" => "audio/mpeg",
                        "mp4" => "video/mp4", "rs" => "text/x-rust",
                        "ts" | "tsx" => "text/typescript", "py" => "text/x-python",
                        _ => "application/octet-stream",
                    };
                    info["mime_type"] = json!(mime);
                }
            }

            Ok(info)
        }

        "local_disk_usage" => {
            let path = resolve_local_path(&get_str(&args, "path")?, context_local_path.as_deref());
            validate_path(&path, "path")?;

            let p = std::path::Path::new(&path);
            if !p.is_dir() {
                return Err(format!("Path is not a directory: {}", path));
            }

            // Inline calculation (same logic as filesystem.rs calculate_folder_size)
            let mut total_bytes: u64 = 0;
            let mut file_count: u64 = 0;
            let mut dir_count: u64 = 0;
            const MAX_ENTRIES: u64 = 500_000;
            let mut entry_count: u64 = 0;

            for entry in walkdir::WalkDir::new(&path)
                .follow_links(false)
                .max_depth(100)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                entry_count += 1;
                if entry_count > MAX_ENTRIES { break; }
                if entry.file_type().is_file() {
                    total_bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
                    file_count += 1;
                } else if entry.file_type().is_dir() && entry.path() != p {
                    dir_count += 1;
                }
            }

            Ok(json!({
                "path": path,
                "total_bytes": total_bytes,
                "total_human": format!("{:.1} MB", total_bytes as f64 / 1_048_576.0),
                "file_count": file_count,
                "dir_count": dir_count,
            }))
        }

        "local_find_duplicates" => {
            let path = resolve_local_path(&get_str(&args, "path")?, context_local_path.as_deref());
            validate_path(&path, "path")?;
            let min_size = args.get("min_size").and_then(|v| v.as_u64()).unwrap_or(1024);

            let p = std::path::Path::new(&path);
            if !p.is_dir() {
                return Err(format!("Path is not a directory: {}", path));
            }

            // Phase 1: group files by size
            let mut size_groups: std::collections::HashMap<u64, Vec<std::path::PathBuf>> = std::collections::HashMap::new();
            const MAX_SCAN: u64 = 50_000;
            let mut scan_count: u64 = 0;

            for entry in walkdir::WalkDir::new(&path)
                .follow_links(false)
                .max_depth(50)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                if !entry.file_type().is_file() { continue; }
                scan_count += 1;
                if scan_count > MAX_SCAN { break; }
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                if size < min_size { continue; }
                size_groups.entry(size).or_default().push(entry.into_path());
            }

            // Phase 2: hash files with matching sizes
            use md5::{Md5, Digest};
            use std::io::Read;
            let mut hash_groups: std::collections::HashMap<String, (u64, Vec<String>)> = std::collections::HashMap::new();

            for (size, files) in &size_groups {
                if files.len() < 2 { continue; }
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
                        let entry = hash_groups.entry(hash).or_insert_with(|| (*size, Vec::new()));
                        entry.1.push(file_path.to_string_lossy().to_string());
                    }
                }
            }

            // Phase 3: collect duplicates
            let mut duplicates: Vec<Value> = hash_groups
                .into_iter()
                .filter(|(_, (_, files))| files.len() >= 2)
                .map(|(hash, (size, files))| json!({
                    "hash": hash,
                    "size": size,
                    "count": files.len(),
                    "wasted_bytes": size * (files.len() as u64 - 1),
                    "files": files,
                }))
                .collect();

            duplicates.sort_by(|a, b| {
                let wa = a["wasted_bytes"].as_u64().unwrap_or(0);
                let wb = b["wasted_bytes"].as_u64().unwrap_or(0);
                wb.cmp(&wa)
            });

            let total_wasted: u64 = duplicates.iter()
                .map(|d| d["wasted_bytes"].as_u64().unwrap_or(0))
                .sum();

            Ok(json!({
                "groups": duplicates.len(),
                "total_wasted_bytes": total_wasted,
                "total_wasted_human": format!("{:.1} MB", total_wasted as f64 / 1_048_576.0),
                "duplicates": duplicates,
            }))
        }

        "local_edit" => {
            let path = resolve_local_path(&get_str(&args, "path")?, context_local_path.as_deref());
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

        "archive_compress" => {
            let paths: Vec<String> = args.get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .ok_or("Missing 'paths' array parameter")?;
            let output_path = get_str(&args, "output_path")?;
            let format = get_str_opt(&args, "format").unwrap_or_else(|| "zip".to_string());
            let password = get_str_opt(&args, "password");
            let compression_level = args.get("compression_level").and_then(|v| v.as_i64());
            validate_path(&output_path, "output_path")?;
            for p in &paths {
                validate_path(p, "path")?;
            }

            // Delegate to existing Tauri compress commands
            let result = match format.as_str() {
                "zip" => {
                    crate::compress_files_core(paths, output_path.clone(), password, compression_level).await
                }
                "7z" => {
                    crate::compress_7z_core(paths, output_path.clone(), password, compression_level).await
                }
                "tar" | "tar.gz" | "tar.bz2" | "tar.xz" => {
                    crate::compress_tar_core(paths, output_path.clone(), format.clone(), compression_level).await
                }
                _ => Err(format!("Unsupported format: {}. Use zip, 7z, tar, tar.gz, tar.bz2, or tar.xz", format)),
            };

            match result {
                Ok(msg) => Ok(json!({
                    "success": true,
                    "message": msg,
                    "output_path": output_path,
                    "format": format,
                })),
                Err(e) => Err(e),
            }
        }

        "archive_decompress" => {
            let archive_path = get_str(&args, "archive_path")?;
            let output_dir = get_str(&args, "output_dir")?;
            let password = get_str_opt(&args, "password");
            let create_subfolder = args.get("create_subfolder").and_then(|v| v.as_bool()).unwrap_or(true);
            validate_path(&archive_path, "archive_path")?;
            validate_path(&output_dir, "output_dir")?;

            // Detect format from extension
            let lower = archive_path.to_lowercase();
            let result = if lower.ends_with(".zip") {
                crate::extract_archive_core(archive_path.clone(), output_dir.clone(), create_subfolder, password).await
            } else if lower.ends_with(".7z") {
                crate::extract_7z_core(archive_path.clone(), output_dir.clone(), password, create_subfolder).await
            } else if lower.ends_with(".tar") || lower.ends_with(".tar.gz") || lower.ends_with(".tgz")
                || lower.ends_with(".tar.bz2") || lower.ends_with(".tar.xz") {
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
                Err(e) => Err(e),
            }
        }

        "hash_file" => {
            let path = get_str(&args, "path")?;
            let algorithm = get_str_opt(&args, "algorithm").unwrap_or_else(|| "sha256".to_string());
            validate_path(&path, "path")?;

            let p = std::path::Path::new(&path);
            if !p.is_file() {
                return Err(format!("Path is not a file: {}", path));
            }

            // Delegate to existing cyber_tools::hash_file
            let hash = crate::cyber_tools::hash_file(path.clone(), algorithm.clone()).await?;

            Ok(json!({
                "path": path,
                "algorithm": algorithm,
                "hash": hash,
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
