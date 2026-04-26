//! Remote tool handlers for T3 Gate 2 Area C.
//!
//! These handlers use `ToolCtx::remote_backend()` so MCP can run through the
//! unified dispatcher while GUI/CLI keep their legacy fallback until their
//! per-surface remote backends are fully wired.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use serde_json::{json, Value};

use crate::ai_core::tools::{ToolCtx, ToolError};

const MAX_READ_PREVIEW_BYTES: usize = 1_048_576;
const DEFAULT_PREVIEW_BYTES: usize = 5 * 1024;

fn get_str(args: &Value, key: &str) -> Result<String, ToolError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| ToolError::InvalidArgs {
            tool: "remote".to_string(),
            reason: format!("Missing required argument: {key}"),
        })
}

fn get_str_opt(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

fn get_bool_opt(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| v.as_bool())
}

fn validate_remote_path(path: &str, label: &str) -> Result<(), ToolError> {
    if path.contains('\0') {
        return Err(ToolError::InvalidArgs {
            tool: "remote".to_string(),
            reason: format!("{label} contains null bytes"),
        });
    }
    Ok(())
}

fn backend_error(e: String) -> ToolError {
    if e.contains("remote_backend not wired") {
        ToolError::NotMigrated("remote_backend".to_string())
    } else {
        ToolError::Exec(e)
    }
}

fn entry_json(e: &crate::providers::RemoteEntry, include_permissions: bool) -> Value {
    let mut v = json!({
        "name": e.name,
        "path": e.path,
        "is_dir": e.is_dir,
        "size": e.size,
        "modified": e.modified,
    });
    if include_permissions {
        if let Some(obj) = v.as_object_mut() {
            obj.insert("permissions".to_string(), json!(e.permissions));
            obj.insert("owner".to_string(), json!(e.owner));
        }
    }
    v
}

fn normalize_server(args: &Value) -> Result<String, ToolError> {
    get_str(args, "server")
}

fn normalize_path_arg(args: &Value, primary: &str, default: &str) -> String {
    get_str_opt(args, primary).unwrap_or_else(|| default.to_string())
}

fn alias_name(tool_name: &str) -> &str {
    match tool_name {
        "remote_list" | "remote_list_files" => "aeroftp_list_files",
        "remote_read" | "remote_read_file" => "aeroftp_read_file",
        "remote_download" | "remote_download_file" => "aeroftp_download_file",
        "remote_upload" | "remote_upload_file" => "aeroftp_upload_file",
        "remote_upload_many" => "aeroftp_upload_many",
        "remote_delete" => "aeroftp_delete",
        "remote_delete_many" => "aeroftp_delete_many",
        "remote_mkdir" | "remote_create_directory" => "aeroftp_create_directory",
        "remote_rename" => "aeroftp_rename",
        "remote_stat" | "remote_info" | "remote_file_info" => "aeroftp_file_info",
        "remote_search" | "remote_search_files" => "aeroftp_search_files",
        "remote_storage_quota" => "aeroftp_storage_quota",
        "remote_list_servers" | "server_list_saved" => "aeroftp_list_servers",
        "remote_agent_connect" | "agent_connect" => "aeroftp_agent_connect",
        other => other,
    }
}

fn parent_remote_dir(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    let idx = trimmed.rfind('/')?;
    if idx == 0 {
        None
    } else {
        Some(trimmed[..idx].to_string())
    }
}

async fn ensure_remote_parents(
    ctx: &dyn ToolCtx,
    server: &str,
    path: &str,
) -> Result<(), ToolError> {
    let Some(parent) = parent_remote_dir(path) else {
        return Ok(());
    };
    let backend = ctx.remote_backend(server).await.map_err(backend_error)?;
    let mut acc = String::new();
    let leading_slash = parent.starts_with('/');
    for part in parent.split('/').filter(|p| !p.is_empty()) {
        if leading_slash || !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(part);
        match backend.mkdir(&acc).await {
            Ok(()) => {}
            Err(e) => {
                let low = e.to_ascii_lowercase();
                if low.contains("already exists")
                    || low.contains("file exists")
                    || low.contains("eexist")
                {
                    continue;
                }
                return Err(ToolError::Exec(format!("mkdir {} failed: {}", acc, e)));
            }
        }
    }
    Ok(())
}

