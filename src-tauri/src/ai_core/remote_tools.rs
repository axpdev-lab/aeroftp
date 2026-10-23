//! Remote tool handlers for T3 Gate 2 Area C.
//!
//! These handlers use `ToolCtx::remote_backend()` so MCP can run through the
//! unified dispatcher while GUI/CLI keep their legacy fallback until their
//! per-surface remote backends are fully wired.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use serde_json::{json, Value};

use crate::ai_core::remote_backend::RemoteBackend;
use crate::ai_core::tools::{ToolCtx, ToolError};
use crate::dedupe::{self, SimilarityMode};
use crate::providers::bulk::{
    remote_parent, uncertain_items, BulkTransferItem, BulkTransferOutcome,
};
use crate::transfer_bulk::{
    bulk_item, group_key, plan_groups_by_key, settle_bulk_outcome, BulkSettlement,
};
use crate::transfer_domain::TransferEntry;

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

/// Restricted-character check on the leaf a write tool is about to create or
/// set. Runs after `validate_remote_path` and after the backend is resolved,
/// in the same order the GUI uses (`provider_rename`). The two checks do not
/// overlap: `validate_remote_path` judges the whole path string as an
/// argument and is provider-agnostic; this one judges the leaf as a name
/// against the target backend. If its type cannot be resolved, fail closed:
/// a later write could reconnect, so skipping this check is unsafe.
async fn reject_restricted_leaf(backend: &dyn RemoteBackend, path: &str) -> Result<(), ToolError> {
    let provider = backend.provider_type().await.ok_or_else(|| {
        ToolError::Exec("Cannot validate destination name: remote provider unavailable".to_string())
    })?;
    crate::restricted_chars::validate_path(provider, path)
        .map_err(|e| ToolError::Exec(e.to_string()))
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
    // W1-2: `server` is optional. On the GUI an empty value selects the active
    // connection (TauriToolCtx::remote_backend("")); on CLI/MCP an empty value
    // surfaces a clear "server not found" from the backend resolver, since those
    // surfaces always operate on a named vault profile.
    Ok(get_str_opt(args, "server").unwrap_or_default())
}

fn normalize_path_arg(args: &Value, primary: &str, default: &str) -> String {
    get_str_opt(args, primary).unwrap_or_else(|| default.to_string())
}

pub(crate) fn alias_name(tool_name: &str) -> &str {
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
        "remote_edit" => "aeroftp_edit",
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

/// What a listing lets us conclude about descending into a path.
///
/// There are two answers and the missing third is the whole point.
///
/// A listing can PROVE a path is a directory: `is_dir` comes from the leading
/// `d` in the permission string (ftp_listing.rs), and the MLSD path sets it for
/// `type=dir|cdir|pdir`. Nothing a listing says proves a path is NOT one. A
/// symlink pointing at a directory reports `is_dir == false` because its
/// permissions begin with `l`, and MLSD recognises only two spellings of a
/// symlink while RFC 3659 allows a third, so a symlink can arrive with both
/// flags false. `is_dir == false` therefore means "not shown to be a
/// directory", never "shown not to be one".
///
/// The rule this encodes: refuse only on positive evidence of the negative,
/// and treat "not proven a directory" as unknown rather than as refuted. It is
/// a type and not a comment because the same mistake was made twice in this
/// one function, once in the walk and once in the pre-check written after the
/// walk was fixed. Removing an instance leaves the reasoning that produced it.
/// There is no `NotADirectory` variant because a listing cannot hand one out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DescendEvidence {
    ProvenDirectory,
    Unknown,
}

fn descend_evidence(entry: &crate::providers::RemoteEntry) -> DescendEvidence {
    if entry.is_dir {
        DescendEvidence::ProvenDirectory
    } else {
        DescendEvidence::Unknown
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

    // A known parent directory needs no walk. Otherwise inspect each level
    // before creating it, validating only names that are not already present.
    // If stat is unsupported, the mkdir response still supplies the fallback
    // existence evidence below.
    if let Ok(entry) = backend.stat(&parent).await {
        if descend_evidence(&entry) == DescendEvidence::ProvenDirectory {
            return Ok(());
        }
        // Unknown. Not a reason to stop: fall through to the walk, which is
        // what happened before this check existed.
    }

    let mut acc = String::new();
    // The first level that exists but is not reported as a directory. It is
    // almost always the real obstacle, and the failure lands one level below.
    let mut blocked_at: Option<String> = None;
    let leading_slash = parent.starts_with('/');
    for part in parent.split('/').filter(|p| !p.is_empty()) {
        if leading_slash || !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(part);
        // Existing names remain usable, including symlinks whose stat does
        // not prove they are directories. Only a new component needs the
        // provider's name rules before mkdir can send it to the server.
        if let Ok(entry) = backend.stat(&acc).await {
            if descend_evidence(&entry) == DescendEvidence::Unknown && blocked_at.is_none() {
                blocked_at = Some(acc.clone());
            }
            continue;
        }
        reject_restricted_leaf(backend.as_ref(), &acc).await?;
        match backend.mkdir(&acc).await {
            Ok(()) => {}
            Err(e) => {
                // Some FTP servers (e.g. ProFTPD, as used by Aruba shared
                // hosting) answer MKD on an already-existing directory with
                // "550 Permission denied" instead of a recognizable "exists"
                // message. Rather than widen the string match (which would
                // mask genuine permission failures), ask stat.
                //
                // The question is not "does something exist here", it is "can
                // I descend into it", and stat answers that one: it returns a
                // `RemoteEntry` carrying `is_dir`. Reading only `is_ok()`
                // discarded the half that mattered, so a plain file sitting
                // where a parent directory belongs was forgiven, and the
                // failure surfaced one level further down naming a path that
                // was not the problem.
                if let Ok(entry) = backend.stat(&acc).await {
                    match descend_evidence(&entry) {
                        DescendEvidence::ProvenDirectory => continue,
                        // Something is here and the listing does not prove it
                        // is a directory. That is worth REMEMBERING and not
                        // worth stopping for: it is the likely cause of
                        // whatever fails next, and a symlink to a perfectly
                        // good directory lands in this same branch. Carry on
                        // exactly as before and keep the level to name later.
                        DescendEvidence::Unknown => {
                            if blocked_at.is_none() {
                                blocked_at = Some(acc.clone());
                            }
                            continue;
                        }
                    }
                }
                // stat could not answer at all.
                // stat could not answer. Fall back to the server's own words,
                // which are weaker: they say something is already there, not
                // that it is a directory.
                let low = e.to_ascii_lowercase();
                if low.contains("already exists")
                    || low.contains("file exists")
                    || low.contains("eexist")
                {
                    continue;
                }
                return Err(mkdir_failed(&acc, &e, blocked_at.as_deref()));
            }
        }
    }
    Ok(())
}

/// The mkdir that failed, plus the level that is probably why.
///
/// The message used to name only the path whose mkdir failed. When a parent
/// higher up is occupied by something that is not a directory, that path is
/// not the problem, and a reader who trusts the message looks one level below
/// the obstacle. Naming both keeps the failure where it happened and says
/// where to look.
fn mkdir_failed(path: &str, error: &str, blocked_at: Option<&str>) -> ToolError {
    match blocked_at {
        Some(blocker) => ToolError::Exec(format!(
            "mkdir {path} failed: {error}. {blocker} already exists and the listing does not \
             report it as a directory, which is the likely cause"
        )),
        None => ToolError::Exec(format!("mkdir {path} failed: {error}")),
    }
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
        "aeroftp_edit" => edit(ctx, args).await,
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
        "remote_versions" => remote_versions(ctx, args).await,
        "remote_trash" => remote_trash(ctx, args).await,
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
    // F-007: the per-profile transfer_capabilities block is ~20 identical keys
    // (source: "profile_defaults") repeated for every same-protocol profile. On
    // a vault of 80+ servers the unfiltered response overflows the MCP
    // tool-result cap with boilerplate, so an agent's natural first call ("list
    // the servers") fails by default. Omit it unless explicitly requested; the
    // identity fields are all an agent needs to choose a server.
    let include_capabilities = args
        .get("include_capabilities")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Snapshot vault keyset once so per-profile auth_state derivation is
    // O(1) instead of one decryption per call. When the vault isn't open
    // (rare in MCP, possible in CLI agent context), derivation falls back
    // to "unknown", but here we have the cache.
    let auth_lookup = crate::credential_store::CredentialStore::from_cache().map(|store| {
        let user_id = crate::user_partitions::cli_get_active_user(&store)
            .ok()
            .flatten()
            .map(|user| user.id);
        let accounts: std::collections::HashSet<String> = store
            .list_accounts()
            .unwrap_or_default()
            .into_iter()
            .collect();
        (store, accounts, user_id)
    });
    let profiles = ctx.credentials().list_servers().map_err(ToolError::Exec)?;
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
                .map(|(store, accounts, user_id)| {
                    crate::profile_auth_state::derive_profile_auth_state_for_user(
                        store,
                        accounts,
                        &p.id,
                        &p.protocol,
                        p.auth_source,
                        *user_id,
                    )
                })
                .unwrap_or("unknown");
            let mut entry = json!({
                "id": p.id,
                "name": p.name,
                "protocol": p.protocol,
                "host": p.host,
                "port": p.port,
                "username": p.username,
                "initialPath": p.initial_path,
                "providerId": p.provider_id,
                "auth_state": auth_state,
                // G27, and CLI-JSON-01: both keys are ALWAYS emitted, with
                // `cryptOverlay` null when the profile carries no binding. A
                // key that appears only when it has something to say cannot be
                // read as an absence — a client sees the same shape whether the
                // profile is unencrypted or the surface simply forgot to
                // answer, which is the state this tool was in before.
                "cryptOverlay": p.crypt_overlay,
                "protocolClass": p.protocol_class,
            });
            if include_capabilities {
                entry["transfer_capabilities"] = crate::agent_session::transfer_capabilities_block(
                    &p.protocol,
                    None,
                    "profile_defaults",
                );
            }
            entry
        })
        .collect();
    let matched_total = filtered.len();
    let page: Vec<Value> = filtered.into_iter().skip(offset).take(limit).collect();
    let returned = page.len();
    let mut result = json!({
        "servers": page,
        "count": returned,
        "total_matched": matched_total,
        "offset": offset,
        "limit": limit,
        "truncated": offset + returned < matched_total,
        "capabilities_included": include_capabilities,
    });
    if !include_capabilities {
        result["hint"] = json!(
            "transfer_capabilities omitted for a lean response; pass include_capabilities:true to embed them."
        );
    }
    Ok(result)
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

// RemoteBackend currently transports ProviderError through Display. Only the
// leading typed discriminator is evidence of absence; the detail may mention
// a missing service, token or metadata without saying this path is absent.
fn backend_reported_not_found(message: &str) -> bool {
    message.starts_with(&crate::providers::ProviderError::NotFound(String::new()).to_string())
}

/// A guard may permit a write only after a definite missing-path response.
async fn remote_path_exists(
    backend: &dyn RemoteBackend,
    path: &str,
    guard: &str,
) -> Result<bool, ToolError> {
    match backend.stat(path).await {
        Ok(_) => Ok(true),
        Err(e) if backend_reported_not_found(&e) => Ok(false),
        Err(e) => Err(ToolError::Exec(format!(
            "{guard}: cannot verify whether '{path}' exists ({e}); nothing was written"
        ))),
    }
}

