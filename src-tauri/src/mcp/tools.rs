//! MCP tool definitions and dispatch
//!
//! 16 curated tools (12 core + 4 extended) that provide unique value -
//! remote file operations that MCP clients don't have natively.
//!
//! Excludes local tools (local_list, shell_execute, etc.) that clients already have.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use crate::mcp::pool::ConnectionPool;
use crate::mcp::security::{self, RateCategory};
use crate::providers::ShareLinkOptions;
use serde_json::{json, Value};
use std::time::Instant;

/// Hard cap for in-memory read previews.
const MAX_READ_PREVIEW_BYTES: u64 = 1_048_576;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeleteKind {
    File,
    Directory,
    DirectoryRecursive,
}

/// Sanitize provider error messages to prevent credential leakage to AI clients.
/// Strips potential tokens, keys, and excessively long error bodies.
fn sanitize_error(e: impl std::fmt::Display) -> String {
    crate::providers::sanitize_api_error(&e.to_string())
}

/// MCP tool definition for `tools/list`.
pub struct McpToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub category: RateCategory,
}

/// Get all 16 curated tool definitions.
pub fn tool_definitions() -> Vec<McpToolDef> {
    vec![
        // -- Tier 1: Core (12 tools) --
        McpToolDef {
            name: "aeroftp_list_servers",
            description: "List all saved server profiles from the encrypted vault. Returns names, protocols, hosts. Passwords are never exposed.",
            input_schema: json!({ "type": "object", "properties": {}, "required": [] }),
            category: RateCategory::ReadOnly,
        },
        McpToolDef {
            name: "aeroftp_list_files",
            description: "List files and directories on a remote server.",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID from aeroftp_list_servers" },
                "path": { "type": "string", "description": "Remote directory path (default: /)" }
            }, "required": ["server"] }),
            category: RateCategory::ReadOnly,
        },
        McpToolDef {
            name: "aeroftp_read_file",
            description: "Read a remote text file (5 KB preview). For binary files, use aeroftp_download_file instead.",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID" },
                "path": { "type": "string", "description": "Remote file path" }
            }, "required": ["server", "path"] }),
            category: RateCategory::ReadOnly,
        },
        McpToolDef {
            name: "aeroftp_file_info",
            description: "Get metadata for a remote file or directory: size, modified date, permissions, owner.",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID" },
                "path": { "type": "string", "description": "Remote path" }
            }, "required": ["server", "path"] }),
            category: RateCategory::ReadOnly,
        },
        McpToolDef {
            name: "aeroftp_search_files",
            description: "Search for files matching a glob pattern on a remote server.",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID" },
                "path": { "type": "string", "description": "Base path to search in (default: /)" },
                "pattern": { "type": "string", "description": "Search pattern (e.g. \"*.log\", \"report*\")" }
            }, "required": ["server", "pattern"] }),
            category: RateCategory::ReadOnly,
        },
        McpToolDef {
            name: "aeroftp_upload_file",
            description: "Upload a local file or inline text content to a remote server.",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID" },
                "remote_path": { "type": "string", "description": "Destination path on the server" },
                "local_path": { "type": "string", "description": "Local file path to upload (mutually exclusive with content)" },
                "content": { "type": "string", "description": "Inline text content to upload (mutually exclusive with local_path)" }
            }, "required": ["server", "remote_path"] }),
            category: RateCategory::Mutative,
        },
        McpToolDef {
            name: "aeroftp_download_file",
            description: "Download a file from a remote server to a local path.",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID" },
                "remote_path": { "type": "string", "description": "Source path on the server" },
                "local_path": { "type": "string", "description": "Destination local path" }
            }, "required": ["server", "remote_path", "local_path"] }),
            category: RateCategory::Mutative,
        },
        McpToolDef {
            name: "aeroftp_create_directory",
            description: "Create a directory on a remote server.",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID" },
                "path": { "type": "string", "description": "Remote directory path to create" }
            }, "required": ["server", "path"] }),
            category: RateCategory::Mutative,
        },
        McpToolDef {
            name: "aeroftp_delete",
            description: "Delete a file or directory on a remote server. Set recursive=true to remove non-empty directories.",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID" },
                "path": { "type": "string", "description": "Remote path to delete" },
                "recursive": { "type": "boolean", "description": "Delete directories recursively (default: false)" }
            }, "required": ["server", "path"] }),
            category: RateCategory::Destructive,
        },
        McpToolDef {
            name: "aeroftp_rename",
            description: "Rename or move a file/directory on a remote server.",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID" },
                "from": { "type": "string", "description": "Current remote path" },
                "to": { "type": "string", "description": "New remote path" }
            }, "required": ["server", "from", "to"] }),
            category: RateCategory::Mutative,
        },
        McpToolDef {
            name: "aeroftp_storage_quota",
            description: "Get storage usage and quota information for a remote server.",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID" }
            }, "required": ["server"] }),
            category: RateCategory::ReadOnly,
        },
        McpToolDef {
            name: "aeroftp_server_info",
            description: "Get server type, features, and version information.",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID" }
            }, "required": ["server"] }),
            category: RateCategory::ReadOnly,
        },
        // -- Tier 2: Extended (4 tools) --
        McpToolDef {
            name: "aeroftp_create_share_link",
            description: "Generate a share link for a remote file (when supported by the provider).",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID" },
                "path": { "type": "string", "description": "Remote file path" },
                "expires_in_secs": { "type": "number", "description": "Link expiration in seconds (optional)" },
                "password": { "type": "string", "description": "Password protection (optional)" }
            }, "required": ["server", "path"] }),
            category: RateCategory::Mutative,
        },
        McpToolDef {
            name: "aeroftp_server_copy",
            description: "Copy a file server-side without download/upload (when supported).",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID" },
                "from": { "type": "string", "description": "Source remote path" },
                "to": { "type": "string", "description": "Destination remote path" }
            }, "required": ["server", "from", "to"] }),
            category: RateCategory::Mutative,
        },
        McpToolDef {
            name: "aeroftp_file_versions",
            description: "List version history of a file (when supported by the provider).",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID" },
                "path": { "type": "string", "description": "Remote file path" }
            }, "required": ["server", "path"] }),
            category: RateCategory::ReadOnly,
        },
        McpToolDef {
            name: "aeroftp_checksum",
            description: "Get server-side checksum(s) of a file (when supported).",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID" },
                "path": { "type": "string", "description": "Remote file path" }
            }, "required": ["server", "path"] }),
            category: RateCategory::ReadOnly,
        },
    ]
}