pub async fn dispatch_remote_tool(
    ctx: &dyn ToolCtx,
    tool_name: &str,
    args: &Value,
) -> Result<Value, ToolError> {
    match alias_name(tool_name) {
        "aeroftp_list_servers" => list_servers(ctx, args).await,
        "aeroftp_list_files" => list_files(ctx, args).await,
        "aeroftp_read_file" => read_file(ctx, args).await,
        "aeroftp_file_info" => file_info(ctx, args).await,
        "aeroftp_search_files" => search_files(ctx, args).await,
        "aeroftp_upload_file" => upload_file(ctx, args).await,
        "aeroftp_upload_many" => upload_many(ctx, args).await,
        "aeroftp_download_file" => download_file(ctx, args).await,
        "aeroftp_create_directory" => create_directory(ctx, args).await,
        "aeroftp_delete" => delete(ctx, args).await,
        "aeroftp_delete_many" => delete_many(ctx, args).await,
        "aeroftp_rename" => rename(ctx, args).await,
        "aeroftp_storage_quota" => storage_quota(ctx, args).await,
        "aeroftp_agent_connect" => agent_connect(ctx, args).await,
        "server_exec" => server_exec(ctx, args).await,
        _ => Err(ToolError::NotMigrated(tool_name.to_string())),
    }
}

async fn list_servers(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let name_contains = get_str_opt(args, "name_contains").map(|s| s.to_lowercase());
    let protocol = get_str_opt(args, "protocol").map(|s| s.to_lowercase());
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| n.min(1_000) as usize)
        .unwrap_or(200);
    let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

    let profiles = ctx.credentials().list_servers().map_err(ToolError::Exec)?;
    // Snapshot vault keyset once so per-profile auth_state derivation is
    // O(1) instead of one decryption per call. When the vault isn't open
    // (rare in MCP, possible in CLI agent context), derivation falls back
    // to "unknown" via auth_state_from_cache, but here we have the cache.
    let auth_lookup = crate::credential_store::CredentialStore::from_cache().map(|store| {
        let accounts: std::collections::HashSet<String> = store
            .list_accounts()
            .unwrap_or_default()
            .into_iter()
            .collect();
        (store, accounts)
    });
    let filtered: Vec<Value> = profiles
        .into_iter()
        .filter(|p| {
            let name_ok = name_contains
                .as_ref()
                .map(|needle| p.name.to_lowercase().contains(needle))
                .unwrap_or(true);
            let proto_ok = protocol
                .as_ref()
                .map(|proto| p.protocol.to_lowercase() == *proto)
                .unwrap_or(true);
            name_ok && proto_ok
        })
        .map(|p| {
            let auth_state = auth_lookup
                .as_ref()
                .map(|(store, accounts)| {
                    crate::profile_auth_state::derive_profile_auth_state(
                        store,
                        accounts,
                        &p.id,
                        &p.protocol,
                    )
                })
                .unwrap_or("unknown");
            json!({
                "id": p.id,
                "name": p.name,
                "protocol": p.protocol,
                "host": p.host,
                "port": p.port,
                "username": p.username,
                "initialPath": p.initial_path,
                "providerId": p.provider_id,
                "auth_state": auth_state,
            })
        })
        .collect();
    let matched_total = filtered.len();
    let page: Vec<Value> = filtered.into_iter().skip(offset).take(limit).collect();
    let returned = page.len();
    Ok(json!({
        "servers": page,
        "count": returned,
        "total_matched": matched_total,
        "offset": offset,
        "limit": limit,
        "truncated": offset + returned < matched_total,
    }))
}

async fn list_files(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let server = normalize_server(args)?;
    let path = normalize_path_arg(args, "path", "/");
    validate_remote_path(&path, "path")?;
    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;
    let entries = backend.list(&path).await.map_err(ToolError::Exec)?;
    let items: Vec<Value> = entries
        .iter()
        .take(200)
        .map(|e| entry_json(e, false))
        .collect();
    Ok(json!({
        "server": server,
        "path": path,
        "entries": items,
        "total": entries.len(),
        "truncated": entries.len() > 200,
    }))
}

