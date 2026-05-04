//! AI Tool Execution via StorageProvider trait + FTP fallback
//!
//! Provides a unified `execute_ai_tool` command that routes AI tool calls
//! through the active StorageProvider (14 protocols). When no provider is
//! connected, falls back to `AppState.ftp_manager` for FTP/FTPS sessions.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use crate::provider_commands::ProviderState;
use crate::AppState;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::LazyLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{Emitter, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tokio::process::Command as TokioCommand;
use uuid::Uuid;

/// Allowed tool names (whitelist)
const ALLOWED_TOOLS: &[&str] = &[
    "remote_list",
    "remote_read",
    "remote_upload",
    "remote_download",
    "remote_delete",
    "remote_rename",
    "remote_mkdir",
    "remote_search",
    "remote_info",
    "local_list",
    "local_read",
    "local_write",
    "local_mkdir",
    "local_delete",
    "local_rename",
    "local_search",
    "local_edit",
    "local_move_files",
    "local_batch_rename",
    "local_copy_files",
    "local_trash",
    "local_file_info",
    "local_disk_usage",
    "local_find_duplicates",
    "remote_edit",
    // Batch transfer tools
    "upload_files",
    "download_files",
    "generate_transfer_plan",
    // Advanced tools
    "sync_preview",
    "archive_compress",
    "archive_decompress",
    // RAG tools
    "rag_index",
    "rag_search",
    // Preview tools
    "preview_edit",
    // Agent memory
    "agent_memory_write",
    // Cyber tools
    "hash_file",
    // Content inspection tools
    "local_grep",
    "local_head",
    "local_tail",
    "local_stat_batch",
    "local_diff",
    "local_tree",
    // Clipboard tools
    "clipboard_read",
    "clipboard_write",
    // App control tools
    "set_theme",
    "app_info",
    "sync_control",
    "vault_peek",
    // Shell execution
    "shell_execute",
    // Server management (cross-server operations via saved profiles)
    "server_list_saved",
    "server_exec",
    "cross_profile_transfer",
];

const AI_APPROVAL_REQUIRED_REASON: &str =
    "This AI tool requires explicit approval in the AeroFTP desktop backend. Re-run it through the approved AI flow.";
const AI_APPROVAL_REQUEST_TTL_MS: u64 = 5 * 60 * 1000;
const AI_ONE_SHOT_GRANT_TTL_MS: u64 = 2 * 60 * 1000;
const AI_SESSION_GRANT_TTL_MS: u64 = 8 * 60 * 60 * 1000;
const MAX_AI_APPROVAL_REQUESTS: usize = 256;
const MAX_AI_APPROVAL_GRANTS: usize = 512;

fn server_exec_operation(args: &Value) -> Option<&str> {
    args.get("operation").and_then(|value| value.as_str())
}

fn server_exec_is_mutating(args: &Value) -> bool {
    !matches!(
        server_exec_operation(args),
        Some("ls" | "cat" | "stat" | "find" | "df")
    )
}

fn sync_control_requires_approval(args: &Value) -> bool {
    !matches!(
        args.get("action").and_then(|value| value.as_str()),
        Some("status")
    )
}

fn requires_backend_write_approval(tool_name: &str, args: &Value) -> bool {
    match tool_name {
        "sync_control" => sync_control_requires_approval(args),
        "server_exec" | "cross_profile_transfer" => true,
        _ => matches!(
            tool_name,
            "remote_upload"
                | "remote_delete"
                | "remote_rename"
                | "remote_mkdir"
                | "remote_edit"
                | "local_write"
                | "local_mkdir"
                | "local_delete"
                | "local_rename"
                | "local_edit"
                | "local_move_files"
                | "local_batch_rename"
                | "local_copy_files"
                | "local_trash"
                | "upload_files"
                | "download_files"
                | "archive_compress"
                | "archive_decompress"
                | "clipboard_write"
                | "shell_execute"
        ),
    }
}

fn allows_session_grant(tool_name: &str, args: &Value) -> bool {
    if tool_name == "shell_execute" {
        return false;
    }

    if (tool_name == "server_exec" && server_exec_is_mutating(args))
        || tool_name == "cross_profile_transfer"
    {
        return false;
    }

    !matches!(
        tool_name,
        "remote_delete" | "local_delete" | "local_trash" | "archive_decompress"
    )
}

/// Validate a remote path argument: reject null bytes and leading dash (argument injection)
pub(crate) fn validate_remote_path(path: &str, param: &str) -> Result<(), String> {
    if path.contains('\0') {
        return Err(format!("{}: path contains null bytes", param));
    }
    if path.starts_with('-') {
        return Err(format!(
            "{}: path must not start with '-' (argument injection risk)",
            param
        ));
    }
    if path.len() > 4096 {
        return Err(format!("{}: path exceeds 4096 characters", param));
    }
    Ok(())
}

/// Validate a path argument: reject null bytes, traversal, excessive length
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
            .unwrap_or(Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no parent",
            )))
    });
    if let Ok(canonical) = resolved {
        let s = canonical.to_string_lossy();
        // Block sensitive system paths (deny-list)
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
        if denied.iter().any(|d| s.starts_with(d)) {
            return Err(format!("{}: access to system path denied: {}", param, s));
        }
        // Block sensitive home-relative paths
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
                if s.starts_with(&format!("{}/{}", home, sensitive)) {
                    return Err(format!("{}: access to sensitive path denied: {}", param, s));
                }
            }
        }
        // Block runtime secrets directory
        if s.starts_with("/run/secrets") {
            return Err(format!("{}: access to system path denied: {}", param, s));
        }
    }
    Ok(())
}

