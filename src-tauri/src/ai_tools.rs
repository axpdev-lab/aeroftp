//! AI Tool Execution via StorageProvider trait + FTP fallback
//!
//! Provides a unified `execute_ai_tool` command that routes AI tool calls
//! through the active StorageProvider (13 protocols). When no provider is
//! connected, falls back to `AppState.ftp_manager` for FTP/FTPS sessions.

use serde_json::{json, Value};
use tauri::State;
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
];

/// Validate a path argument — reject null bytes, traversal, excessive length
fn validate_path(path: &str, param: &str) -> Result<(), String> {
    if path.len() > 4096 {
        return Err(format!("{}: path exceeds 4096 characters", param));
    }
    if path.contains('\0') {
        return Err(format!("{}: path contains null bytes", param));
    }
    for component in path.split('/').chain(path.split('\\')) {
        if component == ".." {
            return Err(format!("{}: path traversal ('..') not allowed", param));
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

#[tauri::command]
pub async fn execute_ai_tool(
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

            if has_provider(&state).await {
                let mut provider = state.provider.lock().await;
                let provider = provider.as_mut().unwrap();
                let bytes = provider.download_to_bytes(&path).await.map_err(|e| e.to_string())?;

                let max_bytes = 5120;
                let truncated = bytes.len() > max_bytes;
                let content = String::from_utf8_lossy(&bytes[..bytes.len().min(max_bytes)]).to_string();

                Ok(json!({ "content": content, "size": bytes.len(), "truncated": truncated }))
            } else if has_ftp(&app_state).await {
                let mut manager = app_state.ftp_manager.lock().await;
                let bytes = manager.download_to_bytes(&path).await.map_err(|e| e.to_string())?;

                let max_bytes = 5120;
                let truncated = bytes.len() > max_bytes;
                let content = String::from_utf8_lossy(&bytes[..bytes.len().min(max_bytes)]).to_string();

                Ok(json!({ "content": content, "size": bytes.len(), "truncated": truncated }))
            } else {
                Err("Not connected to any server".to_string())
            }
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

            let bytes = std::fs::read(&path)
                .map_err(|e| format!("Failed to read file: {}", e))?;

            let max_bytes = 5120;
            let truncated = bytes.len() > max_bytes;
            let content = String::from_utf8_lossy(&bytes[..bytes.len().min(max_bytes)]).to_string();

            Ok(json!({
                "content": content,
                "size": bytes.len(),
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

            let content = std::fs::read_to_string(&path)
                .map_err(|e| format!("Failed to read file: {}", e))?;

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
            let bytes = if has_provider(&state).await {
                let mut provider = state.provider.lock().await;
                let provider = provider.as_mut().unwrap();
                provider.download_to_bytes(&path).await.map_err(|e| e.to_string())?
            } else if has_ftp(&app_state).await {
                let mut manager = app_state.ftp_manager.lock().await;
                manager.download_to_bytes(&path).await.map_err(|e| e.to_string())?
            } else {
                return Err("Not connected to any server".to_string());
            };

            let content = String::from_utf8(bytes)
                .map_err(|_| "File is not valid UTF-8 text".to_string())?;

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

            for local_path in &local_paths {
                validate_path(local_path, "path").map_err(|e| e.to_string())?;
                let filename = std::path::Path::new(local_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "file".to_string());
                let remote_path = format!("{}/{}", remote_dir.trim_end_matches('/'), filename);

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

            for remote_path in &remote_paths {
                validate_path(remote_path, "path").map_err(|e| e.to_string())?;
                let filename = std::path::Path::new(remote_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "file".to_string());
                let local_path = format!("{}/{}", local_dir.trim_end_matches('/'), filename);

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

        _ => Err(format!("Tool not implemented: {}", tool_name)),
    }
}