// ─── Helpers ─────────────────────────────────────────────────────────

fn get_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("Missing required argument: {}", key))
}

fn get_str_opt(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn get_bool_opt(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| v.as_bool())
}

fn validate_read_preview_target(is_dir: bool, size: u64) -> Result<(), String> {
    if is_dir {
        return Err("Cannot read a directory. Use aeroftp_list_files instead.".into());
    }
    if size > MAX_READ_PREVIEW_BYTES {
        return Err(format!(
            "File too large for preview ({:.1} KB). Max: {} KB. Use aeroftp_download_file instead.",
            size as f64 / 1024.0,
            MAX_READ_PREVIEW_BYTES / 1024,
        ));
    }
    Ok(())
}

fn delete_kind(is_dir: bool, recursive: bool) -> DeleteKind {
    match (is_dir, recursive) {
        (true, true) => DeleteKind::DirectoryRecursive,
        (true, false) => DeleteKind::Directory,
        (false, _) => DeleteKind::File,
    }
}

/// Validate server + optional path, return error tuple if invalid.
fn validate_sp(server: &str, path: Option<&str>) -> Result<(), String> {
    security::validate_server_query(server)?;
    if let Some(p) = path {
        security::validate_remote_path(p)?;
    }
    Ok(())
}

/// Build error result.
fn err(msg: String) -> (Value, bool) {
    (json!({ "error": msg }), true)
}

/// Build success result.
fn ok(val: Value) -> (Value, bool) {
    (val, false)
}

/// Audit log and return.
fn finish(
    tool: &str,
    server: Option<&str>,
    path: Option<&str>,
    result: (Value, bool),
    start: Instant,
) -> (Value, bool) {
    let duration_ms = start.elapsed().as_millis() as u64;
    let status = if result.1 { "error" } else { "ok" };
    security::audit_log(tool, server, path, status, duration_ms);
    result
}