async fn read_file(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let server = normalize_server(args)?;
    let path = get_str(args, "path")?;
    validate_remote_path(&path, "path")?;
    let requested_kb = args
        .get("preview_kb")
        .and_then(|v| v.as_u64())
        .unwrap_or(5)
        .max(1);
    let preview_bytes = (requested_kb.saturating_mul(1024) as usize).min(MAX_READ_PREVIEW_BYTES);
    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;
    let entry = backend.stat(&path).await.map_err(ToolError::Exec)?;
    if entry.is_dir {
        return Err(ToolError::Exec(
            "Cannot read a directory. Use aeroftp_list_files instead.".to_string(),
        ));
    }
    if entry.size > MAX_READ_PREVIEW_BYTES as u64 {
        return Err(ToolError::Exec(format!(
            "File too large for preview ({:.1} KB). Hard cap: {} KB. Use aeroftp_download_file for larger files.",
            entry.size as f64 / 1024.0,
            MAX_READ_PREVIEW_BYTES / 1024
        )));
    }
    let data = backend
        .download_to_bytes(&path)
        .await
        .map_err(ToolError::Exec)?;
    let window = preview_bytes.max(DEFAULT_PREVIEW_BYTES.min(preview_bytes));
    let truncated = data.len() > window;
    let preview = &data[..data.len().min(window)];
    Ok(json!({
        "server": server,
        "path": path,
        "content": String::from_utf8_lossy(preview).to_string(),
        "size": data.len(),
        "truncated": truncated,
        "preview_kb": window / 1024,
    }))
}

async fn file_info(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let server = normalize_server(args)?;
    let path = get_str(args, "path")?;
    validate_remote_path(&path, "path")?;
    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;
    let entry = backend.stat(&path).await.map_err(ToolError::Exec)?;
    let mut out = entry_json(&entry, true);
    if let Some(obj) = out.as_object_mut() {
        obj.insert("server".to_string(), json!(server));
    }
    Ok(out)
}

async fn search_files(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let server = normalize_server(args)?;
    let path = normalize_path_arg(args, "path", "/");
    let pattern = get_str(args, "pattern")?;
    validate_remote_path(&path, "path")?;
    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;
    let entries = backend
        .search(&path, &pattern)
        .await
        .map_err(ToolError::Exec)?;
    let items: Vec<Value> = entries
        .iter()
        .take(100)
        .map(|e| entry_json(e, false))
        .collect();
    Ok(json!({
        "server": server,
        "path": path,
        "pattern": pattern,
        "results": items,
        "total": entries.len(),
        "truncated": entries.len() > 100,
    }))
}

async fn upload_file(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let server = normalize_server(args)?;
    let remote_path = get_str(args, "remote_path")?;
    validate_remote_path(&remote_path, "remote_path")?;
    let create_parents = get_bool_opt(args, "create_parents").unwrap_or(false);
    let no_clobber = get_bool_opt(args, "no_clobber").unwrap_or(false);
    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;
    if no_clobber && backend.stat(&remote_path).await.is_ok() {
        return Ok(json!({
            "server": server,
            "remote_path": remote_path,
            "uploaded": false,
            "skipped": true,
            "reason": "exists",
            "no_clobber": true,
        }));
    }
    if create_parents {
        ensure_remote_parents(ctx, &server, &remote_path).await?;
    }
    if let Some(local_path) = get_str_opt(args, "local_path") {
        backend
            .upload(&local_path, &remote_path)
            .await
            .map_err(ToolError::Exec)?;
        let bytes = std::fs::metadata(&local_path).map(|m| m.len()).unwrap_or(0);
        Ok(json!({
            "server": server,
            "remote_path": remote_path,
            "uploaded": true,
            "bytes": bytes,
        }))
    } else if let Some(content) = get_str_opt(args, "content") {
        backend
            .upload_from_bytes(content.as_bytes(), &remote_path)
            .await
            .map_err(ToolError::Exec)?;
        Ok(json!({
            "server": server,
            "remote_path": remote_path,
            "uploaded": true,
            "bytes": content.len(),
        }))
    } else {
        Err(ToolError::InvalidArgs {
            tool: "aeroftp_upload_file".to_string(),
            reason: "Provide either 'local_path' or 'content'".to_string(),
        })
    }
}