// `get_str` / `get_str_opt` live in `ai_core::local_tools` after the
// T3 Gate 3 cleanup: every tool body that used them now sits in the
// unified dispatcher.

/// Check if the StorageProvider is connected
pub(crate) async fn has_provider(state: &ProviderState) -> bool {
    state.provider.lock().await.is_some()
}

/// Check if FTP manager has an active connection
pub(crate) async fn has_ftp(app_state: &AppState) -> bool {
    app_state.ftp_manager.lock().await.is_connected()
}

/// Emit tool progress event for iterative operations
pub(crate) fn emit_tool_progress(
    app: &tauri::AppHandle,
    tool: &str,
    current: u32,
    total: u32,
    item: &str,
) {
    let _ = app.emit(
        "ai-tool-progress",
        json!({
            "tool": tool,
            "current": current,
            "total": total,
            "item": item,
        }),
    );
}

const MAX_AI_DOWNLOAD_SIZE: u64 = 50 * 1024 * 1024; // 50MB

// The legacy AI_TOOL_RESULT_CACHE / CachedToolResult / gc_tool_cache /
// get/store/invalidate helpers were removed in T3 Gate 3. The unified
// dispatcher (ai_core::tools::dispatch_tool) does not yet expose a
// per-session result cache; reintroduce it as a ToolCtx feature when
// real demand surfaces. cache_session_key + build_tool_cache_key stay
// because the approval pipeline still uses them as scope keys.

pub(crate) fn cache_session_key(session_id: Option<&str>) -> String {
    session_id
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("__default__")
        .to_string()
}

fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_millis() as u64
}

#[derive(Clone)]
struct AiToolApprovalRequest {
    session_key: String,
    tool_name: String,
    scope_key: String,
    created_at_ms: u64,
    allow_session_grant: bool,
    message: String,
}