async fn upload_file(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let server = normalize_server(args)?;
    let remote_path = get_str(args, "remote_path")?;
    validate_remote_path(&remote_path, "remote_path")?;
    let create_parents = get_bool_opt(args, "create_parents").unwrap_or(false);
    let no_clobber = get_bool_opt(args, "no_clobber").unwrap_or(false);
    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;
    reject_restricted_leaf(backend.as_ref(), &remote_path).await?;
    if no_clobber && remote_path_exists(backend.as_ref(), &remote_path, "no_clobber").await? {
        return Ok(no_clobber_skipped(&server, &remote_path));
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
        Ok(local_file_uploaded(&server, &local_path, &remote_path))
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

/// The result `upload_file` reports when `no_clobber` kept a destination.
fn no_clobber_skipped(server: &str, remote_path: &str) -> Value {
    json!({
        "server": server,
        "remote_path": remote_path,
        "uploaded": false,
        "skipped": true,
        "reason": "exists",
        "no_clobber": true,
    })
}

/// The result `upload_file` reports when a local file reached the server.
fn local_file_uploaded(server: &str, local_path: &str, remote_path: &str) -> Value {
    let bytes = std::fs::metadata(local_path).map(|m| m.len()).unwrap_or(0);
    json!({
        "server": server,
        "remote_path": remote_path,
        "uploaded": true,
        "bytes": bytes,
    })
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
    // The batch-level `no_clobber` is the default of every item that does
    // not set its own, on the bulk and the single-file path alike.
    let batch_no_clobber = get_bool_opt(args, "no_clobber");
    let delay_ms = args
        .get("delay_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        .min(MAX_UPLOAD_DELAY_MS);
    let mut tally = UploadTally::new(items.len(), delay_ms);
    let started = std::time::Instant::now();
    // A native bulk sends a whole group at once, so a batch that must stop
    // at its first failure keeps one upload per item.
    let plan = if continue_on_error {
        plan_bulk_uploads(ctx, &server, items, batch_no_clobber).await
    } else {
        None
    };
    if let Some(plan) = plan {
        run_bulk_uploads(ctx, &server, plan, &mut tally).await?;
    } else {
        for (idx, item) in items.iter().enumerate() {
            // M9: Stop ends the batch after the file in flight.
            crate::ai_core::tools::check_cancelled(ctx)?;
            let Some(item_args) = upload_many_item_args(item, &server, batch_no_clobber) else {
                return Err(ToolError::InvalidArgs {
                    tool: "aeroftp_upload_many".to_string(),
                    reason: format!("items[{idx}] must be an object"),
                });
            };
            tally.pace(ctx).await?;
            if !tally.record(idx, upload_file(ctx, &item_args).await) && !continue_on_error {
                break;
            }
        }
    }
    let elapsed_secs = started.elapsed().as_secs();
    let UploadTally {
        results,
        uploaded,
        skipped,
        errors,
        bulk_groups,
        ..
    } = tally;
    let results: Vec<Value> = results.into_iter().flatten().collect();
    Ok(json!({
        "server": server,
        "results": results,
        "summary": {
            "planned": items.len(),
            "processed": results.len(),
            "uploaded": uploaded,
            "skipped": skipped,
            "errors": errors,
            "bulk_groups": bulk_groups,
            "delay_ms": delay_ms,
            "totals": {
                "requested": items.len(),
                "succeeded": uploaded,
                "failed": errors,
                "skipped": skipped,
                "elapsed_secs": elapsed_secs,
            },
        },
    }))
}

/// Cap of `aeroftp_upload_many`'s `delay_ms`, as the tool schema states.
const MAX_UPLOAD_DELAY_MS: u64 = 2_000;

/// The `upload_many` item as `upload_file` receives it: `server` set, and
/// the batch's `no_clobber` unless the item sets its own. Only a boolean is
/// an item's own value: `upload_file` reads `null` or any other type as
/// false, so leaving one in place would let it overwrite what the batch
/// protects. `None` for an item that is not an object.
fn upload_many_item_args(
    item: &Value,
    server: &str,
    batch_no_clobber: Option<bool>,
) -> Option<Value> {
    let mut item_args = item.clone();
    let obj = item_args.as_object_mut()?;
    obj.insert("server".to_string(), json!(server));
    if let Some(no_clobber) = batch_no_clobber {
        if !obj.get("no_clobber").is_some_and(Value::is_boolean) {
            obj.insert("no_clobber".to_string(), json!(no_clobber));
        }
    }
    Some(item_args)
}

/// Per-item results of `upload_many`, in item order whatever order the
/// uploads ran in. A slot stays empty only for an item never reached.
struct UploadTally {
    results: Vec<Option<Value>>,
    uploaded: u32,
    /// Items `no_clobber` kept: the destination was already there.
    skipped: u32,
    errors: u32,
    /// Native bulk calls made.
    bulk_groups: u32,
    /// `delay_ms`: the pause before every upload call after the first, a
    /// native bulk counting as one call.
    pause: std::time::Duration,
    paced: bool,
}

impl UploadTally {
    fn new(items: usize, delay_ms: u64) -> Self {
        Self {
            results: vec![None; items],
            uploaded: 0,
            skipped: 0,
            errors: 0,
            bulk_groups: 0,
            pause: std::time::Duration::from_millis(delay_ms),
            paced: false,
        }
    }

    /// Waits `delay_ms` before an upload call, except the first. Stop ends
    /// the wait and the batch.
    async fn pace(&mut self, ctx: &dyn ToolCtx) -> Result<(), ToolError> {
        if std::mem::replace(&mut self.paced, true) && !self.pause.is_zero() {
            match ctx.cancel_token() {
                Some(token) => tokio::select! {
                    biased;
                    _ = token.cancelled() => return Err(ToolError::Cancelled),
                    _ = tokio::time::sleep(self.pause) => {}
                },
                None => tokio::time::sleep(self.pause).await,
            }
        }
        Ok(())
    }

    /// Records one item as `upload_file` reports it; false when it failed.
    fn record(&mut self, index: usize, outcome: Result<Value, ToolError>) -> bool {
        match outcome {
            Ok(v) => {
                self.uploaded +=
                    u32::from(v.get("uploaded").and_then(|b| b.as_bool()).unwrap_or(false));
                self.skipped +=
                    u32::from(v.get("skipped").and_then(|b| b.as_bool()).unwrap_or(false));
                self.results[index] = Some(v);
                true
            }
            Err(e) => {
                self.fail(index, e.to_string());
                false
            }
        }
    }

    /// Records a failure whose message is already rendered.
    fn fail(&mut self, index: usize, error: String) {
        self.errors += 1;
        self.results[index] = Some(json!({"uploaded": false, "error": error}));
    }
}

/// One `upload_many` item that can travel in a native bulk: a regular local
/// file that passed every check `upload_file` makes on it before it writes.
struct BulkCandidate {
    local_path: String,
    remote_path: String,
    size: u64,
    no_clobber: bool,
    create_parents: bool,
}

struct BulkUploadPlan {
    backend: std::sync::Arc<dyn RemoteBackend>,
    /// Every item with `server` set, as `upload_file` receives it.
    item_args: Vec<Value>,
    candidates: Vec<Option<BulkCandidate>>,
    /// Item indices in the order they run (`plan_groups_by_key`).
    groups: Vec<Vec<usize>>,
}

/// `None` keeps `upload_many` on one `upload_file` per item, exactly as
/// before: fewer than two items, an item that is not an object (refused
/// where the loop reaches it), a backend without a native bulk, or no
/// provider to judge names against.
async fn plan_bulk_uploads(
    ctx: &dyn ToolCtx,
    server: &str,
    items: &[Value],
    batch_no_clobber: Option<bool>,
) -> Option<BulkUploadPlan> {
    if items.len() < 2 {
        return None;
    }
    let item_args = items
        .iter()
        .map(|item| upload_many_item_args(item, server, batch_no_clobber))
        .collect::<Option<Vec<_>>>()?;
    let backend = ctx.remote_backend(server).await.ok()?;
    // An empty bulk only asks whether the provider has one.
    if !matches!(backend.upload_bulk(&[]).await, Some(Ok(_))) {
        return None;
    }
    let provider = backend.provider_type().await?;
    let candidates: Vec<_> = item_args
        .iter()
        .map(|args| bulk_candidate(args, provider))
        .collect();
    let entries: Vec<_> = item_args
        .iter()
        .zip(&candidates)
        .enumerate()
        .map(|(index, (args, candidate))| TransferEntry {
            id: index.to_string(),
            display_name: String::new(),
            local_path: get_str_opt(args, "local_path").unwrap_or_default(),
            remote_path: get_str_opt(args, "remote_path").unwrap_or_default(),
            size: candidate.as_ref().map_or(0, |c| c.size),
            modified: None,
        })
        .collect();
    // Every item enters the planner, so one that keeps `upload_file` still
    // orders the writes around it.
    let groups = plan_groups_by_key(&entries, |entry| {
        let index: usize = entry.id.parse().ok()?;
        candidates[index].as_ref()?;
        group_key(entry, true)
    });
    Some(BulkUploadPlan {
        backend,
        item_args,
        candidates,
        groups,
    })
}

fn bulk_candidate(args: &Value, provider: crate::providers::ProviderType) -> Option<BulkCandidate> {
    let remote_path = get_str_opt(args, "remote_path")?;
    let local_path = get_str_opt(args, "local_path")?;
    validate_remote_path(&remote_path, "remote_path").ok()?;
    crate::restricted_chars::validate_path(provider, &remote_path).ok()?;
    validate_local_path(&local_path, "local_path").ok()?;
    // A bulk carries files only: anything else keeps `upload_file`, and its
    // error with it.
    let size = std::fs::metadata(&local_path)
        .ok()
        .filter(|m| m.is_file())?
        .len();
    Some(BulkCandidate {
        no_clobber: get_bool_opt(args, "no_clobber").unwrap_or(false),
        create_parents: get_bool_opt(args, "create_parents").unwrap_or(false),
        local_path,
        remote_path,
        size,
    })
}

/// Runs the planned groups in order. A group of one keeps `upload_file`;
/// a larger one settles `upload_file`'s checks once and makes one native
/// call, and an item the bulk did not finish safely gets `upload_file` once.
async fn run_bulk_uploads(
    ctx: &dyn ToolCtx,
    server: &str,
    plan: BulkUploadPlan,
    tally: &mut UploadTally,
) -> Result<(), ToolError> {
    let BulkUploadPlan {
        backend,
        item_args,
        candidates,
        groups,
    } = plan;
    let mut parents_ready = std::collections::HashMap::new();
    let mut native = true;
    for group in groups {
        // M9: Stop ends the batch after the group in flight.
        crate::ai_core::tools::check_cancelled(ctx)?;
        let members = if native {
            bulk_members(&group, &candidates)
        } else {
            None
        };
        let Some((parent, members)) = members else {
            upload_each(ctx, &item_args, &group, tally).await?;
            continue;
        };
        let pending = bulk_preconditions(
            ctx,
            backend.as_ref(),
            server,
            &parent,
            members,
            &mut parents_ready,
            tally,
        )
        .await;
        if pending.len() < 2 {
            // Its checks are settled: only the write is left.
            for (index, member) in pending {
                crate::ai_core::tools::check_cancelled(ctx)?;
                tally.pace(ctx).await?;
                let outcome = backend
                    .upload(&member.local_path, &member.remote_path)
                    .await
                    .map(|()| local_file_uploaded(server, &member.local_path, &member.remote_path))
                    .map_err(ToolError::Exec);
                tally.record(index, outcome);
            }
            continue;
        }
        let items: Vec<BulkTransferItem> = pending
            .iter()
            .map(|(_, m)| bulk_item(&m.local_path, &m.remote_path, m.size))
            .collect();
        tally.pace(ctx).await?;
        // A bulk carries the whole group, so Stop does not wait for it: the
        // provider stops its own wire work when the call is dropped, and
        // what already landed stays.
        let answer = match ctx.cancel_token() {
            Some(token) => tokio::select! {
                biased;
                _ = token.cancelled() => return Err(ToolError::Cancelled),
                answer = backend.upload_bulk(&items) => answer,
            },
            None => backend.upload_bulk(&items).await,
        };
        let Some(answer) = answer else {
            // The backend stopped offering a bulk (the GUI's active
            // connection can change under a batch): one `upload_file` per
            // item from here on, each with its own checks again.
            native = false;
            let indices: Vec<usize> = pending.iter().map(|(index, _)| *index).collect();
            upload_each(ctx, &item_args, &indices, tally).await?;
            continue;
        };
        tally.bulk_groups += 1;
        let outcomes = match answer {
            Ok(outcomes) if outcomes.len() == items.len() => outcomes,
            Ok(_) => uncertain_items(
                items.len(),
                "Bulk did not return complete outcomes; inspect the destination before retrying",
            ),
            Err(error) => {
                for (index, _) in pending {
                    tally.record(index, Err(ToolError::Exec(error.clone())));
                }
                continue;
            }
        };
        let mut single_file = Vec::new();
        for ((index, member), outcome) in pending.into_iter().zip(outcomes) {
            // The settlement classifies a failure; the provider's own words
            // say what it was. MCP sanitizes every provider error it hands an
            // agent, and a bulk outcome arrives raw, so it is sanitized here.
            let detail = match &outcome {
                BulkTransferOutcome::Failed(error) => Some(error.to_string()),
                _ => None,
            };
            match settle_bulk_outcome(outcome, &member.local_path) {
                BulkSettlement::Done => {
                    tally.record(
                        index,
                        Ok(local_file_uploaded(
                            server,
                            &member.local_path,
                            &member.remote_path,
                        )),
                    );
                }
                BulkSettlement::SingleFile => single_file.push(index),
                BulkSettlement::Failed(failure) => {
                    let message = detail.unwrap_or(failure.message);
                    tally.fail(
                        index,
                        ToolError::Exec(crate::providers::sanitize_api_error(&message)).to_string(),
                    );
                }
            }
        }
        // Never reached the wire, or failed for a transient reason: neither
        // can duplicate a committed write.
        upload_each(ctx, &item_args, &single_file, tally).await?;
    }
    Ok(())
}

/// One `upload_file` per item: the established single-file path.
async fn upload_each(
    ctx: &dyn ToolCtx,
    item_args: &[Value],
    indices: &[usize],
    tally: &mut UploadTally,
) -> Result<(), ToolError> {
    for &index in indices {
        crate::ai_core::tools::check_cancelled(ctx)?;
        tally.pace(ctx).await?;
        tally.record(index, upload_file(ctx, &item_args[index]).await);
    }
    Ok(())
}

/// The members of a planned group that travels as one bulk, with the remote
/// parent they share. `None` for a group of one, and for any group the
/// planner should not have formed, which then keeps `upload_file`.
fn bulk_members<'a>(
    group: &[usize],
    candidates: &'a [Option<BulkCandidate>],
) -> Option<(String, Vec<(usize, &'a BulkCandidate)>)> {
    if group.len() < 2 {
        return None;
    }
    let members = group
        .iter()
        .map(|&index| Some((index, candidates.get(index)?.as_ref()?)))
        .collect::<Option<Vec<_>>>()?;
    let parent = remote_parent(&members[0].1.remote_path)?;
    members
        .iter()
        .all(|(_, m)| remote_parent(&m.remote_path) == Some(parent))
        .then(|| (parent.to_string(), members))
}

/// What `upload_file` checks before it writes, settled once for a group
/// that shares `parent`: one listing answers every `no_clobber` member and
/// one `ensure_remote_parents` serves every `create_parents` member, as it
/// does for every later group under the same parent. Skipped and failed
/// members are recorded; the rest come back in order.
async fn bulk_preconditions<'a>(
    ctx: &dyn ToolCtx,
    backend: &dyn RemoteBackend,
    server: &str,
    parent: &str,
    members: Vec<(usize, &'a BulkCandidate)>,
    parents_ready: &mut std::collections::HashMap<String, Result<(), String>>,
    tally: &mut UploadTally,
) -> Vec<(usize, &'a BulkCandidate)> {
    let listing = if members.iter().any(|(_, m)| m.no_clobber) {
        Some(backend.list(parent).await)
    } else {
        None
    };
    let authoritative = backend.listing_is_authoritative().await;
    let mut pending = Vec::with_capacity(members.len());
    for (index, member) in members {
        if member.no_clobber {
            let name = member.remote_path.rsplit('/').next().unwrap_or("");
            let listed = match &listing {
                Some(Ok(entries)) => match listed_name(entries, name) {
                    Listed::Absent if !authoritative => Listed::Unsure,
                    answer => answer,
                },
                // Nothing lives under a folder that is not there.
                Some(Err(e)) if backend_reported_not_found(e) => Listed::Absent,
                _ => Listed::Unsure,
            };
            let exists = match listed {
                Listed::Present => true,
                Listed::Absent => false,
                Listed::Unsure => {
                    match remote_path_exists(backend, &member.remote_path, "no_clobber").await {
                        Ok(exists) => exists,
                        Err(e) => {
                            tally.record(index, Err(e));
                            continue;
                        }
                    }
                }
            };
            if exists {
                tally.record(index, Ok(no_clobber_skipped(server, &member.remote_path)));
                continue;
            }
        }
        pending.push((index, member));
    }
    let first = pending
        .iter()
        .find(|(_, m)| m.create_parents)
        .map(|(_, m)| *m);
    if let Some(first) = first {
        let ready = match parents_ready.get(parent) {
            Some(ready) => ready.clone(),
            None => {
                let ready = ensure_remote_parents(ctx, server, &first.remote_path)
                    .await
                    .map_err(|e| e.to_string());
                parents_ready.insert(parent.to_string(), ready.clone());
                ready
            }
        };
        if let Err(error) = ready {
            pending.retain(|(index, m)| {
                if m.create_parents {
                    tally.fail(*index, error.clone());
                }
                !m.create_parents
            });
        }
    }
    pending
}

/// What one listing of a folder says about a name in it.
enum Listed {
    Present,
    Absent,
    /// The listing cannot settle it: stat decides, as in `upload_file`.
    Unsure,
}

fn listed_name(entries: &[crate::providers::RemoteEntry], name: &str) -> Listed {
    use unicode_normalization::UnicodeNormalization;
    let names = || entries.iter().map(|e| e.name.trim_end_matches('/'));
    if names().any(|listed| listed == name) {
        return Listed::Present;
    }
    // A listing can leave out what stat finds (FTP `LIST` hides dot names),
    // and a provider can match a name across case or Unicode form: only a
    // clear absence is taken from the listing.
    let fold = |s: &str| s.nfc().collect::<String>().to_lowercase();
    let folded = fold(name);
    if name.starts_with('.') || names().any(|listed| fold(listed) == folded) {
        Listed::Unsure
    } else {
        Listed::Absent
    }
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
    reject_restricted_leaf(backend.as_ref(), &path).await?;
    backend.mkdir(&path).await.map_err(ToolError::Exec)?;
    Ok(json!({ "server": server, "path": path, "created": true }))
}

/// Caps for the `dry_run` walk. A preview must never become the most
/// expensive call in the session, and it must never run unbounded on a
/// hostile tree.
const DELETE_PREVIEW_MAX_ENTRIES: usize = 100_000;
const DELETE_PREVIEW_MAX_DEPTH: usize = 100;

/// What a real delete of `path` would remove.
#[derive(Debug)]
struct DeletePreview {
    files: u64,
    directories: u64,
    bytes: u64,
    /// The walk hit a cap, or met a folder it would not list (see
    /// [`walk_admits`]), so `bytes`/`files` do not cover the whole tree.
    truncated: bool,
}

/// Walk `path` and total up what a real delete would remove, deleting
/// nothing. The recursive delete removes as it walks, so the byte figure has
/// to come from a separate pre-pass.
async fn preview_delete(
    backend: &dyn RemoteBackend,
    path: &str,
    recursive: bool,
) -> Result<DeletePreview, ToolError> {
    // S3/MinIO (and similar) use pseudo-directories: a prefix like "/dir"
    // may have children under "/dir/..." but no object at the exact "/dir" key.
    // stat() then fails with "not found", but recursive delete and list(prefix)
    // succeed. Tolerate the not-found for recursive dir previews so dry-run
    // is predictive (addresses Codex finding 2026-07-12).
    let (is_dir, root_size): (bool, u64) = match backend.stat(path).await {
        Ok(r) => (r.is_dir, r.size),
        Err(e) => {
            if recursive && backend_reported_not_found(&e) {
                // Assume pseudo-directory for recursive preview; contents will
                // be discovered by list below.
                (true, 0)
            } else {
                return Err(ToolError::Exec(e));
            }
        }
    };

    if !is_dir {
        return Ok(DeletePreview {
            files: 1,
            directories: 0,
            bytes: root_size,
            truncated: false,
        });
    }

    if !recursive {
        let children = backend.list(path).await.map_err(ToolError::Exec)?;
        if !children.is_empty() {
            return Err(ToolError::InvalidArgs {
                tool: "aeroftp_delete".to_string(),
                reason: format!(
                    "'{}' is a directory with {} entries; pass recursive=true to delete it",
                    path,
                    children.len()
                ),
            });
        }
        return Ok(DeletePreview {
            files: 0,
            directories: 1,
            bytes: 0,
            truncated: false,
        });
    }

    let mut files = 0u64;
    let mut directories = 1u64; // the root itself (even for pseudo-dir)
    let mut bytes = 0u64;
    let mut truncated = false;
    let mut queue: Vec<(String, usize)> = vec![(path.to_string(), 0)];
    // The preview totals what sits under `path`, each path once. A listing
    // that named a folder inside itself, or every folder as `/`, sent it
    // round the same folder to its depth cap, and one that named a folder
    // outside `path` added what that folder holds. A folder it does not
    // follow leaves the totals `truncated`.
    let mut scope = crate::sync_core::WalkScope::new(path);
    let mut counted: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut refused: u32 = 0;

    while let Some((dir, depth)) = queue.pop() {
        if depth >= DELETE_PREVIEW_MAX_DEPTH
            || (files + directories) as usize >= DELETE_PREVIEW_MAX_ENTRIES
        {
            truncated = true;
            continue;
        }
        let listing = backend.list(&dir).await.map_err(ToolError::Exec)?;
        if depth == 0 {
            scope.learn_root_listing(&listing);
        }
        for entry in listing {
            if (files + directories) as usize >= DELETE_PREVIEW_MAX_ENTRIES {
                truncated = true;
                break;
            }
            // A symlink is unlinked, never followed: following a
            // symlink-to-dir would open a cur/parent cycle and inflate the
            // total.
            let follows = entry.is_dir && !entry.is_symlink;
            if !walk_admits(
                &mut scope,
                &dir,
                &entry.path,
                follows,
                &mut refused,
                "delete preview",
            ) {
                continue;
            }
            if follows {
                directories += 1;
                queue.push((entry.path, depth + 1));
            } else if counted.insert(crate::sync_core::walk_path_key(&entry.path)) {
                files += 1;
                if !entry.is_symlink {
                    bytes = bytes.saturating_add(entry.size);
                }
            }
        }
    }
    truncated |= refused > 0;

    Ok(DeletePreview {
        files,
        directories,
        bytes,
        truncated,
    })
}

/// Delete one path, or preview it when `dry_run` is set. `Ok(Some(preview))`
/// means nothing was touched.
async fn delete_one(
    backend: &dyn RemoteBackend,
    path: &str,
    recursive: bool,
    dry_run: bool,
) -> Result<Option<DeletePreview>, ToolError> {
    if dry_run {
        return preview_delete(backend, path, recursive).await.map(Some);
    }
    if recursive {
        return backend
            .delete_recursive(path)
            .await
            .map_err(ToolError::Exec)
            .map(|()| None);
    }
    match backend.delete(path).await {
        Ok(()) => Ok(None),
        Err(e) => {
            // A non-recursive delete of a non-empty directory surfaces as the
            // provider's raw rmdir error ("Failure: Failure" on SFTP), which
            // tells an agent nothing. Pay a stat+list on the failure path only
            // to turn it into an actionable message. An EMPTY directory that
            // fails for some other reason (permissions) must keep its original
            // error: suggesting `recursive=true` there would be a lie.
            let is_non_empty_dir = match backend.stat(path).await {
                Ok(entry) if entry.is_dir => backend
                    .list(path)
                    .await
                    .map(|children| !children.is_empty())
                    .unwrap_or(false),
                _ => false,
            };
            if is_non_empty_dir {
                return Err(ToolError::InvalidArgs {
                    tool: "aeroftp_delete".to_string(),
                    reason: format!(
                        "'{path}' is a non-empty directory; pass recursive=true to delete it and everything under it (underlying error: {e})"
                    ),
                });
            }
            Err(ToolError::Exec(e))
        }
    }
}

async fn delete(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    if args.get("paths").is_some() {
        return delete_many(ctx, args).await;
    }
    let server = normalize_server(args)?;
    let path = get_str(args, "path")?;
    validate_remote_path(&path, "path")?;
    let recursive = get_bool_opt(args, "recursive").unwrap_or(false);
    let dry_run = get_bool_opt(args, "dry_run").unwrap_or(false);
    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;

    match delete_one(backend.as_ref(), &path, recursive, dry_run).await? {
        Some(preview) => Ok(json!({
            "server": server,
            "path": path,
            "recursive": recursive,
            "dry_run": true,
            "deleted": false,
            "would_delete": true,
            "files": preview.files,
            "directories": preview.directories,
            "bytes": preview.bytes,
            "truncated": preview.truncated,
        })),
        None => Ok(json!({ "server": server, "path": path, "deleted": true })),
    }
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
    let recursive = get_bool_opt(args, "recursive").unwrap_or(false);
    let dry_run = get_bool_opt(args, "dry_run").unwrap_or(false);
    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;
    let mut results = Vec::with_capacity(paths.len());
    let mut deleted = 0u32;
    let mut would_delete = 0u32;
    let mut errors = 0u32;
    let mut bytes = 0u64;
    let mut truncated = false;
    let started = std::time::Instant::now();
    for path in paths {
        crate::ai_core::tools::check_cancelled(ctx)?;
        let Some(path) = path.as_str() else {
            return Err(ToolError::InvalidArgs {
                tool: "aeroftp_delete_many".to_string(),
                reason: "every entry of 'paths' must be a string".to_string(),
            });
        };
        validate_remote_path(path, "path")?;
        match delete_one(backend.as_ref(), path, recursive, dry_run).await {
            Ok(Some(preview)) => {
                would_delete += 1;
                bytes = bytes.saturating_add(preview.bytes);
                truncated |= preview.truncated;
                results.push(json!({
                    "path": path,
                    "deleted": false,
                    "would_delete": true,
                    "files": preview.files,
                    "directories": preview.directories,
                    "bytes": preview.bytes,
                    "truncated": preview.truncated,
                }));
            }
            Ok(None) => {
                deleted += 1;
                results.push(json!({"path": path, "deleted": true}));
            }
            Err(e) => {
                errors += 1;
                let message = match e {
                    ToolError::InvalidArgs { ref reason, .. } => reason.clone(),
                    ref other => other.to_string(),
                };
                results.push(json!({"path": path, "deleted": false, "error": message}));
                if !continue_on_error {
                    break;
                }
            }
        }
    }
    let elapsed_secs = started.elapsed().as_secs();
    // Same envelope for both modes so an agent can diff a dry-run against the
    // real run: only `deleted` vs `would_delete` move.
    let succeeded = if dry_run { would_delete } else { deleted };
    Ok(json!({
        "server": server,
        "dry_run": dry_run,
        "recursive": recursive,
        "results": results,
        "summary": {
            "planned": paths.len(),
            "processed": results.len(),
            "deleted": deleted,
            "would_delete": would_delete,
            "bytes": bytes,
            "truncated": truncated,
            "errors": errors,
            "totals": {
                "requested": paths.len(),
                "succeeded": succeeded,
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
    // Only `to` sets a new name; `from` addresses an object that already
    // exists, and refusing a character there would block acting on something
    // the user already has (same exclusion class as delete).
    reject_restricted_leaf(backend.as_ref(), &to).await?;
    backend.rename(&from, &to).await.map_err(ToolError::Exec)?;
    Ok(json!({ "server": server, "from": from, "to": to, "renamed": true }))
}

const MAX_EDIT_BYTES: u64 = 10 * 1024 * 1024;

fn reject_uneditable_entry(entry: &crate::providers::RemoteEntry) -> Result<(), ToolError> {
    if entry.is_dir {
        return Err(ToolError::Exec("Cannot edit a directory.".to_string()));
    }
    if entry.size > MAX_EDIT_BYTES {
        return Err(ToolError::Exec(format!(
            "File too large ({} bytes). Limit: 10 MB. Use aeroftp_download_file + local edit for larger files.",
            entry.size
        )));
    }
    Ok(())
}

fn reject_oversize_edit_download(data_len: usize) -> Result<(), ToolError> {
    if data_len as u64 > MAX_EDIT_BYTES {
        return Err(ToolError::Exec(format!(
            "Downloaded file exceeded the edit limit ({} bytes). Limit: 10 MB.",
            data_len
        )));
    }
    Ok(())
}

pub(crate) fn edit_temp_path(path: &str) -> String {
    // Keep the staging file beside the target without copying its leaf:
    // an existing name may contain characters forbidden for new objects.
    let parent = path.rsplit_once('/').map(|(parent, _)| parent);
    let leaf = format!(".aeroedit-{}.tmp", uuid::Uuid::new_v4());
    match parent {
        Some(parent) => format!("{parent}/{leaf}"),
        None => leaf,
    }
}

/// Find-and-replace on a remote UTF-8 text file (MCP/CLI surface).
/// In-memory for files <= 10 MiB. Rejects directories and non-UTF8 (binary).
/// If no match for `find`, returns no_op and does not re-upload.
async fn edit(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let server = normalize_server(args)?;
    let path = get_str(args, "path")?;
    validate_remote_path(&path, "path")?;
    let find = get_str(args, "find")?;
    let replace = get_str(args, "replace")?;
    let first_only = get_bool_opt(args, "first").unwrap_or(false);
    let allow_non_atomic = get_bool_opt(args, "allow_non_atomic").unwrap_or(false);

    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;
    let entry = backend.stat(&path).await.map_err(ToolError::Exec)?;
    reject_uneditable_entry(&entry)?;
    // The edit publishes a new file in the target's place: over a link that
    // is a regular file where the link was, and the file it points to left
    // as it was. Refused before anything is read or written.
    crate::providers::refuse_edit_of_symlink(&entry, &path).map_err(ToolError::Exec)?;

    // Capped, streaming download: the backend refuses mid-read once the buffer
    // would cross MAX_EDIT_BYTES, so a server that under-reports its size in
    // `stat` cannot force a full 500 MB in-memory materialization here.
    let data = backend
        .download_to_bytes_capped(&path, MAX_EDIT_BYTES)
        .await
        .map_err(ToolError::Exec)?;
    // Backstop for any backend that ignores the cap (default fallbacks read
    // fully): reject before we allocate the String / parse UTF-8.
    reject_oversize_edit_download(data.len())?;
    let original = match String::from_utf8(data) {
        Ok(s) => s,
        Err(_) => {
            return Err(ToolError::Exec(
                "File appears binary (non-UTF-8). aeroftp_edit supports UTF-8 text only."
                    .to_string(),
            ))
        }
    };

    let bytes_before = original.len() as u64;

    let (new_text, replacements) = if find.is_empty() {
        // Empty find: treat as no-op (or could insert at start, but spec implies search).
        (original.clone(), 0usize)
    } else if first_only {
        if let Some(pos) = original.find(&find) {
            let mut s = original.clone();
            let end = pos + find.len();
            s.replace_range(pos..end, &replace);
            (s, 1)
        } else {
            (original.clone(), 0)
        }
    } else {
        let count = original.matches(&find).count();
        if count == 0 {
            (original.clone(), 0)
        } else {
            (original.replace(&find, &replace), count)
        }
    };

    let bytes_after = new_text.len() as u64;

    if replacements == 0 {
        return Ok(json!({
            "server": server,
            "path": path,
            "replacements": 0,
            "bytes_before": bytes_before,
            "bytes_after": bytes_before,
            "no_op": true,
            "reason": "no match for find string"
        }));
    }

    // Asked before the temporary exists, not after: a backend that cannot put
    // one file over another refuses while the server is still untouched, so
    // the refusal the agent reads can say that nothing was written (G119).
    // `allow_non_atomic` accepts the set-aside replace, and only on a backend
    // whose replace does set the previous file aside: on any other the
    // replace would refuse the taken name after the temporary was uploaded.
    if !backend
        .supports_atomic_replace()
        .await
        .map_err(ToolError::Exec)?
    {
        const OPT_IN: &str = "`allow_non_atomic` true";
        let sets_aside = backend
            .replace_sets_aside()
            .await
            .map_err(ToolError::Exec)?;
        if allow_non_atomic && !sets_aside {
            return Err(ToolError::Exec(crate::providers::opt_in_cannot_set_aside(
                &path, OPT_IN,
            )));
        }
        if !allow_non_atomic {
            let mut refusal = format!(
                "cannot edit `{path}` in place: this server offers no atomic way to put one \
                 file over another, and doing it in two steps would leave a moment with no \
                 file at all. Nothing was written and `{path}` is unchanged."
            );
            if sets_aside {
                refusal.push(' ');
                refusal.push_str(&crate::providers::set_aside_opt_in_hint(OPT_IN));
            }
            return Err(ToolError::Exec(refusal));
        }
    }

    // The new file does not inherit the mode of the one it replaces (on SFTP
    // it gets the server default): it is set on the temporary before the
    // replace, where the backend reports one and can set it.
    let can_chmod = backend.supports_chmod().await.map_err(ToolError::Exec)?;
    let original = crate::providers::EditOriginal::of(&entry, can_chmod);

    let temp_path = edit_temp_path(&path);
    reject_restricted_leaf(backend.as_ref(), &temp_path).await?;
    if let Err(e) = backend
        .upload_from_bytes(new_text.as_bytes(), &temp_path)
        .await
    {
        let _ = backend.delete(&temp_path).await;
        return Err(ToolError::Exec(e));
    }
    // Best effort: a refused chmod does not fail an edit that is otherwise
    // done, and the answer says what was not kept.
    let not_kept = match &original {
        crate::providers::EditOriginal::Nothing => None,
        crate::providers::EditOriginal::Unreadable(permissions) => Some(
            crate::providers::edit_permissions_not_readable(&path, permissions),
        ),
        crate::providers::EditOriginal::Mode(mode) => backend
            .chmod(&temp_path, *mode)
            .await
            .err()
            .map(|error| crate::providers::edit_mode_not_kept(&path, *mode, &error)),
    };
    // `replace` and not `rename`: the destination exists by definition here,
    // and `rename` keeps refusing that case so an ordinary move cannot
    // destroy a file the caller did not mean to lose.
    if let Err(e) = backend.replace(&temp_path, &path).await {
        let _ = backend.delete(&temp_path).await;
        return Err(ToolError::Exec(e));
    }

    let mut answer = json!({
        "server": server,
        "path": path,
        "replacements": replacements,
        "bytes_before": bytes_before,
        "bytes_after": bytes_after
    });
    if let Some(warning) = not_kept {
        tracing::warn!("{warning}");
        answer["warnings"] = json!([warning]);
    }
    Ok(answer)
}

/// `remote_versions`: browse and manage a file's version history.
/// action=list returns the versions (newest first); restore copies an older
/// version forward to a new current version; purge permanently removes one
/// version (irreversible). restore/purge require `version_id`.
async fn remote_versions(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let server = normalize_server(args)?;
    let action = get_str_opt(args, "action").unwrap_or_else(|| "list".to_string());
    let path = get_str(args, "path")?;
    validate_remote_path(&path, "path")?;
    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;
    match action.as_str() {
        "list" => {
            let versions = backend
                .list_versions(&path)
                .await
                .map_err(ToolError::Exec)?;
            let items: Vec<Value> = versions
                .iter()
                .map(|v| {
                    json!({
                        "version_id": v.id,
                        "modified": v.modified,
                        "size": v.size,
                        "modified_by": v.modified_by,
                    })
                })
                .collect();
            Ok(json!({
                "server": server,
                "path": path,
                "count": items.len(),
                "versions": items,
            }))
        }
        "restore" => {
            let version_id = get_str(args, "version_id")?;
            backend
                .restore_version(&path, &version_id)
                .await
                .map_err(ToolError::Exec)?;
            Ok(
                json!({ "server": server, "path": path, "version_id": version_id, "restored": true }),
            )
        }
        "purge" => {
            let version_id = get_str(args, "version_id")?;
            backend
                .delete_version(&path, &version_id)
                .await
                .map_err(ToolError::Exec)?;
            Ok(json!({ "server": server, "path": path, "version_id": version_id, "purged": true }))
        }
        other => Err(ToolError::InvalidArgs {
            tool: "remote_versions".to_string(),
            reason: format!("unknown action '{}' (expected list, restore, purge)", other),
        }),
    }
}

/// `remote_trash`: browse and manage the S3-family soft-delete trash under a
/// prefix. action=list returns every version and delete marker (with
/// include_noncurrent to include older versions); undelete drops a delete marker
/// so the object reappears; restore copies an older version forward; purge
/// permanently removes one version or marker; empty purges the whole trash under
/// the prefix (defaults to dry_run=true, a preview that deletes nothing; a real
/// whole-bucket purge with an empty prefix also needs confirm_whole_bucket=true).
/// All destructive actions are irreversible.
async fn remote_trash(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let server = normalize_server(args)?;
    let action = get_str_opt(args, "action").unwrap_or_else(|| "list".to_string());
    let prefix = normalize_path_arg(args, "prefix", "");
    let include_noncurrent = get_bool_opt(args, "include_noncurrent").unwrap_or(false);
    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;
    match action.as_str() {
        "list" => {
            let entries = backend
                .list_object_versions(&prefix, include_noncurrent)
                .await
                .map_err(ToolError::Exec)?;
            let items: Vec<Value> = entries
                .iter()
                .map(|e| {
                    json!({
                        "key": e.key,
                        "display_key": e.display_key,
                        "version_id": e.version_id,
                        "is_delete_marker": e.is_delete_marker,
                        "is_latest": e.is_latest,
                        "size": e.size,
                        "last_modified": e.last_modified,
                    })
                })
                .collect();
            Ok(json!({
                "server": server,
                "prefix": prefix,
                "count": items.len(),
                "entries": items,
            }))
        }
        "undelete" => {
            let key = get_str(args, "key")?;
            let version_id = get_str(args, "version_id")?;
            backend
                .delete_version(&key, &version_id)
                .await
                .map_err(ToolError::Exec)?;
            Ok(json!({ "server": server, "key": key, "version_id": version_id, "undeleted": true }))
        }
        "restore" => {
            let key = get_str(args, "key")?;
            let version_id = get_str(args, "version_id")?;
            backend
                .restore_version(&key, &version_id)
                .await
                .map_err(ToolError::Exec)?;
            Ok(json!({ "server": server, "key": key, "version_id": version_id, "restored": true }))
        }
        "purge" => {
            let key = get_str(args, "key")?;
            let version_id = get_str(args, "version_id")?;
            backend
                .delete_version(&key, &version_id)
                .await
                .map_err(ToolError::Exec)?;
            Ok(json!({ "server": server, "key": key, "version_id": version_id, "purged": true }))
        }
        "empty" => {
            // Destructive-default guards: preview unless told otherwise, and a
            // real whole-bucket sweep needs an explicit acknowledgement.
            let dry_run = get_bool_opt(args, "dry_run").unwrap_or(true);
            let confirm_whole_bucket = get_bool_opt(args, "confirm_whole_bucket").unwrap_or(false);
            if !dry_run && prefix.is_empty() && !confirm_whole_bucket {
                return Err(ToolError::InvalidArgs {
                    tool: "remote_trash".to_string(),
                    reason: "action=empty with an empty prefix permanently purges every version and delete marker in the whole bucket: pass a non-empty 'prefix' to scope the purge, or set confirm_whole_bucket=true to acknowledge a whole-bucket purge".to_string(),
                });
            }
            let (count, bytes) = backend
                .empty_object_versions(&prefix, include_noncurrent, dry_run)
                .await
                .map_err(ToolError::Exec)?;
            Ok(json!({
                "server": server,
                "prefix": prefix,
                "dry_run": dry_run,
                "count": count,
                "bytes": bytes,
            }))
        }
        other => Err(ToolError::InvalidArgs {
            tool: "remote_trash".to_string(),
            reason: format!(
                "unknown action '{}' (expected list, undelete, restore, purge, empty)",
                other
            ),
        }),
    }
}

/// Depth / entry caps for the optional `scan` mode of `aeroftp_storage_quota`.
/// Mirrors the CLI `df --scan` project-wide scan caps so an agent gets the
/// same bounded BFS. The walk only retains the directory queue (not every
/// entry), so memory stays bounded by tree breadth.
const MAX_QUOTA_SCAN_DEPTH: usize = 100;
const MAX_QUOTA_SCAN_ENTRIES: usize = 500_000;

async fn storage_quota(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let server = normalize_server(args)?;
    let scan = args.get("scan").and_then(|v| v.as_bool()).unwrap_or(false);
    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;

    if !scan {
        let info = backend.storage_info().await.map_err(ToolError::Exec)?;
        return Ok(json!({
            "server": server,
            "scanned": false,
            "used_bytes": info.used,
            "total_bytes": info.total,
            "free_bytes": info.available,
        }));
    }

    // --- Explicit recursive used-storage scan (CLI `df --scan` parity) ----
    // For backends with no quota-total API (raw FTP/FTPS, most S3/WebDAV)
    // the only way to a real used/total/% is to sum file sizes. Never run
    // automatically: the agent opts in with scan:true.
    let full = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
    let root = if full {
        "/".to_string()
    } else {
        get_str_opt(args, "path")
            .filter(|p| !p.trim().is_empty())
            .unwrap_or_else(|| "/".to_string())
    };

    // An API total still wins for the cap even in scan mode; the scan only
    // ever supplies `used`. storage_info erroring is expected for the
    // no-quota backends this targets, so it is non-fatal.
    let api_total = backend
        .storage_info()
        .await
        .ok()
        .map(|i| i.total)
        .filter(|t| *t > 0)
        .unwrap_or(0);

    let mut used_bytes: u64 = 0;
    let mut file_count: u64 = 0;
    let mut dir_count: u64 = 0;
    let mut visited: usize = 0;
    let mut truncated = false;
    // BFS queue of (path, depth). Only directories are queued; entries are
    // summed in-place so memory stays bounded by tree breadth.
    let mut queue: std::collections::VecDeque<(String, usize)> = std::collections::VecDeque::new();
    queue.push_back((root.clone(), 0));
    // Each folder under the root is listed once and each file counted once
    // (see `walk_admits`); a folder the walk does not follow leaves the
    // figure truncated, like one it could not list.
    let mut scope = crate::sync_core::WalkScope::new(&root);
    let mut counted: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut refused: u32 = 0;

    while let Some((dir, depth)) = queue.pop_front() {
        if depth > MAX_QUOTA_SCAN_DEPTH {
            truncated = true;
            continue;
        }
        crate::ai_core::tools::check_cancelled(ctx)?;
        let entries = match backend.list(&dir).await {
            Ok(e) => e,
            // A single unreadable directory is non-fatal: skip it, keep the
            // running total as a lower bound rather than failing the tool.
            Err(_) => {
                truncated = true;
                continue;
            }
        };
        if depth == 0 {
            scope.learn_root_listing(&entries);
        }
        dir_count += 1;
        for entry in entries {
            visited += 1;
            if visited > MAX_QUOTA_SCAN_ENTRIES {
                truncated = true;
                break;
            }
            // Skip symlinks to avoid traversal loops and double-counting,
            // matching the CLI used-scan behavior.
            if entry.is_symlink {
                continue;
            }
            if !walk_admits(
                &mut scope,
                &dir,
                &entry.path,
                entry.is_dir,
                &mut refused,
                "storage scan",
            ) {
                continue;
            }
            if entry.is_dir {
                queue.push_back((entry.path, depth + 1));
            } else if counted.insert(crate::sync_core::walk_path_key(&entry.path)) {
                // saturating_add: entry.size is fully server-controlled
                // (SFTP attrs, WebDAV getcontentlength, S3 Size). A hostile
                // server returning sizes near u64::MAX must not overflow
                // (debug panic / release wrap). Matches used_scan.rs and
                // provider_commands.rs.
                used_bytes = used_bytes.saturating_add(entry.size);
                file_count += 1;
            }
        }
        if visited > MAX_QUOTA_SCAN_ENTRIES {
            truncated = true;
            break;
        }
    }
    truncated |= refused > 0;

    let used_percent = if api_total > 0 {
        (used_bytes as f64 / api_total as f64) * 100.0
    } else {
        0.0
    };
    let total_source = if api_total > 0 {
        Value::String("api".to_string())
    } else {
        Value::Null
    };

    Ok(json!({
        "server": server,
        "scanned": true,
        "used_bytes": used_bytes,
        "total_bytes": api_total,
        "used_percent": used_percent,
        "total_source": total_source,
        "scan_files": file_count,
        "scan_dirs": dir_count,
        "scan_truncated": truncated,
        "scan_scope": if full { "account-root" } else { "initial-path" },
        "scan_root": root,
    }))
}

/// Hard cap per `aeroftp_hashsum`: sopra questo size il tool ritorna errore
/// `file_too_large` invece di tentare il download. Coerente con il fatto che
/// il backend.download_to_bytes carica tutto il payload in RAM (stessa
/// pipeline della CLI `aeroftp hashsum --download`).
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
    // Capped, not the general 500 MB download: the size above comes from the
    // server's own listing, and a server that under-reports it would otherwise
    // walk this tool up to the general ceiling. The cap is the same one the
    // size check just applied, so nothing legitimate changes.
    let data = backend
        .download_to_bytes_capped(&path, MAX_HASHSUM_BYTES)
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
    // Each path under the root is shown once. An entry that points back at
    // the folder listed or above it, or outside the root, gets no row (its
    // path is not under the root) and is named in `errors`; one already
    // shown is not shown or listed again.
    let mut scope = crate::sync_core::WalkScope::new(&root);
    let mut shown: std::collections::HashSet<String> = std::collections::HashSet::new();

    while let Some((dir, depth)) = queue.pop_front() {
        if depth > max_depth {
            continue;
        }
        crate::ai_core::tools::check_cancelled(ctx)?;
        let listing = match backend.list(&dir).await {
            Ok(v) => v,
            Err(e) => {
                errors.push(json!({"path": dir, "error": e}));
                continue;
            }
        };
        if depth == 0 {
            scope.learn_root_listing(&listing);
        }
        for entry in listing {
            total_visited += 1;
            if entries.len() >= max_entries {
                truncated = true;
                break;
            }
            use crate::sync_core::WalkStep;
            match scope.place(&dir, &entry.path, entry.is_walkable_dir()) {
                WalkStep::Follow => {}
                WalkStep::AlreadyListed => continue,
                step => {
                    tracing::warn!(
                        "tree: not showing {:?}, named in the listing of {dir}: {}",
                        entry.path,
                        step.reason()
                    );
                    errors.push(json!({
                        "path": entry.path,
                        "error": format!(
                            "not shown: the listing of {dir} names it, but {}",
                            step.explain()
                        ),
                        "reason": step.reason(),
                    }));
                    continue;
                }
            }
            if !shown.insert(crate::sync_core::walk_path_key(&entry.path)) {
                continue;
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
                // A6/A7b: route through the same `entry_json` chokepoint as
                // `list_files` and `search_files` so the MCP listing surfaces
                // emit one entry shape, then layer the tree-only `depth` on top.
                // `entry_json` already emits name/path/is_dir/size/modified in
                // this order, so this stays byte-identical to the old inline
                // `json!`; the unification is a pure refactor, not a contract change.
                let mut entry_v = entry_json(&entry, false);
                if let Some(obj) = entry_v.as_object_mut() {
                    obj.insert("depth".to_string(), json!(depth + 1));
                }
                entries.push(entry_v);
            }
            // Reported with `is_dir: true`, but a symlink-to-dir is never
            // walked (GAP-A02): a link to `..` would never terminate. A
            // walkable folder reaching here was admitted above.
            if entry.is_walkable_dir() && depth + 1 < max_depth + 1 {
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
    let mut capabilities = agent_session::capabilities_block(&profile.protocol);

    // `remote_backend()` opens (or reuses) the pooled connection: its
    // outcome IS the "connect" block. No separate connect() call needed.
    let started = std::time::Instant::now();
    let backend_result = ctx.remote_backend(&server).await;
    let elapsed_ms = started.elapsed().as_millis();

    let (connect, quota) = match backend_result {
        Ok(backend) => {
            let connect = agent_session::connect_block_ok(&profile.id, elapsed_ms);
            if let Ok(Some(live_caps)) = backend.transfer_capabilities().await {
                capabilities["transfer_capabilities"] = agent_session::transfer_capabilities_block(
                    &profile.protocol,
                    Some(live_caps),
                    "live_provider",
                );
            }
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

    // The destination leaf is a new name on the destination backend: apply
    // the same restricted-character rule the CLI `transfer` applies, before
    // the name reaches the wire. Applies to dry runs too, so a preview
    // predicts the refusal instead of reporting a plan that cannot run.
    crate::restricted_chars::validate_path(dst_provider.provider_type(), &dst_path)
        .map_err(|e| ToolError::Exec(e.to_string()))?;

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
            src_provider.mtime_precision(),
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

    // Every destination leaf the plan would create is a new name on the
    // destination backend. Refuse a leaf its rules forbid up front, before
    // any byte moves and before the dry-run report, so a preview cannot
    // promise a tree the destination would reject halfway through.
    for entry in plan.directories.iter().chain(&plan.entries) {
        crate::restricted_chars::validate_path(dst_provider.provider_type(), &entry.dest_path)
            .map_err(|e| ToolError::Exec(format!("{}: {}", entry.dest_path, e)))?;
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
                "planned_directories": plan.directories.len(),
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
            "directories": plan.directories,
            "plan": entries,
            "total_files": plan.total_files,
            "total_bytes": plan.total_bytes,
            "max_files": max_files,
            "truncated": false,
            "duration_ms": elapsed,
        }));
    }

    // ── Execute ──
    let directories_result = crate::cross_profile_transfer::ensure_planned_directories(
        dst_provider.as_mut(),
        &plan.directories,
        || ctx.cancel_token().is_some_and(|token| token.is_cancelled()),
    )
    .await;
    if let Err(error) = directories_result {
        let _ = src_provider.disconnect().await;
        let _ = dst_provider.disconnect().await;
        crate::ai_core::tools::check_cancelled(ctx)?;
        return Err(ToolError::Exec(error.to_string()));
    }
    let total_planned = plan.total_files;
    let mut transferred_files: u64 = 0;
    let mut skipped_files: u64 = 0;
    let mut failed_files: u64 = 0;
    let mut total_bytes_transferred: u64 = 0;
    let mut errors: Vec<Value> = Vec::new();

    // Pace progress events with the shared cadence: every 150 ms or 2 % of
    // the files, and the last file. A step of files alone (every 5 files)
    // left a tree of large files without news for minutes.
    let mut cadence = crate::progress_cadence::ProgressCadence::new();
    let files_pct = |done: u64| (done * 100).checked_div(total_planned).unwrap_or(100) as i64;

    for entry in plan.entries.iter() {
        crate::ai_core::tools::check_cancelled(ctx)?;
        if skip_existing {
            match crate::cross_profile_transfer::should_skip_existing(
                src_provider.mtime_precision(),
                dst_provider.as_mut(),
                &entry.dest_path,
                entry,
            )
            .await
            {
                Ok(true) => {
                    skipped_files += 1;
                    let done = transferred_files + skipped_files + failed_files;
                    if cadence.admit(files_pct(done), done >= total_planned) {
                        ctx.event_sink()
                            .emit_tool_progress(&crate::ai_core::ToolProgress {
                                tool: "aeroftp_transfer_tree".to_string(),
                                current: done as u32,
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

        let done = transferred_files + skipped_files + failed_files;
        if cadence.admit(files_pct(done), done >= total_planned) {
            ctx.event_sink()
                .emit_tool_progress(&crate::ai_core::ToolProgress {
                    tool: "aeroftp_transfer_tree".to_string(),
                    current: done as u32,
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
    // agent (mtime change without explicit intent). A stat that cannot answer
    // creates nothing: the empty upload would land over the file it missed.
    if remote_path_exists(backend.as_ref(), &path, "touch").await? {
        return Ok(json!({
            "server": server,
            "path": path,
            "action": "exists",
            "created": false,
        }));
    }
    // Only the create branch sets a new name, so only it is checked: an
    // existing file named with a forbidden character must stay touchable.
    reject_restricted_leaf(backend.as_ref(), &path).await?;
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

/// Whether a remote walk takes `child`, an entry the listing of `listed`
/// named (see [`crate::sync_core::WalkScope::place`]): a folder it `follows`
/// is listed, any other entry counted, compared or deleted. An entry that
/// points back at the folder being listed or above it, or outside the walk
/// root, is counted in `scan_errors`: the walk did not see the tree that
/// listing describes, and what sits there is not the walk's to act on. A
/// folder already listed is simply not listed again, so nothing under it is
/// collected twice.
fn walk_admits(
    scope: &mut crate::sync_core::WalkScope,
    listed: &str,
    child: &str,
    follows: bool,
    scan_errors: &mut u32,
    tool: &str,
) -> bool {
    let step = scope.place(listed, child, follows);
    walk_takes(step, listed, child, scan_errors, tool)
}

/// [`walk_admits`] for a walk that deletes what it takes (`cleanup`,
/// `dedupe`): the entry must also keep the letter case of the root
/// ([`crate::sync_core::WalkScope::place_to_delete`]).
fn delete_walk_admits(
    scope: &mut crate::sync_core::WalkScope,
    listed: &str,
    child: &str,
    follows: bool,
    scan_errors: &mut u32,
    tool: &str,
) -> bool {
    let step = scope.place_to_delete(listed, child, follows);
    walk_takes(step, listed, child, scan_errors, tool)
}

fn walk_takes(
    step: crate::sync_core::WalkStep,
    listed: &str,
    child: &str,
    scan_errors: &mut u32,
    tool: &str,
) -> bool {
    use crate::sync_core::WalkStep;
    match step {
        WalkStep::Follow => true,
        WalkStep::AlreadyListed => false,
        step => {
            *scan_errors += 1;
            tracing::warn!(
                "{tool}: skipping {child:?}, named in the listing of {listed}: {}",
                step.reason()
            );
            false
        }
    }
}

/// Files a sync scan found, by path relative to the folder scanned: size and
/// modification time (seconds on the local side, the provider's text on the
/// remote one).
type LocalFiles = std::collections::HashMap<String, (u64, Option<i64>)>;
type RemoteFiles = std::collections::HashMap<String, (u64, Option<String>)>;

/// The files under `local_path` a sync would read, and whether the walk read
/// all of it. `sync_doctor` and `reconcile` answer from this scan, so a
/// folder or file the walk could not read is counted in `list_errors` and a
/// folder at the depth cap marks it truncated, as `aeroftp-cli sync-doctor`
/// and `reconcile` report it: a file never seen would otherwise read as
/// missing on this side.
fn scan_local_files(
    local_path: &std::path::Path,
    exclude: &crate::sync_exclude::ExcludeMatcher,
) -> (LocalFiles, crate::sync_core::ScanCompleteness) {
    let relative_of = |path: &std::path::Path| {
        crate::sync::normalize_relative_key(
            &path
                .strip_prefix(local_path)
                .unwrap_or(path)
                .to_string_lossy(),
        )
    };
    let mut files = LocalFiles::new();
    let mut completeness = crate::sync_core::ScanCompleteness::default();
    for entry in walkdir::WalkDir::new(local_path)
        .follow_links(false)
        .max_depth(MAX_BFS_DEPTH)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !exclude.is_excluded(&relative_of(e.path())))
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                completeness.list_errors += 1;
                tracing::debug!("local scan: cannot read {:?}: {error}", error.path());
                continue;
            }
        };
        if entry.file_type().is_dir() && entry.depth() == MAX_BFS_DEPTH {
            // Not entered: what it holds is unseen.
            completeness.truncated = true;
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = relative_of(entry.path());
        if rel.is_empty() || exclude.is_excluded(&rel) {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                completeness.list_errors += 1;
                tracing::debug!("local scan: cannot stat {rel}: {error}");
                continue;
            }
        };
        let mtime_secs = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        files.insert(rel, (metadata.len(), mtime_secs));
    }
    (files, completeness)
}

/// The files under `remote_dir` a sync would read, each path once, and
/// whether the walk read all of it. Only folders strictly under `remote_dir`
/// are listed, each once ([`walk_admits`]). A folder the walk could not list
/// or did not follow is counted in `list_errors`, and a cap reached marks
/// the scan truncated.
async fn scan_remote_files(
    ctx: &dyn ToolCtx,
    backend: &dyn RemoteBackend,
    remote_dir: &str,
    exclude: &crate::sync_exclude::ExcludeMatcher,
    tool: &str,
) -> Result<(RemoteFiles, crate::sync_core::ScanCompleteness), ToolError> {
    let mut files = RemoteFiles::new();
    let mut completeness = crate::sync_core::ScanCompleteness::default();
    let mut scope = crate::sync_core::WalkScope::new(remote_dir);
    let mut refused: u32 = 0;
    let mut queue: Vec<(String, usize)> = vec![(remote_dir.to_string(), 0)];
    while let Some((dir, depth)) = queue.pop() {
        if depth >= MAX_BFS_DEPTH {
            completeness.truncated = true;
            continue;
        }
        if files.len() >= MAX_BFS_ENTRIES {
            completeness.truncated = true;
            break;
        }
        crate::ai_core::tools::check_cancelled(ctx)?;
        let entries = match backend.list(&dir).await {
            Ok(entries) => entries,
            Err(error) => {
                completeness.list_errors += 1;
                tracing::debug!("{tool}: cannot list {dir}: {error}");
                continue;
            }
        };
        if depth == 0 {
            scope.learn_root_listing(&entries);
        }
        for e in entries {
            let rel = e
                .path
                .strip_prefix(remote_dir)
                .unwrap_or(&e.path)
                .trim_start_matches('/');
            if e.is_dir {
                if e.is_walkable_dir()
                    && !exclude.is_excluded_entry(rel, &e.name)
                    && walk_admits(&mut scope, &dir, &e.path, true, &mut refused, tool)
                {
                    queue.push((e.path.clone(), depth + 1));
                }
            } else if !rel.is_empty()
                && !exclude.is_excluded_entry(rel, &e.name)
                && walk_admits(&mut scope, &dir, &e.path, false, &mut refused, tool)
            {
                files.insert(rel.to_string(), (e.size, e.modified.clone()));
            }
        }
    }
    completeness.list_errors += refused as usize;
    Ok((files, completeness))
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
    let resolved_root = backend.stat(&root).await.ok().filter(|entry| entry.is_dir);
    let mut scope = crate::sync_core::WalkScope::for_delete(
        &root,
        resolved_root.as_ref().map(|entry| entry.path.as_str()),
    );

    while let Some((dir, depth)) = dirs.pop() {
        if depth >= MAX_BFS_DEPTH {
            continue;
        }
        if orphans.len() >= MAX_BFS_ENTRIES {
            break;
        }
        crate::ai_core::tools::check_cancelled(ctx)?;
        match backend.list(&dir).await {
            Ok(entries) => {
                if depth == 0 {
                    scope.learn_root_listing(&entries);
                }
                for entry in entries {
                    if entry.is_dir {
                        if entry.is_walkable_dir()
                            && delete_walk_admits(
                                &mut scope,
                                &dir,
                                &entry.path,
                                true,
                                &mut scan_errors,
                                "cleanup",
                            )
                        {
                            dirs.push((entry.path.clone(), depth + 1));
                        }
                    } else if (entry.name.ends_with(".aerotmp") || entry.path.ends_with(".aerotmp"))
                        && delete_walk_admits(
                            &mut scope,
                            &dir,
                            &entry.path,
                            false,
                            &mut scan_errors,
                            "cleanup",
                        )
                    {
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

    crate::sync_core::retain_first_per_path(&mut orphans, |(path, _)| path.as_str());
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
    // Whether the path came from the caller decides how strict the overwrite
    // guard below has to be: an auto-generated UUID scratch path cannot be
    // user data, a caller-named one can be anything.
    let caller_named_path = get_str_opt(args, "remote_path");
    let path_is_caller_named = caller_named_path.is_some();
    let remote_path = caller_named_path.unwrap_or_else(|| {
        crate::speed_payload::name(&format!("/.aeroftp-speedtest-{}", uuid::Uuid::new_v4()))
    });
    validate_remote_path(&remote_path, "remote_path")?;

    let size_bytes = size_mb * 1024 * 1024;

    // Allocate once, but stamp fresh entropy into every iteration below.
    // Re-uploading identical bytes lets content-addressed providers deduplicate
    // iterations 2+ and report impossible throughput instead of a transfer.
    let mut payload: Vec<u8> = (0..size_bytes)
        .map(|i| ((i ^ 0x9e37_79b9) & 0xff) as u8)
        .collect();
    let mut upload_sha = String::new();

    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;

    reject_restricted_leaf(backend.as_ref(), &remote_path).await?;

    // Overwrite guard: the test uploads to remote_path and then deletes it,
    // so a path that already exists is a real user file about to be destroyed.
    // Refuse instead of clobbering (no overwrite flag exists by design).
    //
    // The guard fails CLOSED on a caller-named path: `stat` returning an error
    // is only taken as "absent" when the provider actually said not-found. An
    // auth failure, a dropped connection or a provider 5xx must not be read as
    // permission to write — that is the same fail-open shape this guard exists
    // to close. An auto-generated UUID scratch path is exempt: it holds no user
    // data by construction, so a flaky `stat` must not block a legitimate run.
    match backend.stat(&remote_path).await {
        Ok(_) => {
            return Err(ToolError::InvalidArgs {
                tool: "aeroftp_speed".to_string(),
                reason: format!(
                    "remote_path '{remote_path}' already exists; the speed test would overwrite and then delete it. Omit remote_path for an auto-generated scratch path, or pick a path that does not exist"
                ),
            });
        }
        Err(e) if path_is_caller_named && !backend_reported_not_found(&e) => {
            return Err(ToolError::InvalidArgs {
                tool: "aeroftp_speed".to_string(),
                reason: format!(
                    "cannot confirm remote_path '{remote_path}' is free: the existence check failed with '{e}'. The speed test overwrites and then deletes this path, so it will not run on an unverified path. Retry, or omit remote_path for an auto-generated scratch path"
                ),
            });
        }
        Err(_) => {}
    }

    let started = std::time::Instant::now();
    let mut upload_total_bps: f64 = 0.0;
    let mut download_total_bps: f64 = 0.0;
    let mut integrity_verified = false;
    let mut last_download_sha = String::new();

    for _ in 0..iterations {
        let nonce = uuid::Uuid::new_v4();
        payload[..nonce.as_bytes().len()].copy_from_slice(nonce.as_bytes());
        if verify_integrity {
            upload_sha = format!("{:x}", sha2::Sha256::digest(&payload));
        }
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

/// A runnable `aeroftp-cli sync` line for a tool result. The profile goes
/// first (`sync LOCAL REMOTE` alone reads LOCAL as the URL, and the command
/// fails), and only flags `sync` really has follow. Every value is one quoted
/// shell argument (`shell_quote::shell_arg`). `server` empty means the GUI's
/// active connection, which has no name to put here, so the line carries the
/// `NAME` placeholder the CLI `sync-doctor` uses.
pub fn suggest_sync_command(
    server: &str,
    local_dir: &str,
    remote_dir: &str,
    flags: &str,
) -> String {
    use crate::shell_quote::shell_arg;
    let profile = if server.is_empty() {
        "NAME".to_string()
    } else {
        shell_arg(server)
    };
    format!(
        "aeroftp-cli sync --profile {} {} {}{}",
        profile,
        shell_arg(local_dir),
        shell_arg(remote_dir),
        flags
    )
}

async fn sync_doctor(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let server = normalize_server(args)?;
    let local_dir = get_str(args, "local_dir")?;
    let remote_dir = get_str(args, "remote_dir")?;
    validate_remote_path(&remote_dir, "remote_dir")?;
    validate_local_path(&local_dir, "local_dir")?;
    let direction = get_str_opt(args, "direction").unwrap_or_else(|| "both".to_string());
    // A fixed set: the value is written into the suggested command line.
    if !matches!(direction.as_str(), "upload" | "download" | "both") {
        return Err(ToolError::InvalidArgs {
            tool: "aeroftp_sync_doctor".to_string(),
            reason: format!(
                "direction must be upload, download or both, got '{}'",
                direction
            ),
        });
    }
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

    // The matcher every sync surface shares (the one `aeroftp sync-doctor`
    // uses): an invalid pattern is reported, never skipped, so the tool never
    // answers for a narrower exclude list than the one it was given.
    let exclude_matchers =
        crate::sync::compile_excludes(&exclude).map_err(|reason| ToolError::InvalidArgs {
            tool: "aeroftp_sync_doctor".to_string(),
            reason,
        })?;

    // Both scans say whether they read the whole tree: counts from a scan
    // that missed part of it are partial, and the report says so.
    let (local_map, local_scan) = scan_local_files(local_path, &exclude_matchers);
    let local_files = local_map.len();
    let local_bytes = local_map
        .values()
        .fold(0u64, |sum, (size, _)| sum.saturating_add(*size));

    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;
    let remote_root_ok = backend.list(&remote_dir).await.is_ok();
    let (remote_map, remote_scan) = if remote_root_ok {
        scan_remote_files(
            ctx,
            backend.as_ref(),
            &remote_dir,
            &exclude_matchers,
            "sync_doctor",
        )
        .await?
    } else {
        (
            RemoteFiles::new(),
            crate::sync_core::ScanCompleteness::default(),
        )
    };
    let remote_files = remote_map.len();
    let remote_bytes = remote_map
        .values()
        .fold(0u64, |sum, (size, _)| sum.saturating_add(*size));
    let scans = [("local", &local_scan), ("remote", &remote_scan)];
    let scans_complete = scans.iter().all(|(_, scan)| scan.is_complete());

    let mut checks = vec![
        json!({"name": "local_path_exists", "ok": true, "path": local_dir}),
        json!({"name": "remote_path_reachable", "ok": remote_root_ok, "path": remote_dir}),
    ];
    for (side, scan) in scans {
        checks.push(json!({
            "name": format!("{side}_scan_complete"),
            "ok": scan.is_complete(),
            "errors": scan.list_errors,
            "truncated": scan.truncated,
        }));
    }
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
        // `sync` compares size and mtime and has no --checksum; content
        // verification is `check --checksum`, run after the sync.
        risks.push(
            "checksum requested: sync compares size and mtime; verify contents afterwards with `aeroftp-cli check --checksum`"
                .to_string(),
        );
    }
    if !remote_root_ok {
        risks.push("remote path could not be listed".to_string());
    }
    for (side, scan) in scans {
        if !scan.is_complete() {
            risks.push(format!(
                "the {side} scan did not read the whole tree ({} error(s){}); the file counts are partial",
                scan.list_errors,
                if scan.truncated { ", truncated" } else { "" }
            ));
        }
    }
    if local_files == 0 && remote_files == 0 && scans_complete {
        risks.push("both sides are empty; sync will be a no-op".to_string());
    }

    let mut flags = format!(" --direction {} --dry-run --json", direction);
    if delete {
        flags.push_str(" --delete");
    }
    if track_renames {
        flags.push_str(" --track-renames");
    }
    for pattern in &exclude {
        flags.push_str(&format!(
            " --exclude {}",
            crate::shell_quote::shell_arg(pattern)
        ));
    }
    let suggested_next_command = suggest_sync_command(&server, &local_dir, &remote_dir, &flags);

    Ok(json!({
        "server": server,
        "status": if remote_root_ok && scans_complete { "ok" } else { "attention" },
        "summary": {
            "direction": direction,
            "local_files": local_files,
            "local_bytes": local_bytes,
            "remote_files": remote_files,
            "remote_bytes": remote_bytes,
            "delete": delete,
            "track_renames": track_renames,
            "checksum": checksum,
            "local_scan_incomplete": !local_scan.is_complete(),
            "local_scan_errors": local_scan.list_errors,
            "local_scan_truncated": local_scan.truncated,
            "remote_scan_incomplete": !remote_scan.is_complete(),
            "remote_scan_errors": remote_scan.list_errors,
            "remote_scan_truncated": remote_scan.truncated,
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

    // Phase 3: new optional params (back-compat: default "exact" keeps old fast-path 100%)
    let similarity = get_str_opt(args, "similarity").unwrap_or_else(|| "exact".to_string());
    let sim_mode = SimilarityMode::parse(&similarity);
    let distance = args
        .get("distance")
        .and_then(|v| v.as_u64())
        .map(|d| d as u32);
    let is_non_identical = sim_mode == SimilarityMode::NonIdentical;

    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;

    // BFS scan to collect (path, size, mtime). Same for both modes.
    let mut files: Vec<(String, u64, Option<String>)> = Vec::new();
    let mut dirs: Vec<(String, usize)> = vec![(root.clone(), 0)];
    let mut scan_errors: u32 = 0;
    let resolved_root = backend.stat(&root).await.ok().filter(|entry| entry.is_dir);
    let mut scope = crate::sync_core::WalkScope::for_delete(
        &root,
        resolved_root.as_ref().map(|entry| entry.path.as_str()),
    );
    while let Some((dir, depth)) = dirs.pop() {
        if depth >= MAX_BFS_DEPTH || files.len() >= MAX_BFS_ENTRIES {
            continue;
        }
        crate::ai_core::tools::check_cancelled(ctx)?;
        match backend.list(&dir).await {
            Ok(entries) => {
                if depth == 0 {
                    scope.learn_root_listing(&entries);
                }
                for e in entries {
                    if e.is_dir {
                        if e.is_walkable_dir()
                            && delete_walk_admits(
                                &mut scope,
                                &dir,
                                &e.path,
                                true,
                                &mut scan_errors,
                                "dedupe",
                            )
                        {
                            dirs.push((e.path, depth + 1));
                        }
                    } else if delete_walk_admits(
                        &mut scope,
                        &dir,
                        &e.path,
                        false,
                        &mut scan_errors,
                        "dedupe",
                    ) {
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

    // A file is never its own duplicate: whatever a listing did, each path is
    // grouped once, so the copies a group deletes are always other files.
    let repeated =
        crate::sync_core::retain_first_per_path(&mut files, |(path, _, _)| path.as_str());
    if repeated > 0 {
        tracing::warn!(
            "dedupe: {repeated} path(s) were listed more than once and are counted once"
        );
    }

    let mut hash_errors: u32 = 0;
    let mut duplicate_groups: Vec<Vec<(String, u64, Option<String>)>> = Vec::new();
    let mut total_duplicates: u32 = 0;
    let mut wasted_bytes: u64 = 0;

    if is_non_identical {
        // Non-identical path (Phase 3): download to temp preserving extension so
        // detect_modality + TLSH/SimHash/pHash work inside the shared engine.
        // Reuses the exact staging + remap pattern from aeroftp_cli.rs dedupe --similarity non-identical.
        // Exact fast-path (below) is left 100% untouched.
        let temp_dir = match tempfile::tempdir() {
            Ok(d) => d,
            Err(e) => {
                return Err(ToolError::Exec(format!(
                    "Failed to create temp dir for non-identical dedupe: {}",
                    e
                )));
            }
        };

        let mut temp_map: std::collections::HashMap<
            std::path::PathBuf,
            (String, u64, Option<String>),
        > = std::collections::HashMap::new();

        let mut dl_errors = 0u32;

        for (orig_path, sz, mtime) in &files {
            if *sz == 0 || *sz > MAX_DEDUPE_FILE_BYTES {
                hash_errors += 1;
                continue;
            }
            let p = std::path::Path::new(orig_path);
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("bin");
            let tmp_name = format!("dedupe_{}_{}.{}", files.len(), temp_map.len(), ext);
            let tmp_path = temp_dir.path().join(&tmp_name);

            match backend
                .download_to_bytes_capped(orig_path, MAX_DEDUPE_FILE_BYTES)
                .await
            {
                Ok(bytes) => {
                    if let Err(e) = std::fs::write(&tmp_path, &bytes) {
                        dl_errors += 1;
                        tracing::debug!("dedupe(non-id): stage fail {}: {}", orig_path, e);
                        continue;
                    }
                    temp_map.insert(tmp_path.clone(), (orig_path.clone(), *sz, mtime.clone()));
                }
                Err(e) => {
                    dl_errors += 1;
                    tracing::debug!(
                        "dedupe: failed to download {} for similarity: {}",
                        orig_path,
                        e
                    );
                    continue;
                }
            }
        }

        let tmp_paths: Vec<std::path::PathBuf> = temp_map.keys().cloned().collect();
        let sim_groups = dedupe::find_similar_local(&tmp_paths, sim_mode, distance, None);

        for g in sim_groups {
            if g.files.len() < 2 {
                continue;
            }
            let mut remapped: Vec<(String, u64, Option<String>)> = Vec::new();
            let mut group_size = 0u64;
            for tf in &g.files {
                if let Some((orig, sz, mt)) = temp_map.get(std::path::Path::new(tf)) {
                    if group_size == 0 {
                        group_size = *sz;
                    }
                    remapped.push((orig.clone(), *sz, mt.clone()));
                }
            }
            if remapped.len() >= 2 {
                let dupes = (remapped.len() as u32).saturating_sub(1);
                total_duplicates += dupes;
                wasted_bytes += group_size * dupes as u64;
                duplicate_groups.push(remapped);
            }
        }

        // Note: dl_errors folded into hash_errors for the final report (consistent with CLI non-id path)
        if dl_errors > 0 {
            hash_errors += dl_errors;
        }
    } else {
        // EXACT fast-path: 100% unchanged per Phase 3 spec (size prefilter + SHA-256).
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

        for (size, candidates) in candidate_groups {
            if size > MAX_DEDUPE_FILE_BYTES {
                // Skip oversize files: would blow the agent memory budget.
                hash_errors += candidates.len() as u32;
                continue;
            }
            let mut hash_map: std::collections::HashMap<
                String,
                Vec<(String, u64, Option<String>)>,
            > = std::collections::HashMap::new();
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
    }

    // Sort each group to determine the keeper (applies to both exact and non-id).
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

    // Augment response for non-identical (top level) while keeping exact shape stable.
    let mut out = json!({
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
    });
    if is_non_identical {
        out["similarity"] = json!("non-identical");
        if let Some(d) = distance {
            out["distance"] = json!(d);
        }
    } else {
        out["similarity"] = json!("exact");
    }

    Ok(out)
}

/// One equal-size pair checked by content for the `checksum` reconcile path.
enum PairChecksum {
    /// Both sides hashed to the same blake3 digest (carries the digest).
    Equal(String),
    /// Same size, different content: the case a size-only compare cannot see.
    Different { local: String, remote: String },
    /// Over the per-file memory cap; the pair is reported by size, flagged.
    SkippedTooLarge,
    /// A side could not be read or hashed; the pair is reported by size,
    /// flagged, and counted. Never silently called a match by content.
    Failed(String),
}

/// Hash one equal-size local/remote pair. Both digests are computed here with
/// blake3: the local side from disk, the remote side from a capped download
/// (the same ceiling `hashsum` uses to protect the agent process). Unlike the
/// CLI, which prefers server-side checksums where the provider offers them,
/// this route always reads the remote bytes, so it is provider-independent
/// and its cost is the download itself.
///
/// WHERE THE CAP IS ENFORCED, and it is not the same everywhere. Providers that
/// override `download_to_bytes_capped` (SFTP, MTP) stop reading as soon as the
/// accumulated bytes would pass the cap, so an oversized body is never held.
/// The others inherit the default in `providers::StorageProvider`, which reads
/// first and refuses after; their read is still bounded, by the streaming guard
/// in `response_bytes_with_limit` at `MAX_DOWNLOAD_TO_BYTES`, so a server that
/// under-reports the size in its listing can make this path materialize up to
/// that larger ceiling before the refusal. Narrowing it for every provider is a
/// change in the provider layer, not here: this call already asks for the
/// tighter cap and gets it wherever a provider can honor it.
async fn checksum_pair(
    backend: &dyn RemoteBackend,
    local_root: &std::path::Path,
    remote_root: &str,
    rel: &str,
    size: u64,
) -> PairChecksum {
    if size > MAX_HASHSUM_BYTES {
        return PairChecksum::SkippedTooLarge;
    }
    let mut local_file = local_root.to_path_buf();
    for component in rel.split('/') {
        local_file.push(component);
    }
    let local_hash = match std::fs::read(&local_file) {
        Ok(data) => blake3::hash(&data).to_hex().to_string(),
        Err(e) => return PairChecksum::Failed(format!("local read failed: {e}")),
    };
    let remote_path = format!("{}/{}", remote_root.trim_end_matches('/'), rel);
    match backend
        .download_to_bytes_capped(&remote_path, MAX_HASHSUM_BYTES)
        .await
    {
        Ok(data) => {
            let remote_hash = blake3::hash(&data).to_hex().to_string();
            if remote_hash == local_hash {
                PairChecksum::Equal(local_hash)
            } else {
                PairChecksum::Different {
                    local: local_hash,
                    remote: remote_hash,
                }
            }
        }
        Err(e) => PairChecksum::Failed(format!("remote read failed: {e}")),
    }
}

async fn reconcile(ctx: &dyn ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let server = normalize_server(args)?;
    let local_dir = get_str(args, "local_dir")?;
    let remote_dir = get_str(args, "remote_dir")?;
    validate_remote_path(&remote_dir, "remote_dir")?;
    validate_local_path(&local_dir, "local_dir")?;
    let checksum = get_bool_opt(args, "checksum").unwrap_or(false);
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

    // Same matcher and same rule for invalid patterns as `aeroftp reconcile`.
    let exclude_matchers =
        crate::sync::compile_excludes(&exclude).map_err(|reason| ToolError::InvalidArgs {
            tool: "aeroftp_reconcile".to_string(),
            reason,
        })?;

    let started = std::time::Instant::now();

    // Both scans say whether they read the whole tree: a comparison against
    // a scan that missed part of it is `partial`, not a verdict, since a file
    // never seen reads as missing on that side.
    let (local_map, local_scan) = scan_local_files(local_path, &exclude_matchers);
    let backend = ctx.remote_backend(&server).await.map_err(backend_error)?;
    let (remote_map, remote_scan) = scan_remote_files(
        ctx,
        backend.as_ref(),
        &remote_dir,
        &exclude_matchers,
        "reconcile",
    )
    .await?;

    // Compare.
    let mut matches_g: Vec<Value> = Vec::new();
    let mut differ_g: Vec<Value> = Vec::new();
    let mut missing_remote_g: Vec<Value> = Vec::new();
    let mut missing_local_g: Vec<Value> = Vec::new();
    let mut checksum_hashed: u64 = 0;
    let mut checksum_skipped_large: u64 = 0;
    let mut checksum_errors: u64 = 0;

    for (rel, (lsize, _lmtime)) in &local_map {
        match remote_map.get(rel) {
            Some((rsize, _rmtime)) => {
                if lsize == rsize {
                    // With `checksum` the size verdict is not enough: equal
                    // length says nothing about equal content. Each pair is
                    // hashed; pairs that cannot be hashed keep their size
                    // verdict but say so on the entry and in the summary.
                    if checksum {
                        match checksum_pair(backend.as_ref(), local_path, &remote_dir, rel, *lsize)
                            .await
                        {
                            PairChecksum::Equal(hash) => {
                                checksum_hashed += 1;
                                matches_g.push(json!({
                                    "path": rel,
                                    "local_size": lsize,
                                    "remote_size": rsize,
                                    "compare_method": "blake3",
                                    "checksum": hash,
                                }));
                            }
                            PairChecksum::Different { local, remote } => {
                                checksum_hashed += 1;
                                differ_g.push(json!({
                                    "path": rel,
                                    "local_size": lsize,
                                    "remote_size": rsize,
                                    "compare_method": "blake3",
                                    "local_checksum": local,
                                    "remote_checksum": remote,
                                }));
                            }
                            PairChecksum::SkippedTooLarge => {
                                checksum_skipped_large += 1;
                                matches_g.push(json!({
                                    "path": rel,
                                    "local_size": lsize,
                                    "remote_size": rsize,
                                    "compare_method": "size",
                                    "checksum": "skipped_too_large",
                                }));
                            }
                            PairChecksum::Failed(reason) => {
                                checksum_errors += 1;
                                matches_g.push(json!({
                                    "path": rel,
                                    "local_size": lsize,
                                    "remote_size": rsize,
                                    "compare_method": "size",
                                    "checksum": "error",
                                    "checksum_error": reason,
                                }));
                            }
                        }
                    } else {
                        matches_g.push(json!({
                            "path": rel,
                            "local_size": lsize,
                            "remote_size": rsize,
                            "compare_method": "size",
                        }));
                    }
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
    let suggested_next_command =
        suggest_sync_command(&server, &local_dir, &remote_dir, " --dry-run --json");

    let status = if !local_scan.is_complete() || !remote_scan.is_complete() {
        "partial"
    } else if differ_g.is_empty() && missing_remote_g.is_empty() && missing_local_g.is_empty() {
        "ok"
    } else {
        "differences_found"
    };

    let summary = json!({
        "match_count": matches_g.len(),
        "differ_count": differ_g.len(),
        "missing_remote_count": missing_remote_g.len(),
        "missing_local_count": missing_local_g.len(),
        "local_scan_incomplete": !local_scan.is_complete(),
        "local_scan_errors": local_scan.list_errors,
        "local_scan_truncated": local_scan.truncated,
        "remote_scan_incomplete": !remote_scan.is_complete(),
        "remote_scan_errors": remote_scan.list_errors,
        "remote_scan_truncated": remote_scan.truncated,
        "elapsed_secs": elapsed,
        "checksum": checksum,
        "checksum_hashed": checksum_hashed,
        "checksum_skipped_large": checksum_skipped_large,
        "checksum_errors": checksum_errors,
    });

    let compare_method = if checksum { "blake3" } else { "size" };

    if summary_only {
        return Ok(json!({
            "server": server,
            "status": status,
            "local_dir": local_dir,
            "remote_dir": remote_dir,
            "summary": summary,
            "summary_only": true,
            "suggested_next_command": suggested_next_command,
            "compare_method": compare_method,
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
        "compare_method": compare_method,
        "suggested_next_command": suggested_next_command,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ProviderType;

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

    #[cfg(unix)]
    #[test]
    fn literal_unix_paths_agent_scan_keeps_distinct_entries() {
        let dir = tempfile::tempdir().unwrap();
        let entries = [
            (r"back\slash.txt", "literal"),
            ("back/slash.txt", "nested twin"),
            (r"dir\name/leaf\name.txt", "literal nested"),
            ("dir/name/leaf/name.txt", "actual nested twin"),
        ];
        for (rel, bytes) in entries {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        let (files, complete) =
            scan_local_files(dir.path(), &crate::sync_exclude::ExcludeMatcher::default());
        assert!(complete.is_complete());
        assert_eq!(files.len(), 4);
        for (rel, bytes) in entries {
            assert_eq!(files[rel].0, bytes.len() as u64);
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

    // --- dry_run / recursive contract for the delete verbs ----------------

    use crate::ai_core::credential_provider::ProviderExtraOptions;
    use crate::ai_core::remote_backend::StorageQuota;
    use crate::ai_core::{CredentialProvider, EventSink, ServerCredentials, ServerProfile};
    use crate::ai_stream::StreamChunk;
    use crate::providers::RemoteEntry;
    use std::sync::{Arc, Mutex};

    fn entry(path: &str, is_dir: bool, size: u64) -> RemoteEntry {
        RemoteEntry {
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            path: path.to_string(),
            is_dir,
            size,
            modified: None,
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: None,
            metadata: std::collections::HashMap::new(),
        }
    }

    /// In-memory tree that records every destructive call, so a test can
    /// assert that a dry-run touched nothing.
    struct FakeBackend {
        /// dir path -> children
        tree: std::collections::HashMap<String, Vec<RemoteEntry>>,
        /// path -> (is_dir, size)
        stats: std::collections::HashMap<String, (bool, u64)>,
        downloads: std::collections::HashMap<String, Vec<u8>>,
        /// High-water mark of bytes materialized by the last
        /// `download_to_bytes_capped` call, so a test can prove the streaming
        /// cap never held the full (size-lying) body in memory.
        capped_high_water: Mutex<usize>,
        remote_files: Mutex<std::collections::HashMap<String, Vec<u8>>>,
        uploads: Mutex<Vec<(String, Vec<u8>)>>,
        renames: Mutex<Vec<(String, String)>>,
        deleted: Mutex<Vec<String>>,
        deleted_recursive: Mutex<Vec<String>>,
        /// Every `empty_object_versions` call as (prefix, include_noncurrent,
        /// dry_run), so trash tests can assert the flags the tool passed.
        empty_calls: Mutex<Vec<(String, bool, bool)>>,
        /// When set, `delete` fails with this message, the way a provider's
        /// `rmdir` fails on a non-empty directory.
        delete_fails_with: Option<String>,
        rename_fails_with: Option<String>,
        /// When set, `stat` fails with this message for every path, the way a
        /// provider fails on an expired token or a dropped connection.
        stat_fails_with: Option<String>,
        list_fails_with: Option<String>,
        listing_authoritative: bool,
        /// When set, `mkdir` fails with this message instead of the default
        /// "unused", so a test can hand the walk the wording a real server
        /// uses when the path is already taken.
        mkdir_fails_with: Option<String>,
        mkdir_calls: Mutex<Vec<String>>,
        /// Paths a listing would mark with a leading `l`. `is_dir` stays
        /// false for these, exactly as the real parser reports them.
        symlinks: std::collections::HashSet<String>,
        /// The provider this fake answers as. FTP by default (a provider
        /// that refuses control characters), switchable so a test can stand
        /// on the encoding side of the rule (Box/Dropbox/Jottacloud/OpenDrive).
        provider_type: Option<ProviderType>,
        /// What `supports_atomic_replace` answers. True, matching the trait
        /// default, unless a test stands on a set-aside backend.
        atomic_replace: bool,
        /// What `replace_sets_aside` answers. False, matching the trait
        /// default: with `atomic_replace` false too, this is a backend whose
        /// replace is its rename.
        sets_aside: bool,
        /// Unix mode per path, reported by `stat` as `-rw-r--r--`. A new
        /// path gets 0644, the server default; `rename` moves the mode of
        /// the file it moves, as an SFTP posix-rename does.
        modes: Mutex<std::collections::HashMap<String, u32>>,
        /// What `supports_chmod` answers.
        chmod_supported: bool,
        /// When set, `chmod` fails with this message.
        chmod_fails_with: Option<String>,
        /// Runs at every `upload`, before it fails as unused: a test flips a
        /// cancel token from here, the way Stop lands while a file is in
        /// flight.
        on_upload: Option<Arc<dyn Fn() + Send + Sync>>,
        /// Every path `list` and `stat` were asked, so a test can count the
        /// probes a batch made.
        list_calls: Mutex<Vec<String>>,
        stat_calls: Mutex<Vec<String>>,
        /// When set, `upload` lands and records its remote path in
        /// `local_uploads` instead of failing as unused.
        upload_succeeds: bool,
        local_uploads: Mutex<Vec<String>>,
        /// The outcome a native bulk reports for each item. `None` is a
        /// backend without one, as the trait default.
        #[allow(clippy::type_complexity)]
        bulk_outcome: Option<Arc<dyn Fn(&BulkTransferItem) -> BulkTransferOutcome + Send + Sync>>,
        /// Remote paths of every bulk that carried items, in call order.
        bulk_calls: Mutex<Vec<Vec<String>>>,
        /// When set, a bulk never returns once it has drawn its outcomes,
        /// so a test can stop the batch while the bulk is in flight.
        bulk_hangs: bool,
    }

    impl FakeBackend {
        /// `/root` holds `a.txt` (100 B), `b.txt` (200 B) and `sub/`, which
        /// holds `c.txt` (300 B). Total 600 B over 3 files and 2 dirs.
        fn sample() -> Self {
            let mut tree = std::collections::HashMap::new();
            tree.insert(
                "/root".to_string(),
                vec![
                    entry("/root/a.txt", false, 100),
                    entry("/root/b.txt", false, 200),
                    entry("/root/sub", true, 0),
                ],
            );
            tree.insert(
                "/root/sub".to_string(),
                vec![entry("/root/sub/c.txt", false, 300)],
            );
            tree.insert("/empty".to_string(), vec![]);
            let mut stats = std::collections::HashMap::new();
            stats.insert("/root".to_string(), (true, 0));
            stats.insert("/root/sub".to_string(), (true, 0));
            stats.insert("/empty".to_string(), (true, 0));
            stats.insert("/root/a.txt".to_string(), (false, 100));
            Self {
                tree,
                stats,
                downloads: std::collections::HashMap::new(),
                capped_high_water: Mutex::new(0),
                remote_files: Mutex::new(std::collections::HashMap::new()),
                uploads: Mutex::new(Vec::new()),
                renames: Mutex::new(Vec::new()),
                deleted: Mutex::new(Vec::new()),
                deleted_recursive: Mutex::new(Vec::new()),
                empty_calls: Mutex::new(Vec::new()),
                delete_fails_with: None,
                rename_fails_with: None,
                stat_fails_with: None,
                list_fails_with: None,
                listing_authoritative: true,
                mkdir_fails_with: None,
                mkdir_calls: Mutex::new(Vec::new()),
                symlinks: std::collections::HashSet::new(),
                provider_type: Some(ProviderType::Ftp),
                atomic_replace: true,
                sets_aside: false,
                modes: Mutex::new(std::collections::HashMap::new()),
                chmod_supported: false,
                chmod_fails_with: None,
                on_upload: None,
                list_calls: Mutex::new(Vec::new()),
                stat_calls: Mutex::new(Vec::new()),
                upload_succeeds: false,
                local_uploads: Mutex::new(Vec::new()),
                bulk_outcome: None,
                bulk_calls: Mutex::new(Vec::new()),
                bulk_hangs: false,
            }
        }

        fn with_provider_type(mut self, provider_type: ProviderType) -> Self {
            self.provider_type = Some(provider_type);
            self
        }

        fn touched_anything(&self) -> bool {
            !self.deleted.lock().unwrap().is_empty()
                || !self.deleted_recursive.lock().unwrap().is_empty()
        }
    }

    #[async_trait::async_trait]
    impl RemoteBackend for FakeBackend {
        async fn listing_is_authoritative(&self) -> bool {
            self.listing_authoritative
        }
        async fn is_connected(&self) -> bool {
            true
        }
        async fn provider_type(&self) -> Option<ProviderType> {
            self.provider_type
        }
        async fn list(&self, path: &str) -> Result<Vec<RemoteEntry>, String> {
            self.list_calls.lock().unwrap().push(path.to_string());
            if let Some(error) = &self.list_fails_with {
                return Err(error.clone());
            }
            self.tree.get(path).cloned().ok_or_else(|| {
                crate::providers::ProviderError::NotFound(path.to_string()).to_string()
            })
        }
        async fn stat(&self, path: &str) -> Result<RemoteEntry, String> {
            self.stat_calls.lock().unwrap().push(path.to_string());
            if let Some(msg) = &self.stat_fails_with {
                return Err(msg.clone());
            }
            let (is_dir, size) = *self.stats.get(path).ok_or_else(|| {
                crate::providers::ProviderError::NotFound(path.to_string()).to_string()
            })?;
            let mut e = entry(path, is_dir, size);
            e.is_symlink = self.symlinks.contains(path);
            e.permissions = self.modes.lock().unwrap().get(path).map(|mode| {
                let bit = |mask: u32, letter: char| if mode & mask != 0 { letter } else { '-' };
                [
                    '-',
                    bit(0o400, 'r'),
                    bit(0o200, 'w'),
                    bit(0o100, 'x'),
                    bit(0o040, 'r'),
                    bit(0o020, 'w'),
                    bit(0o010, 'x'),
                    bit(0o004, 'r'),
                    bit(0o002, 'w'),
                    bit(0o001, 'x'),
                ]
                .iter()
                .collect()
            });
            Ok(e)
        }
        async fn download_to_bytes(&self, _path: &str) -> Result<Vec<u8>, String> {
            // Pre-seeded fixture first, then whatever a previous
            // `upload_from_bytes` left there — a real backend reads back what
            // was just written, which is what the speed test relies on for a
            // path it generated itself.
            self.downloads
                .get(_path)
                .cloned()
                .or_else(|| self.remote_files.lock().unwrap().get(_path).cloned())
                .ok_or_else(|| format!("unused download: {_path}"))
        }
        async fn download_to_bytes_capped(
            &self,
            path: &str,
            max_bytes: u64,
        ) -> Result<Vec<u8>, String> {
            // Simulate a streaming reader: append fixed-size chunks and refuse
            // BEFORE the accumulator would cross `max_bytes`, exactly like the
            // real SFTP override. Record the high-water mark so a test can prove
            // the full (size-lying) body was never materialized.
            const CHUNK: usize = 64 * 1024;
            let full = self
                .downloads
                .get(path)
                .ok_or_else(|| format!("unused download: {path}"))?;
            let mut data: Vec<u8> = Vec::new();
            for chunk in full.chunks(CHUNK) {
                if data.len() as u64 + chunk.len() as u64 > max_bytes {
                    *self.capped_high_water.lock().unwrap() = data.len();
                    return Err(format!(
                        "Download exceeded the {:.0} MB cap.",
                        max_bytes as f64 / 1_048_576.0,
                    ));
                }
                data.extend_from_slice(chunk);
            }
            *self.capped_high_water.lock().unwrap() = data.len();
            Ok(data)
        }
        async fn upload_from_bytes(&self, data: &[u8], path: &str) -> Result<(), String> {
            self.modes
                .lock()
                .unwrap()
                .entry(path.to_string())
                .or_insert(0o644);
            self.uploads
                .lock()
                .unwrap()
                .push((path.to_string(), data.to_vec()));
            self.remote_files
                .lock()
                .unwrap()
                .insert(path.to_string(), data.to_vec());
            Ok(())
        }
        async fn download(&self, _remote: &str, _local: &str) -> Result<(), String> {
            Err("unused".into())
        }
        async fn upload(&self, _local: &str, remote: &str) -> Result<(), String> {
            if let Some(hook) = &self.on_upload {
                hook();
            }
            if self.upload_succeeds {
                self.local_uploads.lock().unwrap().push(remote.to_string());
                return Ok(());
            }
            Err("unused".into())
        }
        async fn upload_bulk(
            &self,
            items: &[BulkTransferItem],
        ) -> Option<Result<Vec<BulkTransferOutcome>, String>> {
            let outcome = self.bulk_outcome.as_ref()?;
            if items.is_empty() {
                return Some(Ok(Vec::new()));
            }
            self.bulk_calls
                .lock()
                .unwrap()
                .push(items.iter().map(|i| i.remote_path.clone()).collect());
            let outcomes = items.iter().map(outcome.as_ref()).collect();
            if self.bulk_hangs {
                std::future::pending::<()>().await;
            }
            Some(Ok(outcomes))
        }
        async fn delete(&self, path: &str) -> Result<(), String> {
            if let Some(msg) = &self.delete_fails_with {
                return Err(msg.clone());
            }
            self.remote_files.lock().unwrap().remove(path);
            self.deleted.lock().unwrap().push(path.to_string());
            Ok(())
        }
        async fn delete_recursive(&self, path: &str) -> Result<(), String> {
            self.deleted_recursive
                .lock()
                .unwrap()
                .push(path.to_string());
            Ok(())
        }
        async fn mkdir(&self, path: &str) -> Result<(), String> {
            self.mkdir_calls.lock().unwrap().push(path.to_string());
            Err(self
                .mkdir_fails_with
                .clone()
                .unwrap_or_else(|| "unused".to_string()))
        }
        async fn supports_atomic_replace(&self) -> Result<bool, String> {
            Ok(self.atomic_replace)
        }
        async fn replace_sets_aside(&self) -> Result<bool, String> {
            Ok(self.sets_aside)
        }
        async fn supports_chmod(&self) -> Result<bool, String> {
            Ok(self.chmod_supported)
        }
        async fn chmod(&self, path: &str, mode: u32) -> Result<(), String> {
            if let Some(msg) = &self.chmod_fails_with {
                return Err(msg.clone());
            }
            self.modes.lock().unwrap().insert(path.to_string(), mode);
            Ok(())
        }
        async fn rename(&self, from: &str, to: &str) -> Result<(), String> {
            if let Some(msg) = &self.rename_fails_with {
                return Err(msg.clone());
            }
            let data = self
                .remote_files
                .lock()
                .unwrap()
                .remove(from)
                .ok_or_else(|| format!("not found: {from}"))?;
            self.remote_files
                .lock()
                .unwrap()
                .insert(to.to_string(), data);
            let moved = self.modes.lock().unwrap().remove(from);
            if let Some(mode) = moved {
                self.modes.lock().unwrap().insert(to.to_string(), mode);
            }
            self.renames
                .lock()
                .unwrap()
                .push((from.to_string(), to.to_string()));
            Ok(())
        }
        async fn search(&self, _path: &str, _pattern: &str) -> Result<Vec<RemoteEntry>, String> {
            Err("unused".into())
        }
        async fn storage_info(&self) -> Result<StorageQuota, String> {
            Err("unused".into())
        }
        async fn empty_object_versions(
            &self,
            prefix: &str,
            include_noncurrent: bool,
            dry_run: bool,
        ) -> Result<(u64, u64), String> {
            self.empty_calls.lock().unwrap().push((
                prefix.to_string(),
                include_noncurrent,
                dry_run,
            ));
            Ok((7, 1234))
        }
    }

    struct NoopSink;

    impl EventSink for NoopSink {
        fn emit_stream_chunk(&self, _stream_id: &str, _chunk: &StreamChunk) {}
        fn emit_tool_progress(&self, _progress: &crate::ai_core::event_sink::ToolProgress) {}
        fn emit_app_control(&self, _event_name: &str, _payload: &serde_json::Value) {}
    }

    /// Empty by default, as every existing test expects; `list_servers`
    /// needs profiles to have anything to report, so it supplies its own.
    #[derive(Default)]
    struct NoopCreds {
        profiles: Vec<ServerProfile>,
    }

    impl CredentialProvider for NoopCreds {
        fn list_servers(&self) -> Result<Vec<ServerProfile>, String> {
            Ok(self.profiles.clone())
        }

        fn get_credentials(&self, _server_id: &str) -> Result<ServerCredentials, String> {
            Err("unused".to_string())
        }

        fn get_extra_options(&self, _server_id: &str) -> Result<ProviderExtraOptions, String> {
            Ok(ProviderExtraOptions::new())
        }
    }

    struct TestCtx {
        backend: Arc<dyn RemoteBackend>,
        sink: NoopSink,
        creds: NoopCreds,
        cancel: Option<tokio_util::sync::CancellationToken>,
    }

    #[async_trait::async_trait]
    impl ToolCtx for TestCtx {
        fn event_sink(&self) -> &dyn EventSink {
            &self.sink
        }

        fn credentials(&self) -> &dyn CredentialProvider {
            &self.creds
        }

        async fn remote_backend(&self, _server_id: &str) -> Result<Arc<dyn RemoteBackend>, String> {
            Ok(Arc::clone(&self.backend))
        }

        fn surface(&self) -> crate::ai_core::tools::Surfaces {
            crate::ai_core::tools::Surfaces::MCP
        }

        fn cancel_token(&self) -> Option<&tokio_util::sync::CancellationToken> {
            self.cancel.as_ref()
        }
    }

    /// M9 (4.2.1 review): Stop reached the chat request and the stream, and a
    /// batch upload ran on to its last file. The token flipped while the
    /// first file is in flight stops the batch before the second.
    #[tokio::test]
    async fn stop_ends_an_upload_batch_after_the_file_in_flight() {
        let token = tokio_util::sync::CancellationToken::new();
        let flip = token.clone();
        let backend = Arc::new(FakeBackend {
            on_upload: Some(Arc::new(move || flip.cancel())),
            ..FakeBackend::sample()
        });
        let ctx = TestCtx {
            cancel: Some(token),
            ..test_ctx(Arc::clone(&backend))
        };
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("f.txt");
        std::fs::write(&local, b"x").unwrap();
        let local = local.to_string_lossy().to_string();
        let args = json!({
            "server": "srv",
            "items": [
                {"local_path": local, "remote_path": "/root/one.txt"},
                {"local_path": local, "remote_path": "/root/two.txt"},
            ],
        });
        let err = upload_many(&ctx, &args).await.unwrap_err();
        assert!(matches!(err, ToolError::Cancelled), "{err}");

        // Without a Stop the same batch reaches its second file.
        let backend = Arc::new(FakeBackend::sample());
        let plain = test_ctx(Arc::clone(&backend));
        let report = upload_many(&plain, &args).await.unwrap();
        assert_eq!(report["summary"]["processed"], 2);
    }

    /// Local files named as their remote leaves under `/root`, the shape a
    /// native bulk accepts, and the `upload_many` items that send them.
    fn bulk_fixture(names: &[&str]) -> (tempfile::TempDir, Vec<Value>) {
        let dir = tempfile::tempdir().unwrap();
        let items = names
            .iter()
            .map(|name| {
                let local = dir.path().join(name);
                std::fs::write(&local, name.as_bytes()).unwrap();
                json!({
                    "local_path": local.to_string_lossy(),
                    "remote_path": format!("/root/{name}"),
                })
            })
            .collect();
        (dir, items)
    }

    fn bulk_backend(
        outcome: impl Fn(&BulkTransferItem) -> BulkTransferOutcome + Send + Sync + 'static,
    ) -> FakeBackend {
        FakeBackend {
            upload_succeeds: true,
            bulk_outcome: Some(Arc::new(outcome)),
            ..FakeBackend::sample()
        }
    }

    /// Proton Drive in CLI mode launched one `proton-drive` process per
    /// `aeroftp_upload_many` item, 1 to 3 s each. Plain files under one
    /// folder now make one native call, and every item keeps its own result,
    /// in order, in the shape `upload_file` reports.
    #[tokio::test]
    async fn upload_many_sends_one_folder_as_one_native_bulk() {
        let names = ["n0.txt", "n1.txt", "n2.txt", "n3.txt", "n4.txt"];
        let (_dir, items) = bulk_fixture(&names);
        let fake = Arc::new(bulk_backend(|_| BulkTransferOutcome::Completed));
        let ctx = test_ctx(Arc::clone(&fake));

        let report = upload_many(&ctx, &json!({"server": "srv", "items": items}))
            .await
            .unwrap();
        let remote: Vec<String> = names.iter().map(|n| format!("/root/{n}")).collect();
        assert_eq!(*fake.bulk_calls.lock().unwrap(), vec![remote.clone()]);
        assert!(fake.local_uploads.lock().unwrap().is_empty());
        let results = report["results"].as_array().unwrap();
        assert_eq!(results.len(), names.len());
        for (result, (path, name)) in results.iter().zip(remote.iter().zip(names)) {
            assert_eq!(
                *result,
                json!({"server": "srv", "remote_path": path, "uploaded": true, "bytes": name.len()})
            );
        }
        let summary = &report["summary"];
        assert_eq!(summary["processed"], 5);
        assert_eq!(summary["uploaded"], 5);
        assert_eq!(summary["errors"], 0);
        assert_eq!(summary["bulk_groups"], 1);
    }

    /// An item the bulk never handed to the wire, or one it lost to a
    /// transient failure, gets one ordinary upload; a permanent failure is
    /// reported with the provider's words and not retried.
    #[tokio::test]
    async fn upload_many_runs_the_single_file_path_once_for_unsent_and_transient_items() {
        let names = ["done.txt", "unsent.txt", "timeout.txt", "denied.txt"];
        let (_dir, items) = bulk_fixture(&names);
        let fake = Arc::new(bulk_backend(|item| match item.remote_path.as_str() {
            "/root/unsent.txt" => BulkTransferOutcome::NotAttempted("not launched".into()),
            "/root/timeout.txt" => {
                BulkTransferOutcome::Failed(crate::providers::ProviderError::Timeout)
            }
            "/root/denied.txt" => BulkTransferOutcome::Failed(
                crate::providers::ProviderError::PermissionDenied("read-only share".into()),
            ),
            _ => BulkTransferOutcome::Completed,
        }));
        let ctx = test_ctx(Arc::clone(&fake));

        let report = upload_many(&ctx, &json!({"server": "srv", "items": items}))
            .await
            .unwrap();
        assert_eq!(fake.bulk_calls.lock().unwrap().len(), 1);
        assert_eq!(
            *fake.local_uploads.lock().unwrap(),
            vec!["/root/unsent.txt", "/root/timeout.txt"]
        );
        let results = report["results"].as_array().unwrap();
        for (result, name) in results.iter().zip(&names[..3]) {
            assert_eq!(result["uploaded"], true, "{name}: {result}");
            assert_eq!(result["remote_path"], format!("/root/{name}"));
        }
        assert_eq!(results[3]["uploaded"], false);
        let error = results[3]["error"].as_str().unwrap();
        assert!(error.contains("read-only share"), "{error}");
        assert_eq!(report["summary"]["uploaded"], 3);
        assert_eq!(report["summary"]["errors"], 1);
    }

    /// An uncertain item may already be on the server: it is reported and
    /// never uploaded again, which could add a second revision.
    #[tokio::test]
    async fn upload_many_never_retries_an_uncertain_bulk_item() {
        let names = ["u1.txt", "u2.txt", "u3.txt"];
        let (_dir, items) = bulk_fixture(&names);
        let fake = Arc::new(bulk_backend(|item| {
            if item.remote_path == "/root/u2.txt" {
                BulkTransferOutcome::Uncertain("process exited mid-transfer".into())
            } else {
                BulkTransferOutcome::Completed
            }
        }));
        let ctx = test_ctx(Arc::clone(&fake));

        let report = upload_many(&ctx, &json!({"server": "srv", "items": items}))
            .await
            .unwrap();
        assert!(fake.local_uploads.lock().unwrap().is_empty());
        assert_eq!(fake.bulk_calls.lock().unwrap().len(), 1);
        let results = report["results"].as_array().unwrap();
        assert_eq!(
            results[1],
            json!({"uploaded": false, "error": "execution failed: process exited mid-transfer"})
        );
        assert_eq!(results[0]["uploaded"], true);
        assert_eq!(results[2]["uploaded"], true);
        assert_eq!(report["summary"]["errors"], 1);
    }

    /// `no_clobber` across one folder: a single listing answers every item
    /// instead of a stat each, and the existing file is skipped with exactly
    /// the result `upload_file` gives it.
    #[tokio::test]
    async fn upload_many_no_clobber_lists_the_folder_once() {
        // `/root/a.txt` exists in the sample tree.
        let names = ["a.txt", "fresh1.txt", "fresh2.txt"];
        let (_dir, mut items) = bulk_fixture(&names);
        for item in &mut items {
            item["no_clobber"] = json!(true);
        }
        let fake = Arc::new(bulk_backend(|_| BulkTransferOutcome::Completed));
        let ctx = test_ctx(Arc::clone(&fake));

        let report = upload_many(&ctx, &json!({"server": "srv", "items": items}))
            .await
            .unwrap();
        assert_eq!(*fake.list_calls.lock().unwrap(), vec!["/root"]);
        assert!(fake.stat_calls.lock().unwrap().is_empty());
        assert_eq!(
            *fake.bulk_calls.lock().unwrap(),
            vec![vec!["/root/fresh1.txt", "/root/fresh2.txt"]]
        );
        let mut single = items[0].clone();
        single["server"] = json!("srv");
        let expected = upload_file(&test_ctx(Arc::new(FakeBackend::sample())), &single)
            .await
            .unwrap();
        assert_eq!(expected["skipped"], true);
        assert_eq!(report["results"][0], expected);
        assert_eq!(report["results"][1]["uploaded"], true);
        assert_eq!(report["results"][2]["uploaded"], true);
        assert_eq!(report["summary"]["uploaded"], 2);
        assert_eq!(report["summary"]["errors"], 0);
    }

    /// Requested 80 on #1318: the schema promised a batch-level `no_clobber`
    /// as the default of every item without its own, and the handler never
    /// read it, so a batch marked `no_clobber` overwrote existing files.
    #[tokio::test]
    async fn upload_many_applies_the_batch_no_clobber_to_items_without_their_own() {
        // `/root/a.txt` exists in the sample tree.
        let (_dir, items) = bulk_fixture(&["a.txt", "fresh1.txt"]);
        let fake = Arc::new(FakeBackend {
            upload_succeeds: true,
            ..FakeBackend::sample()
        });
        let ctx = test_ctx(Arc::clone(&fake));
        let report = upload_many(
            &ctx,
            &json!({"server": "srv", "items": items, "no_clobber": true}),
        )
        .await
        .unwrap();
        assert_eq!(
            *fake.local_uploads.lock().unwrap(),
            vec!["/root/fresh1.txt"]
        );
        assert_eq!(report["results"][0]["skipped"], true);
        assert_eq!(report["summary"]["skipped"], 1);
        assert_eq!(report["summary"]["totals"]["skipped"], 1);
        assert_eq!(report["summary"]["uploaded"], 1);

        // An item's own `no_clobber: false` overrides the batch default.
        let (_dir, mut items) = bulk_fixture(&["a.txt"]);
        items[0]["no_clobber"] = json!(false);
        let fake = Arc::new(FakeBackend {
            upload_succeeds: true,
            ..FakeBackend::sample()
        });
        let report = upload_many(
            &test_ctx(Arc::clone(&fake)),
            &json!({"server": "srv", "items": items, "no_clobber": true}),
        )
        .await
        .unwrap();
        assert_eq!(*fake.local_uploads.lock().unwrap(), vec!["/root/a.txt"]);
        assert_eq!(report["summary"]["skipped"], 0);
    }

    /// CodeRabbit on #1338: an item's `"no_clobber": null` (or a string) kept
    /// its slot, `upload_file` read it as false, and the file the batch
    /// protects was overwritten. Only a boolean overrides the batch, on both
    /// paths.
    #[tokio::test]
    async fn upload_many_batch_no_clobber_overrides_a_null_or_non_boolean_item_value() {
        for value in [json!(null), json!("yes"), json!(1)] {
            let (_dir, mut items) = bulk_fixture(&["a.txt", "fresh1.txt"]);
            items[0]["no_clobber"] = value.clone();
            let fake = Arc::new(FakeBackend {
                upload_succeeds: true,
                ..FakeBackend::sample()
            });
            let report = upload_many(
                &test_ctx(Arc::clone(&fake)),
                &json!({"server": "srv", "items": items, "no_clobber": true}),
            )
            .await
            .unwrap();
            assert_eq!(
                *fake.local_uploads.lock().unwrap(),
                vec!["/root/fresh1.txt"],
                "single-file path, item no_clobber {value}"
            );
            assert_eq!(report["results"][0]["skipped"], true);

            let (_dir, mut items) = bulk_fixture(&["a.txt", "fresh1.txt", "fresh2.txt"]);
            items[0]["no_clobber"] = value.clone();
            let fake = Arc::new(bulk_backend(|_| BulkTransferOutcome::Completed));
            let report = upload_many(
                &test_ctx(Arc::clone(&fake)),
                &json!({"server": "srv", "items": items, "no_clobber": true}),
            )
            .await
            .unwrap();
            assert_eq!(
                *fake.bulk_calls.lock().unwrap(),
                vec![vec!["/root/fresh1.txt", "/root/fresh2.txt"]],
                "bulk path, item no_clobber {value}"
            );
            assert_eq!(report["results"][0]["skipped"], true);
        }
    }

    /// The batch default reaches the native bulk too: one listing settles it
    /// and the existing file stays out of the bulk.
    #[tokio::test]
    async fn upload_many_applies_the_batch_no_clobber_on_the_bulk_path() {
        let (_dir, items) = bulk_fixture(&["a.txt", "fresh1.txt", "fresh2.txt"]);
        let fake = Arc::new(bulk_backend(|_| BulkTransferOutcome::Completed));
        let report = upload_many(
            &test_ctx(Arc::clone(&fake)),
            &json!({"server": "srv", "items": items, "no_clobber": true}),
        )
        .await
        .unwrap();
        assert_eq!(*fake.list_calls.lock().unwrap(), vec!["/root"]);
        assert_eq!(
            *fake.bulk_calls.lock().unwrap(),
            vec![vec!["/root/fresh1.txt", "/root/fresh2.txt"]]
        );
        assert_eq!(report["results"][0]["skipped"], true);
        assert_eq!(report["summary"]["totals"]["skipped"], 1);
    }

    #[tokio::test]
    async fn probe_contract_mcp_bulk_probes_names_omitted_by_a_partial_listing() {
        let (_directory, items) = bulk_fixture(&["a.txt", "fresh1.txt", "fresh2.txt"]);
        let mut fake = bulk_backend(|_| BulkTransferOutcome::Completed);
        fake.listing_authoritative = false;
        fake.tree.insert("/root".into(), Vec::new());
        // stat still finds a.txt, which the incomplete list omitted.
        let fake = Arc::new(fake);
        let report = upload_many(
            &test_ctx(fake.clone()),
            &json!({"server":"s", "items":items, "no_clobber":true}),
        )
        .await
        .unwrap();
        assert_eq!(report["summary"]["skipped"], 1);
        assert_eq!(
            *fake.bulk_calls.lock().unwrap(),
            vec![vec!["/root/fresh1.txt", "/root/fresh2.txt"]]
        );
        assert!(fake
            .stat_calls
            .lock()
            .unwrap()
            .iter()
            .any(|path| path == "/root/a.txt"));
    }

    /// `delay_ms` was advertised and ignored. It pauses before every upload
    /// call after the first, a native bulk counting as one call, and is
    /// capped at 2000 ms.
    #[tokio::test(start_paused = true)]
    async fn upload_many_pauses_delay_ms_between_upload_calls() {
        let (_dir, items) = bulk_fixture(&["fresh1.txt", "fresh2.txt", "fresh3.txt"]);
        let fake = Arc::new(FakeBackend {
            upload_succeeds: true,
            ..FakeBackend::sample()
        });
        let started = tokio::time::Instant::now();
        let report = upload_many(
            &test_ctx(Arc::clone(&fake)),
            &json!({"server": "srv", "items": items, "delay_ms": 300}),
        )
        .await
        .unwrap();
        assert_eq!(started.elapsed(), std::time::Duration::from_millis(600));
        assert_eq!(fake.local_uploads.lock().unwrap().len(), 3);
        assert_eq!(report["summary"]["delay_ms"], 300);

        // Two folders, two native calls, one pause.
        let (_dir, mut items) = bulk_fixture(&["n0.txt", "n1.txt", "n2.txt", "n3.txt"]);
        items[2]["remote_path"] = json!("/other/n2.txt");
        items[3]["remote_path"] = json!("/other/n3.txt");
        let fake = Arc::new(bulk_backend(|_| BulkTransferOutcome::Completed));
        let started = tokio::time::Instant::now();
        upload_many(
            &test_ctx(Arc::clone(&fake)),
            &json!({"server": "srv", "items": items, "delay_ms": 300}),
        )
        .await
        .unwrap();
        assert_eq!(fake.bulk_calls.lock().unwrap().len(), 2);
        assert_eq!(started.elapsed(), std::time::Duration::from_millis(300));

        let (_dir, items) = bulk_fixture(&["fresh1.txt"]);
        let report = upload_many(
            &test_ctx(Arc::new(FakeBackend {
                upload_succeeds: true,
                ..FakeBackend::sample()
            })),
            &json!({"server": "srv", "items": items, "delay_ms": 60_000}),
        )
        .await
        .unwrap();
        assert_eq!(report["summary"]["delay_ms"], 2_000);
    }

    /// A backend without a native bulk keeps one `upload_file` per item,
    /// probe for probe: a stat per `no_clobber` item and no listing.
    #[tokio::test]
    async fn upload_many_without_a_native_bulk_keeps_one_upload_per_item() {
        let names = ["a.txt", "fresh1.txt", "fresh2.txt"];
        let (_dir, mut items) = bulk_fixture(&names);
        for item in &mut items {
            item["no_clobber"] = json!(true);
        }
        let per_item = || FakeBackend {
            upload_succeeds: true,
            ..FakeBackend::sample()
        };
        let fake = Arc::new(per_item());
        let ctx = test_ctx(Arc::clone(&fake));

        let report = upload_many(&ctx, &json!({"server": "srv", "items": items}))
            .await
            .unwrap();
        assert!(fake.list_calls.lock().unwrap().is_empty());
        assert_eq!(
            *fake.stat_calls.lock().unwrap(),
            vec!["/root/a.txt", "/root/fresh1.txt", "/root/fresh2.txt"]
        );
        assert_eq!(
            *fake.local_uploads.lock().unwrap(),
            vec!["/root/fresh1.txt", "/root/fresh2.txt"]
        );
        assert_eq!(report["summary"]["bulk_groups"], 0);
        for (index, item) in items.iter().enumerate() {
            let mut single = item.clone();
            single["server"] = json!("srv");
            let alone = upload_file(&test_ctx(Arc::new(per_item())), &single)
                .await
                .unwrap();
            assert_eq!(report["results"][index], alone);
        }
    }

    /// A batch that must stop at its first failure cannot send a group at
    /// once: it keeps one upload per item even where a bulk is offered.
    #[tokio::test]
    async fn upload_many_that_stops_at_the_first_failure_keeps_one_upload_per_item() {
        let names = ["c1.txt", "c2.txt"];
        let (_dir, items) = bulk_fixture(&names);
        let fake = Arc::new(bulk_backend(|_| BulkTransferOutcome::Completed));
        let ctx = test_ctx(Arc::clone(&fake));

        let report = upload_many(
            &ctx,
            &json!({"server": "srv", "items": items, "continue_on_error": false}),
        )
        .await
        .unwrap();
        assert!(fake.bulk_calls.lock().unwrap().is_empty());
        assert_eq!(
            *fake.local_uploads.lock().unwrap(),
            vec!["/root/c1.txt", "/root/c2.txt"]
        );
        assert_eq!(report["summary"]["uploaded"], 2);
    }

    /// M9 for a bulk: one call carries the whole group, so Stop pressed
    /// while it is in flight ends the batch without waiting for the group.
    #[tokio::test]
    async fn stop_does_not_wait_for_a_bulk_in_flight() {
        let token = tokio_util::sync::CancellationToken::new();
        let flip = token.clone();
        let (_dir, items) = bulk_fixture(&["s1.txt", "s2.txt"]);
        let fake = Arc::new(FakeBackend {
            bulk_hangs: true,
            ..bulk_backend(move |_| {
                flip.cancel();
                BulkTransferOutcome::Completed
            })
        });
        let ctx = TestCtx {
            cancel: Some(token),
            ..test_ctx(Arc::clone(&fake))
        };

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            upload_many(&ctx, &json!({"server": "srv", "items": items})),
        )
        .await
        .expect("Stop must not wait for the bulk to return");
        assert!(matches!(outcome, Err(ToolError::Cancelled)), "{outcome:?}");
        assert_eq!(fake.bulk_calls.lock().unwrap().len(), 1);
    }

    fn test_ctx(backend: Arc<FakeBackend>) -> TestCtx {
        let backend: Arc<dyn RemoteBackend> = backend;
        TestCtx {
            backend,
            sink: NoopSink,
            creds: NoopCreds::default(),
            cancel: None,
        }
    }

    fn test_ctx_with_profiles(backend: Arc<FakeBackend>, profiles: Vec<ServerProfile>) -> TestCtx {
        let backend: Arc<dyn RemoteBackend> = backend;
        TestCtx {
            backend,
            sink: NoopSink,
            creds: NoopCreds { profiles },
            cancel: None,
        }
    }

    /// G27: the MCP server record must SAY whether a profile is crypt-bound,
    /// and say it for every profile.
    ///
    /// Asserting on the value alone would guard nothing: a key that is absent
    /// deserialises as null for a permissive reader, so `cryptOverlay == null`
    /// passes happily against a record that never emits the field — which is
    /// exactly the state this tool was in. What this pins is the key being
    /// PRESENT, on the bound profile and on the plain one alike.
    #[tokio::test]
    async fn list_servers_reports_the_crypt_binding_for_every_profile() {
        let bound = ServerProfile::from_profile_json(&json!({
            "id": "s-bound",
            "name": "vault box",
            "protocol": "s3",
            "aeroCryptOverlay": { "enabled": true, "kind": "rclone-crypt" }
        }))
        .expect("bound profile");
        let plain = ServerProfile::from_profile_json(&json!({
            "id": "s-plain",
            "name": "plain box",
            "protocol": "sftp"
        }))
        .expect("plain profile");

        let ctx = test_ctx_with_profiles(Arc::new(FakeBackend::sample()), vec![bound, plain]);
        let out = list_servers(&ctx, &json!({})).await.expect("list_servers");
        let servers = out["servers"].as_array().expect("servers array");
        assert_eq!(servers.len(), 2);

        let bound = &servers[0];
        assert!(
            bound.get("cryptOverlay").is_some(),
            "the key must be present, not merely null-equal"
        );
        assert_eq!(bound["cryptOverlay"], json!("rclone-crypt"));
        assert_eq!(
            bound["protocolClass"],
            json!("Crypt"),
            "a bound profile classifies by its overlay, not by its transport"
        );

        let plain = &servers[1];
        assert!(
            plain.get("cryptOverlay").is_some(),
            "an unbound profile still answers, with null"
        );
        assert_eq!(plain["cryptOverlay"], json!(null));
        assert_eq!(plain["protocolClass"], json!("SFTP"));
    }

    /// A transient lookup failure must not let a later successful write
    /// bypass destination-name validation.
    #[tokio::test]
    async fn upload_without_provider_type_is_refused_before_the_wire() {
        let mut fake = FakeBackend::sample();
        fake.provider_type = None;
        let fake = Arc::new(fake);
        let ctx = test_ctx(Arc::clone(&fake));
        let err = upload_file(
            &ctx,
            &json!({"server": "s", "remote_path": "/root/bad\tname", "content": "x"}),
        )
        .await
        .unwrap_err();
        assert!(format!("{err:?}").contains("remote provider unavailable"));
        assert!(fake.remote_files.lock().unwrap().is_empty());
    }

    /// Split a command line the way `sh` does, by having `sh` print each
    /// argument NUL-terminated.
    #[cfg(unix)]
    fn shell_split(line: &str) -> Vec<String> {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("printf '%s\\0' {line}"))
            .output()
            .expect("run sh");
        String::from_utf8_lossy(&out.stdout)
            .split('\0')
            .filter(|a| !a.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// The suggested `sync` line carries values the caller chose. A real shell
    /// splits it: a hostile profile name, local path and exclude pattern each
    /// come back as one argument, byte for byte, and nothing runs. A direction
    /// outside the fixed set is refused before it can reach the line.
    #[cfg(unix)]
    #[tokio::test]
    async fn sync_doctor_suggestion_keeps_hostile_values_as_text() {
        let fake = Arc::new(FakeBackend::sample());
        let ctx = test_ctx(Arc::clone(&fake));
        let tmp = tempfile::tempdir().expect("temp dir");
        let marker = tmp.path().join("pwned");
        let local_dir = tmp.path().join(format!("$(touch {})", marker.display()));
        std::fs::create_dir_all(&local_dir).expect("local dir");
        let out = sync_doctor(
            &ctx,
            &json!({
                "server": "`touch pwned` it's!",
                "local_dir": local_dir.to_string_lossy(),
                "remote_dir": "/root",
                "direction": "download",
                "exclude": ["$(rm -rf ~)"],
            }),
        )
        .await
        .expect("doctor runs");
        let line = out["suggested_next_command"]
            .as_str()
            .expect("a suggestion");
        let args = shell_split(line);
        assert!(!marker.exists(), "a substitution ran: {line}");
        assert_eq!(
            args[..6],
            [
                "aeroftp-cli".to_string(),
                "sync".to_string(),
                "--profile".to_string(),
                "`touch pwned` it's!".to_string(),
                local_dir.to_string_lossy().into_owned(),
                "/root".to_string(),
            ],
            "{line}"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--exclude" && w[1] == "$(rm -rf ~)"),
            "{args:?}"
        );

        let err = sync_doctor(
            &ctx,
            &json!({
                "server": "s",
                "local_dir": local_dir.to_string_lossy(),
                "remote_dir": "/root",
                "direction": "both; rm -rf ~",
            }),
        )
        .await
        .unwrap_err();
        assert!(format!("{err:?}").contains("direction must be upload, download or both"));
    }

    /// Parent creation must reject a new restricted component before mkdir.
    #[tokio::test]
    async fn missing_restricted_parent_is_not_sent_to_mkdir() {
        let fake = Arc::new(FakeBackend::sample());
        let ctx = test_ctx(Arc::clone(&fake));
        let err = ensure_remote_parents(&ctx, "s", "/root/bad\tparent/file")
            .await
            .unwrap_err();
        assert!(format!("{err:?}").contains("Restricted character"));
        assert!(fake.mkdir_calls.lock().unwrap().is_empty());
    }

    /// Existing restricted parents remain traversable while new children
    /// still pass through validation.
    #[tokio::test]
    async fn existing_restricted_parent_remains_usable() {
        let mut fake = FakeBackend::sample();
        fake.stats.insert("/root/bad\tparent".into(), (true, 0));
        fake.mkdir_fails_with = Some("File exists".into());
        let fake = Arc::new(fake);
        let ctx = test_ctx(Arc::clone(&fake));
        ensure_remote_parents(&ctx, "s", "/root/bad\tparent/child/file")
            .await
            .unwrap();
        assert_eq!(
            fake.mkdir_calls.lock().unwrap().as_slice(),
            &["/root/bad\tparent/child".to_string()]
        );
    }

    /// Editing an old restricted name must stage under a safe sibling leaf
    /// and publish back to the unchanged existing target.
    #[tokio::test]
    async fn edit_restricted_target_uses_safe_staging_leaf() {
        let path = "/root/bad\tname.txt";
        let mut fake = FakeBackend::sample();
        fake.stats.insert(path.into(), (false, 3));
        fake.downloads.insert(path.into(), b"old".to_vec());
        let fake = Arc::new(fake);
        let ctx = test_ctx(Arc::clone(&fake));
        edit(
            &ctx,
            &json!({"server": "s", "path": path, "find": "old", "replace": "new"}),
        )
        .await
        .unwrap();
        let uploads = fake.uploads.lock().unwrap();
        assert_eq!(uploads.len(), 1);
        let staged = &uploads[0].0;
        assert!(staged.starts_with("/root/.aeroedit-"));
        crate::restricted_chars::validate_path(ProviderType::Ftp, staged).unwrap();
        assert_eq!(
            fake.renames.lock().unwrap().as_slice(),
            &[(staged.clone(), path.into())]
        );
    }

    /// Speed-test uploads create a new object and must obey its provider's
    /// name rules before uploading or deleting anything.
    #[tokio::test]
    async fn speed_rejects_restricted_destination_before_upload() {
        let fake = Arc::new(FakeBackend::sample().with_provider_type(ProviderType::S3));
        let ctx = test_ctx(Arc::clone(&fake));
        let err = speed(
            &ctx,
            &json!({"server": "s", "remote_path": "/root/bad\tname", "size_mb": 1}),
        )
        .await
        .unwrap_err();
        assert!(format!("{err:?}").contains("Restricted character"));
        assert!(fake.uploads.lock().unwrap().is_empty());
        assert!(fake.deleted.lock().unwrap().is_empty());
    }

    /// G24: a write tool must refuse a leaf the target backend forbids BEFORE
    /// anything reaches the wire. `validate_remote_path` deliberately lets a
    /// TAB through (`c != '\t'`), so this is the check that catches it: the
    /// fake stands in for FTP, where the leaf lands in the argument of a
    /// CRLF-terminated command.
    #[tokio::test]
    async fn upload_with_a_tab_in_the_leaf_is_refused_before_the_wire() {
        let fake = Arc::new(FakeBackend::sample());
        let ctx = test_ctx(Arc::clone(&fake));

        let err = upload_file(
            &ctx,
            &json!({
                "server": "s",
                "remote_path": "/root/bad\tname.txt",
                "content": "x",
            }),
        )
        .await
        .unwrap_err();
        let text = format!("{err:?}");
        assert!(
            text.contains("Restricted character") && text.contains("U+0009"),
            "the refusal must name the offending character: {text}"
        );
        assert!(
            fake.remote_files.lock().unwrap().is_empty(),
            "a refused name must not reach the backend"
        );
    }

    /// The control case: Box, Dropbox, Jottacloud and OpenDrive ENCODE the
    /// character reversibly instead of refusing it. A patch that rejects
    /// unconditionally breaks those four providers; this test is what keeps
    /// the rule provider-aware rather than absolute.
    #[tokio::test]
    async fn upload_with_a_tab_in_the_leaf_is_encoded_not_refused_on_box() {
        let fake = Arc::new(FakeBackend::sample().with_provider_type(ProviderType::Box));
        let ctx = test_ctx(Arc::clone(&fake));

        let out = upload_file(
            &ctx,
            &json!({
                "server": "s",
                "remote_path": "/root/bad\tname.txt",
                "content": "x",
            }),
        )
        .await;
        assert!(out.is_ok(), "an encoding provider must not refuse: {out:?}");
        assert!(
            fake.remote_files
                .lock()
                .unwrap()
                .contains_key("/root/bad\tname.txt"),
            "the upload should have landed"
        );
    }

    /// Rename sets one new name: the destination. The source names an object
    /// that already exists, and refusing a character there would block
    /// renaming AWAY from a bad name, the same exclusion class as delete.
    #[tokio::test]
    async fn rename_checks_the_destination_leaf_only() {
        let fake = Arc::new(FakeBackend::sample());
        // A file already stored under a TAB name, as an older client could
        // have written it. The fake's rename reads `remote_files`.
        fake.remote_files
            .lock()
            .unwrap()
            .insert("/root/bad\told.txt".to_string(), b"old".to_vec());
        let ctx = test_ctx(Arc::clone(&fake));

        let err = rename(
            &ctx,
            &json!({"server": "s", "from": "/root/a.txt", "to": "/root/bad\tnew.txt"}),
        )
        .await
        .unwrap_err();
        assert!(
            format!("{err:?}").contains("Restricted character"),
            "destination with a forbidden character must be refused: {err:?}"
        );

        let out = rename(
            &ctx,
            &json!({"server": "s", "from": "/root/bad\told.txt", "to": "/root/fixed.txt"}),
        )
        .await;
        assert!(
            out.is_ok(),
            "renaming away from a bad existing name must stay possible: {out:?}"
        );
    }

    /// Touch creates only when the path does not exist, so the leaf check
    /// belongs on the create branch alone: an existing badly-named file stays
    /// touchable (a no-op), a new one is refused.
    #[tokio::test]
    async fn touch_checks_the_leaf_only_when_creating() {
        let mut fake = FakeBackend::sample();
        fake.stats
            .insert("/root/bad\told.txt".to_string(), (false, 10));
        let fake = Arc::new(fake);
        let ctx = test_ctx(Arc::clone(&fake));

        let out = touch(&ctx, &json!({"server": "s", "path": "/root/bad\told.txt"})).await;
        assert!(
            out.is_ok(),
            "touching an existing file is a no-op and must not be refused: {out:?}"
        );

        let err = touch(&ctx, &json!({"server": "s", "path": "/root/bad\tnew.txt"}))
            .await
            .unwrap_err();
        assert!(
            format!("{err:?}").contains("Restricted character"),
            "creating a file with a forbidden character must be refused: {err:?}"
        );
    }

    /// A stat that does not answer (a timeout) is not a missing file: touch
    /// read it as one and uploaded 0 bytes over the file it could not see.
    #[tokio::test]
    async fn probe_contract_mcp_never_reads_error_details_as_absence() {
        for error in [
            crate::providers::ProviderError::AuthenticationFailed(
                "upstream says 404 not found".into(),
            ),
            crate::providers::ProviderError::PermissionDenied(
                "metadata lookup returned 404".into(),
            ),
            crate::providers::ProviderError::ServerError("cannot find metadata service".into()),
        ] {
            let mut fake = FakeBackend::sample();
            fake.stat_fails_with = Some(error.to_string());
            let fake = Arc::new(fake);
            let ctx = test_ctx(fake.clone());
            let outcome = touch(&ctx, &json!({"server":"s","path":"/root/a.txt"})).await;
            assert!(
                outcome.is_err(),
                "{error}: a probe error permitted touch: {outcome:?}"
            );
            let outcome = upload_file(
                &ctx,
                &json!({"server":"s","remote_path":"/root/a.txt",
                "content":"new","no_clobber":true}),
            )
            .await;
            assert!(
                outcome.is_err(),
                "{error}: a probe error permitted upload: {outcome:?}"
            );
            assert!(fake.remote_files.lock().unwrap().is_empty());
            assert!(speed(
                &ctx,
                &json!({"server":"s", "remote_path":"/root/a.txt", "size_mb":1})
            )
            .await
            .is_err());
            assert!(fake.uploads.lock().unwrap().is_empty());
            assert!(fake.deleted.lock().unwrap().is_empty());

            let (_directory, items) = bulk_fixture(&["a.txt", "fresh1.txt", "fresh2.txt"]);
            let mut bulk = bulk_backend(|_| BulkTransferOutcome::Completed);
            bulk.stat_fails_with = Some(error.to_string());
            bulk.list_fails_with = Some(error.to_string());
            let bulk = Arc::new(bulk);
            let report = upload_many(
                &test_ctx(bulk.clone()),
                &json!({"server":"s", "items":items,"no_clobber":true}),
            )
            .await
            .unwrap();
            assert_eq!(report["summary"]["uploaded"], 0);
            assert!(bulk.bulk_calls.lock().unwrap().is_empty());
            assert!(bulk.local_uploads.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn touch_creates_nothing_when_the_stat_does_not_answer() {
        let mut fake = FakeBackend::sample();
        fake.stat_fails_with = Some("Request timed out".to_string());
        let fake = Arc::new(fake);
        let ctx = test_ctx(Arc::clone(&fake));

        let err = touch(&ctx, &json!({"server": "s", "path": "/root/a.txt"}))
            .await
            .unwrap_err();
        assert!(
            format!("{err:?}").contains("cannot verify"),
            "the unanswered stat must be reported: {err:?}"
        );
        assert!(
            fake.remote_files.lock().unwrap().is_empty(),
            "nothing may be written over a file the stat could not see"
        );
    }

    /// `no_clobber` on a stat that does not answer: the upload went ahead
    /// and overwrote the file the flag exists to protect.
    #[tokio::test]
    async fn no_clobber_uploads_nothing_when_the_stat_does_not_answer() {
        let mut fake = FakeBackend::sample();
        fake.stat_fails_with = Some("Request timed out".to_string());
        let fake = Arc::new(fake);
        let ctx = test_ctx(Arc::clone(&fake));

        let err = upload_file(
            &ctx,
            &json!({
                "server": "s",
                "remote_path": "/root/a.txt",
                "content": "new",
                "no_clobber": true,
            }),
        )
        .await
        .unwrap_err();
        assert!(format!("{err:?}").contains("no_clobber"), "{err:?}");
        assert!(fake.remote_files.lock().unwrap().is_empty());
    }

    /// The two answers keep their meaning: an existing file is skipped,
    /// a missing one is created.
    #[tokio::test]
    async fn touch_and_no_clobber_still_act_on_a_definite_answer() {
        let fake = Arc::new(FakeBackend::sample());
        let ctx = test_ctx(Arc::clone(&fake));

        let out = upload_file(
            &ctx,
            &json!({
                "server": "s",
                "remote_path": "/root/a.txt",
                "content": "new",
                "no_clobber": true,
            }),
        )
        .await
        .unwrap();
        assert_eq!(out["skipped"], true);
        let out = touch(&ctx, &json!({"server": "s", "path": "/root/new.txt"}))
            .await
            .unwrap();
        assert_eq!(out["created"], true);
        assert!(fake
            .remote_files
            .lock()
            .unwrap()
            .contains_key("/root/new.txt"));
    }

    #[tokio::test]
    async fn mkdir_with_a_tab_in_the_leaf_is_refused() {
        let fake = Arc::new(FakeBackend::sample());
        let ctx = test_ctx(Arc::clone(&fake));

        let err = create_directory(&ctx, &json!({"server": "s", "path": "/root/bad\tdir"}))
            .await
            .unwrap_err();
        assert!(
            format!("{err:?}").contains("Restricted character"),
            "mkdir with a forbidden character must be refused: {err:?}"
        );
    }

    /// G26 fixture: a local dir and a fake remote that agree on names and
    /// sizes. `same.txt` has identical bytes on both sides; `diff.txt` has
    /// the same length but different content - the case a size-only compare
    /// cannot see. The fake serves downloads from `downloads`.
    fn checksum_fixture() -> (tempfile::TempDir, Arc<FakeBackend>) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("same.txt"), b"aaa").unwrap();
        std::fs::write(dir.path().join("diff.txt"), b"xxx").unwrap();

        let mut fake = FakeBackend::sample();
        fake.tree.insert(
            "/remote".to_string(),
            vec![
                entry("/remote/same.txt", false, 3),
                entry("/remote/diff.txt", false, 3),
            ],
        );
        fake.downloads
            .insert("/remote/same.txt".to_string(), b"aaa".to_vec());
        fake.downloads
            .insert("/remote/diff.txt".to_string(), b"yyy".to_vec());
        (dir, Arc::new(fake))
    }

    /// G26: with `checksum: true` a same-size pair with different content
    /// must land in `differ`, not `match`. Before the fix the flag was read
    /// into `_checksum` and dropped, and this pair reported a match.
    #[tokio::test]
    async fn reconcile_checksum_catches_same_size_different_content() {
        let (dir, fake) = checksum_fixture();
        let ctx = test_ctx(fake);

        let out = reconcile(
            &ctx,
            &json!({
                "server": "s",
                "local_dir": dir.path().to_string_lossy(),
                "remote_dir": "/remote",
                "checksum": true,
            }),
        )
        .await
        .unwrap();

        assert_eq!(out["status"], "differences_found");
        let differ = out["groups"]["differ"].as_array().unwrap();
        assert_eq!(differ.len(), 1, "exactly diff.txt may differ: {differ:?}");
        assert_eq!(differ[0]["path"], "diff.txt");
        assert_eq!(differ[0]["compare_method"], "blake3");
        assert!(
            differ[0]["local_checksum"] != differ[0]["remote_checksum"],
            "the two digests must differ"
        );

        let matched = out["groups"]["match"].as_array().unwrap();
        assert_eq!(matched.len(), 1, "same.txt matches by content: {matched:?}");
        assert_eq!(matched[0]["path"], "same.txt");
        assert_eq!(matched[0]["compare_method"], "blake3");

        assert_eq!(out["summary"]["checksum_hashed"], 2);
        assert_eq!(out["summary"]["checksum_errors"], 0);
    }

    /// Without the flag the compare stays size-only: same length means match,
    /// whatever the content. That is the documented default, and the fix must
    /// not change it.
    #[tokio::test]
    async fn reconcile_without_checksum_keeps_size_only_compare() {
        let (dir, fake) = checksum_fixture();
        let ctx = test_ctx(fake);

        let out = reconcile(
            &ctx,
            &json!({
                "server": "s",
                "local_dir": dir.path().to_string_lossy(),
                "remote_dir": "/remote",
            }),
        )
        .await
        .unwrap();

        assert_eq!(out["status"], "ok");
        let matched = out["groups"]["match"].as_array().unwrap();
        assert_eq!(
            matched.len(),
            2,
            "size-only sees both as equal: {matched:?}"
        );
        assert!(
            matched.iter().all(|e| e["compare_method"] == "size"),
            "compare_method stays size: {matched:?}"
        );
        assert_eq!(out["summary"]["checksum"], false);
        assert_eq!(out["summary"]["checksum_hashed"], 0);
    }

    /// A plain file where a parent directory belongs must be reported at the
    /// level it is on.
    ///
    /// `stat` returns a `RemoteEntry` carrying `is_dir`, and the old code read
    /// only `is_ok()`. That answers "does something exist here" while the
    /// question is "can I descend into it", so a file was forgiven and the
    /// walk carried on. The failure then surfaced one level further down,
    /// naming a path that was not the problem, which sends whoever reads it
    /// looking below the actual obstacle.
    ///
    /// Seeded so the old behaviour is reachable: /data is a directory, and
    /// /data/report is a FILE. The mock refuses every mkdir, which is what a
    /// server does when the path is taken.
    #[tokio::test]
    async fn a_file_where_a_parent_belongs_is_named_at_its_own_level() {
        let mut fake = FakeBackend::sample();
        fake.stats.insert("/data".to_string(), (true, 0));
        fake.stats.insert("/data/report".to_string(), (false, 12));
        let ctx = test_ctx(Arc::new(fake));

        let err = ensure_remote_parents(&ctx, "s", "/data/report/2026/x.csv")
            .await
            .unwrap_err();
        let text = format!("{err:?}");

        assert!(
            text.contains("/data/report already exists"),
            "the obstacle is /data/report and the error does not name it: {text}"
        );
        assert!(
            text.contains("does not report it as a directory"),
            "the error names the level but not what is wrong with it: {text}"
        );
    }

    /// The rule, checked on the shapes a listing actually produces.
    ///
    /// The symlink test below covers the case; this one covers the reasoning
    /// that produced it, because the same mistake was made twice in one
    /// function and the second time in code written after the first was fixed.
    /// Removing an instance leaves the premise, and the premise keeps working.
    ///
    /// Only a leading `d` proves a directory. Everything else is unknown, and
    /// unknown is not a refusal: a symlink to a directory, a symlink spelling
    /// MLSD does not recognise, and a plain file are indistinguishable here,
    /// so no caller may turn any of them into a rejection.
    #[test]
    fn only_a_positive_says_directory_and_nothing_says_the_opposite() {
        let dir = entry("/a", true, 0);
        let file = entry("/a", false, 12);
        let mut link = entry("/a", false, 0);
        link.is_symlink = true;

        assert_eq!(descend_evidence(&dir), DescendEvidence::ProvenDirectory);
        assert_eq!(descend_evidence(&file), DescendEvidence::Unknown);
        assert_eq!(
            descend_evidence(&link),
            DescendEvidence::Unknown,
            "a symlink to a directory reports is_dir == false; reading that as \
             a refusal is the regression this rule exists to prevent"
        );

        // The type itself is the guard: there is no variant meaning "proven
        // not a directory", so no branch can be written that refuses on the
        // absence of proof. If someone adds one, this stops compiling.
        for e in [&dir, &file, &link] {
            match descend_evidence(e) {
                DescendEvidence::ProvenDirectory | DescendEvidence::Unknown => {}
            }
        }
    }

    /// A parent that is a symlink must keep working.
    ///
    /// This test exists because the first version of this fix broke it. A
    /// listing gives `is_dir` from the leading `d` and `is_symlink` from the
    /// leading `l`, so a symlink pointing AT a directory arrives with
    /// `is_dir == false`. Refusing on that would have failed every upload
    /// whose parent is a symlink, which on shared hosting is ordinary, and it
    /// would have failed them in the name of a better error message.
    ///
    /// The listing is not positive evidence of a regular file either: MLSD
    /// only sets `is_symlink` for two exact spellings, so other symlink forms
    /// land in the same branch. Hence nothing here refuses; it only explains.
    #[tokio::test]
    async fn a_symlinked_parent_is_not_treated_as_an_obstacle() {
        let mut fake = FakeBackend::sample();
        fake.stats.insert("/data".to_string(), (true, 0));
        // A symlink to a directory, as a listing reports it: not a dir.
        fake.symlinks.insert("/data/report".to_string());
        fake.stats.insert("/data/report".to_string(), (false, 0));
        fake.stats
            .insert("/data/report/2026".to_string(), (false, 0));
        fake.mkdir_fails_with = Some("550 Permission denied".to_string());
        let ctx = test_ctx(Arc::new(fake));

        ensure_remote_parents(&ctx, "s", "/data/report/2026/x.csv")
            .await
            .expect("a symlinked parent used to work and must keep working");
    }

    /// An existing parent costs one round trip, not one per level.
    ///
    /// The walk issues an MKD per level and forgives each failure, so the
    /// ordinary case, a tree that is already there, pays for every level. A
    /// stat up front answers the exact question once. Asking precisely can be
    /// cheaper than asking vaguely, because a vague answer leaves the work to
    /// be attempted anyway.
    ///
    /// Only the parent itself is seeded, and the mock refuses every mkdir, so
    /// this passes only if no level is walked at all.
    #[tokio::test]
    async fn an_existing_parent_is_settled_without_walking_it() {
        let mut fake = FakeBackend::sample();
        fake.stats
            .insert("/data/report/2026".to_string(), (true, 0));
        let ctx = test_ctx(Arc::new(fake));

        ensure_remote_parents(&ctx, "s", "/data/report/2026/x.csv")
            .await
            .expect("an existing parent directory needs no mkdir at all");
    }

    /// A backend that cannot stat falls back to the words the server used,
    /// exactly as before the typed check was added.
    #[tokio::test]
    async fn without_stat_the_server_wording_still_decides() {
        let mut fake = FakeBackend::sample();
        fake.stat_fails_with = Some("stat unsupported".to_string());
        fake.mkdir_fails_with = Some("550 File exists".to_string());
        let ctx = test_ctx(Arc::new(fake));

        ensure_remote_parents(&ctx, "s", "/data/report/x.csv")
            .await
            .expect("an \"exists\" reply is still forgiven when stat cannot answer");
    }

    #[tokio::test]
    async fn edit_rejects_directory_before_download() {
        let backend = Arc::new(FakeBackend::sample());
        let ctx = test_ctx(Arc::clone(&backend));
        let err = edit(
            &ctx,
            &json!({
                "server": "s",
                "path": "/root",
                "find": "old",
                "replace": "new",
            }),
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("directory"), "got: {msg}");
        assert!(backend.uploads.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn edit_rejects_stat_oversize_before_download() {
        let mut fake = FakeBackend::sample();
        fake.stats
            .insert("/root/large.txt".to_string(), (false, MAX_EDIT_BYTES + 1));
        fake.downloads
            .insert("/root/large.txt".to_string(), b"old".to_vec());
        let backend = Arc::new(fake);
        let ctx = test_ctx(Arc::clone(&backend));
        let err = edit(
            &ctx,
            &json!({
                "server": "s",
                "path": "/root/large.txt",
                "find": "old",
                "replace": "new",
            }),
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("File too large"), "got: {msg}");
        assert!(backend.uploads.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn edit_rejects_download_that_exceeds_cap_after_stat_lie() {
        let mut fake = FakeBackend::sample();
        fake.stats.insert("/root/lie.txt".to_string(), (false, 3));
        fake.downloads.insert(
            "/root/lie.txt".to_string(),
            vec![b'a'; (MAX_EDIT_BYTES + 1) as usize],
        );
        let backend = Arc::new(fake);
        let ctx = test_ctx(Arc::clone(&backend));
        let err = edit(
            &ctx,
            &json!({
                "server": "s",
                "path": "/root/lie.txt",
                "find": "a",
                "replace": "b",
            }),
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        // The streaming cap now refuses mid-read; the message names the 10 MB cap.
        assert!(msg.contains("cap") && msg.contains("10 MB"), "got: {msg}");
        assert!(backend.uploads.lock().unwrap().is_empty());
    }

    /// Proves the edit download is a TRUE streaming cap: given a server that
    /// lies (`stat` says 3 B) but serves a body far past the 10 MB edit cap,
    /// edit must refuse WITHOUT ever materializing the full body in memory.
    #[tokio::test]
    async fn edit_streaming_cap_never_materializes_lying_body() {
        let mut fake = FakeBackend::sample();
        // Server under-reports: stat claims 3 bytes...
        fake.stats.insert("/root/lie.txt".to_string(), (false, 3));
        // ...but the real body is 15 MB, well past the 10 MB edit cap.
        let real_size = (MAX_EDIT_BYTES + 5 * 1024 * 1024) as usize;
        fake.downloads
            .insert("/root/lie.txt".to_string(), vec![b'a'; real_size]);
        let backend = Arc::new(fake);
        let ctx = test_ctx(Arc::clone(&backend));

        let err = edit(
            &ctx,
            &json!({
                "server": "s",
                "path": "/root/lie.txt",
                "find": "a",
                "replace": "b",
            }),
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("cap") && msg.contains("10 MB"), "got: {msg}");

        // The core guarantee: the accumulator never held the full 15 MB body.
        // It stops within one chunk of the cap, never near `real_size`.
        let high_water = *backend.capped_high_water.lock().unwrap();
        assert!(
            (high_water as u64) <= MAX_EDIT_BYTES,
            "materialized {high_water} bytes, above the {MAX_EDIT_BYTES}-byte cap"
        );
        assert!(
            high_water < real_size,
            "the full lying body was materialized ({high_water} of {real_size} bytes)"
        );
        // Nothing was uploaded: the edit aborted before touching the remote.
        assert!(backend.uploads.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn edit_uploads_to_temp_then_renames_over_target() {
        let mut fake = FakeBackend::sample();
        fake.downloads
            .insert("/root/a.txt".to_string(), b"old text".to_vec());
        let backend = Arc::new(fake);
        let ctx = test_ctx(Arc::clone(&backend));

        let value = edit(
            &ctx,
            &json!({
                "server": "s",
                "path": "/root/a.txt",
                "find": "old",
                "replace": "new",
            }),
        )
        .await
        .expect("edit should publish via temp+rename");

        assert_eq!(value["replacements"], json!(1));
        let uploads = backend.uploads.lock().unwrap();
        assert_eq!(uploads.len(), 1, "one staged upload expected: {uploads:?}");
        let temp_path = uploads[0].0.clone();
        assert!(
            temp_path.starts_with("/root/.aeroedit-") && temp_path.ends_with(".tmp"),
            "unexpected edit temp path: {temp_path}"
        );
        assert_ne!(
            temp_path, "/root/a.txt",
            "edit must never upload replacement bytes directly to the target"
        );
        drop(uploads);

        assert_eq!(
            backend.renames.lock().unwrap().as_slice(),
            &[(temp_path.clone(), "/root/a.txt".to_string())]
        );
        let remote_files = backend.remote_files.lock().unwrap();
        assert_eq!(
            remote_files.get("/root/a.txt").map(Vec::as_slice),
            Some(b"new text".as_slice())
        );
        assert!(
            !remote_files.contains_key(&temp_path),
            "temp path must be gone after successful rename"
        );
        assert!(backend.deleted.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn edit_deletes_staged_temp_when_rename_fails() {
        let mut fake = FakeBackend::sample();
        fake.downloads
            .insert("/root/a.txt".to_string(), b"old text".to_vec());
        fake.rename_fails_with = Some("rename failed".to_string());
        let backend = Arc::new(fake);
        let ctx = test_ctx(Arc::clone(&backend));

        let err = edit(
            &ctx,
            &json!({
                "server": "s",
                "path": "/root/a.txt",
                "find": "old",
                "replace": "new",
            }),
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("rename failed"), "got: {msg}");

        let temp_path = backend.uploads.lock().unwrap()[0].0.clone();
        assert_eq!(
            backend.deleted.lock().unwrap().as_slice(),
            std::slice::from_ref(&temp_path)
        );
        assert!(
            !backend
                .remote_files
                .lock()
                .unwrap()
                .contains_key(&temp_path),
            "rename failure cleanup must remove the staged temp"
        );
    }

    /// A set-aside backend refuses `edit` until `allow_non_atomic` is set,
    /// and then publishes with `replace` rather than writing nothing.
    #[tokio::test]
    async fn edit_allow_non_atomic_replaces_when_the_backend_cannot() {
        let mut fake = FakeBackend::sample();
        fake.atomic_replace = false;
        fake.sets_aside = true;
        fake.downloads
            .insert("/root/a.txt".to_string(), b"old text".to_vec());
        let backend = Arc::new(fake);
        let ctx = test_ctx(Arc::clone(&backend));

        let err = edit(
            &ctx,
            &json!({
                "server": "s",
                "path": "/root/a.txt",
                "find": "old",
                "replace": "new",
            }),
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("allow_non_atomic"),
            "the refusal must name the opt-in, got: {msg}"
        );
        assert!(backend.uploads.lock().unwrap().is_empty());

        let value = edit(
            &ctx,
            &json!({
                "server": "s",
                "path": "/root/a.txt",
                "find": "old",
                "replace": "new",
                "allow_non_atomic": true,
            }),
        )
        .await
        .expect("opted-in edit publishes with replace");
        assert_eq!(value["replacements"], json!(1));
        assert_eq!(
            backend
                .remote_files
                .lock()
                .unwrap()
                .get("/root/a.txt")
                .map(Vec::as_slice),
            Some(b"new text".as_slice())
        );
    }

    /// M1 of the 4.2.1 closeout: on a backend whose replace is its rename
    /// (Box, 4shared, Internxt, WorkDrive, GitHub, SFTP without
    /// posix-rename...) `allow_non_atomic` uploaded the temporary, the
    /// replace refused the taken name, and the temporary was deleted again:
    /// something was written. The opt-in is now refused before the upload,
    /// and neither refusal offers an opt-in that cannot work there.
    #[tokio::test]
    async fn edit_allow_non_atomic_refuses_a_rename_only_backend_before_it_uploads() {
        let mut fake = FakeBackend::sample();
        fake.atomic_replace = false;
        fake.downloads
            .insert("/root/a.txt".to_string(), b"old text".to_vec());
        let backend = Arc::new(fake);
        let ctx = test_ctx(Arc::clone(&backend));
        let args = |allow_non_atomic: bool| {
            json!({
                "server": "s",
                "path": "/root/a.txt",
                "find": "old",
                "replace": "new",
                "allow_non_atomic": allow_non_atomic,
            })
        };

        let opted_in = edit(&ctx, &args(true)).await.unwrap_err().to_string();
        assert!(
            backend.uploads.lock().unwrap().is_empty(),
            "the opt-in must be refused before anything is staged: {opted_in}"
        );
        assert!(
            backend.renames.lock().unwrap().is_empty(),
            "nothing may be published: {opted_in}"
        );
        assert!(
            opted_in.contains("Nothing was written")
                && opted_in.contains("set the previous file aside"),
            "the refusal must say why the opt-in cannot work here: {opted_in}"
        );

        let default = edit(&ctx, &args(false)).await.unwrap_err().to_string();
        assert!(backend.uploads.lock().unwrap().is_empty());
        assert!(
            !default.contains("allow_non_atomic"),
            "a rename-only backend must not be offered the opt-in: {default}"
        );
    }

    fn edit_args() -> Value {
        json!({
            "server": "s",
            "path": "/root/a.txt",
            "find": "old",
            "replace": "new",
        })
    }

    /// M-A of the 4.2.1 closeout: the edit publishes a NEW file over the
    /// target, so the mode of the old one was lost (on SFTP a 0600 `.env`
    /// came back with the server default, 0644). It is copied onto the
    /// temporary before the replace.
    #[tokio::test]
    async fn edit_keeps_the_mode_of_the_file_it_replaces() {
        let mut fake = FakeBackend::sample();
        fake.downloads
            .insert("/root/a.txt".to_string(), b"old text".to_vec());
        fake.chmod_supported = true;
        fake.modes
            .lock()
            .unwrap()
            .insert("/root/a.txt".to_string(), 0o600);
        let backend = Arc::new(fake);
        let ctx = test_ctx(Arc::clone(&backend));

        let value = edit(&ctx, &edit_args()).await.expect("edit");

        assert_eq!(value["replacements"], json!(1));
        assert_eq!(
            backend.modes.lock().unwrap().get("/root/a.txt").copied(),
            Some(0o600),
            "the edited file must keep its mode, not the server default"
        );
        assert!(value.get("warnings").is_none(), "{value}");
    }

    /// A chmod the server refuses does not fail an edit that is otherwise
    /// done, and the answer says what was not kept.
    #[tokio::test]
    async fn edit_says_which_permissions_it_could_not_keep() {
        let mut fake = FakeBackend::sample();
        fake.downloads
            .insert("/root/a.txt".to_string(), b"old text".to_vec());
        fake.chmod_supported = true;
        fake.chmod_fails_with = Some("SITE CHMOD refused".to_string());
        fake.modes
            .lock()
            .unwrap()
            .insert("/root/a.txt".to_string(), 0o600);
        let backend = Arc::new(fake);
        let ctx = test_ctx(Arc::clone(&backend));

        let value = edit(&ctx, &edit_args()).await.expect("edit");

        assert_eq!(
            backend
                .remote_files
                .lock()
                .unwrap()
                .get("/root/a.txt")
                .map(Vec::as_slice),
            Some(b"new text".as_slice())
        );
        let warning = value["warnings"][0].as_str().unwrap_or_default();
        assert!(
            warning.contains("0600") && warning.contains("SITE CHMOD refused"),
            "the answer must name the mode that was not kept and why: {value}"
        );
    }

    /// The replace would put a regular file in the link's place and leave
    /// the file it points to unchanged. A link is refused before anything
    /// is read or written.
    #[tokio::test]
    async fn edit_refuses_a_symlink_before_it_downloads() {
        let mut fake = FakeBackend::sample();
        fake.downloads
            .insert("/root/a.txt".to_string(), b"old text".to_vec());
        fake.symlinks.insert("/root/a.txt".to_string());
        let backend = Arc::new(fake);
        let ctx = test_ctx(Arc::clone(&backend));

        let outcome = edit(&ctx, &edit_args()).await;

        assert!(
            backend.uploads.lock().unwrap().is_empty()
                && backend.renames.lock().unwrap().is_empty(),
            "nothing may be staged over a link: {outcome:?}"
        );
        let text = outcome.unwrap_err().to_string();
        assert!(
            text.contains("symbolic link") && text.contains("Nothing was written"),
            "{text}"
        );
    }

    #[test]
    fn entry_json_is_the_one_shared_listing_shape() {
        // A6/A7b: `list_files`, `search_files` and `tree` must all serialize a
        // remote entry through this single chokepoint. `tree` layers only
        // `depth` on top. If a key here is renamed or dropped, three MCP
        // surfaces drift at once, so lock the exact key set.
        let e = entry("/root/sub/c.txt", false, 300);

        let v = entry_json(&e, false);
        let obj = v.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["is_dir", "modified", "name", "path", "size"]);
        assert_eq!(obj["path"], json!("/root/sub/c.txt"));
        assert_eq!(obj["is_dir"], json!(false));
        assert_eq!(obj["size"], json!(300));
        assert!(
            !obj.contains_key("permissions"),
            "permissions/owner are omitted unless include_permissions is set"
        );

        // include_permissions adds exactly the CLI `file_info` extras.
        let vp = entry_json(&e, true);
        let objp = vp.as_object().unwrap();
        assert!(objp.contains_key("permissions") && objp.contains_key("owner"));

        // The `tree` entry is this shape plus `depth`. Assert the union matches
        // what the BFS at `tree()` now produces.
        let mut tv = entry_json(&e, false);
        tv.as_object_mut()
            .unwrap()
            .insert("depth".to_string(), json!(2));
        let mut tkeys: Vec<&str> = tv.as_object().unwrap().keys().map(String::as_str).collect();
        tkeys.sort_unstable();
        assert_eq!(
            tkeys,
            ["depth", "is_dir", "modified", "name", "path", "size"]
        );
    }

    #[tokio::test]
    async fn dry_run_totals_the_whole_subtree_and_deletes_nothing() {
        let backend = FakeBackend::sample();
        let preview = preview_delete(&backend, "/root", true).await.unwrap();
        assert_eq!(preview.files, 3);
        assert_eq!(preview.directories, 2, "/root plus /root/sub");
        assert_eq!(preview.bytes, 600);
        assert!(!preview.truncated);
        assert!(!backend.touched_anything(), "a dry-run must delete nothing");
    }

    #[tokio::test]
    async fn dry_run_on_a_single_file_reports_its_size() {
        let backend = FakeBackend::sample();
        let preview = preview_delete(&backend, "/root/a.txt", false)
            .await
            .unwrap();
        assert_eq!(
            (preview.files, preview.directories, preview.bytes),
            (1, 0, 100)
        );
        assert!(!backend.touched_anything());
    }

    #[tokio::test]
    async fn dry_run_refuses_a_non_empty_directory_without_recursive() {
        let backend = FakeBackend::sample();
        let err = preview_delete(&backend, "/root", false).await.unwrap_err();
        assert!(
            err.to_string().contains("recursive=true"),
            "the error must name the fix, got: {err}"
        );
        assert!(!backend.touched_anything());
    }

    #[tokio::test]
    async fn dry_run_accepts_an_empty_directory_without_recursive() {
        let backend = FakeBackend::sample();
        let preview = preview_delete(&backend, "/empty", false).await.unwrap();
        assert_eq!(
            (preview.files, preview.directories, preview.bytes),
            (0, 1, 0)
        );
    }

    #[tokio::test]
    async fn recursive_flag_selects_the_recursive_backend_call() {
        // Regression guard: `recursive` used to be ignored, and the MCP
        // backend recursed unconditionally on any directory.
        let backend = FakeBackend::sample();
        delete_one(&backend, "/root", true, false).await.unwrap();
        assert_eq!(&*backend.deleted_recursive.lock().unwrap(), &["/root"]);
        assert!(backend.deleted.lock().unwrap().is_empty());

        let backend = FakeBackend::sample();
        delete_one(&backend, "/root/a.txt", false, false)
            .await
            .unwrap();
        assert_eq!(&*backend.deleted.lock().unwrap(), &["/root/a.txt"]);
        assert!(backend.deleted_recursive.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_failed_delete_of_a_non_empty_dir_names_the_fix() {
        // SFTP answers a non-empty `rmdir` with "Failure: Failure", which
        // tells an agent nothing about how to proceed.
        let mut backend = FakeBackend::sample();
        backend.delete_fails_with = Some("Failure: Failure".to_string());
        let err = delete_one(&backend, "/root", false, false)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("recursive=true"), "got: {msg}");
        assert!(msg.contains("Failure: Failure"), "keep the cause: {msg}");
    }

    #[tokio::test]
    async fn a_failed_delete_of_an_empty_dir_keeps_its_own_error() {
        // Suggesting `recursive=true` for a permission failure would be a lie.
        let mut backend = FakeBackend::sample();
        backend.delete_fails_with = Some("Permission denied".to_string());
        let err = delete_one(&backend, "/empty", false, false)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Permission denied"), "got: {msg}");
        assert!(!msg.contains("recursive=true"), "must not mislead: {msg}");
    }

    // --- destructive-default guards: speed and trash empty -----------------

    #[tokio::test]
    async fn speed_refuses_an_existing_remote_path() {
        // The speed test uploads to remote_path and then deletes it: a path
        // that already exists is a real user file, so the tool must refuse
        // before any upload.
        let backend = Arc::new(FakeBackend::sample());
        let ctx = test_ctx(Arc::clone(&backend));
        let err = speed(
            &ctx,
            &json!({
                "server": "s",
                "remote_path": "/root/a.txt",
                "size_mb": 1,
            }),
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("already exists"), "got: {msg}");
        assert!(backend.uploads.lock().unwrap().is_empty());
        assert!(backend.deleted.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn speed_runs_on_a_fresh_scratch_path() {
        // The guard must not break the normal case: a path that does not
        // exist yet runs the test and cleans up afterwards.
        let mut fake = FakeBackend::sample();
        fake.downloads
            .insert("/scratch/speed.bin".to_string(), vec![0u8; 16]);
        let backend = Arc::new(fake);
        let ctx = test_ctx(Arc::clone(&backend));
        let value = speed(
            &ctx,
            &json!({
                "server": "s",
                "remote_path": "/scratch/speed.bin",
                "size_mb": 1,
                "verify_integrity": false,
            }),
        )
        .await
        .expect("speed should run on a fresh path");
        assert_eq!(value["cleanup_ok"], json!(true));
        assert_eq!(
            backend.deleted.lock().unwrap().as_slice(),
            &["/scratch/speed.bin".to_string()]
        );
    }

    #[tokio::test]
    async fn speed_uses_fresh_content_for_every_iteration() {
        let backend = Arc::new(FakeBackend::sample());
        let ctx = test_ctx(Arc::clone(&backend));
        speed(
            &ctx,
            &json!({
                "server": "s",
                "iterations": 3,
                "size_mb": 1,
                "verify_integrity": false,
            }),
        )
        .await
        .expect("speed should complete");

        let uploads = backend.uploads.lock().unwrap();
        assert_eq!(uploads.len(), 3);
        assert_ne!(uploads[0].1, uploads[1].1);
        assert_ne!(uploads[1].1, uploads[2].1);
    }

    #[tokio::test]
    async fn speed_refuses_a_caller_named_path_when_the_check_cannot_complete() {
        // Fail-closed: a `stat` that fails for any reason OTHER than not-found
        // (expired token, dropped connection, provider 5xx) is not permission
        // to write. Reading every error as "absent" would hand the upload +
        // delete a path that may well hold user data.
        let mut fake = FakeBackend::sample();
        fake.stat_fails_with = Some("503 Service Unavailable".to_string());
        let backend = Arc::new(fake);
        let ctx = test_ctx(Arc::clone(&backend));
        let err = speed(
            &ctx,
            &json!({
                "server": "s",
                "remote_path": "/root/unverifiable.bin",
                "size_mb": 1,
            }),
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("cannot confirm"), "got: {msg}");
        assert!(
            msg.contains("503"),
            "the provider error must reach the caller: {msg}"
        );
        assert!(backend.uploads.lock().unwrap().is_empty());
        assert!(backend.deleted.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn speed_still_runs_on_the_scratch_path_when_the_check_cannot_complete() {
        // The strict check applies only to a caller-named path. The auto-generated
        // UUID scratch path holds no user data by construction, so a provider
        // whose `stat` is flaky or unsupported must not lose the speed test.
        let mut fake = FakeBackend::sample();
        fake.stat_fails_with = Some("stat unsupported by provider".to_string());
        let backend = Arc::new(fake);
        let ctx = test_ctx(Arc::clone(&backend));
        let value = speed(
            &ctx,
            &json!({
                "server": "s",
                "size_mb": 1,
                "verify_integrity": false,
            }),
        )
        .await
        .expect("an auto-generated scratch path must not be blocked by a flaky stat");
        assert_eq!(value["cleanup_ok"], json!(true));
        let uploaded = backend.uploads.lock().unwrap();
        assert_eq!(uploaded.len(), 1);
        assert!(
            uploaded[0].0.starts_with("/.aeroftp-speedtest-"),
            "got: {}",
            uploaded[0].0
        );
    }

    #[tokio::test]
    async fn trash_empty_defaults_to_dry_run() {
        let backend = Arc::new(FakeBackend::sample());
        let ctx = test_ctx(Arc::clone(&backend));
        let value = remote_trash(&ctx, &json!({"server": "s", "action": "empty"}))
            .await
            .expect("empty with defaults is a preview");
        assert_eq!(value["dry_run"], json!(true));
        let calls = backend.empty_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].2, "backend must be called with dry_run=true");
    }

    #[tokio::test]
    async fn trash_empty_whole_bucket_requires_confirm_flag() {
        let backend = Arc::new(FakeBackend::sample());
        let ctx = test_ctx(Arc::clone(&backend));
        let err = remote_trash(
            &ctx,
            &json!({"server": "s", "action": "empty", "dry_run": false}),
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("confirm_whole_bucket"), "got: {msg}");
        assert!(backend.empty_calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn trash_empty_scoped_prefix_purges_without_confirm() {
        let backend = Arc::new(FakeBackend::sample());
        let ctx = test_ctx(Arc::clone(&backend));
        let value = remote_trash(
            &ctx,
            &json!({
                "server": "s",
                "action": "empty",
                "dry_run": false,
                "prefix": "logs/",
            }),
        )
        .await
        .expect("a scoped purge needs no whole-bucket flag");
        assert_eq!(value["dry_run"], json!(false));
        let calls = backend.empty_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "logs/");
        assert!(!calls[0].2);
    }

    #[tokio::test]
    async fn trash_empty_whole_bucket_with_confirm_flag_proceeds() {
        let backend = Arc::new(FakeBackend::sample());
        let ctx = test_ctx(Arc::clone(&backend));
        let value = remote_trash(
            &ctx,
            &json!({
                "server": "s",
                "action": "empty",
                "dry_run": false,
                "confirm_whole_bucket": true,
            }),
        )
        .await
        .expect("explicit confirmation allows the whole-bucket purge");
        assert_eq!(value["dry_run"], json!(false));
        let calls = backend.empty_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "");
        assert!(!calls[0].2);
    }

    /// SECVAL-A (v4.2.0 pre-release validation, lead 7): the real MCP
    /// `aeroftp_download_file` handler validates `local_path`, then runs
    /// `create_dir_all(parent)` BEFORE the backend download. With a missing
    /// intermediate directory under a denied HOME prefix the validator must
    /// refuse, and nothing may be created inside `~/.ssh`. The fake backend's
    /// `download` always errors, so the only side effect a pass-through can
    /// leave is the directory tree.
    #[test]
    fn secval_a_download_file_refuses_missing_dir_under_home_ssh() {
        // Synchronous on purpose: the shared environment lock is a std mutex
        // and must not be held across an `.await`.
        let _env = crate::test_env::lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let home = std::fs::canonicalize(dir.path())
            .expect("canonical tempdir")
            .join("home");
        std::fs::create_dir_all(home.join(".ssh")).expect("create ~/.ssh");
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", &home);
        let target = home.join(".ssh").join("nested").join("authorized_keys");
        let ctx = test_ctx(Arc::new(FakeBackend::sample()));
        let out = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(download_file(
                &ctx,
                &json!({
                    "server": "s",
                    "remote_path": "/pub/key",
                    "local_path": target.to_str().expect("utf8"),
                }),
            ));
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        let created = home.join(".ssh").join("nested").exists();
        let msg = match &out {
            Ok(v) => format!("Ok({v})"),
            Err(e) => format!("Err({e})"),
        };
        assert!(
            !created,
            "download_file created {} inside the denied ~/.ssh; result: {msg}",
            home.join(".ssh").join("nested").display()
        );
        assert!(
            msg.contains("denied"),
            "expected a denylist refusal, got: {msg}"
        );
    }

    /// The local side of `reconcile` and `sync_doctor` is a directory the tool
    /// walks and, for reconcile with `checksum`, reads: it passes the same
    /// sensitive-path list as every other local path a remote tool takes
    /// (upload's and download's `local_path`). Both read it unchecked, so
    /// `~/.ssh` was listed, sized and hashed for any agent that asked.
    #[test]
    fn reconcile_and_sync_doctor_refuse_a_local_dir_under_home_ssh() {
        let _env = crate::test_env::lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let home = std::fs::canonicalize(dir.path())
            .expect("canonical tempdir")
            .join("home");
        std::fs::create_dir_all(home.join(".ssh")).expect("create ~/.ssh");
        std::fs::write(home.join(".ssh").join("id_ed25519"), b"key").expect("write key");
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", &home);
        let local_dir = home.join(".ssh");
        let args = json!({
            "server": "s",
            "local_dir": local_dir.to_str().expect("utf8"),
            "remote_dir": "/pub",
        });
        let ctx = test_ctx(Arc::new(FakeBackend::sample()));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let reconciled = runtime.block_on(reconcile(&ctx, &args));
        let doctored = runtime.block_on(sync_doctor(&ctx, &args));
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        for (tool, out) in [("reconcile", reconciled), ("sync_doctor", doctored)] {
            let msg = match &out {
                Ok(v) => format!("Ok({v})"),
                Err(e) => format!("Err({e})"),
            };
            assert!(
                out.is_err() && msg.contains("denied"),
                "{tool} read ~/.ssh: {msg}"
            );
        }
    }

    /// Swift's old listing: `/root/d1` held `d1` again at `/root/d1`, the
    /// folder inside itself. One copy of each file, `c.aerotmp` included.
    fn self_listing_backend() -> FakeBackend {
        let mut tree = std::collections::HashMap::new();
        tree.insert(
            "/root".to_string(),
            vec![entry("/root/a.txt", false, 4), entry("/root/d1", true, 0)],
        );
        tree.insert(
            "/root/d1".to_string(),
            vec![
                entry("/root/d1", true, 0),
                entry("/root/d1/b.txt", false, 4),
                entry("/root/d1/c.aerotmp", false, 4),
            ],
        );
        let downloads = std::collections::HashMap::from([
            ("/root/a.txt".to_string(), b"aaaa".to_vec()),
            ("/root/d1/b.txt".to_string(), b"bbbb".to_vec()),
            ("/root/d1/c.aerotmp".to_string(), b"cccc".to_vec()),
        ]);
        FakeBackend {
            tree,
            downloads,
            ..FakeBackend::sample()
        }
    }

    /// ImageKit's old listing: every folder with the path `/`, at the account
    /// root. One copy of each file.
    fn root_spelled_folders_backend() -> FakeBackend {
        let mut folder = entry("/", true, 0);
        folder.name = "d1".to_string();
        let mut tree = std::collections::HashMap::new();
        tree.insert(
            "/".to_string(),
            vec![
                entry("/a.txt", false, 4),
                folder,
                entry("/c.aerotmp", false, 4),
            ],
        );
        let downloads = std::collections::HashMap::from([
            ("/a.txt".to_string(), b"aaaa".to_vec()),
            ("/c.aerotmp".to_string(), b"cccc".to_vec()),
        ]);
        FakeBackend {
            tree,
            downloads,
            ..FakeBackend::sample()
        }
    }

    /// A folder whose listing names files of its parent again: the walk has
    /// no reason to refuse the folder, so only the tool itself can keep a file
    /// from being grouped with itself (dedupe) or deleted twice (cleanup).
    fn cross_listing_backend() -> FakeBackend {
        let mut tree = std::collections::HashMap::new();
        tree.insert(
            "/root".to_string(),
            vec![entry("/root/a.txt", false, 4), entry("/root/d1", true, 0)],
        );
        tree.insert(
            "/root/d1".to_string(),
            vec![
                entry("/root/a.txt", false, 4),
                entry("/root/c.aerotmp", false, 4),
            ],
        );
        tree.get_mut("/root")
            .unwrap()
            .push(entry("/root/c.aerotmp", false, 4));
        let downloads = std::collections::HashMap::from([
            ("/root/a.txt".to_string(), b"aaaa".to_vec()),
            ("/root/c.aerotmp".to_string(), b"cccc".to_vec()),
        ]);
        FakeBackend {
            tree,
            downloads,
            ..FakeBackend::sample()
        }
    }

    /// A listing that names a folder inside itself, or every folder as `/`,
    /// sent the walk through the same files again and again, and dedupe found
    /// each file to be a duplicate of itself: with `dry_run: false` it deleted
    /// the only copy. A listing that names a file outside the folder made it
    /// a duplicate of the folder's own copy, and one of the two was deleted.
    /// Each folder is now listed once, the looping one and the file outside
    /// are counted as scan errors, each path is grouped once whatever the
    /// listings say, and a tree of single copies loses nothing.
    #[tokio::test]
    async fn dedupe_deletes_nothing_when_a_listing_loops_over_single_copies() {
        for (backend, root, scanned, scan_errors) in [
            (self_listing_backend(), "/root", 3, 1),
            (root_spelled_folders_backend(), "/", 2, 1),
            (cross_listing_backend(), "/root", 2, 0),
            (outside_file_backend(), "/root", 3, 1),
        ] {
            let backend = Arc::new(backend);
            let ctx = test_ctx(Arc::clone(&backend));
            let args = json!({"server": "srv", "path": root, "mode": "newest", "dry_run": false});
            let report = dedupe(&ctx, &args).await.expect("dedupe runs");
            assert!(
                backend.deleted.lock().unwrap().is_empty(),
                "{root}: the only copies were deleted: {report}"
            );
            assert_eq!(report["duplicates_found"], 0, "{root}: {report}");
            assert_eq!(report["scanned"], scanned, "{root}: {report}");
            assert_eq!(report["scan_errors"], scan_errors, "{root}: {report}");
        }

        // A real second copy under the looping folder is still found, once.
        let mut backend = self_listing_backend();
        backend
            .tree
            .get_mut("/root/d1")
            .unwrap()
            .push(entry("/root/d1/a-copy.txt", false, 4));
        backend
            .downloads
            .insert("/root/d1/a-copy.txt".to_string(), b"aaaa".to_vec());
        let backend = Arc::new(backend);
        let ctx = test_ctx(Arc::clone(&backend));
        let args = json!({"server": "srv", "path": "/root", "mode": "newest", "dry_run": false});
        let report = dedupe(&ctx, &args).await.expect("dedupe runs");
        assert_eq!(report["duplicates_found"], 1, "{report}");
        assert_eq!(backend.deleted.lock().unwrap().len(), 1, "{report}");
    }

    /// The same two listings sent cleanup through the same orphan once per
    /// level: it deleted it, then failed or deleted it again for every repeat.
    /// It is deleted once, and the looping folder is a scan error. An orphan
    /// a listing names outside the folder was deleted with the folder's; it
    /// is a scan error and stays.
    #[tokio::test]
    async fn cleanup_deletes_each_orphan_once_when_a_listing_loops() {
        for (backend, root, orphan, scan_errors) in [
            (self_listing_backend(), "/root", "/root/d1/c.aerotmp", 1),
            (root_spelled_folders_backend(), "/", "/c.aerotmp", 1),
            (cross_listing_backend(), "/root", "/root/c.aerotmp", 0),
            (outside_file_backend(), "/root", "/root/d1/c.aerotmp", 1),
        ] {
            let backend = Arc::new(backend);
            let ctx = test_ctx(Arc::clone(&backend));
            let args = json!({"server": "srv", "path": root, "dry_run": false});
            let report = cleanup(&ctx, &args).await.expect("cleanup runs");
            assert_eq!(
                *backend.deleted.lock().unwrap(),
                [orphan],
                "{root}: {report}"
            );
            assert_eq!(report["orphans"], 1, "{root}: {report}");
            assert_eq!(report["scan_errors"], scan_errors, "{root}: {report}");
        }
    }

    /// A listing that names a folder twice: the walk lists it once, and
    /// nothing under it is counted twice.
    fn twice_named_backend() -> FakeBackend {
        let mut tree = std::collections::HashMap::new();
        tree.insert(
            "/root".to_string(),
            vec![
                entry("/root/a.txt", false, 4),
                entry("/root/d1", true, 0),
                entry("/root/d1", true, 0),
            ],
        );
        tree.insert(
            "/root/d1".to_string(),
            vec![entry("/root/d1/b.txt", false, 4)],
        );
        FakeBackend {
            tree,
            ..FakeBackend::sample()
        }
    }

    /// A listing of `/root` that names `/elsewhere`, a folder outside it with
    /// a big file a walk of `/root` must never reach.
    fn outside_root_backend() -> FakeBackend {
        let mut tree = std::collections::HashMap::new();
        tree.insert(
            "/root".to_string(),
            vec![entry("/root/a.txt", false, 4), entry("/elsewhere", true, 0)],
        );
        tree.insert(
            "/elsewhere".to_string(),
            vec![entry("/elsewhere/big.bin", false, 1000)],
        );
        FakeBackend {
            tree,
            ..FakeBackend::sample()
        }
    }

    /// A listing of `/root` that names `/keep.aerotmp`, a file outside it: an
    /// orphan to cleanup and a copy of `/root/a.txt` to dedupe, which a walk
    /// of `/root` must never count, delete, or delete `a.txt` in favour of.
    /// `/root/d1` holds a real orphan.
    fn outside_file_backend() -> FakeBackend {
        let mut tree = std::collections::HashMap::new();
        tree.insert(
            "/root".to_string(),
            vec![
                entry("/root/a.txt", false, 4),
                entry("/root/d1", true, 0),
                entry("/keep.aerotmp", false, 4),
            ],
        );
        tree.insert(
            "/root/d1".to_string(),
            vec![
                entry("/root/d1/b.txt", false, 4),
                entry("/root/d1/c.aerotmp", false, 4),
            ],
        );
        let downloads = std::collections::HashMap::from([
            ("/root/a.txt".to_string(), b"aaaa".to_vec()),
            ("/keep.aerotmp".to_string(), b"aaaa".to_vec()),
            ("/root/d1/b.txt".to_string(), b"bbbb".to_vec()),
            ("/root/d1/c.aerotmp".to_string(), b"cccc".to_vec()),
        ]);
        FakeBackend {
            tree,
            downloads,
            ..FakeBackend::sample()
        }
    }

    /// A listing of `/root/d1` that names its files under `/ROOT/d1`: on a
    /// case-sensitive store that is another folder. A read takes them, as a
    /// case-insensitive provider may spell a path its own way; a walk that
    /// deletes must not.
    fn other_case_backend() -> FakeBackend {
        let mut tree = std::collections::HashMap::new();
        tree.insert(
            "/root".to_string(),
            vec![entry("/root/a.txt", false, 4), entry("/root/d1", true, 0)],
        );
        tree.insert(
            "/root/d1".to_string(),
            vec![
                entry("/ROOT/d1/a-copy.txt", false, 4),
                entry("/ROOT/d1/c.aerotmp", false, 4),
            ],
        );
        let downloads = std::collections::HashMap::from([
            ("/root/a.txt".to_string(), b"aaaa".to_vec()),
            ("/ROOT/d1/a-copy.txt".to_string(), b"aaaa".to_vec()),
            ("/ROOT/d1/c.aerotmp".to_string(), b"cccc".to_vec()),
        ]);
        FakeBackend {
            tree,
            downloads,
            ..FakeBackend::sample()
        }
    }

    /// `cleanup` and `dedupe` delete, so they take a path only as the root
    /// spells it, letter case included, as a recursive delete does: the
    /// orphan and the copy under `/ROOT/d1` were deleted from a walk of
    /// `/root`. They are scan errors now and stay.
    #[tokio::test]
    async fn cleanup_and_dedupe_delete_nothing_a_listing_spells_in_another_case() {
        let backend = Arc::new(other_case_backend());
        let ctx = test_ctx(Arc::clone(&backend));
        let args = json!({"server": "srv", "path": "/root", "dry_run": false});
        let report = cleanup(&ctx, &args).await.expect("cleanup runs");
        assert!(backend.deleted.lock().unwrap().is_empty(), "{report}");
        assert_eq!(report["orphans"], 0, "{report}");
        assert_eq!(report["scan_errors"], 1, "{report}");

        let backend = Arc::new(other_case_backend());
        let ctx = test_ctx(Arc::clone(&backend));
        let args = json!({"server": "srv", "path": "/root", "mode": "newest", "dry_run": false});
        let report = dedupe(&ctx, &args).await.expect("dedupe runs");
        assert!(backend.deleted.lock().unwrap().is_empty(), "{report}");
        assert_eq!(report["duplicates_found"], 0, "{report}");
        assert_eq!(report["scan_errors"], 2, "{report}");
    }

    #[tokio::test]
    async fn destructive_walks_refuse_listing_derived_outside_root_aliases() {
        for tool in ["cleanup", "dedupe"] {
            let mut backend = FakeBackend::sample();
            backend.tree = std::collections::HashMap::from([(
                "/root".to_string(),
                vec![
                    entry("/root/a", false, 4),
                    entry("/outside/root/x.aerotmp", false, 4),
                ],
            )]);
            backend.downloads = std::collections::HashMap::from([
                ("/root/a".to_string(), b"same".to_vec()),
                ("/outside/root/x.aerotmp".to_string(), b"same".to_vec()),
            ]);
            let backend = Arc::new(backend);
            let ctx = test_ctx(Arc::clone(&backend));
            let args =
                json!({"server": "srv", "path": "/root", "mode": "newest", "dry_run": false});
            let report = if tool == "cleanup" {
                cleanup(&ctx, &args).await
            } else {
                dedupe(&ctx, &args).await
            }
            .unwrap();
            assert_eq!(report["scan_errors"], 1, "{tool}: {report}");
            assert!(
                backend.deleted.lock().unwrap().is_empty(),
                "{tool}: {report}"
            );
        }
    }

    /// What a walk of the root holds in each defective listing:
    /// (case, backend, root, files, bytes, folders listed with the root,
    /// entries refused). A folder that loops back or sits outside the root
    /// is refused, and so is a file outside it; one named twice is listed
    /// once, which loses nothing.
    #[allow(clippy::type_complexity)]
    fn defective_listings() -> Vec<(&'static str, FakeBackend, &'static str, u64, u64, u64, u64)> {
        vec![
            (
                "a folder listed inside itself",
                self_listing_backend(),
                "/root",
                3,
                12,
                2,
                1,
            ),
            (
                "every folder listed as /",
                root_spelled_folders_backend(),
                "/",
                2,
                8,
                1,
                1,
            ),
            (
                "a folder named twice",
                twice_named_backend(),
                "/root",
                2,
                8,
                2,
                0,
            ),
            (
                "a file named twice",
                cross_listing_backend(),
                "/root",
                2,
                8,
                2,
                0,
            ),
            (
                "a folder outside the root",
                outside_root_backend(),
                "/root",
                1,
                4,
                1,
                1,
            ),
            (
                "a file outside the root",
                outside_file_backend(),
                "/root",
                3,
                12,
                2,
                1,
            ),
        ]
    }

    /// A delete preview totals what a recursive delete of the folder removes:
    /// each path under it once. A listing that looped sent the preview round
    /// the same folder up to its depth cap, and one that named a folder
    /// outside added what that folder holds. A folder it does not follow
    /// makes the totals `truncated`.
    #[tokio::test]
    async fn a_delete_preview_counts_each_path_under_the_folder_once() {
        for (case, mut backend, root, files, bytes, dirs, refused) in defective_listings() {
            backend.stats.insert(root.to_string(), (true, 0));
            let preview = preview_delete(&backend, root, true).await.unwrap();
            assert_eq!(
                (preview.files, preview.bytes, preview.directories),
                (files, bytes, dirs),
                "{case}: {preview:?}"
            );
            assert_eq!(preview.truncated, refused > 0, "{case}: {preview:?}");
            assert!(
                !backend.touched_anything(),
                "{case}: a preview deletes nothing"
            );
        }
    }

    /// The same for the used-storage scan: each path under the root counted
    /// once, nothing from outside it, and a folder it does not follow makes
    /// the figure `scan_truncated`.
    #[tokio::test]
    async fn a_storage_scan_counts_each_path_under_the_root_once() {
        for (case, backend, root, files, bytes, dirs, refused) in defective_listings() {
            let ctx = test_ctx(Arc::new(backend));
            let args = json!({"server": "srv", "scan": true, "path": root});
            let report = storage_quota(&ctx, &args).await.expect("the scan runs");
            assert_eq!(report["scan_files"], files, "{case}: {report}");
            assert_eq!(report["used_bytes"], bytes, "{case}: {report}");
            assert_eq!(report["scan_dirs"], dirs, "{case}: {report}");
            assert_eq!(report["scan_truncated"], refused > 0, "{case}: {report}");
        }
    }

    /// A tree shows each path under the root once. A folder it does not
    /// follow gets no row (its path is not under the root, or names a folder
    /// already shown) and is named in `errors` with the reason.
    #[tokio::test]
    async fn a_tree_shows_each_path_under_the_root_once() {
        for (case, backend, root, files, _, dirs, refused) in defective_listings() {
            let ctx = test_ctx(Arc::new(backend));
            let args = json!({"server": "srv", "path": root, "max_depth": 5});
            let report = tree(&ctx, &args).await.expect("the tree runs");
            let rows: Vec<&str> = report["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["path"].as_str().unwrap())
                .collect();
            let unique: std::collections::HashSet<&str> = rows.iter().copied().collect();
            assert_eq!(
                unique.len(),
                rows.len(),
                "{case}: a path shown twice: {rows:?}"
            );
            assert_eq!(rows.len() as u64, files + dirs - 1, "{case}: {rows:?}");
            assert!(
                rows.iter().all(
                    |row| crate::sync_core::WalkScope::new(root).locate(root, row)
                        != crate::sync_core::WalkStep::OutsideRoot
                ),
                "{case}: a row outside {root}: {rows:?}"
            );
            assert_eq!(report["total_files"], files, "{case}: {report}");
            assert_eq!(report["total_dirs"], dirs - 1, "{case}: {report}");
            let errors = report["errors"].as_array().unwrap();
            assert_eq!(errors.len() as u64, refused, "{case}: {report}");
            for error in errors {
                let text = error["error"].as_str().unwrap();
                assert!(text.contains("names it"), "{case}: {error}");
            }
        }
    }

    /// `sync_doctor` counts what a sync would read on each side, and must say
    /// when that count is partial. A folder it does not follow is counted
    /// like a folder it could not list: the remote scan is incomplete, the
    /// status asks for attention, and a risk says the counts are partial.
    #[tokio::test]
    async fn sync_doctor_counts_each_remote_path_once_and_says_when_it_did_not() {
        for (case, backend, root, files, bytes, _, refused) in defective_listings() {
            let local = tempfile::tempdir().expect("tempdir");
            let ctx = test_ctx(Arc::new(backend));
            let args = json!({
                "server": "srv",
                "local_dir": local.path().to_string_lossy(),
                "remote_dir": root,
            });
            let report = sync_doctor(&ctx, &args).await.expect("the doctor runs");
            let summary = &report["summary"];
            assert_eq!(summary["remote_files"], files, "{case}: {report}");
            assert_eq!(summary["remote_bytes"], bytes, "{case}: {report}");
            assert_eq!(
                summary["remote_scan_incomplete"],
                refused > 0,
                "{case}: {report}"
            );
            assert_eq!(summary["remote_scan_errors"], refused, "{case}: {report}");
            assert_eq!(summary["remote_scan_truncated"], false, "{case}: {report}");
            assert_eq!(summary["local_scan_incomplete"], false, "{case}: {report}");
            let expected = if refused > 0 { "attention" } else { "ok" };
            assert_eq!(report["status"], expected, "{case}: {report}");
            let partial = report["risks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|risk| risk.as_str().unwrap().contains("partial"));
            assert_eq!(partial, refused > 0, "{case}: {report}");
        }
    }

    /// `reconcile` compares only what sits under the remote folder, and a
    /// comparison against a scan that missed part of the tree is `partial`,
    /// not a verdict: a file it never saw would read as missing.
    #[tokio::test]
    async fn reconcile_compares_each_remote_path_once_and_says_when_it_did_not() {
        for (case, backend, root, files, _, _, refused) in defective_listings() {
            let local = tempfile::tempdir().expect("tempdir");
            let ctx = test_ctx(Arc::new(backend));
            let args = json!({
                "server": "srv",
                "local_dir": local.path().to_string_lossy(),
                "remote_dir": root,
            });
            let report = reconcile(&ctx, &args).await.expect("reconcile runs");
            let summary = &report["summary"];
            assert_eq!(summary["missing_local_count"], files, "{case}: {report}");
            assert_eq!(
                summary["remote_scan_incomplete"],
                refused > 0,
                "{case}: {report}"
            );
            assert_eq!(summary["remote_scan_errors"], refused, "{case}: {report}");
            assert_eq!(summary["remote_scan_truncated"], false, "{case}: {report}");
            assert_eq!(summary["local_scan_incomplete"], false, "{case}: {report}");
            let expected = if refused > 0 {
                "partial"
            } else {
                "differences_found"
            };
            assert_eq!(report["status"], expected, "{case}: {report}");
        }
    }

    /// A folder the remote cannot list was skipped in silence: the doctor
    /// answered `ok` and reconcile gave a verdict on a tree it had not read.
    /// Both now say the remote scan is incomplete.
    #[tokio::test]
    async fn sync_doctor_and_reconcile_say_when_a_remote_folder_could_not_be_listed() {
        let unlistable = || {
            let mut tree = std::collections::HashMap::new();
            tree.insert(
                "/root".to_string(),
                vec![entry("/root/a.txt", false, 4), entry("/root/gone", true, 0)],
            );
            Arc::new(FakeBackend {
                tree,
                ..FakeBackend::sample()
            })
        };
        let local = tempfile::tempdir().expect("tempdir");
        let args = json!({
            "server": "srv",
            "local_dir": local.path().to_string_lossy(),
            "remote_dir": "/root",
        });

        let doctor = sync_doctor(&test_ctx(unlistable()), &args).await.unwrap();
        assert_eq!(doctor["status"], "attention", "{doctor}");
        assert_eq!(
            doctor["summary"]["remote_scan_incomplete"], true,
            "{doctor}"
        );
        assert_eq!(doctor["summary"]["remote_scan_errors"], 1, "{doctor}");
        assert_eq!(doctor["summary"]["remote_files"], 1, "{doctor}");

        let reconciled = reconcile(&test_ctx(unlistable()), &args).await.unwrap();
        assert_eq!(reconciled["status"], "partial", "{reconciled}");
        assert_eq!(
            reconciled["summary"]["remote_scan_incomplete"], true,
            "{reconciled}"
        );
        assert_eq!(
            reconciled["summary"]["remote_scan_errors"], 1,
            "{reconciled}"
        );
    }

    /// The same for a local folder the walk cannot read: its files are not
    /// counted, and the report says the local scan is incomplete.
    #[cfg(unix)]
    #[tokio::test]
    async fn sync_doctor_and_reconcile_say_when_a_local_folder_could_not_be_read() {
        use std::os::unix::fs::PermissionsExt;
        let local = tempfile::tempdir().expect("tempdir");
        let locked = local.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::write(locked.join("x.txt"), b"x").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let readable = std::fs::read_dir(&locked).is_ok();
        let args = json!({
            "server": "srv",
            "local_dir": local.path().to_string_lossy(),
            "remote_dir": "/root",
        });
        let doctor = sync_doctor(&test_ctx(Arc::new(FakeBackend::sample())), &args).await;
        let reconciled = reconcile(&test_ctx(Arc::new(FakeBackend::sample())), &args).await;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        if readable {
            // Running as root: nothing is unreadable, so there is nothing to see.
            return;
        }
        let doctor = doctor.unwrap();
        assert_eq!(doctor["status"], "attention", "{doctor}");
        assert_eq!(doctor["summary"]["local_scan_incomplete"], true, "{doctor}");
        assert_eq!(doctor["summary"]["local_scan_errors"], 1, "{doctor}");
        assert_eq!(
            doctor["summary"]["remote_scan_incomplete"], false,
            "{doctor}"
        );
        let reconciled = reconciled.unwrap();
        assert_eq!(reconciled["status"], "partial", "{reconciled}");
        assert_eq!(
            reconciled["summary"]["local_scan_incomplete"], true,
            "{reconciled}"
        );
        assert_eq!(
            reconciled["summary"]["local_scan_errors"], 1,
            "{reconciled}"
        );
    }

    /// A provider may spell the root its own way: a relative root resolved
    /// against the home folder, another letter case, Yandex's `disk:`
    /// namespace, spaces trimmed. Its listing then names the children under
    /// that spelling, and every walk still reads the whole tree: two files,
    /// one in a subfolder, nothing refused.
    #[tokio::test]
    async fn a_root_the_provider_spells_its_own_way_is_walked_whole() {
        for (root, spelled) in [
            ("docs", "/home/u/docs"),
            ("/Docs", "/docs"),
            ("disk:/docs", "/docs"),
            ("/docs ", "/docs"),
        ] {
            let backend = || {
                let mut tree = std::collections::HashMap::new();
                tree.insert(
                    root.to_string(),
                    vec![
                        entry(&format!("{spelled}/a.txt"), false, 4),
                        entry(&format!("{spelled}/sub"), true, 0),
                    ],
                );
                tree.insert(
                    format!("{spelled}/sub"),
                    vec![entry(&format!("{spelled}/sub/b.txt"), false, 4)],
                );
                let mut backend = FakeBackend {
                    tree,
                    ..FakeBackend::sample()
                };
                backend.stats.insert(root.to_string(), (true, 0));
                Arc::new(backend)
            };

            let preview = preview_delete(backend().as_ref(), root, true)
                .await
                .unwrap();
            assert_eq!((preview.files, preview.truncated), (2, false), "{root:?}");

            let args = json!({"server": "srv", "scan": true, "path": root});
            let quota = storage_quota(&test_ctx(backend()), &args).await.unwrap();
            assert_eq!(quota["scan_files"], 2, "{root:?}: {quota}");
            assert_eq!(quota["scan_truncated"], false, "{root:?}: {quota}");

            let args = json!({"server": "srv", "path": root});
            let shown = tree(&test_ctx(backend()), &args).await.unwrap();
            assert_eq!(shown["total_files"], 2, "{root:?}: {shown}");
            assert_eq!(shown["errors"], json!([]), "{root:?}: {shown}");

            let local = tempfile::tempdir().expect("tempdir");
            let args = json!({
                "server": "srv",
                "local_dir": local.path().to_string_lossy(),
                "remote_dir": root,
            });
            let doctor = sync_doctor(&test_ctx(backend()), &args).await.unwrap();
            assert_eq!(doctor["summary"]["remote_files"], 2, "{root:?}: {doctor}");
            assert_eq!(doctor["status"], "ok", "{root:?}: {doctor}");
            let reconciled = reconcile(&test_ctx(backend()), &args).await.unwrap();
            assert_eq!(reconciled["summary"]["missing_local_count"], 2, "{root:?}");
            assert_eq!(
                reconciled["status"], "differences_found",
                "{root:?}: {reconciled}"
            );
        }
    }
}