async fn upload_many(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let server = normalize_server(args)?;
    let items = args
        .get("items")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ToolError::InvalidArgs {
            tool: "aeroftp_upload_many".to_string(),
            reason: "'items' must be an array".to_string(),
        })?;
    if items.len() > 100 {
        return Err(ToolError::InvalidArgs {
            tool: "aeroftp_upload_many".to_string(),
            reason: format!("items exceeds max (100): got {}", items.len()),
        });
    }
    let continue_on_error = get_bool_opt(args, "continue_on_error").unwrap_or(true);
    let mut results = Vec::with_capacity(items.len());
    let mut uploaded = 0u32;
    let mut errors = 0u32;
    let started = std::time::Instant::now();
    for (idx, item) in items.iter().enumerate() {
        let mut item_args = item.clone();
        let Some(obj) = item_args.as_object_mut() else {
            return Err(ToolError::InvalidArgs {
                tool: "aeroftp_upload_many".to_string(),
                reason: format!("items[{idx}] must be an object"),
            });
        };
        obj.insert("server".to_string(), json!(server));
        match upload_file(ctx, &item_args).await {
            Ok(v) => {
                uploaded += u32::from(v.get("uploaded").and_then(|b| b.as_bool()).unwrap_or(false));
                results.push(v);
            }
            Err(e) => {
                errors += 1;
                results.push(json!({"uploaded": false, "error": e.to_string()}));
                if !continue_on_error {
                    break;
                }
            }
        }
    }
    let elapsed_secs = started.elapsed().as_secs();
    Ok(json!({
        "server": server,
        "results": results,
        "summary": {
            "planned": items.len(),
            "processed": results.len(),
            "uploaded": uploaded,
            "errors": errors,
            "totals": {
                "requested": items.len(),
                "succeeded": uploaded,
                "failed": errors,
                "skipped": 0u32,
                "elapsed_secs": elapsed_secs,
            },
        },
    }))
}

async fn download_file(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let server = normalize_server(args)?;
    let remote_path = get_str(args, "remote_path")?;
    let local_path = get_str(args, "local_path")?;
    validate_remote_path(&remote_path, "remote_path")?;
    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;
    if let Some(parent) = std::path::Path::new(&local_path).parent() {
        std::fs::create_dir_all(parent).map_err(|e| ToolError::Exec(e.to_string()))?;
    }
    backend
        .download(&remote_path, &local_path)
        .await
        .map_err(ToolError::Exec)?;
    Ok(json!({
        "server": server,
        "remote_path": remote_path,
        "local_path": local_path,
        "downloaded": true,
    }))
}

async fn create_directory(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let server = normalize_server(args)?;
    let path = get_str(args, "path")?;
    validate_remote_path(&path, "path")?;
    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;
    backend.mkdir(&path).await.map_err(ToolError::Exec)?;
    Ok(json!({ "server": server, "path": path, "created": true }))
}

async fn delete(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    if args.get("paths").is_some() {
        return delete_many(ctx, args).await;
    }
    let server = normalize_server(args)?;
    let path = get_str(args, "path")?;
    validate_remote_path(&path, "path")?;
    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;
    backend.delete(&path).await.map_err(ToolError::Exec)?;
    Ok(json!({ "server": server, "path": path, "deleted": true }))
}

async fn delete_many(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let server = normalize_server(args)?;
    let paths = args
        .get("paths")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ToolError::InvalidArgs {
            tool: "aeroftp_delete_many".to_string(),
            reason: "'paths' must be an array".to_string(),
        })?;
    if paths.len() > 100 {
        return Err(ToolError::InvalidArgs {
            tool: "aeroftp_delete_many".to_string(),
            reason: format!("paths exceeds max (100): got {}", paths.len()),
        });
    }
    let continue_on_error = get_bool_opt(args, "continue_on_error").unwrap_or(true);
    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;
    let mut results = Vec::with_capacity(paths.len());
    let mut deleted = 0u32;
    let mut errors = 0u32;
    let started = std::time::Instant::now();
    for path in paths {
        let Some(path) = path.as_str() else {
            return Err(ToolError::InvalidArgs {
                tool: "aeroftp_delete_many".to_string(),
                reason: "every entry of 'paths' must be a string".to_string(),
            });
        };
        validate_remote_path(path, "path")?;
        match backend.delete(path).await {
            Ok(()) => {
                deleted += 1;
                results.push(json!({"path": path, "deleted": true}));
            }
            Err(e) => {
                errors += 1;
                results.push(json!({"path": path, "deleted": false, "error": e}));
                if !continue_on_error {
                    break;
                }
            }
        }
    }
    let elapsed_secs = started.elapsed().as_secs();
    Ok(json!({
        "server": server,
        "results": results,
        "summary": {
            "planned": paths.len(),
            "processed": results.len(),
            "deleted": deleted,
            "errors": errors,
            "totals": {
                "requested": paths.len(),
                "succeeded": deleted,
                "failed": errors,
                "skipped": 0u32,
                "elapsed_secs": elapsed_secs,
            },
        },
    }))
}