#[derive(Clone)]
struct AiToolApprovalGrant {
    session_key: String,
    tool_name: String,
    scope_key: String,
    created_at_ms: u64,
    expires_at_ms: u64,
    remember_for_session: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiToolApprovalPreparation {
    pub approval_required: bool,
    pub request_id: Option<String>,
    pub allow_session_grant: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiToolApprovalGrantResponse {
    pub approved: bool,
    pub grant_id: Option<String>,
}

static AI_TOOL_APPROVAL_REQUESTS: LazyLock<
    tokio::sync::Mutex<HashMap<String, AiToolApprovalRequest>>,
> = LazyLock::new(|| tokio::sync::Mutex::new(HashMap::new()));
static AI_TOOL_APPROVAL_GRANTS: LazyLock<tokio::sync::Mutex<HashMap<String, AiToolApprovalGrant>>> =
    LazyLock::new(|| tokio::sync::Mutex::new(HashMap::new()));

fn truncate_display(value: &str, max_len: usize) -> String {
    if value.chars().count() <= max_len {
        return value.to_string();
    }

    let truncated: String = value.chars().take(max_len).collect();
    format!("{}...", truncated)
}

fn format_approval_value(value: &Value) -> String {
    match value {
        Value::String(string_value) => truncate_display(string_value, 160),
        Value::Array(items) => format!("{} item(s)", items.len()),
        Value::Bool(boolean_value) => boolean_value.to_string(),
        Value::Number(number_value) => number_value.to_string(),
        Value::Null => "null".to_string(),
        other => truncate_display(&other.to_string(), 160),
    }
}

fn build_ai_tool_approval_details(tool_name: &str, args: &Value) -> Vec<String> {
    let mut details = vec![format!("tool: {}", tool_name)];

    for key in [
        "server",
        "operation",
        "command",
        "path",
        "local_path",
        "remote_path",
        "from",
        "to",
        "destination",
        "local_dir",
        "remote_dir",
        "pattern",
        "action",
        "theme",
        "entry",
        "category",
    ] {
        if let Some(value) = args.get(key) {
            details.push(format!("{}: {}", key, format_approval_value(value)));
        }
    }

    if let Some(paths) = args.get("paths").and_then(|value| value.as_array()) {
        let preview: Vec<String> = paths
            .iter()
            .take(3)
            .filter_map(|value| value.as_str())
            .map(|path| truncate_display(path, 120))
            .collect();

        if preview.is_empty() {
            details.push(format!("paths: {} item(s)", paths.len()));
        } else {
            details.push(format!("paths: {} item(s)", paths.len()));
            for item in preview {
                details.push(format!("path item: {}", item));
            }
        }
    }

    if tool_name == "cross_profile_transfer" {
        for key in [
            "source_server",
            "dest_server",
            "source_path",
            "dest_path",
            "recursive",
            "skip_existing",
            "dry_run",
        ] {
            if let Some(value) = args.get(key) {
                details.push(format!("{}: {}", key, format_approval_value(value)));
            }
        }
    }

    details
}

fn human_tool_label(tool_name: &str) -> &str {
    match tool_name {
        "local_write" => "Write Local File",
        "local_delete" => "Delete Local File",
        "local_trash" => "Move to Trash",
        "local_mkdir" => "Create Local Directory",
        "local_rename" => "Rename Local File",
        "local_edit" => "Edit Local File",
        "local_move_files" => "Move Local Files",
        "local_batch_rename" => "Batch Rename Files",
        "local_copy_files" => "Copy Local Files",
        "remote_upload" => "Upload to Remote",
        "remote_download" => "Download from Remote",
        "remote_delete" => "Delete Remote File",
        "remote_rename" => "Rename Remote File",
        "remote_mkdir" => "Create Remote Directory",
        "remote_edit" => "Edit Remote File",
        "upload_files" => "Upload Multiple Files",
        "download_files" => "Download Multiple Files",
        "shell_execute" => "Execute Shell Command",
        "server_exec" => "Server Operation",
        "cross_profile_transfer" => "Cross-Profile Transfer",
        "archive_compress" => "Create Archive",
        "archive_decompress" => "Extract Archive",
        "clipboard_write" => "Write to Clipboard",
        "sync_control" => "Sync Control",
        other => other,
    }
}

pub(crate) fn build_ai_tool_approval_message(tool_name: &str, args: &Value) -> String {
    let label = human_tool_label(tool_name);
    let mut lines = vec![format!("AeroAgent wants to: {}", label), String::new()];

    let details = build_ai_tool_approval_details(tool_name, args);
    // Skip the first detail (tool name, already in the title)
    for detail in details.into_iter().skip(1) {
        lines.push(format!("  {}", detail));
    }

    if tool_name == "server_exec" {
        lines.push(String::new());
        lines.push("Uses saved server credentials from the vault.".to_string());
    }

    if tool_name == "shell_execute" {
        lines.push(String::new());
        lines.push("Runs a shell command on this machine.".to_string());
    }

    lines.push(String::new());
    lines.push("This confirmation runs in the desktop process, not in the webview.".to_string());

    lines.join("\n")
}

fn approval_scope_message(remember_for_session: bool) -> &'static str {
    if remember_for_session {
        "Grant scope: remember this tool for the current chat session."
    } else {
        "Grant scope: approve this exact tool plus argument set once."
    }
}

fn prune_ai_tool_approval_requests(requests: &mut HashMap<String, AiToolApprovalRequest>) {
    let now = current_time_ms();
    requests.retain(|_, request| {
        now.saturating_sub(request.created_at_ms) <= AI_APPROVAL_REQUEST_TTL_MS
    });

    while requests.len() > MAX_AI_APPROVAL_REQUESTS {
        let Some(oldest_id) = requests
            .iter()
            .min_by_key(|(_, request)| request.created_at_ms)
            .map(|(request_id, _)| request_id.clone())
        else {
            break;
        };
        requests.remove(&oldest_id);
    }
}

fn prune_ai_tool_approval_grants(grants: &mut HashMap<String, AiToolApprovalGrant>) {
    let now = current_time_ms();
    grants.retain(|_, grant| grant.expires_at_ms > now);

    while grants.len() > MAX_AI_APPROVAL_GRANTS {
        let Some(oldest_id) = grants
            .iter()
            .min_by_key(|(_, grant)| grant.created_at_ms)
            .map(|(grant_id, _)| grant_id.clone())
        else {
            break;
        };
        grants.remove(&oldest_id);
    }
}

fn has_matching_session_grant(
    grants: &HashMap<String, AiToolApprovalGrant>,
    session_key: &str,
    tool_name: &str,
    scope_key: &str,
    now: u64,
) -> bool {
    grants.values().any(|grant| {
        grant.remember_for_session
            && grant.expires_at_ms > now
            && grant.session_key == session_key
            && grant.tool_name == tool_name
            && grant.scope_key == scope_key
    })
}

pub(crate) async fn ensure_ai_tool_approval(
    session_id: Option<&str>,
    tool_name: &str,
    scope_key: &str,
    approval_grant_id: Option<&str>,
) -> Result<(), String> {
    let session_key = cache_session_key(session_id);
    let now = current_time_ms();
    let mut grants = AI_TOOL_APPROVAL_GRANTS.lock().await;
    prune_ai_tool_approval_grants(&mut grants);

    if let Some(grant_id) = approval_grant_id {
        let Some(grant) = grants.get(grant_id).cloned() else {
            return Err(AI_APPROVAL_REQUIRED_REASON.to_string());
        };

        let scope_matches = if grant.remember_for_session {
            true
        } else {
            grant.scope_key == scope_key
        };

        if grant.session_key != session_key
            || grant.tool_name != tool_name
            || !scope_matches
            || grant.expires_at_ms <= now
        {
            grants.remove(grant_id);
            return Err(AI_APPROVAL_REQUIRED_REASON.to_string());
        }

        if !grant.remember_for_session {
            grants.remove(grant_id);
        }

        return Ok(());
    }

    if has_matching_session_grant(&grants, &session_key, tool_name, scope_key, now) {
        return Ok(());
    }

    Err(AI_APPROVAL_REQUIRED_REASON.to_string())
}

pub(crate) async fn prepare_backend_approval_request(
    session_id: Option<&str>,
    tool_name: &str,
    scope_key: String,
    allow_session_grant: bool,
    message: String,
) -> AiToolApprovalPreparation {
    let session_key = cache_session_key(session_id);

    {
        let now = current_time_ms();
        let mut grants = AI_TOOL_APPROVAL_GRANTS.lock().await;
        prune_ai_tool_approval_grants(&mut grants);
        if has_matching_session_grant(&grants, &session_key, tool_name, &scope_key, now) {
            return AiToolApprovalPreparation {
                approval_required: false,
                request_id: None,
                allow_session_grant: false,
            };
        }
    }

    let request_id = Uuid::new_v4().to_string();
    let request = AiToolApprovalRequest {
        session_key,
        tool_name: tool_name.to_string(),
        scope_key,
        created_at_ms: current_time_ms(),
        allow_session_grant,
        message,
    };

    let mut requests = AI_TOOL_APPROVAL_REQUESTS.lock().await;
    prune_ai_tool_approval_requests(&mut requests);
    requests.insert(request_id.clone(), request);

    AiToolApprovalPreparation {
        approval_required: true,
        request_id: Some(request_id),
        allow_session_grant,
    }
}

fn build_tool_cache_key(
    tool_name: &str,
    args: &Value,
    context_local_path: Option<&str>,
    remote_context: Option<&str>,
) -> Result<String, String> {
    serde_json::to_string(&json!({
        "tool": tool_name,
        "args": args,
        "context_local_path": context_local_path,
        "remote_context": remote_context,
    }))
    .map_err(|e| format!("Failed to build tool cache key: {}", e))
}

async fn build_remote_cache_context(state: &ProviderState, app_state: &AppState) -> Option<String> {
    if let Some(config) = state.config.lock().await.clone() {
        return Some(format!(
            "provider:{}:{}:{}:{}:{}",
            serde_json::to_string(&config.provider_type).ok()?,
            config.host,
            config.effective_port(),
            config.username.as_deref().unwrap_or(""),
            config.initial_path.as_deref().unwrap_or(""),
        ));
    }

    let ftp_manager = app_state.ftp_manager.lock().await;
    ftp_manager
        .connected_host()
        .map(|host| format!("ftp:{}", host))
}

// `value_as_string_array` lives in `ai_core::local_tools` so the unified
// dispatcher and per-area handlers (gui_tools, system_tools, ...) all
// share a single implementation.

pub(crate) fn path_basename(path: &str) -> Option<String> {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
}

pub(crate) fn join_remote_path(base: &str, leaf: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        leaf.trim_start_matches('/')
    )
}

