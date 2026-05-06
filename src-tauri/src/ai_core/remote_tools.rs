//! Remote tool handlers for T3 Gate 2 Area C.
//!
//! These handlers use `ToolCtx::remote_backend()` so MCP can run through the
//! unified dispatcher while GUI/CLI keep their legacy fallback until their
//! per-surface remote backends are fully wired.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

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
    if path.len() > 4096 {
        return Err(ToolError::InvalidArgs {
            tool: "remote".to_string(),
            reason: format!("{label} exceeds 4096 characters"),
        });
    }
    if path.contains('\0') {
        return Err(ToolError::InvalidArgs {
            tool: "remote".to_string(),
            reason: format!("{label} contains null bytes"),
        });
    }
    if path.starts_with('-') {
        return Err(ToolError::InvalidArgs {
            tool: "remote".to_string(),
            reason: format!("{label} must not start with '-'"),
        });
    }
    let normalized = path.replace('\\', "/");
    if normalized.split('/').any(|component| component == "..") {
        return Err(ToolError::InvalidArgs {
            tool: "remote".to_string(),
            reason: format!("{label} must not contain '..' traversal components"),
        });
    }
    if path.chars().any(|c| c.is_control() && c != '\t') {
        return Err(ToolError::InvalidArgs {
            tool: "remote".to_string(),
            reason: format!("{label} contains control characters"),
        });
    }
    Ok(())
}

fn validate_local_path(path: &str, label: &str) -> Result<(), ToolError> {
    crate::ai_core::local_tools::validate_path(path, label).map_err(|reason| {
        ToolError::InvalidArgs {
            tool: "remote".to_string(),
            reason,
        }
    })
}

fn backend_error(e: String) -> ToolError {
    if e.contains("remote_backend not wired") {
        ToolError::NotMigrated("remote_backend".to_string())
    } else {
        ToolError::Exec(e)
    }
}

/// Hard cap per pagina per `aeroftp_list_files` / `aeroftp_search_files`.
/// Se l'agente passa un `limit` superiore, viene clampato a questo valore.
const MAX_LIST_PAGE: usize = 5_000;

#[derive(Debug)]
struct ListFilterOpts {
    limit: usize,
    offset: usize,
    sort: ListSort,
    reverse: bool,
    files_only: bool,
    dirs_only: bool,
    min_size: Option<u64>,
    max_size: Option<u64>,
    /// Lower bound (inclusive) for `modified` ISO 8601 string.
    min_mtime: Option<String>,
    /// Upper bound (inclusive) for `modified` ISO 8601 string.
    max_mtime: Option<String>,
}

#[derive(Debug, Clone, Copy)]
enum ListSort {
    Name,
    Size,
    Mtime,
}

impl ListFilterOpts {
    fn from_args(args: &Value, default_limit: usize) -> Self {
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| (n as usize).min(MAX_LIST_PAGE))
            .filter(|n| *n > 0)
            .unwrap_or(default_limit);
        let offset = args
            .get("offset")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(0);
        let sort = match args.get("sort").and_then(|v| v.as_str()) {
            Some("size") => ListSort::Size,
            Some("mtime") | Some("date") => ListSort::Mtime,
            _ => ListSort::Name,
        };
        let reverse = args
            .get("reverse")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let files_only = args
            .get("files_only")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let dirs_only = args
            .get("dirs_only")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let min_size = args.get("min_size").and_then(|v| v.as_u64());
        let max_size = args.get("max_size").and_then(|v| v.as_u64());
        let min_mtime = get_str_opt(args, "min_mtime");
        let max_mtime = get_str_opt(args, "max_mtime");
        Self {
            limit,
            offset,
            sort,
            reverse,
            files_only,
            dirs_only,
            min_size,
            max_size,
            min_mtime,
            max_mtime,
        }
    }

    fn has_filter(&self) -> bool {
        self.files_only
            || self.dirs_only
            || self.min_size.is_some()
            || self.max_size.is_some()
            || self.min_mtime.is_some()
            || self.max_mtime.is_some()
    }
}

fn apply_entry_filter(
    entries: Vec<crate::providers::RemoteEntry>,
    opts: &ListFilterOpts,
) -> Vec<crate::providers::RemoteEntry> {
    if !opts.has_filter() {
        return entries;
    }
    entries
        .into_iter()
        .filter(|e| {
            if opts.files_only && e.is_dir {
                return false;
            }
            if opts.dirs_only && !e.is_dir {
                return false;
            }
            // Size filters apply only to files (directories have size=0
            // which would always trip min_size unless we exempt them).
            if !e.is_dir {
                if let Some(min) = opts.min_size {
                    if e.size < min {
                        return false;
                    }
                }
                if let Some(max) = opts.max_size {
                    if e.size > max {
                        return false;
                    }
                }
            }
            // ISO 8601 strings are lexicographically sortable for the same
            // timezone offset (e.g. all 'Z'). When the entry has no mtime,
            // we keep it (best-effort): agent can sort=mtime to push them
            // to the end if it cares.
            if let (Some(ref min), Some(ref m)) = (&opts.min_mtime, &e.modified) {
                if m.as_str() < min.as_str() {
                    return false;
                }
            }
            if let (Some(ref max), Some(ref m)) = (&opts.max_mtime, &e.modified) {
                if m.as_str() > max.as_str() {
                    return false;
                }
            }
            true
        })
        .collect()
}