// ─── Execute ─────────────────────────────────────────────────────────

/// Execute a tool call. Returns `(result_json, is_error)`.
pub async fn execute_tool(
    tool_name: &str,
    args: &Value,
    pool: &ConnectionPool,
    rate_limiter: &security::RateLimiter,
) -> (Value, bool) {
    let start = Instant::now();

    // Find rate category
    let category = tool_definitions()
        .iter()
        .find(|t| t.name == tool_name)
        .map(|t| t.category)
        .unwrap_or(RateCategory::ReadOnly);

    if let Err(retry_after) = rate_limiter.check(category) {
        security::audit_log(tool_name, None, None, "rate_limited", 0);
        return err(format!(
            "Rate limit exceeded. Retry after {:.1} seconds.",
            retry_after
        ));
    }

    match tool_name {
        "aeroftp_list_servers" => {
            let result = match crate::mcp::load_safe_profiles() {
                Ok(profiles) => ok(json!({ "servers": profiles, "count": profiles.len() })),
                Err(e) => err(e),
            };
            finish(tool_name, None, None, result, start)
        }

        "aeroftp_list_files" => {
            let server = match get_str(args, "server") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, None, None, err(e), start),
            };
            let path = get_str_opt(args, "path").unwrap_or_else(|| "/".to_string());
            if let Err(e) = validate_sp(&server, Some(&path)) {
                return finish(tool_name, Some(&server), Some(&path), err(e), start);
            }

            let result = match pool.get_provider(&server).await {
                Err(e) => err(e),
                Ok(arc) => {
                    let mut p = arc.lock().await;
                    match p.list(&path).await {
                        Err(e) => err(sanitize_error(e)),
                        Ok(entries) => {
                            let items: Vec<Value> = entries
                                .iter()
                                .take(200)
                                .map(|e| {
                                    json!({
                                        "name": e.name, "path": e.path, "is_dir": e.is_dir,
                                        "size": e.size, "modified": e.modified,
                                    })
                                })
                                .collect();
                            ok(json!({
                                "server": server, "path": path, "entries": items,
                                "total": entries.len(), "truncated": entries.len() > 200,
                            }))
                        }
                    }
                }
            };
            finish(tool_name, Some(&server), Some(&path), result, start)
        }

        "aeroftp_read_file" => {
            let server = match get_str(args, "server") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, None, None, err(e), start),
            };
            let path = match get_str(args, "path") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, Some(&server), None, err(e), start),
            };
            if let Err(e) = validate_sp(&server, Some(&path)) {
                return finish(tool_name, Some(&server), Some(&path), err(e), start);
            }

            let result = match pool.get_provider(&server).await {
                Err(e) => err(e),
                Ok(arc) => {
                    let mut p = arc.lock().await;
                    match p.stat(&path).await {
                        Err(e) => err(sanitize_error(e)),
                        Ok(entry) => match validate_read_preview_target(entry.is_dir, entry.size) {
                            Err(msg) => err(msg),
                            Ok(()) => match p.download_to_bytes(&path).await {
                                Err(e) => err(sanitize_error(e)),
                                Ok(data) => {
                                    let truncated = data.len() > 5 * 1024;
                                    let preview = if truncated {
                                        &data[..5 * 1024]
                                    } else {
                                        &data[..]
                                    };
                                    let content = String::from_utf8_lossy(preview).to_string();
                                    ok(
                                        json!({ "path": path, "content": content, "size": data.len(), "truncated": truncated }),
                                    )
                                }
                            },
                        },
                    }
                }
            };
            finish(tool_name, Some(&server), Some(&path), result, start)
        }

        "aeroftp_file_info" => {
            let server = match get_str(args, "server") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, None, None, err(e), start),
            };
            let path = match get_str(args, "path") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, Some(&server), None, err(e), start),
            };
            if let Err(e) = validate_sp(&server, Some(&path)) {
                return finish(tool_name, Some(&server), Some(&path), err(e), start);
            }

            let result = match pool.get_provider(&server).await {
                Err(e) => err(e),
                Ok(arc) => {
                    let mut p = arc.lock().await;
                    match p.stat(&path).await {
                        Err(e) => err(sanitize_error(e)),
                        Ok(entry) => ok(json!({
                            "path": path, "name": entry.name, "is_dir": entry.is_dir,
                            "size": entry.size, "modified": entry.modified,
                            "permissions": entry.permissions, "owner": entry.owner,
                        })),
                    }
                }
            };
            finish(tool_name, Some(&server), Some(&path), result, start)
        }

        "aeroftp_search_files" => {
            let server = match get_str(args, "server") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, None, None, err(e), start),
            };
            let pattern = match get_str(args, "pattern") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, Some(&server), None, err(e), start),
            };
            let path = get_str_opt(args, "path").unwrap_or_else(|| "/".to_string());
            if let Err(e) = validate_sp(&server, Some(&path)) {
                return finish(tool_name, Some(&server), Some(&path), err(e), start);
            }

            let result = match pool.get_provider(&server).await {
                Err(e) => err(e),
                Ok(arc) => {
                    let mut p = arc.lock().await;
                    match p.find(&path, &pattern).await {
                        Err(e) => err(sanitize_error(e)),
                        Ok(entries) => {
                            let items: Vec<Value> = entries.iter().take(100).map(|e| json!({
                                "name": e.name, "path": e.path, "is_dir": e.is_dir, "size": e.size,
                            })).collect();
                            ok(json!({
                                "path": path, "pattern": pattern, "results": items,
                                "total": entries.len(), "truncated": entries.len() > 100,
                            }))
                        }
                    }
                }
            };
            finish(tool_name, Some(&server), Some(&path), result, start)
        }

        "aeroftp_upload_file" => {
            let server = match get_str(args, "server") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, None, None, err(e), start),
            };
            let remote_path = match get_str(args, "remote_path") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, Some(&server), None, err(e), start),
            };
            if let Err(e) = validate_sp(&server, Some(&remote_path)) {
                return finish(tool_name, Some(&server), Some(&remote_path), err(e), start);
            }
            let local_path = get_str_opt(args, "local_path");
            let content = get_str_opt(args, "content");
            if local_path.is_none() && content.is_none() {
                return finish(
                    tool_name,
                    Some(&server),
                    Some(&remote_path),
                    err("Provide either 'local_path' or 'content'".into()),
                    start,
                );
            }
            if let Some(ref lp) = local_path {
                if let Err(e) = security::validate_local_path(lp) {
                    return finish(tool_name, Some(&server), Some(&remote_path), err(e), start);
                }
            }
            if let Some(ref c) = content {
                if let Err(e) = security::validate_text_content(c) {
                    return finish(tool_name, Some(&server), Some(&remote_path), err(e), start);
                }
            }

            let result = match pool.get_provider(&server).await {
                Err(e) => err(e),
                Ok(arc) => {
                    let mut p = arc.lock().await;
                    if let Some(ref local) = local_path {
                        let bytes = std::fs::metadata(local).map(|m| m.len()).unwrap_or(0);
                        match p.upload(local, &remote_path, None).await {
                            Ok(()) => ok(
                                json!({ "remote_path": remote_path, "uploaded": true, "bytes": bytes }),
                            ),
                            Err(e) => err(sanitize_error(e)),
                        }
                    } else {
                        let text = content.as_deref().unwrap_or_default();
                        let bytes = text.len();
                        let nonce = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_nanos())
                            .unwrap_or(0);
                        let tmp =
                            std::env::temp_dir().join(format!("aeroftp_mcp_upload_{}", nonce));
                        if let Err(e) = std::fs::write(&tmp, text) {
                            return finish(
                                tool_name,
                                Some(&server),
                                Some(&remote_path),
                                err(sanitize_error(e)),
                                start,
                            );
                        }
                        let upload_result = p
                            .upload(tmp.to_str().unwrap_or(""), &remote_path, None)
                            .await;
                        let _ = std::fs::remove_file(&tmp);
                        match upload_result {
                            Ok(()) => ok(
                                json!({ "remote_path": remote_path, "uploaded": true, "bytes": bytes }),
                            ),
                            Err(e) => err(sanitize_error(e)),
                        }
                    }
                }
            };
            finish(tool_name, Some(&server), Some(&remote_path), result, start)
        }

        "aeroftp_download_file" => {
            let server = match get_str(args, "server") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, None, None, err(e), start),
            };
            let remote_path = match get_str(args, "remote_path") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, Some(&server), None, err(e), start),
            };
            let local_path = match get_str(args, "local_path") {
                Ok(s) => s,
                Err(e) => {
                    return finish(tool_name, Some(&server), Some(&remote_path), err(e), start)
                }
            };
            if let Err(e) = validate_sp(&server, Some(&remote_path)) {
                return finish(tool_name, Some(&server), Some(&remote_path), err(e), start);
            }
            if let Err(e) = security::validate_local_path(&local_path) {
                return finish(tool_name, Some(&server), Some(&remote_path), err(e), start);
            }
            if let Some(parent) = std::path::Path::new(&local_path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }

            let result = match pool.get_provider(&server).await {
                Err(e) => err(e),
                Ok(arc) => {
                    let mut p = arc.lock().await;
                    match p.download(&remote_path, &local_path, None).await {
                        Ok(()) => ok(
                            json!({ "remote_path": remote_path, "local_path": local_path, "downloaded": true }),
                        ),
                        Err(e) => err(sanitize_error(e)),
                    }
                }
            };
            finish(tool_name, Some(&server), Some(&remote_path), result, start)
        }

        "aeroftp_create_directory" => {
            let server = match get_str(args, "server") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, None, None, err(e), start),
            };
            let path = match get_str(args, "path") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, Some(&server), None, err(e), start),
            };
            if let Err(e) = validate_sp(&server, Some(&path)) {
                return finish(tool_name, Some(&server), Some(&path), err(e), start);
            }

            let result = match pool.get_provider(&server).await {
                Err(e) => err(e),
                Ok(arc) => {
                    let mut p = arc.lock().await;
                    match p.mkdir(&path).await {
                        Ok(()) => ok(json!({ "path": path, "created": true })),
                        Err(e) => err(sanitize_error(e)),
                    }
                }
            };
            finish(tool_name, Some(&server), Some(&path), result, start)
        }

        "aeroftp_delete" => {
            let server = match get_str(args, "server") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, None, None, err(e), start),
            };
            let path = match get_str(args, "path") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, Some(&server), None, err(e), start),
            };
            let recursive = get_bool_opt(args, "recursive").unwrap_or(false);
            if let Err(e) = validate_sp(&server, Some(&path)) {
                return finish(tool_name, Some(&server), Some(&path), err(e), start);
            }

            let result = match pool.get_provider(&server).await {
                Err(e) => err(e),
                Ok(arc) => {
                    let mut p = arc.lock().await;
                    match p.stat(&path).await {
                        Err(e) => err(sanitize_error(e)),
                        Ok(entry) => match delete_kind(entry.is_dir, recursive) {
                            DeleteKind::Directory => match p.rmdir(&path).await {
                                Ok(()) => ok(json!({
                                    "path": path,
                                    "deleted": true,
                                    "is_dir": true,
                                    "recursive": false,
                                })),
                                Err(e) => err(sanitize_error(e)),
                            },
                            DeleteKind::DirectoryRecursive => {
                                match p.rmdir_recursive(&path).await {
                                    Ok(()) => ok(json!({
                                        "path": path,
                                        "deleted": true,
                                        "is_dir": true,
                                        "recursive": true,
                                    })),
                                    Err(e) => err(sanitize_error(e)),
                                }
                            }
                            DeleteKind::File => match p.delete(&path).await {
                                Ok(()) => ok(json!({
                                    "path": path,
                                    "deleted": true,
                                    "is_dir": false,
                                    "recursive": false,
                                })),
                                Err(e) => err(sanitize_error(e)),
                            },
                        },
                    }
                }
            };
            finish(tool_name, Some(&server), Some(&path), result, start)
        }

        "aeroftp_rename" => {
            let server = match get_str(args, "server") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, None, None, err(e), start),
            };
            let from = match get_str(args, "from") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, Some(&server), None, err(e), start),
            };
            let to = match get_str(args, "to") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, Some(&server), Some(&from), err(e), start),
            };
            if let Err(e) = validate_sp(&server, Some(&from)) {
                return finish(tool_name, Some(&server), Some(&from), err(e), start);
            }
            if let Err(e) = security::validate_remote_path(&to) {
                return finish(tool_name, Some(&server), Some(&from), err(e), start);
            }

            let result = match pool.get_provider(&server).await {
                Err(e) => err(e),
                Ok(arc) => {
                    let mut p = arc.lock().await;
                    match p.rename(&from, &to).await {
                        Ok(()) => ok(json!({ "from": from, "to": to, "renamed": true })),
                        Err(e) => err(sanitize_error(e)),
                    }
                }
            };
            finish(tool_name, Some(&server), Some(&from), result, start)
        }

        "aeroftp_storage_quota" => {
            let server = match get_str(args, "server") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, None, None, err(e), start),
            };
            if let Err(e) = security::validate_server_query(&server) {
                return finish(tool_name, Some(&server), None, err(e), start);
            }

            let result = match pool.get_provider(&server).await {
                Err(e) => err(e),
                Ok(arc) => {
                    let mut p = arc.lock().await;
                    match p.storage_info().await {
                        Err(e) => err(sanitize_error(e)),
                        Ok(info) => {
                            let used_pct = if info.total > 0 {
                                format!("{:.1}%", info.used as f64 / info.total as f64 * 100.0)
                            } else {
                                "N/A".to_string()
                            };
                            ok(
                                json!({ "used": info.used, "total": info.total, "free": info.free, "used_pct": used_pct }),
                            )
                        }
                    }
                }
            };
            finish(tool_name, Some(&server), None, result, start)
        }

        "aeroftp_server_info" => {
            let server = match get_str(args, "server") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, None, None, err(e), start),
            };
            if let Err(e) = security::validate_server_query(&server) {
                return finish(tool_name, Some(&server), None, err(e), start);
            }

            let result = match pool.get_provider(&server).await {
                Err(e) => err(e),
                Ok(arc) => {
                    let mut p = arc.lock().await;
                    match p.server_info().await {
                        Err(e) => err(sanitize_error(e)),
                        Ok(info) => {
                            let provider_type = p.provider_type().to_string();
                            ok(json!({
                                "provider_type": provider_type, "server_info": info,
                                "supports_share_links": p.supports_share_links(),
                                "supports_server_copy": p.supports_server_copy(),
                                "supports_versions": p.supports_versions(),
                                "supports_checksum": p.supports_checksum(),
                                "supports_find": p.supports_find(),
                            }))
                        }
                    }
                }
            };
            finish(tool_name, Some(&server), None, result, start)
        }

        "aeroftp_create_share_link" => {
            let server = match get_str(args, "server") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, None, None, err(e), start),
            };
            let path = match get_str(args, "path") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, Some(&server), None, err(e), start),
            };
            if let Err(e) = validate_sp(&server, Some(&path)) {
                return finish(tool_name, Some(&server), Some(&path), err(e), start);
            }
            let expires = args.get("expires_in_secs").and_then(|v| v.as_u64());
            let password = get_str_opt(args, "password");

            let result = match pool.get_provider(&server).await {
                Err(e) => err(e),
                Ok(arc) => {
                    let mut p = arc.lock().await;
                    let opts = ShareLinkOptions {
                        expires_in_secs: expires,
                        password,
                        permissions: None,
                    };
                    match p.create_share_link(&path, opts).await {
                        Err(e) => err(sanitize_error(e)),
                        Ok(link) => ok(
                            json!({ "url": link.url, "password": link.password, "expires_at": link.expires_at }),
                        ),
                    }
                }
            };
            finish(tool_name, Some(&server), Some(&path), result, start)
        }

        "aeroftp_server_copy" => {
            let server = match get_str(args, "server") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, None, None, err(e), start),
            };
            let from = match get_str(args, "from") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, Some(&server), None, err(e), start),
            };
            let to = match get_str(args, "to") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, Some(&server), Some(&from), err(e), start),
            };
            if let Err(e) = validate_sp(&server, Some(&from)) {
                return finish(tool_name, Some(&server), Some(&from), err(e), start);
            }
            if let Err(e) = security::validate_remote_path(&to) {
                return finish(tool_name, Some(&server), Some(&from), err(e), start);
            }

            let result = match pool.get_provider(&server).await {
                Err(e) => err(e),
                Ok(arc) => {
                    let mut p = arc.lock().await;
                    match p.server_copy(&from, &to).await {
                        Ok(()) => ok(json!({ "from": from, "to": to, "copied": true })),
                        Err(e) => err(sanitize_error(e)),
                    }
                }
            };
            finish(tool_name, Some(&server), Some(&from), result, start)
        }

        "aeroftp_file_versions" => {
            let server = match get_str(args, "server") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, None, None, err(e), start),
            };
            let path = match get_str(args, "path") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, Some(&server), None, err(e), start),
            };
            if let Err(e) = validate_sp(&server, Some(&path)) {
                return finish(tool_name, Some(&server), Some(&path), err(e), start);
            }

            let result = match pool.get_provider(&server).await {
                Err(e) => err(e),
                Ok(arc) => {
                    let mut p = arc.lock().await;
                    match p.list_versions(&path).await {
                        Err(e) => err(sanitize_error(e)),
                        Ok(versions) => {
                            let items: Vec<Value> = versions
                                .iter()
                                .map(|v| {
                                    json!({
                                        "id": v.id, "modified": v.modified, "size": v.size,
                                    })
                                })
                                .collect();
                            ok(json!({ "path": path, "versions": items, "total": versions.len() }))
                        }
                    }
                }
            };
            finish(tool_name, Some(&server), Some(&path), result, start)
        }

        "aeroftp_checksum" => {
            let server = match get_str(args, "server") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, None, None, err(e), start),
            };
            let path = match get_str(args, "path") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, Some(&server), None, err(e), start),
            };
            if let Err(e) = validate_sp(&server, Some(&path)) {
                return finish(tool_name, Some(&server), Some(&path), err(e), start);
            }

            let result = match pool.get_provider(&server).await {
                Err(e) => err(e),
                Ok(arc) => {
                    let mut p = arc.lock().await;
                    match p.checksum(&path).await {
                        Err(e) => err(sanitize_error(e)),
                        Ok(checksums) => ok(json!({ "path": path, "checksums": checksums })),
                    }
                }
            };
            finish(tool_name, Some(&server), Some(&path), result, start)
        }

        _ => finish(
            tool_name,
            None,
            None,
            err(format!("Unknown tool: {}", tool_name)),
            start,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        delete_kind, tool_definitions, validate_read_preview_target, DeleteKind,
        MAX_READ_PREVIEW_BYTES,
    };

    #[test]
    fn read_preview_rejects_directories() {
        let err = validate_read_preview_target(true, 0).unwrap_err();
        assert!(err.contains("Cannot read a directory"));
    }

    #[test]
    fn read_preview_rejects_large_files() {
        let err = validate_read_preview_target(false, MAX_READ_PREVIEW_BYTES + 1).unwrap_err();
        assert!(err.contains("File too large for preview"));
    }

    #[test]
    fn read_preview_accepts_regular_small_files() {
        assert!(validate_read_preview_target(false, 4096).is_ok());
    }

    #[test]
    fn delete_kind_distinguishes_file_directory_and_recursive_directory() {
        assert_eq!(delete_kind(false, false), DeleteKind::File);
        assert_eq!(delete_kind(false, true), DeleteKind::File);
        assert_eq!(delete_kind(true, false), DeleteKind::Directory);
        assert_eq!(delete_kind(true, true), DeleteKind::DirectoryRecursive);
    }

    #[test]
    fn delete_tool_schema_exposes_recursive_flag() {
        let delete_tool = tool_definitions()
            .into_iter()
            .find(|tool| tool.name == "aeroftp_delete")
            .expect("delete tool definition");

        let recursive = delete_tool
            .input_schema
            .get("properties")
            .and_then(|props| props.get("recursive"));

        assert!(recursive.is_some());
    }
}