/// Download a remote file to bytes via StorageProvider or FTP fallback
pub(crate) async fn download_from_provider(
    state: &State<'_, ProviderState>,
    app_state: &State<'_, AppState>,
    path: &str,
) -> Result<Vec<u8>, String> {
    if has_provider(state).await {
        let mut provider = state.provider.lock().await;
        let provider = match provider.as_mut() {
            Some(p) => p,
            None => return Err("No active provider connection".into()),
        };
        // Check file size before downloading
        if let Ok(entry) = provider.stat(path).await {
            if entry.size > MAX_AI_DOWNLOAD_SIZE {
                return Err(format!(
                    "File too large to download ({:.1} MB). Limit is {} MB.",
                    entry.size as f64 / 1_048_576.0,
                    MAX_AI_DOWNLOAD_SIZE / 1_048_576
                ));
            }
        }
        provider
            .download_to_bytes(path)
            .await
            .map_err(|e| e.to_string())
    } else if has_ftp(app_state).await {
        let mut manager = app_state.ftp_manager.lock().await;
        manager
            .download_to_bytes(path)
            .await
            .map_err(|e| e.to_string())
    } else {
        Err("Not connected to any server".to_string())
    }
}

#[tauri::command]
pub async fn validate_tool_args(tool_name: String, args: Value) -> Result<Value, String> {
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
                // Skip existence checks for relative paths: they will be resolved
                // against context_local_path at execution time
                if p.is_absolute() {
                    if tool_name == "local_list" || tool_name == "local_search" {
                        if !p.is_dir() {
                            errors.push(format!("Directory not found: {}", path));
                        }
                    } else if !p.exists() {
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
                // Check parent exists (only for absolute paths: relative paths
                // will be resolved against context_local_path at execution time)
                if p.is_absolute() {
                    if let Some(parent) = p.parent() {
                        if !parent.exists() {
                            warnings.push(format!(
                                "Parent directory does not exist: {}",
                                parent.display()
                            ));
                        }
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
                    warnings.push(format!("Path does not exist (nothing to delete): {}", path));
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
                    if path.is_absolute() && !path.exists() {
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
                    if path.is_absolute() && !path.exists() {
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
                    errors.push(format!(
                        "Unsupported format: {}. Use zip, 7z, tar, tar.gz, tar.bz2, or tar.xz",
                        fmt
                    ));
                }
            }
        }
        "archive_decompress" => {
            if let Some(path) = args.get("archive_path").and_then(|v| v.as_str()) {
                if let Err(e) = validate_path(path, "archive_path") {
                    errors.push(e);
                }
                let p = std::path::Path::new(path);
                if p.is_absolute() && !p.exists() {
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
                    errors.push(format!(
                        "Unsupported algorithm: {}. Use md5, sha1, sha256, sha512, or blake3",
                        algo
                    ));
                }
            }
        }
        "local_file_info" | "local_disk_usage" | "local_find_duplicates" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                let p = std::path::Path::new(path);
                if !p.exists() {
                    errors.push(format!("Path not found: {}", path));
                }
                if (tool_name == "local_disk_usage" || tool_name == "local_find_duplicates")
                    && !p.is_dir()
                {
                    errors.push(format!("Path is not a directory: {}", path));
                }
            }
        }
        "local_grep" | "local_tree" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                let p = std::path::Path::new(path);
                if !p.is_dir() {
                    errors.push(format!("Directory not found: {}", path));
                }
            }
            if tool_name == "local_grep" {
                if let Some(pattern) = args.get("pattern").and_then(|v| v.as_str()) {
                    if regex::Regex::new(pattern).is_err() {
                        errors.push(format!("Invalid regex pattern: {}", pattern));
                    }
                } else {
                    errors.push("Missing 'pattern' parameter".to_string());
                }
            }
        }
        "local_head" | "local_tail" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                let p = std::path::Path::new(path);
                if !p.exists() {
                    errors.push(format!("File not found: {}", path));
                } else if p.is_dir() {
                    errors.push(format!("Path is a directory, not a file: {}", path));
                }
            }
        }
        "local_stat_batch" => {
            let paths = args.get("paths").and_then(|v| v.as_array());
            if paths.is_none() || paths.is_some_and(|a| a.is_empty()) {
                errors.push("'paths' array is missing or empty".to_string());
            } else if let Some(arr) = paths {
                if arr.len() > 100 {
                    errors.push(format!("Too many paths: {} (max 100)", arr.len()));
                }
            }
        }
        "local_diff" => {
            for key in &["path_a", "path_b"] {
                if let Some(path) = args.get(key).and_then(|v| v.as_str()) {
                    if let Err(e) = validate_path(path, key) {
                        errors.push(e);
                    }
                    let p = std::path::Path::new(path);
                    if !p.exists() {
                        errors.push(format!("File not found: {}", path));
                    } else if p.is_dir() {
                        errors.push(format!("Path is a directory, not a file: {}", path));
                    }
                } else {
                    errors.push(format!("Missing '{}' parameter", key));
                }
            }
        }
        "clipboard_write" if args.get("content").and_then(|v| v.as_str()).is_none() => {
            errors.push("Missing 'content' parameter".to_string());
        }
        "set_theme" => match args.get("theme").and_then(|v| v.as_str()) {
            Some(t) if ["light", "dark", "tokyo", "cyber"].contains(&t) => {}
            Some(t) => errors.push(format!(
                "Invalid theme '{}'. Use: light, dark, tokyo, cyber",
                t
            )),
            None => errors.push("Missing required parameter 'theme'".to_string()),
        },
        "sync_control" => match args.get("action").and_then(|v| v.as_str()) {
            Some(a) if ["start", "stop", "status"].contains(&a) => {}
            Some(a) => errors.push(format!("Invalid action '{}'. Use: start, stop, status", a)),
            None => errors.push("Missing required parameter 'action'".to_string()),
        },
        "vault_peek" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                if let Err(e) = validate_path(path, "path") {
                    errors.push(e);
                }
                if !path.ends_with(".aerovault") {
                    warnings.push("File does not have .aerovault extension".to_string());
                }
            } else {
                errors.push("Missing required parameter 'path'".to_string());
            }
        }
        // app_info needs no validation (no parameters)
        _ => {} // Remote tools: path format already validated above
    }

    Ok(json!({
        "valid": errors.is_empty(),
        "errors": errors,
        "warnings": warnings,
    }))
}