fn apply_entry_sort(entries: &mut [crate::providers::RemoteEntry], opts: &ListFilterOpts) {
    match opts.sort {
        ListSort::Name => entries.sort_by_key(|e| e.name.to_lowercase()),
        ListSort::Size => entries.sort_by_key(|e| e.size),
        ListSort::Mtime => entries.sort_by(|a, b| {
            // Entries senza mtime in fondo nell'ordine asc (None > Some(_))
            // ISO 8601 e' lex-ordinato per data, quindi cmp(&str) e' corretto.
            match (&a.modified, &b.modified) {
                (Some(x), Some(y)) => x.cmp(y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
        }),
    }
    if opts.reverse {
        entries.reverse();
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
        "remote_hashsum" => "aeroftp_hashsum",
        "remote_head" => "aeroftp_head",
        "remote_tail" => "aeroftp_tail",
        "remote_tree" => "aeroftp_tree",
        "remote_transfer" => "aeroftp_transfer",
        "remote_transfer_tree" => "aeroftp_transfer_tree",
        "remote_touch" => "aeroftp_touch",
        "remote_cleanup" => "aeroftp_cleanup",
        "remote_speed" => "aeroftp_speed",
        "remote_sync_doctor" => "aeroftp_sync_doctor",
        "remote_dedupe" => "aeroftp_dedupe",
        "remote_reconcile" => "aeroftp_reconcile",
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
        "aeroftp_hashsum" => hashsum(ctx, args).await,
        "aeroftp_head" => head_file(ctx, args).await,
        "aeroftp_tail" => tail_file(ctx, args).await,
        "aeroftp_tree" => tree(ctx, args).await,
        "aeroftp_transfer" => transfer_one(ctx, args).await,
        "aeroftp_transfer_tree" => transfer_tree(ctx, args).await,
        "aeroftp_touch" => touch(ctx, args).await,
        "aeroftp_cleanup" => cleanup(ctx, args).await,
        "aeroftp_speed" => speed(ctx, args).await,
        "aeroftp_sync_doctor" => sync_doctor(ctx, args).await,
        "aeroftp_dedupe" => dedupe(ctx, args).await,
        "aeroftp_reconcile" => reconcile(ctx, args).await,
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
    let opts = ListFilterOpts::from_args(args, 200);
    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;
    let entries = backend.list(&path).await.map_err(ToolError::Exec)?;
    let total_unfiltered = entries.len();
    let mut filtered = apply_entry_filter(entries, &opts);
    let total_matched = filtered.len();
    apply_entry_sort(&mut filtered, &opts);
    let page: Vec<Value> = filtered
        .into_iter()
        .skip(opts.offset)
        .take(opts.limit)
        .map(|e| entry_json(&e, false))
        .collect();
    let returned = page.len();
    Ok(json!({
        "server": server,
        "path": path,
        "entries": page,
        "count": returned,
        "total": total_unfiltered,
        "total_matched": total_matched,
        "offset": opts.offset,
        "limit": opts.limit,
        "truncated": opts.offset + returned < total_matched,
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
    let opts = ListFilterOpts::from_args(args, 100);
    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;
    let entries = backend
        .search(&path, &pattern)
        .await
        .map_err(ToolError::Exec)?;
    let total_unfiltered = entries.len();
    let mut filtered = apply_entry_filter(entries, &opts);
    let total_matched = filtered.len();
    apply_entry_sort(&mut filtered, &opts);
    let page: Vec<Value> = filtered
        .into_iter()
        .skip(opts.offset)
        .take(opts.limit)
        .map(|e| entry_json(&e, false))
        .collect();
    let returned = page.len();
    Ok(json!({
        "server": server,
        "path": path,
        "pattern": pattern,
        "results": page,
        "count": returned,
        "total": total_unfiltered,
        "total_matched": total_matched,
        "offset": opts.offset,
        "limit": opts.limit,
        "truncated": opts.offset + returned < total_matched,
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
        validate_local_path(&local_path, "local_path")?;
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
    validate_local_path(&local_path, "local_path")?;
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

/// Hard cap per `aeroftp_hashsum`: sopra questo size il tool ritorna errore
/// `file_too_large` invece di tentare il download. Coerente con il fatto che
/// il backend.download_to_bytes carica tutto il payload in RAM (stessa
/// pipeline della CLI `aeroftp hashsum`).
const MAX_HASHSUM_BYTES: u64 = 256 * 1024 * 1024;

async fn hashsum(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let server = normalize_server(args)?;
    let path = get_str(args, "path")?;
    validate_remote_path(&path, "path")?;
    let algorithm = get_str_opt(args, "algorithm")
        .map(|s| s.to_lowercase())
        .unwrap_or_else(|| "sha256".to_string());
    if !matches!(
        algorithm.as_str(),
        "sha256" | "sha1" | "sha512" | "md5" | "blake3"
    ) {
        return Err(ToolError::InvalidArgs {
            tool: "aeroftp_hashsum".to_string(),
            reason: format!(
                "unsupported algorithm '{algorithm}'. Use one of: sha256, sha1, sha512, md5, blake3"
            ),
        });
    }
    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;
    let entry = backend.stat(&path).await.map_err(ToolError::Exec)?;
    if entry.is_dir {
        return Err(ToolError::Exec("Cannot hash a directory.".to_string()));
    }
    if entry.size > MAX_HASHSUM_BYTES {
        return Ok(json!({
            "server": server,
            "path": path,
            "algorithm": algorithm,
            "error": "file_too_large",
            "size": entry.size,
            "max_size": MAX_HASHSUM_BYTES,
            "hint": "Use the CLI 'aeroftp hashsum' for files larger than 256 MB; MCP keeps a memory cap to protect the agent process."
        }));
    }
    let data = backend
        .download_to_bytes(&path)
        .await
        .map_err(ToolError::Exec)?;
    let hash = match algorithm.as_str() {
        "md5" => {
            use md5::Digest;
            format!("{:x}", md5::Md5::digest(&data))
        }
        "sha1" => {
            use sha1::Digest;
            format!("{:x}", sha1::Sha1::digest(&data))
        }
        "sha512" => {
            use sha2::Digest;
            format!("{:x}", sha2::Sha512::digest(&data))
        }
        "blake3" => blake3::hash(&data).to_hex().to_string(),
        _ => {
            use sha2::Digest;
            format!("{:x}", sha2::Sha256::digest(&data))
        }
    };
    Ok(json!({
        "server": server,
        "path": path,
        "algorithm": algorithm,
        "hash": hash,
        "bytes_hashed": data.len(),
    }))
}

/// Hard cap per scan in `aeroftp_head` / `aeroftp_tail`. Protegge il
/// processo MCP quando un agente chiede ultime/prime N righe di un log
/// gigantesco. Stessa pipeline di `read_file` ma esposta per linee.
const MAX_HEAD_TAIL_BYTES: u64 = 16 * 1024 * 1024;
const DEFAULT_HEAD_TAIL_LINES: usize = 50;
const MAX_HEAD_TAIL_LINES: usize = 10_000;

fn parse_lines_arg(args: &Value) -> Result<usize, ToolError> {
    let n = args
        .get("lines")
        .and_then(|v| v.as_i64())
        .unwrap_or(DEFAULT_HEAD_TAIL_LINES as i64);
    if n <= 0 {
        return Err(ToolError::InvalidArgs {
            tool: "head/tail".to_string(),
            reason: format!("lines must be > 0 (got {n})"),
        });
    }
    Ok((n as usize).min(MAX_HEAD_TAIL_LINES))
}

async fn fetch_text_for_line_op(
    ctx: &dyn ToolCtx,
    server: &str,
    path: &str,
) -> Result<(String, u64, bool), ToolError> {
    let backend = ctx.remote_backend(server).await.map_err(backend_error)?;
    let entry = backend.stat(path).await.map_err(ToolError::Exec)?;
    if entry.is_dir {
        return Err(ToolError::Exec("Cannot head/tail a directory.".to_string()));
    }
    let truncated = entry.size > MAX_HEAD_TAIL_BYTES;
    if truncated {
        return Err(ToolError::Exec(format!(
            "File too large ({} bytes). Cap: {} bytes. Use aeroftp_download_file then process locally.",
            entry.size, MAX_HEAD_TAIL_BYTES
        )));
    }
    let data = backend
        .download_to_bytes(path)
        .await
        .map_err(ToolError::Exec)?;
    let total_size = data.len() as u64;
    let text = String::from_utf8_lossy(&data).into_owned();
    Ok((text, total_size, truncated))
}

async fn head_file(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let server = normalize_server(args)?;
    let path = get_str(args, "path")?;
    validate_remote_path(&path, "path")?;
    let lines = parse_lines_arg(args)?;
    let (text, total_size, _) = fetch_text_for_line_op(ctx, &server, &path).await?;
    let total_lines = text.lines().count();
    let collected: Vec<&str> = text.lines().take(lines).collect();
    let returned = collected.len();
    let body = collected.join("\n");
    Ok(json!({
        "server": server,
        "path": path,
        "lines_requested": lines,
        "lines_returned": returned,
        "total_lines": total_lines,
        "total_size": total_size,
        "truncated": returned < total_lines,
        "content": body,
    }))
}

async fn tail_file(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let server = normalize_server(args)?;
    let path = get_str(args, "path")?;
    validate_remote_path(&path, "path")?;
    let lines = parse_lines_arg(args)?;
    let (text, total_size, _) = fetch_text_for_line_op(ctx, &server, &path).await?;
    let all_lines: Vec<&str> = text.lines().collect();
    let total_lines = all_lines.len();
    let start = total_lines.saturating_sub(lines);
    let slice = &all_lines[start..];
    let returned = slice.len();
    let body = slice.join("\n");
    Ok(json!({
        "server": server,
        "path": path,
        "lines_requested": lines,
        "lines_returned": returned,
        "total_lines": total_lines,
        "total_size": total_size,
        "truncated": start > 0,
        "content": body,
    }))
}

/// Hard cap per `aeroftp_tree`. Combinato con `max_depth`, protegge il
/// processo dall'esplorazione di alberi enormi (proxy di `find` ricorsivo).
const TREE_DEFAULT_MAX_DEPTH: u64 = 3;
const TREE_DEFAULT_MAX_ENTRIES: usize = 500;
const TREE_HARD_MAX_ENTRIES: usize = 5_000;
const TREE_HARD_MAX_DEPTH: u64 = 20;

async fn tree(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let server = normalize_server(args)?;
    let root = normalize_path_arg(args, "path", "/");
    validate_remote_path(&root, "path")?;
    let max_depth = args
        .get("max_depth")
        .and_then(|v| v.as_u64())
        .map(|n| n.min(TREE_HARD_MAX_DEPTH))
        .unwrap_or(TREE_DEFAULT_MAX_DEPTH);
    let max_entries = args
        .get("max_entries")
        .and_then(|v| v.as_u64())
        .map(|n| (n as usize).min(TREE_HARD_MAX_ENTRIES))
        .filter(|n| *n > 0)
        .unwrap_or(TREE_DEFAULT_MAX_ENTRIES);
    let files_only = get_bool_opt(args, "files_only").unwrap_or(false);
    let dirs_only = get_bool_opt(args, "dirs_only").unwrap_or(false);

    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;
    // BFS bounded by both max_depth and max_entries. We use BFS over a
    // queue rather than recursion so the cap is enforced uniformly across
    // wide trees (deep narrow vs shallow wide). Each queue item is
    // (path, depth). The output is flat: agent reconstructs hierarchy
    // from `path` if needed (cheaper than nested JSON for large trees).
    let mut queue: std::collections::VecDeque<(String, u64)> = std::collections::VecDeque::new();
    queue.push_back((root.clone(), 0));
    let mut entries: Vec<Value> = Vec::new();
    let mut total_visited: usize = 0;
    let mut total_dirs: usize = 0;
    let mut total_files: usize = 0;
    let mut truncated = false;
    let mut errors: Vec<Value> = Vec::new();

    while let Some((dir, depth)) = queue.pop_front() {
        if depth > max_depth {
            continue;
        }
        let listing = match backend.list(&dir).await {
            Ok(v) => v,
            Err(e) => {
                errors.push(json!({"path": dir, "error": e}));
                continue;
            }
        };
        for entry in listing {
            total_visited += 1;
            if entries.len() >= max_entries {
                truncated = true;
                break;
            }
            if entry.is_dir {
                total_dirs += 1;
            } else {
                total_files += 1;
            }
            let drop_for_files_only = files_only && entry.is_dir;
            let drop_for_dirs_only = dirs_only && !entry.is_dir;
            let keep = !drop_for_files_only && !drop_for_dirs_only;
            if keep {
                entries.push(json!({
                    "name": entry.name,
                    "path": entry.path,
                    "is_dir": entry.is_dir,
                    "size": entry.size,
                    "modified": entry.modified,
                    "depth": depth + 1,
                }));
            }
            if entry.is_dir && depth + 1 < max_depth + 1 {
                queue.push_back((entry.path, depth + 1));
            }
        }
        if truncated {
            break;
        }
    }

    Ok(json!({
        "server": server,
        "root": root,
        "max_depth": max_depth,
        "max_entries": max_entries,
        "entries": entries,
        "count": entries.len(),
        "total_visited": total_visited,
        "total_dirs": total_dirs,
        "total_files": total_files,
        "truncated": truncated,
        "errors": errors,
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

    // `remote_backend()` opens (or reuses) the pooled connection: its
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
        "get" | "put" | "mkdir" | "rm" | "mv" => {
            return Err(ToolError::InvalidArgs {
                tool: "server_exec".to_string(),
                reason: format!(
                    "Operation '{operation}' is mutative; use the dedicated AeroFTP tool with explicit approval"
                ),
            });
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

// ── Cross-profile transfer (Wave 5 / Gap 10) ──────────────────────────────
//
// Two MCP tools (`aeroftp_transfer`, `aeroftp_transfer_tree`) bridge the
// existing CLI/GUI cross-profile engine into the agent surface. Plumbing is
// "Option A" from the Gap 10 handoff: we go straight through `create_temp_provider`
// instead of the MCP pool so the agent gets the same code path as the GUI's
// cross-profile panel: including the SFTP key-based delta upload that
// `copy_one_file` decides on automatically.
//
// Scope guarantees:
// - same src/dst profile is rejected (id and name match)
// - `..`, null bytes, leading '-' and >4096 char paths are rejected
// - `transfer_tree` opens src+dst connections ONCE and reuses them across the
//   whole batch (cap: 1000 default / 10000 hard) so 1000 files = 2 connections
// - secrets never leave Rust: agent passes server names/IDs, vault resolution
//   happens inside `create_temp_provider`
// - audit log via `tracing::info!` per transfer (server IDs, paths, duration,
//   bytes: never credentials)

/// Soft cap on planned files for `aeroftp_transfer_tree` when the agent does
/// not specify `max_files`. Hard cap is `MAX_TRANSFER_TREE_FILES`.
const DEFAULT_TRANSFER_TREE_FILES: u64 = 1_000;
/// Hard cap above which we refuse the plan even if the agent requested it.
/// Forces the agent to narrow the path or break the work into batches.
const MAX_TRANSFER_TREE_FILES: u64 = 10_000;

/// Validate a remote path used as source/destination in a cross-profile
/// transfer. Stricter than [`validate_remote_path`] (which only catches
/// null bytes) because the same path is interpreted by TWO providers and a
/// component of `..` could escape the destination root on FTP servers
/// that root the user at the home directory.
fn validate_transfer_path(path: &str, label: &str) -> Result<(), ToolError> {
    if path.is_empty() {
        return Err(ToolError::InvalidArgs {
            tool: "aeroftp_transfer".to_string(),
            reason: format!("{label} must not be empty"),
        });
    }
    if path.len() > 4096 {
        return Err(ToolError::InvalidArgs {
            tool: "aeroftp_transfer".to_string(),
            reason: format!("{label} exceeds 4096 characters"),
        });
    }
    if path.contains('\0') {
        return Err(ToolError::InvalidArgs {
            tool: "aeroftp_transfer".to_string(),
            reason: format!("{label} contains null bytes"),
        });
    }
    if path.starts_with('-') {
        return Err(ToolError::InvalidArgs {
            tool: "aeroftp_transfer".to_string(),
            reason: format!("{label} must not start with '-' (argument injection risk)"),
        });
    }
    let normalized = path.replace('\\', "/");
    for component in normalized.split('/') {
        if component == ".." {
            return Err(ToolError::InvalidArgs {
                tool: "aeroftp_transfer".to_string(),
                reason: format!("{label} must not contain '..' path traversal"),
            });
        }
    }
    Ok(())
}

/// Resolve a fuzzy server query to a concrete `SavedServerInfo`. Same logic as
/// [`crate::ai_tools::find_server_by_name_or_id`] but returns
/// [`ToolError::Exec`] so the dispatcher can surface a structured error.
fn resolve_profile(
    profiles: &[crate::ai_tools::SavedServerInfo],
    query: &str,
) -> Result<crate::ai_tools::SavedServerInfo, ToolError> {
    crate::ai_tools::find_server_by_name_or_id(profiles, query).map_err(ToolError::Exec)
}

/// Reject identical source/destination profiles by id (preferred) or by
/// canonical name. Same invariant enforced by `cross_profile_plan`.
fn ensure_distinct_profiles(
    src: &crate::ai_tools::SavedServerInfo,
    dst: &crate::ai_tools::SavedServerInfo,
) -> Result<(), ToolError> {
    if src.id == dst.id || src.name.eq_ignore_ascii_case(&dst.name) {
        return Err(ToolError::InvalidArgs {
            tool: "aeroftp_transfer".to_string(),
            reason: "Source and destination must be different saved profiles".to_string(),
        });
    }
    Ok(())
}

async fn transfer_one(_ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let src_server_query = get_str(args, "src_server")?;
    let dst_server_query = get_str(args, "dst_server")?;
    let src_path = get_str(args, "src_path")?;
    let dst_path = get_str(args, "dst_path")?;
    validate_transfer_path(&src_path, "src_path")?;
    validate_transfer_path(&dst_path, "dst_path")?;
    let skip_existing = get_bool_opt(args, "skip_existing").unwrap_or(false);
    let dry_run = get_bool_opt(args, "dry_run").unwrap_or(false);

    let profiles = crate::ai_tools::load_saved_servers().map_err(ToolError::Exec)?;
    let src_server = resolve_profile(&profiles, &src_server_query)?;
    let dst_server = resolve_profile(&profiles, &dst_server_query)?;
    ensure_distinct_profiles(&src_server, &dst_server)?;

    let started = std::time::Instant::now();

    // Connect to source first to stat the file. The dest connection is opened
    // only when actually needed (skip_existing check or transfer).
    let src_box = crate::ai_tools::create_temp_provider(&src_server)
        .await
        .map_err(ToolError::Exec)?;
    let mut src_provider = src_box;
    let src_stat = src_provider
        .stat(&src_path)
        .await
        .map_err(|e| ToolError::Exec(format!("source stat failed: {e}")))?;
    if src_stat.is_dir {
        let _ = src_provider.disconnect().await;
        return Err(ToolError::InvalidArgs {
            tool: "aeroftp_transfer".to_string(),
            reason: format!(
                "src_path '{src_path}' is a directory; use aeroftp_transfer_tree instead"
            ),
        });
    }

    let dst_box = crate::ai_tools::create_temp_provider(&dst_server)
        .await
        .map_err(|e| ToolError::Exec(format!("destination connect failed: {e}")))?;
    let mut dst_provider = dst_box;

    let entry = crate::cross_profile_transfer::CrossProfileTransferEntry {
        source_path: src_path.clone(),
        dest_path: dst_path.clone(),
        display_name: src_path.rsplit('/').next().unwrap_or(&src_path).to_string(),
        size: src_stat.size,
        modified: src_stat.modified.clone(),
        is_dir: false,
    };

    if dry_run {
        let _ = src_provider.disconnect().await;
        let _ = dst_provider.disconnect().await;
        let elapsed = started.elapsed().as_millis() as u64;
        return Ok(json!({
            "src_server": src_server.name,
            "src_path": src_path,
            "dst_server": dst_server.name,
            "dst_path": dst_path,
            "transferred": false,
            "skipped": false,
            "dry_run": true,
            "size": src_stat.size,
            "duration_ms": elapsed,
        }));
    }

    if skip_existing {
        match crate::cross_profile_transfer::should_skip_existing(
            dst_provider.as_mut(),
            &dst_path,
            &entry,
        )
        .await
        {
            Ok(true) => {
                let _ = src_provider.disconnect().await;
                let _ = dst_provider.disconnect().await;
                let elapsed = started.elapsed().as_millis() as u64;
                return Ok(json!({
                    "src_server": src_server.name,
                    "src_path": src_path,
                    "dst_server": dst_server.name,
                    "dst_path": dst_path,
                    "transferred": false,
                    "skipped": true,
                    "bytes": 0,
                    "duration_ms": elapsed,
                }));
            }
            Ok(false) => {}
            Err(e) => {
                tracing::debug!("skip_existing probe failed (continuing): {e}");
            }
        }
    }

    let copy_result = crate::cross_profile_transfer::copy_one_file(
        src_provider.as_mut(),
        dst_provider.as_mut(),
        &src_path,
        &dst_path,
        src_stat.modified.as_deref(),
    )
    .await;

    let elapsed = started.elapsed().as_millis() as u64;
    let _ = src_provider.disconnect().await;
    let _ = dst_provider.disconnect().await;

    match copy_result {
        Ok(()) => {
            tracing::info!(
                target: "aeroftp::mcp::transfer",
                src_id = %src_server.id,
                dst_id = %dst_server.id,
                src_path = %src_path,
                dst_path = %dst_path,
                bytes = src_stat.size,
                duration_ms = elapsed,
                "cross-profile single-file transfer ok"
            );
            Ok(json!({
                "src_server": src_server.name,
                "src_path": src_path,
                "dst_server": dst_server.name,
                "dst_path": dst_path,
                "transferred": true,
                "skipped": false,
                "bytes": src_stat.size,
                "duration_ms": elapsed,
            }))
        }
        Err(e) => Err(ToolError::Exec(format!("transfer failed: {e}"))),
    }
}

async fn transfer_tree(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let src_server_query = get_str(args, "src_server")?;
    let dst_server_query = get_str(args, "dst_server")?;
    let src_path = get_str(args, "src_path")?;
    let dst_path = get_str(args, "dst_path")?;
    validate_transfer_path(&src_path, "src_path")?;
    validate_transfer_path(&dst_path, "dst_path")?;
    let skip_existing = get_bool_opt(args, "skip_existing").unwrap_or(false);
    let dry_run = get_bool_opt(args, "dry_run").unwrap_or(false);
    let summary_only = get_bool_opt(args, "summary_only").unwrap_or(false);
    let max_files = args
        .get("max_files")
        .and_then(|v| v.as_u64())
        .map(|n| n.min(MAX_TRANSFER_TREE_FILES))
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_TRANSFER_TREE_FILES);

    let profiles = crate::ai_tools::load_saved_servers().map_err(ToolError::Exec)?;
    let src_server = resolve_profile(&profiles, &src_server_query)?;
    let dst_server = resolve_profile(&profiles, &dst_server_query)?;
    ensure_distinct_profiles(&src_server, &dst_server)?;

    let started = std::time::Instant::now();

    // Open BOTH connections once and reuse them across the whole batch: the
    // cap on `max_files` keeps temp-file fan-out bounded while sticking to
    // 2 underlying TCP/SSH sessions (Option A in the Gap 10 handoff).
    let mut src_provider = crate::ai_tools::create_temp_provider(&src_server)
        .await
        .map_err(ToolError::Exec)?;
    let mut dst_provider = crate::ai_tools::create_temp_provider(&dst_server)
        .await
        .map_err(|e| ToolError::Exec(format!("destination connect failed: {e}")))?;

    let request = crate::cross_profile_transfer::CrossProfileTransferRequest {
        source_profile: src_server.name.clone(),
        dest_profile: dst_server.name.clone(),
        source_path: src_path.clone(),
        dest_path: dst_path.clone(),
        recursive: true,
        dry_run,
        skip_existing,
    };
    let plan = crate::cross_profile_transfer::plan_transfer(
        src_provider.as_mut(),
        dst_provider.as_mut(),
        &request,
    )
    .await
    .map_err(|e| ToolError::Exec(format!("planning failed: {e}")));
    let plan = match plan {
        Ok(p) => p,
        Err(e) => {
            let _ = src_provider.disconnect().await;
            let _ = dst_provider.disconnect().await;
            return Err(e);
        }
    };

    if plan.total_files > max_files {
        let _ = src_provider.disconnect().await;
        let _ = dst_provider.disconnect().await;
        return Err(ToolError::InvalidArgs {
            tool: "aeroftp_transfer_tree".to_string(),
            reason: format!(
                "Plan would transfer {} files but max_files cap is {}. Narrow the source path or split the operation.",
                plan.total_files, max_files
            ),
        });
    }

    if dry_run {
        let _ = src_provider.disconnect().await;
        let _ = dst_provider.disconnect().await;
        let elapsed = started.elapsed().as_millis() as u64;
        if summary_only {
            return Ok(json!({
                "src_server": src_server.name,
                "src_path": src_path,
                "dst_server": dst_server.name,
                "dst_path": dst_path,
                "dry_run": true,
                "total_files": plan.total_files,
                "total_bytes": plan.total_bytes,
                "max_files": max_files,
                "truncated": false,
                "duration_ms": elapsed,
            }));
        }
        let entries: Vec<Value> = plan
            .entries
            .iter()
            .map(|e| {
                json!({
                    "source_path": e.source_path,
                    "dest_path": e.dest_path,
                    "size": e.size,
                    "modified": e.modified,
                    "is_dir": e.is_dir,
                })
            })
            .collect();
        return Ok(json!({
            "src_server": src_server.name,
            "src_path": src_path,
            "dst_server": dst_server.name,
            "dst_path": dst_path,
            "dry_run": true,
            "plan": entries,
            "total_files": plan.total_files,
            "total_bytes": plan.total_bytes,
            "max_files": max_files,
            "truncated": false,
            "duration_ms": elapsed,
        }));
    }

    // ── Execute ──
    let total_planned = plan.total_files;
    let mut transferred_files: u64 = 0;
    let mut skipped_files: u64 = 0;
    let mut failed_files: u64 = 0;
    let mut total_bytes_transferred: u64 = 0;
    let mut errors: Vec<Value> = Vec::new();

    // Throttle progress events (emit every 5 files OR ~2% delta) to mirror
    // the F6 fix in v2.1.2 download path. `progress_step` is at least 1.
    let progress_step = std::cmp::max((total_planned / 50).max(1), 5);

    for (idx, entry) in plan.entries.iter().enumerate() {
        if skip_existing {
            match crate::cross_profile_transfer::should_skip_existing(
                dst_provider.as_mut(),
                &entry.dest_path,
                entry,
            )
            .await
            {
                Ok(true) => {
                    skipped_files += 1;
                    if (idx as u64) % progress_step == 0 {
                        ctx.event_sink()
                            .emit_tool_progress(&crate::ai_core::ToolProgress {
                                tool: "aeroftp_transfer_tree".to_string(),
                                current: (transferred_files + skipped_files) as u32,
                                total: total_planned as u32,
                                item: entry.source_path.clone(),
                            });
                    }
                    continue;
                }
                Ok(false) => {}
                Err(e) => {
                    tracing::debug!("skip_existing probe failed (continuing): {e}");
                }
            }
        }

        match crate::cross_profile_transfer::copy_one_file(
            src_provider.as_mut(),
            dst_provider.as_mut(),
            &entry.source_path,
            &entry.dest_path,
            entry.modified.as_deref(),
        )
        .await
        {
            Ok(()) => {
                transferred_files += 1;
                total_bytes_transferred += entry.size;
            }
            Err(e) => {
                failed_files += 1;
                errors.push(json!({
                    "source_path": entry.source_path,
                    "error": e.to_string(),
                }));
            }
        }

        if (idx as u64) % progress_step == 0 || (idx as u64) + 1 == total_planned {
            ctx.event_sink()
                .emit_tool_progress(&crate::ai_core::ToolProgress {
                    tool: "aeroftp_transfer_tree".to_string(),
                    current: (transferred_files + skipped_files + failed_files) as u32,
                    total: total_planned as u32,
                    item: entry.source_path.clone(),
                });
        }
    }

    let duration_ms = started.elapsed().as_millis() as u64;
    let _ = src_provider.disconnect().await;
    let _ = dst_provider.disconnect().await;

    tracing::info!(
        target: "aeroftp::mcp::transfer",
        src_id = %src_server.id,
        dst_id = %dst_server.id,
        src_path = %src_path,
        dst_path = %dst_path,
        planned = total_planned,
        transferred = transferred_files,
        skipped = skipped_files,
        failed = failed_files,
        bytes = total_bytes_transferred,
        duration_ms,
        "cross-profile tree transfer complete"
    );

    Ok(json!({
        "src_server": src_server.name,
        "src_path": src_path,
        "dst_server": dst_server.name,
        "dst_path": dst_path,
        "summary": {
            "planned_files": total_planned,
            "transferred_files": transferred_files,
            "skipped_files": skipped_files,
            "failed_files": failed_files,
            "total_bytes": total_bytes_transferred,
            "duration_ms": duration_ms,
        },
        "errors": errors,
        "max_files": max_files,
    }))
}

// ── Wave 6 / Gap 5 closure: touch / cleanup / speed / sync_doctor / dedupe / reconcile ──
//
// All six wrap the existing CLI semantics over the MCP `RemoteBackend` trait.
// They never touch sync_core::scan_remote_tree (which would require
// `&mut Box<dyn StorageProvider>`); instead they do BFS over `backend.list()`,
// keeping the abstraction clean. The reconcile tool surfaces a "light"
// list-based diff (size + mtime) and delegates checksum-aware compares to
// `aeroftp_check_tree` (mcp/tools.rs match arm): the schema documents this
// trade-off.

/// Hard caps shared by cleanup / dedupe / sync_doctor / reconcile.
const MAX_BFS_ENTRIES: usize = 100_000;
const MAX_BFS_DEPTH: usize = 100;

/// Hard cap per file in `aeroftp_dedupe`. Same memory invariant as `hashsum`.
const MAX_DEDUPE_FILE_BYTES: u64 = 256 * 1024 * 1024;

/// Caps for `aeroftp_speed`. Tighter than CLI to keep the agent process
/// responsive (CLI can run minute-long benchmarks; agent should not).
const SPEED_DEFAULT_SIZE_MB: u64 = 4;
const SPEED_MAX_SIZE_MB: u64 = 64;
const SPEED_DEFAULT_ITERATIONS: u32 = 1;
const SPEED_MAX_ITERATIONS: u32 = 3;

async fn touch(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let server = normalize_server(args)?;
    let path = get_str(args, "path")?;
    validate_remote_path(&path, "path")?;
    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;
    // First check if the file exists. If yes, this is a no-op: providers
    // generally lack a portable utime API and a re-upload would surprise the
    // agent (mtime change without explicit intent).
    if backend.stat(&path).await.is_ok() {
        return Ok(json!({
            "server": server,
            "path": path,
            "action": "exists",
            "created": false,
        }));
    }
    backend
        .upload_from_bytes(b"", &path)
        .await
        .map_err(ToolError::Exec)?;
    Ok(json!({
        "server": server,
        "path": path,
        "action": "created",
        "created": true,
    }))
}

async fn cleanup(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let server = normalize_server(args)?;
    let root = normalize_path_arg(args, "path", "/");
    validate_remote_path(&root, "path")?;
    let dry_run = get_bool_opt(args, "dry_run").unwrap_or(true);
    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;

    // BFS scan for `.aerotmp` entries. Mirrors `cmd_cleanup` in CLI.
    let mut orphans: Vec<(String, u64)> = Vec::new();
    let mut dirs: Vec<(String, usize)> = vec![(root.clone(), 0)];
    let mut scan_errors: u32 = 0;

    while let Some((dir, depth)) = dirs.pop() {
        if depth >= MAX_BFS_DEPTH {
            continue;
        }
        if orphans.len() >= MAX_BFS_ENTRIES {
            break;
        }
        match backend.list(&dir).await {
            Ok(entries) => {
                for entry in entries {
                    if entry.is_dir {
                        dirs.push((entry.path.clone(), depth + 1));
                    } else if entry.name.ends_with(".aerotmp") || entry.path.ends_with(".aerotmp") {
                        orphans.push((entry.path.clone(), entry.size));
                    }
                }
            }
            Err(e) => {
                scan_errors += 1;
                tracing::debug!("cleanup: failed to list {}: {}", dir, e);
            }
        }
    }

    let total_bytes: u64 = orphans.iter().map(|(_, s)| *s).sum();

    if dry_run || orphans.is_empty() {
        let files: Vec<Value> = orphans
            .iter()
            .map(|(p, s)| json!({"path": p, "size": s}))
            .collect();
        return Ok(json!({
            "server": server,
            "path": root,
            "dry_run": dry_run,
            "orphans": orphans.len(),
            "bytes": total_bytes,
            "scan_errors": scan_errors,
            "delete_errors": 0,
            "cleaned": 0,
            "bytes_freed": 0,
            "files": files,
        }));
    }

    let mut cleaned: u32 = 0;
    let mut bytes_freed: u64 = 0;
    let mut delete_errors: u32 = 0;
    let mut errors: Vec<Value> = Vec::new();
    for (p, s) in &orphans {
        match backend.delete(p).await {
            Ok(()) => {
                cleaned += 1;
                bytes_freed += *s;
            }
            Err(e) => {
                delete_errors += 1;
                errors.push(json!({"path": p, "error": e}));
            }
        }
    }

    Ok(json!({
        "server": server,
        "path": root,
        "dry_run": false,
        "orphans": orphans.len(),
        "bytes": total_bytes,
        "scan_errors": scan_errors,
        "delete_errors": delete_errors,
        "cleaned": cleaned,
        "bytes_freed": bytes_freed,
        "errors": errors,
    }))
}

async fn speed(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    use sha2::Digest;

    let server = normalize_server(args)?;
    let size_mb = args
        .get("size_mb")
        .and_then(|v| v.as_u64())
        .map(|n| n.min(SPEED_MAX_SIZE_MB))
        .filter(|n| *n > 0)
        .unwrap_or(SPEED_DEFAULT_SIZE_MB);
    let iterations = args
        .get("iterations")
        .and_then(|v| v.as_u64())
        .map(|n| (n as u32).clamp(1, SPEED_MAX_ITERATIONS))
        .unwrap_or(SPEED_DEFAULT_ITERATIONS);
    let verify_integrity = get_bool_opt(args, "verify_integrity").unwrap_or(true);
    let remote_path = get_str_opt(args, "remote_path")
        .unwrap_or_else(|| format!("/.aeroftp-speedtest-{}.bin", uuid::Uuid::new_v4()));
    validate_remote_path(&remote_path, "remote_path")?;

    let size_bytes = size_mb * 1024 * 1024;

    // Allocate the random payload once; reuse across iterations.
    let payload: Vec<u8> = (0..size_bytes)
        .map(|i| ((i ^ 0x9e37_79b9) & 0xff) as u8)
        .collect();
    let upload_sha = format!("{:x}", sha2::Sha256::digest(&payload));

    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;
    let started = std::time::Instant::now();
    let mut upload_total_bps: f64 = 0.0;
    let mut download_total_bps: f64 = 0.0;
    let mut integrity_verified = false;
    let mut last_download_sha = String::new();

    for _ in 0..iterations {
        let up_start = std::time::Instant::now();
        backend
            .upload_from_bytes(&payload, &remote_path)
            .await
            .map_err(|e| ToolError::Exec(format!("upload failed: {e}")))?;
        let up_secs = up_start.elapsed().as_secs_f64().max(0.0001);
        upload_total_bps += size_bytes as f64 / up_secs;

        let down_start = std::time::Instant::now();
        let downloaded = backend
            .download_to_bytes(&remote_path)
            .await
            .map_err(|e| ToolError::Exec(format!("download failed: {e}")))?;
        let down_secs = down_start.elapsed().as_secs_f64().max(0.0001);
        download_total_bps += downloaded.len() as f64 / down_secs;

        if verify_integrity {
            last_download_sha = format!("{:x}", sha2::Sha256::digest(&downloaded));
        }
    }

    let cleanup_ok = backend.delete(&remote_path).await.is_ok();
    if verify_integrity {
        integrity_verified = !last_download_sha.is_empty() && last_download_sha == upload_sha;
    }

    let upload_bps = upload_total_bps / iterations as f64;
    let download_bps = download_total_bps / iterations as f64;
    Ok(json!({
        "server": server,
        "remote_path": remote_path,
        "test_size": size_bytes,
        "iterations": iterations,
        "upload_bps": upload_bps as u64,
        "download_bps": download_bps as u64,
        "upload_mbps": (upload_bps * 8.0 / 1_000_000.0),
        "download_mbps": (download_bps * 8.0 / 1_000_000.0),
        "integrity_checked": verify_integrity,
        "integrity_verified": integrity_verified,
        "cleanup_ok": cleanup_ok,
        "elapsed_ms": started.elapsed().as_millis() as u64,
    }))
}

async fn sync_doctor(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let server = normalize_server(args)?;
    let local_dir = get_str(args, "local_dir")?;
    let remote_dir = get_str(args, "remote_dir")?;
    validate_remote_path(&remote_dir, "remote_dir")?;
    let direction = get_str_opt(args, "direction").unwrap_or_else(|| "both".to_string());
    let delete = get_bool_opt(args, "delete").unwrap_or(false);
    let track_renames = get_bool_opt(args, "track_renames").unwrap_or(false);
    let checksum = get_bool_opt(args, "checksum").unwrap_or(false);
    let exclude: Vec<String> = args
        .get("exclude")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let local_path = std::path::Path::new(&local_dir);
    if !local_path.is_dir() {
        return Err(ToolError::InvalidArgs {
            tool: "aeroftp_sync_doctor".to_string(),
            reason: format!("local_dir is not a directory: {local_dir}"),
        });
    }

    // Compile glob matchers (best-effort: invalid patterns are skipped silently
    // so a single typo doesn't kill the whole call).
    let exclude_matchers: Vec<globset::GlobMatcher> = exclude
        .iter()
        .filter_map(|p| globset::Glob::new(p).ok().map(|g| g.compile_matcher()))
        .collect();

    // Local scan (walkdir).
    let mut local_files: usize = 0;
    let mut local_bytes: u64 = 0;
    for entry in walkdir::WalkDir::new(local_path)
        .follow_links(false)
        .max_depth(MAX_BFS_DEPTH)
    {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(local_path)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .replace('\\', "/");
        let fname = entry.file_name().to_string_lossy().to_string();
        if exclude_matchers
            .iter()
            .any(|m| m.is_match(&relative) || m.is_match(&fname))
        {
            continue;
        }
        local_files += 1;
        local_bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
    }

    // Remote scan (BFS).
    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;
    let remote_root_ok = backend.list(&remote_dir).await.is_ok();
    let mut remote_files: usize = 0;
    let mut remote_bytes: u64 = 0;
    if remote_root_ok {
        let mut queue: Vec<(String, usize)> = vec![(remote_dir.clone(), 0)];
        while let Some((dir, depth)) = queue.pop() {
            if depth >= MAX_BFS_DEPTH || remote_files >= MAX_BFS_ENTRIES {
                break;
            }
            if let Ok(entries) = backend.list(&dir).await {
                for e in entries {
                    if e.is_dir {
                        queue.push((e.path.clone(), depth + 1));
                    } else {
                        let relative = e
                            .path
                            .strip_prefix(&remote_dir)
                            .unwrap_or(&e.path)
                            .trim_start_matches('/')
                            .to_string();
                        if exclude_matchers
                            .iter()
                            .any(|m| m.is_match(&relative) || m.is_match(&e.name))
                        {
                            continue;
                        }
                        remote_files += 1;
                        remote_bytes += e.size;
                    }
                }
            }
        }
    }

    let mut checks = vec![
        json!({"name": "local_path_exists", "ok": true, "path": local_dir}),
        json!({"name": "remote_path_reachable", "ok": remote_root_ok, "path": remote_dir}),
    ];
    if !exclude.is_empty() {
        checks.push(json!({"name": "exclude_patterns", "ok": true, "count": exclude.len()}));
    }

    let mut risks: Vec<String> = Vec::new();
    if delete {
        risks.push("delete is enabled; sync may remove orphaned files".to_string());
    }
    if direction == "both" && !track_renames {
        risks.push("track-renames is disabled; moved files may be recopied".to_string());
    }
    if checksum {
        risks.push("checksum is enabled; verification will be slower but stricter".to_string());
    }
    if !remote_root_ok {
        risks.push("remote path could not be listed".to_string());
    }
    if local_files == 0 && remote_files == 0 {
        risks.push("both sides are empty; sync will be a no-op".to_string());
    }

    let suggested_next_command = format!(
        "aeroftp-cli sync \"{}\" \"{}\" --direction {} --dry-run --json{}{}{}",
        local_dir.replace('"', "\\\""),
        remote_dir.replace('"', "\\\""),
        direction,
        if delete { " --delete" } else { "" },
        if track_renames {
            " --track-renames"
        } else {
            ""
        },
        if checksum { " --checksum" } else { "" },
    );

    Ok(json!({
        "server": server,
        "status": if remote_root_ok { "ok" } else { "attention" },
        "summary": {
            "direction": direction,
            "local_files": local_files,
            "local_bytes": local_bytes,
            "remote_files": remote_files,
            "remote_bytes": remote_bytes,
            "delete": delete,
            "track_renames": track_renames,
            "checksum": checksum,
        },
        "checks": checks,
        "risks": risks,
        "suggested_next_command": suggested_next_command,
    }))
}

async fn dedupe(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    use sha2::Digest;

    let server = normalize_server(args)?;
    let root = normalize_path_arg(args, "path", "/");
    validate_remote_path(&root, "path")?;
    let mode = get_str_opt(args, "mode").unwrap_or_else(|| "list".to_string());
    if !matches!(
        mode.as_str(),
        "newest" | "oldest" | "largest" | "smallest" | "list"
    ) {
        return Err(ToolError::InvalidArgs {
            tool: "aeroftp_dedupe".to_string(),
            reason: format!(
                "unsupported mode '{mode}'. Use one of: newest, oldest, largest, smallest, list"
            ),
        });
    }
    let dry_run = get_bool_opt(args, "dry_run").unwrap_or(true);
    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;

    // BFS scan to collect (path, size, mtime).
    let mut files: Vec<(String, u64, Option<String>)> = Vec::new();
    let mut dirs: Vec<(String, usize)> = vec![(root.clone(), 0)];
    let mut scan_errors: u32 = 0;
    while let Some((dir, depth)) = dirs.pop() {
        if depth >= MAX_BFS_DEPTH || files.len() >= MAX_BFS_ENTRIES {
            continue;
        }
        match backend.list(&dir).await {
            Ok(entries) => {
                for e in entries {
                    if e.is_dir {
                        dirs.push((e.path, depth + 1));
                    } else {
                        files.push((e.path, e.size, e.modified));
                    }
                }
            }
            Err(e) => {
                scan_errors += 1;
                tracing::debug!("dedupe: failed to list {}: {}", dir, e);
            }
        }
    }

    // Group by size.
    let mut size_groups: std::collections::HashMap<u64, Vec<(String, Option<String>)>> =
        std::collections::HashMap::new();
    for (p, s, m) in &files {
        if *s > 0 {
            size_groups
                .entry(*s)
                .or_default()
                .push((p.clone(), m.clone()));
        }
    }
    type DedupeCandidate = (String, Option<String>);
    type DedupeSizeGroup = (u64, Vec<DedupeCandidate>);
    let candidate_groups: Vec<DedupeSizeGroup> = size_groups
        .into_iter()
        .filter(|(_, v)| v.len() > 1)
        .collect();

    let mut hash_errors: u32 = 0;
    let mut duplicate_groups: Vec<Vec<(String, u64, Option<String>)>> = Vec::new();
    let mut total_duplicates: u32 = 0;
    let mut wasted_bytes: u64 = 0;

    for (size, candidates) in candidate_groups {
        if size > MAX_DEDUPE_FILE_BYTES {
            // Skip oversize files: would blow the agent memory budget.
            hash_errors += candidates.len() as u32;
            continue;
        }
        let mut hash_map: std::collections::HashMap<String, Vec<(String, u64, Option<String>)>> =
            std::collections::HashMap::new();
        for (p, m) in candidates {
            match backend.download_to_bytes(&p).await {
                Ok(data) => {
                    let hash = format!("{:x}", sha2::Sha256::digest(&data));
                    hash_map.entry(hash).or_default().push((p, size, m));
                }
                Err(e) => {
                    hash_errors += 1;
                    tracing::debug!("dedupe: failed to hash {}: {}", p, e);
                }
            }
        }
        for (_, group) in hash_map {
            if group.len() > 1 {
                let dupes = group.len() as u32 - 1;
                total_duplicates += dupes;
                wasted_bytes += size * dupes as u64;
                duplicate_groups.push(group);
            }
        }
    }

    // Sort each group to determine the keeper.
    for group in &mut duplicate_groups {
        match mode.as_str() {
            "newest" => {
                group.sort_by(|a, b| b.2.cmp(&a.2));
            }
            "oldest" => {
                group.sort_by(|a, b| a.2.cmp(&b.2));
            }
            "largest" => {
                group.sort_by_key(|item| std::cmp::Reverse(item.1));
            }
            "smallest" => {
                group.sort_by_key(|item| item.1);
            }
            _ => {}
        }
    }

    let groups_json: Vec<Value> = duplicate_groups
        .iter()
        .map(|group| {
            let entries: Vec<Value> = group
                .iter()
                .enumerate()
                .map(|(i, (p, s, m))| {
                    json!({
                        "path": p,
                        "size": s,
                        "modified": m,
                        "keeper": i == 0 && mode != "list",
                    })
                })
                .collect();
            json!({"entries": entries})
        })
        .collect();

    // Action phase.
    let mut deleted: u32 = 0;
    let mut bytes_freed: u64 = 0;
    let mut action_errors: u32 = 0;
    let mut errors: Vec<Value> = Vec::new();
    if !dry_run && mode != "list" {
        for group in &duplicate_groups {
            // Index 0 is the keeper after sorting; delete the rest.
            for (p, s, _) in group.iter().skip(1) {
                match backend.delete(p).await {
                    Ok(()) => {
                        deleted += 1;
                        bytes_freed += *s;
                    }
                    Err(e) => {
                        action_errors += 1;
                        errors.push(json!({"path": p, "error": e}));
                    }
                }
            }
        }
    }

    Ok(json!({
        "server": server,
        "path": root,
        "mode": mode,
        "dry_run": dry_run,
        "scanned": files.len(),
        "scan_errors": scan_errors,
        "hash_errors": hash_errors,
        "groups": groups_json,
        "duplicates_found": total_duplicates,
        "wasted_bytes": wasted_bytes,
        "deleted": deleted,
        "bytes_freed": bytes_freed,
        "action_errors": action_errors,
        "errors": errors,
    }))
}

async fn reconcile(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let server = normalize_server(args)?;
    let local_dir = get_str(args, "local_dir")?;
    let remote_dir = get_str(args, "remote_dir")?;
    validate_remote_path(&remote_dir, "remote_dir")?;
    let _checksum = get_bool_opt(args, "checksum").unwrap_or(false);
    let one_way = get_bool_opt(args, "one_way").unwrap_or(false);
    let summary_only = get_bool_opt(args, "summary_only").unwrap_or(false);
    let exclude: Vec<String> = args
        .get("exclude")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let local_path = std::path::Path::new(&local_dir);
    if !local_path.is_dir() {
        return Err(ToolError::InvalidArgs {
            tool: "aeroftp_reconcile".to_string(),
            reason: format!("local_dir is not a directory: {local_dir}"),
        });
    }

    let exclude_matchers: Vec<globset::GlobMatcher> = exclude
        .iter()
        .filter_map(|p| globset::Glob::new(p).ok().map(|g| g.compile_matcher()))
        .collect();

    let started = std::time::Instant::now();

    // Local scan: rel_path → (size, mtime_secs).
    let mut local_map: std::collections::HashMap<String, (u64, Option<i64>)> =
        std::collections::HashMap::new();
    for entry in walkdir::WalkDir::new(local_path)
        .follow_links(false)
        .max_depth(MAX_BFS_DEPTH)
    {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(local_path)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .replace('\\', "/");
        if rel.is_empty() {
            continue;
        }
        let fname = entry.file_name().to_string_lossy().to_string();
        if exclude_matchers
            .iter()
            .any(|m| m.is_match(&rel) || m.is_match(&fname))
        {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime_secs = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        local_map.insert(rel, (metadata.len(), mtime_secs));
    }

    // Remote scan: BFS via RemoteBackend.list.
    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;
    let mut remote_map: std::collections::HashMap<String, (u64, Option<String>)> =
        std::collections::HashMap::new();
    let mut queue: Vec<(String, usize)> = vec![(remote_dir.clone(), 0)];
    while let Some((dir, depth)) = queue.pop() {
        if depth >= MAX_BFS_DEPTH || remote_map.len() >= MAX_BFS_ENTRIES {
            break;
        }
        let entries = match backend.list(&dir).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        for e in entries {
            if e.is_dir {
                queue.push((e.path.clone(), depth + 1));
            } else {
                let rel = e
                    .path
                    .strip_prefix(&remote_dir)
                    .unwrap_or(&e.path)
                    .trim_start_matches('/')
                    .to_string();
                if rel.is_empty() {
                    continue;
                }
                if exclude_matchers
                    .iter()
                    .any(|m| m.is_match(&rel) || m.is_match(&e.name))
                {
                    continue;
                }
                remote_map.insert(rel, (e.size, e.modified));
            }
        }
    }

    // Compare.
    let mut matches_g: Vec<Value> = Vec::new();
    let mut differ_g: Vec<Value> = Vec::new();
    let mut missing_remote_g: Vec<Value> = Vec::new();
    let mut missing_local_g: Vec<Value> = Vec::new();

    for (rel, (lsize, _lmtime)) in &local_map {
        match remote_map.get(rel) {
            Some((rsize, _rmtime)) => {
                if lsize == rsize {
                    matches_g.push(json!({
                        "path": rel,
                        "local_size": lsize,
                        "remote_size": rsize,
                        "compare_method": "size",
                    }));
                } else {
                    differ_g.push(json!({
                        "path": rel,
                        "local_size": lsize,
                        "remote_size": rsize,
                        "compare_method": "size",
                    }));
                }
            }
            None => {
                missing_remote_g.push(json!({
                    "path": rel,
                    "local_size": lsize,
                }));
            }
        }
    }
    if !one_way {
        for (rel, (rsize, _)) in &remote_map {
            if !local_map.contains_key(rel) {
                missing_local_g.push(json!({
                    "path": rel,
                    "remote_size": rsize,
                }));
            }
        }
    }

    let elapsed = started.elapsed().as_secs_f64();
    let suggested_next_command = format!(
        "aeroftp-cli sync \"{}\" \"{}\" --dry-run --json",
        local_dir.replace('"', "\\\""),
        remote_dir.replace('"', "\\\""),
    );

    let status = if differ_g.is_empty() && missing_remote_g.is_empty() && missing_local_g.is_empty()
    {
        "ok"
    } else {
        "differences_found"
    };

    let summary = json!({
        "match_count": matches_g.len(),
        "differ_count": differ_g.len(),
        "missing_remote_count": missing_remote_g.len(),
        "missing_local_count": missing_local_g.len(),
        "elapsed_secs": elapsed,
    });

    if summary_only {
        return Ok(json!({
            "server": server,
            "status": status,
            "local_dir": local_dir,
            "remote_dir": remote_dir,
            "summary": summary,
            "summary_only": true,
            "suggested_next_command": suggested_next_command,
            "compare_method": "size",
        }));
    }

    Ok(json!({
        "server": server,
        "status": status,
        "local_dir": local_dir,
        "remote_dir": remote_dir,
        "summary": summary,
        "groups": {
            "match": matches_g,
            "differ": differ_g,
            "missing_remote": missing_remote_g,
            "missing_local": missing_local_g,
        },
        "compare_method": "size",
        "suggested_next_command": suggested_next_command,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(id: &str, name: &str) -> crate::ai_tools::SavedServerInfo {
        crate::ai_tools::SavedServerInfo {
            id: id.to_string(),
            name: name.to_string(),
            host: "host".to_string(),
            port: 22,
            username: "u".to_string(),
            protocol: "sftp".to_string(),
            initial_path: None,
            provider_id: None,
        }
    }

    #[test]
    fn validate_transfer_path_accepts_simple_paths() {
        assert!(validate_transfer_path("/data/file.txt", "src_path").is_ok());
        assert!(validate_transfer_path("relative/dir", "dst_path").is_ok());
    }

    #[test]
    fn validate_transfer_path_rejects_traversal() {
        let err = validate_transfer_path("/data/../etc/passwd", "src_path").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(".."), "got: {msg}");
    }

    #[test]
    fn validate_transfer_path_rejects_null_byte() {
        let err = validate_transfer_path("/data\0file", "src_path").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("null"), "got: {msg}");
    }

    #[test]
    fn validate_transfer_path_rejects_leading_dash() {
        let err = validate_transfer_path("-rf", "dst_path").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("'-'") || msg.contains("argument injection"),
            "got: {msg}"
        );
    }

    #[test]
    fn validate_transfer_path_rejects_empty() {
        assert!(validate_transfer_path("", "src_path").is_err());
    }

    #[test]
    fn validate_transfer_path_rejects_oversize() {
        let p = "/".to_string() + &"a".repeat(4096);
        assert!(validate_transfer_path(&p, "src_path").is_err());
    }

    #[test]
    fn resolve_profile_exact_id() {
        let profiles = vec![server("id-a", "Alpha"), server("id-b", "Beta")];
        let r = resolve_profile(&profiles, "id-b").unwrap();
        assert_eq!(r.id, "id-b");
    }

    #[test]
    fn resolve_profile_exact_name_case_insensitive() {
        let profiles = vec![server("id-a", "Alpha"), server("id-b", "Beta")];
        let r = resolve_profile(&profiles, "alpha").unwrap();
        assert_eq!(r.id, "id-a");
    }

    #[test]
    fn resolve_profile_substring_match() {
        let profiles = vec![server("id-a", "Alpha"), server("id-b", "Beta")];
        let r = resolve_profile(&profiles, "et").unwrap();
        assert_eq!(r.id, "id-b");
    }

    #[test]
    fn resolve_profile_no_match_errors() {
        let profiles = vec![server("id-a", "Alpha")];
        assert!(resolve_profile(&profiles, "Gamma").is_err());
    }

    #[test]
    fn resolve_profile_ambiguous_errors() {
        let profiles = vec![server("id-a", "Alpha"), server("id-b", "Alpine")];
        // "alp" matches both "Alpha" and "Alpine" via fuzzy contains.
        assert!(resolve_profile(&profiles, "alp").is_err());
    }

    #[test]
    fn ensure_distinct_profiles_rejects_same_id() {
        let p = server("id-a", "Alpha");
        assert!(ensure_distinct_profiles(&p, &p).is_err());
    }

    #[test]
    fn ensure_distinct_profiles_rejects_same_name_different_id() {
        let a = server("id-a", "Alpha");
        let b = server("id-b", "alpha");
        assert!(ensure_distinct_profiles(&a, &b).is_err());
    }

    #[test]
    fn ensure_distinct_profiles_accepts_different() {
        let a = server("id-a", "Alpha");
        let b = server("id-b", "Beta");
        assert!(ensure_distinct_profiles(&a, &b).is_ok());
    }
}