async fn rename(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let server = normalize_server(args)?;
    let from = get_str(args, "from")?;
    let to = get_str(args, "to")?;
    validate_remote_path(&from, "from")?;
    validate_remote_path(&to, "to")?;
    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;
    backend.rename(&from, &to).await.map_err(ToolError::Exec)?;
    Ok(json!({ "server": server, "from": from, "to": to, "renamed": true }))
}

async fn storage_quota(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let server = normalize_server(args)?;
    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;
    let info = backend.storage_info().await.map_err(ToolError::Exec)?;
    Ok(json!({
        "server": server,
        "used_bytes": info.used,
        "total_bytes": info.total,
        "free_bytes": info.available,
    }))
}

async fn agent_connect(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    use crate::agent_session;
    let server = normalize_server(args)?;

    // Local-only profile lookup. Distinct top-level shape on miss so the
    // agent can branch without parsing the per-block payload.
    let profile = match agent_session::lookup_profile(&server) {
        Ok(p) => p,
        Err(e) => return Ok(agent_session::lookup_error_payload(&server, &e)),
    };

    let path = agent_session::path_block(&profile);
    let capabilities = agent_session::capabilities_block(&profile.protocol);

    // `remote_backend()` opens (or reuses) the pooled connection — its
    // outcome IS the "connect" block. No separate connect() call needed.
    let started = std::time::Instant::now();
    let backend_result = ctx.remote_backend(&server).await;
    let elapsed_ms = started.elapsed().as_millis();

    let (connect, quota) = match backend_result {
        Ok(backend) => {
            let connect = agent_session::connect_block_ok(&profile.id, elapsed_ms);
            let quota = match backend.storage_info().await {
                Ok(q) => agent_session::quota_block_ok(q.used, q.total, q.available),
                Err(msg) => {
                    let lower = msg.to_ascii_lowercase();
                    if lower.contains("not supported") || lower.contains("notsupported") {
                        agent_session::quota_block_unsupported(&profile.protocol)
                    } else {
                        agent_session::quota_block_unavailable(&msg)
                    }
                }
            };
            (connect, quota)
        }
        Err(msg) => (
            agent_session::connect_block_error(&msg),
            agent_session::quota_block_unavailable("connect failed"),
        ),
    };

    Ok(json!({
        "profile": agent_session::profile_block(&profile),
        "connect": connect,
        "capabilities": capabilities,
        "quota": quota,
        "path": path,
    }))
}

async fn server_exec(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let operation = get_str(args, "operation")?;
    let server = normalize_server(args)?;
    let path = normalize_path_arg(args, "path", "/");
    let mut result = match operation.as_str() {
        "ls" => list_files(ctx, &json!({"server": server, "path": path})).await?,
        "cat" => read_file(ctx, &json!({"server": server, "path": path})).await?,
        "stat" => file_info(ctx, &json!({"server": server, "path": path})).await?,
        "find" => {
            let pattern = get_str_opt(args, "pattern").unwrap_or_else(|| "*".to_string());
            search_files(
                ctx,
                &json!({"server": server, "path": path, "pattern": pattern}),
            )
            .await?
        }
        "df" => storage_quota(ctx, &json!({"server": server})).await?,
        "get" => {
            let local_path =
                get_str(args, "local_path").or_else(|_| get_str(args, "destination"))?;
            download_file(
                ctx,
                &json!({"server": server, "remote_path": path, "local_path": local_path}),
            )
            .await?
        }
        "put" => {
            let local_path = get_str(args, "local_path")?;
            let remote_path = get_str_opt(args, "remote_path").unwrap_or(path);
            upload_file(
                ctx,
                &json!({"server": server, "remote_path": remote_path, "local_path": local_path}),
            )
            .await?
        }
        "mkdir" => create_directory(ctx, &json!({"server": server, "path": path})).await?,
        "rm" => delete(ctx, &json!({"server": server, "path": path})).await?,
        "mv" => {
            let destination = get_str(args, "destination")?;
            rename(
                ctx,
                &json!({"server": server, "from": path, "to": destination}),
            )
            .await?
        }
        _ => {
            return Err(ToolError::InvalidArgs {
                tool: "server_exec".to_string(),
                reason: format!("Invalid operation '{operation}'"),
            });
        }
    };
    if let Some(obj) = result.as_object_mut() {
        obj.insert("operation".to_string(), json!(operation));
    }
    Ok(result)
}