// `resolve_local_path` migrated to `ai_core::local_tools` in T3 Gate 3.

/// Maximum output size from shell commands (512 KB)
const SHELL_MAX_OUTPUT_BYTES: usize = 512 * 1024;

/// Denied command patterns: defense-in-depth (also checked on frontend)
static DENIED_COMMAND_PATTERNS: std::sync::LazyLock<Vec<regex::Regex>> =
    std::sync::LazyLock::new(|| {
        [
            r"^\s*rm\s+(-[a-zA-Z]*)?.*\s+/\s*$", // rm -rf /
            r"^\s*rm\s+(-[a-zA-Z]*f[a-zA-Z]*\s+)?-[a-zA-Z]*r.*\s+/\s*$",
            r"^\s*mkfs\b",                             // mkfs (format disk)
            r"^\s*dd\s+.*of=/dev/",                    // dd to device
            r"^\s*shutdown\b",                         // shutdown
            r"^\s*reboot\b",                           // reboot
            r"^\s*halt\b",                             // halt
            r"^\s*init\s+[06]\b",                      // init 0/6
            r"^\s*:\(\)\s*\{\s*:\|:\s*&\s*\}\s*;\s*:", // fork bomb
            r"^\s*>\s*/dev/sd[a-z]",                   // overwrite disk
            r"^\s*chmod\s+(-[a-zA-Z]*\s+)?777\s+/",    // chmod 777 /
            r"^\s*chown\s+.*\s+/\s*$",                 // chown /
            // L19: belt+suspenders: also caught by meta-char filter, but explicit is better
            r"^\s*python3?\s+-c\b",          // python -c (arbitrary code exec)
            r"\bcurl\b.*\|",                 // curl piped to shell
            r"\bwget\b.*\|",                 // wget piped to shell
            r"^\s*eval\s",                   // eval (arbitrary execution)
            r"^\s*base64\s+(-d|--decode)\b", // base64 decode (obfuscation bypass)
            r"^\s*truncate\b",               // truncate (destroy file contents)
            r"^\s*shred\b",                  // shred (secure delete)
            // A1-07: Additional patterns: system administration and persistence
            r"^\s*crontab\b",    // crontab (schedule persistent commands)
            r"^\s*nohup\b",      // nohup (persist after logout)
            r"^\s*systemctl\b",  // systemctl (service management)
            r"^\s*service\b",    // service (init.d management)
            r"^\s*mount\b",      // mount (filesystem mount)
            r"^\s*umount\b",     // umount (filesystem unmount)
            r"^\s*fdisk\b",      // fdisk (partition table)
            r"^\s*parted\b",     // parted (partition editor)
            r"^\s*iptables\b",   // iptables (firewall rules)
            r"^\s*useradd\b",    // useradd (create users)
            r"^\s*userdel\b",    // userdel (delete users)
            r"^\s*passwd\b",     // passwd (change passwords)
            r"^\s*sudo\b",       // sudo (elevated privileges)
            r"^\s*pkill\s+-9\b", // pkill -9 (force kill)
            r"^\s*killall\b",    // killall (kill by name)
        ]
        .iter()
        .filter_map(|p| regex::Regex::new(p).ok())
        .collect()
    });

/// Read image from system clipboard via arboard (native, works on WebKitGTK).
/// Returns base64 PNG or null if no image in clipboard.
#[tauri::command]
pub fn clipboard_read_image() -> Result<Option<String>, String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| format!("Clipboard init failed: {}", e))?;
    let img = match clipboard.get_image() {
        Ok(img) => img,
        Err(_) => return Ok(None), // No image in clipboard
    };

    // Encode RGBA as BMP-like format, then convert via canvas on frontend.
    // Simpler: encode as raw RGBA + dimensions as JSON, let frontend render via canvas.
    use base64::Engine;
    let rgba_base64 = base64::engine::general_purpose::STANDARD.encode(&img.bytes);
    Ok(Some(format!(
        "{}:{}:{}",
        img.width, img.height, rgba_base64
    )))
}

/// Execute a shell command and capture output.
/// Used by AeroAgent's shell_execute tool.
#[tauri::command]
pub async fn shell_execute(
    command: String,
    working_dir: Option<String>,
    timeout_secs: Option<u64>,
) -> Result<Value, String> {
    if command.trim().is_empty() {
        return Err("No command specified".to_string());
    }

    // Defense-in-depth: reject shell meta-characters that enable denylist bypass
    // (pipes, subshells, backticks, semicolons, eval chains, base64 decode, etc.)
    let meta_chars = ['|', ';', '`', '$', '&', '(', ')', '{', '}', '\n', '\r'];
    if meta_chars.iter().any(|c| command.contains(*c)) {
        return Err(
            "Command contains shell meta-characters (|;&`$(){}\\n\\r). Use simple commands only."
                .to_string(),
        );
    }

    // Security: check denied commands
    if DENIED_COMMAND_PATTERNS
        .iter()
        .any(|rx| rx.is_match(&command))
    {
        return Err("Command blocked: potentially destructive system command".to_string());
    }

    // Determine working directory
    let cwd = working_dir.filter(|d| !d.is_empty()).unwrap_or_else(|| {
        dirs::home_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "/tmp".to_string())
    });

    // Validate working directory exists
    let cwd_path = std::path::Path::new(&cwd);
    if !cwd_path.exists() || !cwd_path.is_dir() {
        return Err(format!("Working directory does not exist: {}", cwd));
    }

    let timeout = std::time::Duration::from_secs(timeout_secs.unwrap_or(30).min(120));

    // Build command with environment isolation
    let mut cmd = TokioCommand::new("sh");
    cmd.args(["-c", &command])
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env_clear();

    // Restore minimal safe environment
    for key in &[
        "PATH", "HOME", "LANG", "LC_ALL", "TERM", "TMPDIR", "USER", "SHELL",
    ] {
        if let Ok(val) = std::env::var(key) {
            cmd.env(key, val);
        }
    }

    let child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn shell: {}", e))?;

    // Wait with timeout
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return Err(format!("Shell process error: {}", e)),
        Err(_) => {
            return Ok(json!({
                "stdout": "",
                "stderr": format!("Command timed out after {}s", timeout.as_secs()),
                "exit_code": -1,
                "success": false,
                "timed_out": true,
                "command": command
            }));
        }
    };

    let exit_code = output.status.code().unwrap_or(-1);

    // Truncate output if too large
    let stdout_raw = &output.stdout[..output.stdout.len().min(SHELL_MAX_OUTPUT_BYTES)];
    let stderr_raw = &output.stderr[..output.stderr.len().min(SHELL_MAX_OUTPUT_BYTES)];

    let stdout = String::from_utf8_lossy(stdout_raw);
    let stderr = String::from_utf8_lossy(stderr_raw);

    let stdout_truncated = output.stdout.len() > SHELL_MAX_OUTPUT_BYTES;
    let stderr_truncated = output.stderr.len() > SHELL_MAX_OUTPUT_BYTES;

    let mut stdout_str = stdout.to_string();
    if stdout_truncated {
        stdout_str.push_str(&format!(
            "\n[...truncated, {} bytes total]",
            output.stdout.len()
        ));
    }
    let mut stderr_str = stderr.to_string();
    if stderr_truncated {
        stderr_str.push_str(&format!(
            "\n[...truncated, {} bytes total]",
            output.stderr.len()
        ));
    }

    Ok(json!({
        "stdout": stdout_str,
        "stderr": stderr_str,
        "exit_code": exit_code,
        "success": output.status.success(),
        "timed_out": false,
        "command": command
    }))
}

// `format_bytes_human` migrated to `ai_core::local_tools::human_bytes`.

// ── Server Exec helpers ──────────────────────────────────────────────

/// Saved server info (credentials excluded: never exposed to AI)
#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
pub(crate) struct SavedServerInfo {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) username: String,
    pub(crate) protocol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) initial_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) provider_id: Option<String>,
}

/// Load saved server profiles from vault. Returns list WITHOUT credentials.
pub(crate) fn load_saved_servers() -> Result<Vec<SavedServerInfo>, String> {
    let store = crate::credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "Credential vault not open. Unlock the vault first.".to_string())?;

    let json = store
        .get("config_server_profiles")
        .map_err(|_| "No saved servers found in vault.".to_string())?;

    let profiles: Vec<serde_json::Value> = serde_json::from_str(&json)
        .map_err(|e| format!("Failed to parse server profiles: {}", e))?;

    let servers: Vec<SavedServerInfo> = profiles
        .iter()
        .filter_map(|p| {
            Some(SavedServerInfo {
                id: p.get("id")?.as_str()?.to_string(),
                name: p.get("name")?.as_str()?.to_string(),
                host: p
                    .get("host")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                port: p.get("port").and_then(|v| v.as_u64()).unwrap_or(0) as u16,
                username: p
                    .get("username")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                protocol: p
                    .get("protocol")
                    .and_then(|v| v.as_str())
                    .unwrap_or("ftp")
                    .to_string(),
                initial_path: p
                    .get("initialPath")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                provider_id: p
                    .get("providerId")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            })
        })
        .collect();

    Ok(servers)
}

/// Find a saved server by name (case-insensitive, fuzzy) or exact ID.
pub(crate) fn find_server_by_name_or_id(
    servers: &[SavedServerInfo],
    query: &str,
) -> Result<SavedServerInfo, String> {
    // 1. Exact ID match
    if let Some(s) = servers.iter().find(|s| s.id == query) {
        return Ok(s.clone());
    }

    // 2. Exact name match (case-insensitive)
    let query_lower = query.to_lowercase();
    if let Some(s) = servers
        .iter()
        .find(|s| s.name.to_lowercase() == query_lower)
    {
        return Ok(s.clone());
    }

    // 3. Fuzzy name match (contains, case-insensitive)
    let matches: Vec<&SavedServerInfo> = servers
        .iter()
        .filter(|s| s.name.to_lowercase().contains(&query_lower))
        .collect();

    match matches.len() {
        0 => Err(format!(
            "Server '{}' not found. Available: {}",
            query,
            servers
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
        1 => Ok(matches[0].clone()),
        _ => Err(format!(
            "Ambiguous server name '{}'. Matches: {}. Use exact name or ID.",
            query,
            matches
                .iter()
                .map(|s| format!("'{}'", s.name))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Load provider-specific extra options from the full server profile in vault.
fn load_provider_extra_options(
    server_id: &str,
) -> Result<std::collections::HashMap<String, String>, String> {
    let store = crate::credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "Vault not open".to_string())?;

    let json = store.get("config_server_profiles").unwrap_or_default();
    let profiles: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap_or_default();

    let mut extra = std::collections::HashMap::new();

    if let Some(profile) = profiles
        .iter()
        .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(server_id))
    {
        if let Some(options) = profile.get("options").and_then(|v| v.as_object()) {
            for (k, v) in options {
                let key = match k.as_str() {
                    "tlsMode" => "tls_mode",
                    "verifyCert" => "verify_cert",
                    "pathStyle" => "path_style",
                    "private_key_path" => "private_key_path",
                    "key_passphrase" => "key_passphrase",
                    "accountName" => "account_name",
                    "accessKey" => "access_key",
                    "sasToken" => "sas_token",
                    "pcloudRegion" | "region" => "region",
                    "two_factor_code" => "two_factor_code",
                    "drive_id" => "drive_id",
                    "trust_unknown_hosts" => "trust_unknown_hosts",
                    other => other,
                };
                if let Some(s) = v.as_str() {
                    extra.insert(key.to_string(), s.to_string());
                } else if let Some(b) = v.as_bool() {
                    extra.insert(key.to_string(), b.to_string());
                } else if let Some(n) = v.as_u64() {
                    extra.insert(key.to_string(), n.to_string());
                }
            }
        }
    }

    Ok(extra)
}

/// Create a temporary StorageProvider from a saved server profile.
/// Credentials resolved internally from vault: never exposed to caller.
pub(crate) async fn create_temp_provider(
    server: &SavedServerInfo,
) -> Result<Box<dyn crate::providers::StorageProvider>, String> {
    let store = crate::credential_store::CredentialStore::from_cache()
        .ok_or_else(|| "Credential vault not open".to_string())?;

    // Server profiles are stored as an array in "config_server_profiles" vault key
    // (frontend uses VAULT_PREFIX="config_" + "server_profiles")
    let profiles_json = store.get("config_server_profiles").map_err(|_| {
        "No server profiles found in vault. Re-save the server with password.".to_string()
    })?;

    let profiles: Vec<serde_json::Value> = serde_json::from_str(&profiles_json)
        .map_err(|e| format!("Failed to parse server profiles: {}", e))?;

    // Find the matching server by ID and extract password
    let profile = profiles
        .iter()
        .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(&server.id))
        .ok_or_else(|| {
            format!(
                "Server '{}' not found in vault profiles. Re-save with password.",
                server.name
            )
        })?;

    #[derive(serde::Deserialize)]
    struct SavedCreds {
        #[serde(default)]
        server: String,
        #[serde(default)]
        username: String,
        #[serde(default)]
        password: String,
    }

    // Password is stored separately in credential store with key "server_{id}"
    // (migrated from inline password in profile: see ServerProfile.password DEPRECATED)
    let password = store
        .get(&format!("server_{}", server.id))
        .unwrap_or_else(|_| {
            // Fallback: check inline password in profile (legacy/pre-migration)
            profile
                .get("password")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        });

    let creds = SavedCreds {
        server: profile
            .get("server")
            .or(profile.get("host"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        username: profile
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        password,
    };

    use crate::providers::ProviderType;
    let provider_type = match server.protocol.as_str() {
        "ftp" => ProviderType::Ftp,
        "ftps" => ProviderType::Ftps,
        "sftp" => ProviderType::Sftp,
        "webdav" => ProviderType::WebDav,
        "s3" => ProviderType::S3,
        "mega" => ProviderType::Mega,
        "azure" => ProviderType::Azure,
        "filen" => ProviderType::Filen,
        "internxt" => ProviderType::Internxt,
        "kdrive" => ProviderType::KDrive,
        "jottacloud" => ProviderType::Jottacloud,
        "filelu" => ProviderType::FileLu,
        "koofr" => ProviderType::Koofr,
        "opendrive" => ProviderType::OpenDrive,
        "yandexdisk" => ProviderType::YandexDisk,
        "googledrive" | "dropbox" | "onedrive" | "box" | "pcloud" | "zohoworkdrive"
        | "fourshared" => {
            return Err(format!(
                "OAuth provider '{}' requires browser authentication. Use the connected session's remote_* tools instead.",
                server.protocol
            ));
        }
        other => return Err(format!("Unsupported protocol: {}", other)),
    };

    let (parsed_host, embedded_port) =
        crate::cloud_provider_factory::parse_server_field(&creds.server);

    let extra = load_provider_extra_options(&server.id)?;

    // SFTP: rely on the saved server's trust configuration: do not override
    // (auto-trusting host keys would bypass TOFU verification and enable MITM)

    let host = if parsed_host.is_empty() {
        server.host.clone()
    } else {
        parsed_host
    };
    let port = embedded_port.or(if server.port > 0 {
        Some(server.port)
    } else {
        None
    });

    let mut provider_config = crate::providers::ProviderConfig {
        name: server.name.clone(),
        provider_type,
        host,
        port,
        username: Some(creds.username),
        password: Some(creds.password),
        initial_path: server.initial_path.clone(),
        extra,
    };

    let mut provider = crate::providers::ProviderFactory::create(&provider_config)
        .map_err(|e| format!("Failed to create provider: {}", e))?;

    let connect_result = provider.connect().await;
    provider_config.zeroize_password();
    connect_result
        .map(|()| provider)
        .map_err(|e| format!("Connection to '{}' failed: {}", server.name, e))
}

#[tauri::command]
pub async fn prepare_ai_tool_approval(
    state: State<'_, ProviderState>,
    app_state: State<'_, AppState>,
    tool_name: String,
    args: Value,
    context_local_path: Option<String>,
    session_id: Option<String>,
) -> Result<AiToolApprovalPreparation, String> {
    if !ALLOWED_TOOLS.contains(&tool_name.as_str()) {
        return Err(format!("Unknown or disallowed tool: {}", tool_name));
    }

    if !requires_backend_write_approval(&tool_name, &args) {
        return Ok(AiToolApprovalPreparation {
            approval_required: false,
            request_id: None,
            allow_session_grant: false,
        });
    }

    let remote_context = build_remote_cache_context(&state, &app_state).await;
    let scope_key = build_tool_cache_key(
        &tool_name,
        &args,
        context_local_path.as_deref(),
        remote_context.as_deref(),
    )?;
    let allow_session_grant = allows_session_grant(&tool_name, &args);
    Ok(prepare_backend_approval_request(
        session_id.as_deref(),
        &tool_name,
        scope_key,
        allow_session_grant,
        build_ai_tool_approval_message(&tool_name, &args),
    )
    .await)
}

#[tauri::command]
pub async fn grant_ai_tool_approval(
    app: tauri::AppHandle,
    request_id: String,
    remember_for_session: bool,
    skip_native_dialog: bool,
) -> Result<AiToolApprovalGrantResponse, String> {
    let request = {
        let mut requests = AI_TOOL_APPROVAL_REQUESTS.lock().await;
        prune_ai_tool_approval_requests(&mut requests);
        requests
            .remove(&request_id)
            .ok_or_else(|| "The AI approval request expired. Please retry the tool.".to_string())?
    };

    if remember_for_session && !request.allow_session_grant {
        return Err("Session approval is not allowed for this AI tool.".to_string());
    }

    // When the frontend already showed an approval panel (expert mode),
    // skip the native OS dialog to avoid double-confirmation.
    // In safe/normal mode, always show the OS dialog as a second factor.
    if !skip_native_dialog {
        let label = human_tool_label(&request.tool_name);
        let dialog_title = if remember_for_session {
            format!("AeroAgent - {} (session)", label)
        } else {
            format!("AeroAgent - {}", label)
        };
        let dialog_message = format!(
            "{}\n\n{}",
            request.message,
            approval_scope_message(remember_for_session)
        );

        let approved = tokio::task::spawn_blocking(move || {
            app.dialog()
                .message(dialog_message)
                .title(dialog_title)
                .kind(MessageDialogKind::Warning)
                .buttons(MessageDialogButtons::OkCancel)
                .blocking_show()
        })
        .await
        .map_err(|error| format!("Failed to show backend approval dialog: {}", error))?;

        if !approved {
            return Ok(AiToolApprovalGrantResponse {
                approved: false,
                grant_id: None,
            });
        }
    }

    let grant_id = Uuid::new_v4().to_string();
    let ttl_ms = if remember_for_session {
        AI_SESSION_GRANT_TTL_MS
    } else {
        AI_ONE_SHOT_GRANT_TTL_MS
    };

    let mut grants = AI_TOOL_APPROVAL_GRANTS.lock().await;
    prune_ai_tool_approval_grants(&mut grants);
    grants.insert(
        grant_id.clone(),
        AiToolApprovalGrant {
            session_key: request.session_key,
            tool_name: request.tool_name,
            scope_key: request.scope_key,
            created_at_ms: current_time_ms(),
            expires_at_ms: current_time_ms().saturating_add(ttl_ms),
            remember_for_session,
        },
    );

    Ok(AiToolApprovalGrantResponse {
        approved: true,
        grant_id: Some(grant_id),
    })
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn execute_ai_tool(
    app: tauri::AppHandle,
    // Tauri State<'_, ...> parameters are still required by the IPC
    // signature so the frontend can keep the same invoke contract; the
    // unified dispatcher (ai_core::tools) does not use them directly.
    _state: tauri::State<'_, crate::provider_commands::ProviderState>,
    _app_state: tauri::State<'_, crate::AppState>,
    tool_name: String,
    args: serde_json::Value,
    context_local_path: Option<String>,
    _session_id: Option<String>,
    approval_grant_id: Option<String>,
) -> Result<serde_json::Value, String> {
    let ctx = crate::ai_core::tauri_impl::TauriToolCtx {
        app: app.clone(),
        sink: crate::ai_core::tauri_impl::TauriEventSink::new(app.clone()),
        creds: crate::ai_core::tauri_impl::VaultCredentialProvider,
        context_local_path,
        approval_grant_id,
    };
    crate::ai_core::tools::dispatch_tool(&ctx, &tool_name, &args)
        .await
        .map_err(|e| e.to_string())
}
