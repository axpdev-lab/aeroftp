//! MCP tool definitions and dispatch
//!
//! Curated tool catalog that provides unique value — remote file and sync
//! operations that MCP clients don't have natively — plus pool introspection
//! tools (`aeroftp_close_connection`) and composite tree operations
//! (`aeroftp_sync_tree`, `aeroftp_check_tree`).
//!
//! Excludes local tools (local_list, shell_execute, etc.) that clients already have.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use crate::mcp::notifier::{format_transfer_message, McpNotifier};
use crate::mcp::pool::ConnectionPool;
use crate::mcp::security::{self, RateCategory};
use crate::providers::{ProviderError, ShareLinkOptions, StorageProvider};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

/// A single progress sample sent to the notifier consumer task.
/// Using a concrete type rather than a boxed future lets the consumer
/// coalesce and throttle without allocating per event.
type ProgressSample = (u64, Option<u64>, String);

/// Drain progress samples from a bounded channel and forward them to the
/// notifier. Runs until every sender drops — at which point the channel
/// closes and the task exits naturally.
fn spawn_progress_consumer(notifier: McpNotifier, mut rx: mpsc::Receiver<ProgressSample>) {
    tokio::spawn(async move {
        while let Some((sent, total_opt, msg)) = rx.recv().await {
            notifier.send_progress(sent, total_opt, Some(msg)).await;
        }
    });
}

/// Drop-semantics helper for a `tokio::sync::mpsc::Sender` used as a
/// progress sink. `try_send` is intentional: if the consumer is slow,
/// samples are dropped (the notifier has its own throttle, so coalescing
/// is safe). Never spawn one task per sample.
#[inline]
fn push_progress(tx: &mpsc::Sender<ProgressSample>, sample: ProgressSample) {
    let _ = tx.try_send(sample);
}

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

/// Classifier for transport-level errors that leave the pooled connection in
/// an unusable state.
///
/// Motivation: `suppaftp` surfaces "Data connection is already open" after any
/// failed PASV/STOR pair — every subsequent call on the same socket returns
/// the same error until the control channel is torn down and re-opened. The
/// pool cannot distinguish a bad connection from a bad argument by return
/// type alone, so we peek at both the `ProviderError` variant and the free-
/// form message.
///
/// Returns `true` when the pool entry MUST be invalidated. Benign errors
/// (`NotFound`, `AlreadyExists`, `InvalidPath`, `PermissionDenied`, auth) are
/// kept — those do not corrupt the session.
fn is_transport_error(err: &ProviderError) -> bool {
    match err {
        // Variant-based classification: these always imply a broken session.
        ProviderError::NotConnected
        | ProviderError::ConnectionFailed(_)
        | ProviderError::Timeout
        | ProviderError::NetworkError(_)
        | ProviderError::IoError(_) => true,
        // String-based variants may wrap a transport failure or a server
        // status code. Inspect the message for known patterns.
        ProviderError::TransferFailed(msg)
        | ProviderError::ServerError(msg)
        | ProviderError::Other(msg)
        | ProviderError::Unknown(msg) => message_implies_broken_session(msg),
        _ => false,
    }
}

/// Case-insensitive pattern match against signatures of a broken session.
/// Keeps the list narrow on purpose — false positives evict healthy pooled
/// connections; false negatives merely fall back to surfacing the original
/// error to the caller.
fn message_implies_broken_session(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    const PATTERNS: &[&str] = &[
        "data connection is already open",
        "data connection already open",
        "connection is already open",
        "broken pipe",
        "pipe closed",
        "connection reset",
        "connection closed",
        "connection aborted",
        "connection refused",
        "not connected",
        "eof from server",
        "unexpected eof",
        "channel closed",
        "session closed",
        "socket closed",
        "stream closed",
        "bad file descriptor",
    ];
    PATTERNS.iter().any(|p| lower.contains(p))
}

/// Run `op` on a pooled provider, retrying once with a fresh connection when
/// the first attempt fails with a transport-level error.
///
/// The closure takes the locked provider guard and returns `Result<T,
/// ProviderError>`. On transport failure, the pool entry is invalidated (fast
/// path, no awaited disconnect) and `op` is retried exactly once on a freshly
/// opened connection. Benign errors (e.g. `NotFound`) bubble up unchanged.
///
/// Centralizing this logic means the dispatch site stays readable and every
/// tool inherits the same invalidation policy automatically.
async fn execute_with_reset<T, F>(
    pool: &ConnectionPool,
    server: &str,
    mut op: F,
) -> Result<T, String>
where
    F: for<'p> FnMut(
        &'p mut Box<dyn StorageProvider>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T, ProviderError>> + Send + 'p>,
    >,
{
    let arc = pool.get_provider(server).await?;
    let first = {
        let mut guard = arc.lock().await;
        op(&mut guard).await
    };
    match first {
        Ok(v) => Ok(v),
        Err(e) if is_transport_error(&e) => {
            // Drop the Arc held by the pool so a fresh get_provider() creates
            // a new connection. We must release our `arc` handle too so the
            // pool sees strong_count == 1 and actually closes the old one.
            drop(arc);
            let _ = pool.invalidate(server).await;
            let retry_arc = pool.get_provider(server).await?;
            let mut guard = retry_arc.lock().await;
            op(&mut guard).await.map_err(sanitize_error)
        }
        Err(e) => Err(sanitize_error(e)),
    }
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
            description: "List saved server profiles from the encrypted vault. Supports optional filtering (name_contains, protocol) and pagination (limit, offset). Passwords are never exposed.",
            input_schema: json!({ "type": "object", "properties": {
                "name_contains": { "type": "string", "description": "Case-insensitive substring filter on profile name" },
                "protocol": { "type": "string", "description": "Filter by protocol (e.g. ftp, sftp, webdav, s3). Case-insensitive." },
                "limit": { "type": "integer", "description": "Maximum entries to return (default: 200, cap: 1000)" },
                "offset": { "type": "integer", "description": "Entries to skip before applying limit (default: 0)" }
            }, "required": [] }),
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
            description: "Read a remote text file (default 5 KB preview, configurable up to 1024 KB via preview_kb). For binary files, use aeroftp_download_file instead.",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID" },
                "path": { "type": "string", "description": "Remote file path" },
                "preview_kb": { "type": "integer", "description": "Max preview size in KB (default: 5, cap: 1024)" }
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
            description: "Upload a local file or inline text content to a remote server. Set create_parents=true to auto-mkdir missing parent directories (idempotent).",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID" },
                "remote_path": { "type": "string", "description": "Destination path on the server" },
                "local_path": { "type": "string", "description": "Local file path to upload (mutually exclusive with content)" },
                "content": { "type": "string", "description": "Inline text content to upload (mutually exclusive with local_path)" },
                "create_parents": { "type": "boolean", "description": "Recursively mkdir the parent directories if missing (default: false)" }
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
            name: "aeroftp_delete_many",
            description: "Delete multiple remote entries in a single batch. Applies a server-friendly backoff between operations to avoid protocol rate limits, and returns a per-item result ({path, deleted, is_dir?, error?}). Set recursive=true to remove non-empty directories.",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID" },
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Remote paths to delete (max 100)"
                },
                "recursive": { "type": "boolean", "description": "Delete directories recursively (default: false)" },
                "continue_on_error": { "type": "boolean", "description": "Keep going after a single-item failure (default: true)" },
                "delay_ms": { "type": "integer", "description": "Pause between deletes in milliseconds (default: 200, cap: 2000)" }
            }, "required": ["server", "paths"] }),
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
        McpToolDef {
            name: "aeroftp_close_connection",
            description: "Close a pooled server connection explicitly. Useful when the agent wants to free resources or force a fresh authentication handshake on the next call.",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID to disconnect" }
            }, "required": ["server"] }),
            category: RateCategory::Mutative,
        },
        McpToolDef {
            name: "aeroftp_check_tree",
            description: "Compare a local directory against a remote directory and report differences grouped as match, differ, missing_local, missing_remote. Each entry carries compare_method ('checksum' or 'size') so agents can tell how the decision was made. When checksum=true, SHA-256 is computed on local files AND on the remote when the provider supports server-side checksums; otherwise comparison falls back to size. Supports glob excludes and one-way mode.",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID" },
                "local_dir": { "type": "string", "description": "Local directory to compare" },
                "remote_dir": { "type": "string", "description": "Remote directory to compare" },
                "one_way": { "type": "boolean", "description": "Skip remote-only entries (default: false)" },
                "checksum": { "type": "boolean", "description": "Compute SHA-256 locally and request server-side checksum when supported (default: false). Falls back to size-only comparison when unsupported." },
                "exclude": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Glob patterns to exclude (e.g. '*.tmp', 'node_modules/**')"
                },
                "max_depth": { "type": "integer", "description": "Max recursion depth (default: 100)" },
                "max_entries_reported": { "type": "integer", "description": "Cap per-group entries returned (default: 200)" }
            }, "required": ["server", "local_dir", "remote_dir"] }),
            category: RateCategory::ReadOnly,
        },
        McpToolDef {
            name: "aeroftp_sync_tree",
            description: "Synchronize a local directory with a remote directory. Direction: upload, download, or both. Supports dry_run, delete_orphans (upload/download only), conflict resolution (larger/newer/skip), and glob excludes. Emits progress notifications when the caller supplies a progressToken.",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID" },
                "local_dir": { "type": "string", "description": "Local root directory" },
                "remote_dir": { "type": "string", "description": "Remote root directory" },
                "direction": {
                    "type": "string",
                    "enum": ["upload", "download", "both"],
                    "description": "Direction of the sync operation"
                },
                "dry_run": { "type": "boolean", "description": "Plan only, no writes (default: false)" },
                "delete_orphans": { "type": "boolean", "description": "Delete files on the destination that no longer exist on the source. Requires direction=upload or direction=download." },
                "conflict_mode": {
                    "type": "string",
                    "enum": ["larger", "newer", "skip"],
                    "description": "How to resolve same-path size conflicts (default: larger)"
                },
                "exclude": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Glob patterns to exclude"
                },
                "max_depth": { "type": "integer", "description": "Max recursion depth (default: 100)" }
            }, "required": ["server", "local_dir", "remote_dir", "direction"] }),
            category: RateCategory::Mutative,
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

/// Gate `aeroftp_read_file` against directories and oversized files.
///
/// Only rejects files that exceed the hard in-memory cap
/// (`MAX_READ_PREVIEW_BYTES`). Files larger than the caller's `preview_bytes`
/// window but within the hard cap are accepted here and truncated downstream
/// with `truncated:true` so agents can get a tail-free preview without having
/// to retry.
fn validate_read_preview_target(is_dir: bool, size: u64, _preview_bytes: u64) -> Result<(), String> {
    if is_dir {
        return Err("Cannot read a directory. Use aeroftp_list_files instead.".into());
    }
    if size > MAX_READ_PREVIEW_BYTES {
        return Err(format!(
            "File too large for preview ({:.1} KB). Hard cap: {} KB. Use aeroftp_download_file for larger files.",
            size as f64 / 1024.0,
            MAX_READ_PREVIEW_BYTES / 1024,
        ));
    }
    Ok(())
}

/// Return the parent directory of `remote_path` using POSIX-style slashes,
/// or `None` when the path has no meaningful parent (root / bare filename).
fn parent_remote_dir(remote_path: &str) -> Option<String> {
    let trimmed = remote_path.trim_end_matches('/');
    let idx = trimmed.rfind('/')?;
    if idx == 0 {
        // Path like "/foo" — parent is the root. No mkdir needed.
        return None;
    }
    Some(trimmed[..idx].to_string())
}

/// Recursively create every parent directory under `dir` on the remote.
///
/// Idempotent: `ProviderError::AlreadyExists` is absorbed. Other errors that
/// plausibly mean "the directory is already there" (server returns 550/EEXIST
/// via `Other`/`ServerError`) are tolerated too — the caller's subsequent
/// upload will surface any genuine problem with a much better error message.
///
/// On transport failure, the pool entry is invalidated and the recursion
/// retried on a fresh connection exactly once to stay consistent with
/// `execute_with_reset` behavior.
async fn ensure_remote_parents(
    pool: &ConnectionPool,
    server: &str,
    dir: &str,
) -> Result<(), String> {
    let dir = dir.trim_end_matches('/');
    if dir.is_empty() {
        return Ok(());
    }
    let mut components: Vec<&str> = Vec::new();
    for part in dir.split('/') {
        if !part.is_empty() {
            components.push(part);
        }
    }
    let leading_slash = dir.starts_with('/');
    let mut accumulated = String::new();
    for part in components {
        if leading_slash || !accumulated.is_empty() {
            accumulated.push('/');
        }
        accumulated.push_str(part);
        let path_for_call = accumulated.clone();
        let result = execute_with_reset(pool, server, move |p| {
            let path = path_for_call.clone();
            Box::pin(async move { p.mkdir(&path).await })
        })
        .await;
        match result {
            Ok(()) => {}
            Err(e) => {
                // Best-effort idempotency: swallow the "already exists" family.
                let low = e.to_ascii_lowercase();
                if low.contains("already exists")
                    || low.contains("file exists")
                    || low.contains("eexist")
                    || low.contains("550")
                {
                    continue;
                }
                return Err(format!("mkdir {} failed: {}", accumulated, e));
            }
        }
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

/// Build a transfer progress callback that forwards bytes sent / total to the
/// MCP notifier. The callback is sync (invoked from inside provider I/O) while
/// the notifier is async, so samples are funneled through a bounded mpsc to
/// a single drain task. Previously this site spawned one task per call — a
/// 1 GB upload at 4 KB chunks would leak ~262 000 no-op tasks before the
/// throttle had a chance to filter them out.
fn build_progress_callback(
    notifier: Option<&McpNotifier>,
    label: &'static str,
) -> Option<Box<dyn Fn(u64, u64) + Send>> {
    let notifier = notifier?.clone();
    if !notifier.is_active() {
        return None;
    }
    // Bounded channel coalesces bursts under backpressure. When the callback
    // drops, the sender drops → rx.recv() returns None → consumer task exits.
    let (tx, rx) = mpsc::channel::<ProgressSample>(32);
    spawn_progress_consumer(notifier, rx);

    let start = Arc::new(Instant::now());
    let last_sent = Arc::new(AtomicU64::new(0));
    let last_elapsed_ms = Arc::new(AtomicU64::new(0));
    Some(Box::new(move |sent: u64, total: u64| {
        let elapsed_ms = start.elapsed().as_millis() as u64;
        let delta_ms = elapsed_ms.saturating_sub(last_elapsed_ms.load(Ordering::Relaxed));
        let delta_bytes = sent.saturating_sub(last_sent.load(Ordering::Relaxed));
        let bps = if delta_ms > 0 {
            delta_bytes.saturating_mul(1000) / delta_ms.max(1)
        } else {
            0
        };
        last_sent.store(sent, Ordering::Relaxed);
        last_elapsed_ms.store(elapsed_ms, Ordering::Relaxed);
        let pct = if total > 0 {
            ((sent as f64 / total as f64) * 100.0).min(100.0) as u64
        } else {
            0
        };
        let message = format!(
            "{}: {}",
            label,
            format_transfer_message(pct, sent, total, bps)
        );
        let total_opt = if total > 0 { Some(total) } else { None };
        push_progress(&tx, (sent, total_opt, message));
    }))
}

/// Execute a tool call. Returns `(result_json, is_error)`.
pub async fn execute_tool(
    tool_name: &str,
    args: &Value,
    pool: &ConnectionPool,
    rate_limiter: &security::RateLimiter,
    notifier: Option<&McpNotifier>,
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
            let name_contains = get_str_opt(args, "name_contains").map(|s| s.to_lowercase());
            let protocol = get_str_opt(args, "protocol").map(|s| s.to_lowercase());
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .map(|n| n.min(1_000) as usize)
                .unwrap_or(200);
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

            let result = match crate::mcp::load_safe_profiles() {
                Ok(profiles) => {
                    let filtered: Vec<Value> = profiles
                        .into_iter()
                        .filter(|p| match (&name_contains, &protocol) {
                            (None, None) => true,
                            (Some(needle), None) => p
                                .get("name")
                                .and_then(|v| v.as_str())
                                .map(|n| n.to_lowercase().contains(needle))
                                .unwrap_or(false),
                            (None, Some(proto)) => p
                                .get("protocol")
                                .and_then(|v| v.as_str())
                                .map(|pr| pr.to_lowercase() == *proto)
                                .unwrap_or(false),
                            (Some(needle), Some(proto)) => {
                                let name_match = p
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .map(|n| n.to_lowercase().contains(needle))
                                    .unwrap_or(false);
                                let proto_match = p
                                    .get("protocol")
                                    .and_then(|v| v.as_str())
                                    .map(|pr| pr.to_lowercase() == *proto)
                                    .unwrap_or(false);
                                name_match && proto_match
                            }
                        })
                        .collect();
                    let matched_total = filtered.len();
                    let page: Vec<Value> = filtered.into_iter().skip(offset).take(limit).collect();
                    let returned = page.len();
                    ok(json!({
                        "servers": page,
                        "count": returned,
                        "total_matched": matched_total,
                        "offset": offset,
                        "limit": limit,
                        "truncated": offset + returned < matched_total,
                    }))
                }
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

            let path_for_call = path.clone();
            let result = match execute_with_reset(pool, &server, move |p| {
                let path = path_for_call.clone();
                Box::pin(async move { p.list(&path).await })
            })
            .await
            {
                Err(e) => err(e),
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

            // Caller-controlled preview size, clamped to [1 KB, 1024 KB]. The
            // MAX_READ_PREVIEW_BYTES constant caps the hard ceiling of what
            // the tool will even consider downloading.
            let requested_kb = args
                .get("preview_kb")
                .and_then(|v| v.as_u64())
                .unwrap_or(5)
                .max(1);
            let preview_bytes = (requested_kb.saturating_mul(1024)).min(MAX_READ_PREVIEW_BYTES);
            let path_for_stat = path.clone();
            let stat_result = execute_with_reset(pool, &server, move |p| {
                let path = path_for_stat.clone();
                Box::pin(async move { p.stat(&path).await })
            })
            .await;
            let result = match stat_result {
                Err(e) => err(e),
                Ok(entry) => {
                    match validate_read_preview_target(entry.is_dir, entry.size, preview_bytes) {
                        Err(msg) => err(msg),
                        Ok(()) => {
                            let path_for_dl = path.clone();
                            match execute_with_reset(pool, &server, move |p| {
                                let path = path_for_dl.clone();
                                Box::pin(async move { p.download_to_bytes(&path).await })
                            })
                            .await
                            {
                                Err(e) => err(e),
                                Ok(data) => {
                                    let window = preview_bytes as usize;
                                    let truncated = data.len() > window;
                                    let preview = if truncated {
                                        &data[..window]
                                    } else {
                                        &data[..]
                                    };
                                    let content = String::from_utf8_lossy(preview).to_string();
                                    ok(json!({
                                        "path": path,
                                        "content": content,
                                        "size": data.len(),
                                        "truncated": truncated,
                                        "preview_kb": preview_bytes / 1024,
                                    }))
                                }
                            }
                        }
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

            let path_for_call = path.clone();
            let result = match execute_with_reset(pool, &server, move |p| {
                let path = path_for_call.clone();
                Box::pin(async move { p.stat(&path).await })
            })
            .await
            {
                Err(e) => err(e),
                Ok(entry) => ok(json!({
                    "path": path, "name": entry.name, "is_dir": entry.is_dir,
                    "size": entry.size, "modified": entry.modified,
                    "permissions": entry.permissions, "owner": entry.owner,
                })),
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

            let path_for_call = path.clone();
            let pattern_for_call = pattern.clone();
            let result =
                match execute_with_reset(pool, &server, move |p| {
                    let path = path_for_call.clone();
                    let pattern = pattern_for_call.clone();
                    Box::pin(async move { p.find(&path, &pattern).await })
                })
                .await
                {
                    Err(e) => err(e),
                    Ok(entries) => {
                        let items: Vec<Value> = entries.iter().take(100).map(|e| json!({
                        "name": e.name, "path": e.path, "is_dir": e.is_dir, "size": e.size,
                    })).collect();
                        ok(json!({
                            "path": path, "pattern": pattern, "results": items,
                            "total": entries.len(), "truncated": entries.len() > 100,
                        }))
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
            let create_parents = get_bool_opt(args, "create_parents").unwrap_or(false);
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

            // Optional parent auto-creation. We deliberately run this BEFORE
            // acquiring the provider for the upload so that mkdir errors never
            // leave the pooled connection in a half-open data-channel state
            // (the exact failure mode that first motivated this refactor).
            if create_parents {
                if let Some(parent) = parent_remote_dir(&remote_path) {
                    if let Err(e) = ensure_remote_parents(pool, &server, &parent).await {
                        return finish(tool_name, Some(&server), Some(&remote_path), err(e), start);
                    }
                }
            }

            let result = match pool.get_provider(&server).await {
                Err(e) => err(e),
                Ok(arc) => {
                    let mut p = arc.lock().await;
                    if let Some(ref local) = local_path {
                        let bytes = std::fs::metadata(local).map(|m| m.len()).unwrap_or(0);
                        let cb = build_progress_callback(notifier, "upload");
                        match p.upload(local, &remote_path, cb).await {
                            Ok(()) => {
                                if let Some(n) = notifier {
                                    n.send_progress_final(
                                        bytes,
                                        Some(bytes),
                                        Some(format!("upload complete: {} bytes", bytes)),
                                    )
                                    .await;
                                }
                                ok(
                                    json!({ "remote_path": remote_path, "uploaded": true, "bytes": bytes }),
                                )
                            }
                            Err(e) => {
                                let transport = is_transport_error(&e);
                                let sanitized = sanitize_error(e);
                                drop(p);
                                drop(arc);
                                if transport {
                                    let _ = pool.invalidate(&server).await;
                                }
                                err(sanitized)
                            }
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
                            Err(e) => {
                                let transport = is_transport_error(&e);
                                let sanitized = sanitize_error(e);
                                drop(p);
                                drop(arc);
                                if transport {
                                    let _ = pool.invalidate(&server).await;
                                }
                                err(sanitized)
                            }
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
                    let cb = build_progress_callback(notifier, "download");
                    match p.download(&remote_path, &local_path, cb).await {
                        Ok(()) => {
                            if let Some(n) = notifier {
                                let bytes =
                                    std::fs::metadata(&local_path).map(|m| m.len()).unwrap_or(0);
                                n.send_progress_final(
                                    bytes,
                                    Some(bytes.max(1)),
                                    Some(format!("download complete: {} bytes", bytes)),
                                )
                                .await;
                            }
                            ok(
                                json!({ "remote_path": remote_path, "local_path": local_path, "downloaded": true }),
                            )
                        }
                        Err(e) => {
                            let transport = is_transport_error(&e);
                            let sanitized = sanitize_error(e);
                            drop(p);
                            drop(arc);
                            if transport {
                                let _ = pool.invalidate(&server).await;
                            }
                            err(sanitized)
                        }
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

            let path_for_call = path.clone();
            let result = match execute_with_reset(pool, &server, move |p| {
                let path = path_for_call.clone();
                Box::pin(async move { p.mkdir(&path).await })
            })
            .await
            {
                Ok(()) => ok(json!({ "path": path, "created": true })),
                Err(e) => err(e),
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

            let path_for_call = path.clone();
            let result = match execute_with_reset(pool, &server, move |p| {
                let path = path_for_call.clone();
                Box::pin(async move {
                    let entry = p.stat(&path).await?;
                    match delete_kind(entry.is_dir, recursive) {
                        DeleteKind::Directory => p.rmdir(&path).await.map(|_| (true, false)),
                        DeleteKind::DirectoryRecursive => {
                            p.rmdir_recursive(&path).await.map(|_| (true, true))
                        }
                        DeleteKind::File => p.delete(&path).await.map(|_| (false, false)),
                    }
                })
            })
            .await
            {
                Err(e) => err(e),
                Ok((is_dir, was_recursive)) => ok(json!({
                    "path": path,
                    "deleted": true,
                    "is_dir": is_dir,
                    "recursive": was_recursive,
                })),
            };
            finish(tool_name, Some(&server), Some(&path), result, start)
        }

        "aeroftp_delete_many" => {
            let server = match get_str(args, "server") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, None, None, err(e), start),
            };
            if let Err(e) = security::validate_server_query(&server) {
                return finish(tool_name, Some(&server), None, err(e), start);
            }
            let paths_val = match args.get("paths").and_then(|v| v.as_array()) {
                Some(a) => a.clone(),
                None => {
                    return finish(
                        tool_name,
                        Some(&server),
                        None,
                        err("'paths' must be a non-empty array of strings".into()),
                        start,
                    );
                }
            };
            if paths_val.is_empty() {
                return finish(
                    tool_name,
                    Some(&server),
                    None,
                    err("'paths' must contain at least one entry".into()),
                    start,
                );
            }
            if paths_val.len() > 100 {
                return finish(
                    tool_name,
                    Some(&server),
                    None,
                    err("Too many paths: max 100 per batch".into()),
                    start,
                );
            }
            let mut paths: Vec<String> = Vec::with_capacity(paths_val.len());
            for v in paths_val {
                match v.as_str() {
                    Some(s) if !s.is_empty() => paths.push(s.to_string()),
                    _ => {
                        return finish(
                            tool_name,
                            Some(&server),
                            None,
                            err("every entry of 'paths' must be a non-empty string".into()),
                            start,
                        );
                    }
                }
            }
            for p in &paths {
                if let Err(e) = security::validate_remote_path(p) {
                    return finish(tool_name, Some(&server), Some(p), err(e), start);
                }
            }
            let recursive = get_bool_opt(args, "recursive").unwrap_or(false);
            let continue_on_error = get_bool_opt(args, "continue_on_error").unwrap_or(true);
            // Default 200 ms — matches the rhythm Lumo had to simulate with
            // external sleeps. Cap at 2 s so an agent cannot stall the tool
            // indefinitely by passing a huge value.
            let delay_ms = args
                .get("delay_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(200)
                .min(2_000);

            let mut results: Vec<Value> = Vec::with_capacity(paths.len());
            let mut deleted_ok: u32 = 0;
            let mut errors: u32 = 0;
            let mut aborted_after: Option<usize> = None;
            for (idx, path) in paths.iter().enumerate() {
                if idx > 0 && delay_ms > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                }
                let path_for_call = path.clone();
                let outcome = execute_with_reset(pool, &server, move |p| {
                    let path = path_for_call.clone();
                    Box::pin(async move {
                        let entry = p.stat(&path).await?;
                        match delete_kind(entry.is_dir, recursive) {
                            DeleteKind::Directory => p.rmdir(&path).await.map(|_| (true, false)),
                            DeleteKind::DirectoryRecursive => {
                                p.rmdir_recursive(&path).await.map(|_| (true, true))
                            }
                            DeleteKind::File => p.delete(&path).await.map(|_| (false, false)),
                        }
                    })
                })
                .await;
                match outcome {
                    Ok((is_dir, was_recursive)) => {
                        deleted_ok += 1;
                        results.push(json!({
                            "path": path,
                            "deleted": true,
                            "is_dir": is_dir,
                            "recursive": was_recursive,
                        }));
                    }
                    Err(e) => {
                        errors += 1;
                        results.push(json!({
                            "path": path,
                            "deleted": false,
                            "error": e,
                        }));
                        if !continue_on_error {
                            aborted_after = Some(idx + 1);
                            break;
                        }
                    }
                }
            }
            let total_planned = paths.len();
            let processed = results.len();
            let result = ok(json!({
                "server": server,
                "results": results,
                "summary": {
                    "planned": total_planned,
                    "processed": processed,
                    "deleted": deleted_ok,
                    "errors": errors,
                    "aborted_after": aborted_after,
                    "delay_ms": delay_ms,
                }
            }));
            finish(tool_name, Some(&server), None, result, start)
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

            let from_for_call = from.clone();
            let to_for_call = to.clone();
            let result = match execute_with_reset(pool, &server, move |p| {
                let from = from_for_call.clone();
                let to = to_for_call.clone();
                Box::pin(async move { p.rename(&from, &to).await })
            })
            .await
            {
                Ok(()) => ok(json!({ "from": from, "to": to, "renamed": true })),
                Err(e) => err(e),
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

            let result = match execute_with_reset(pool, &server, move |p| {
                Box::pin(async move { p.storage_info().await })
            })
            .await
            {
                Err(e) => err(e),
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

            let result = match execute_with_reset(pool, &server, move |p| {
                Box::pin(async move {
                    let info = p.server_info().await?;
                    Ok::<_, ProviderError>((
                        info,
                        p.provider_type().to_string(),
                        p.supports_share_links(),
                        p.supports_server_copy(),
                        p.supports_versions(),
                        p.supports_checksum(),
                        p.supports_find(),
                    ))
                })
            })
            .await
            {
                Err(e) => err(e),
                Ok((info, provider_type, share, copy, versions, checksum, find)) => ok(json!({
                    "provider_type": provider_type,
                    "server_info": info,
                    "supports_share_links": share,
                    "supports_server_copy": copy,
                    "supports_versions": versions,
                    "supports_checksum": checksum,
                    "supports_find": find,
                })),
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

            let path_for_call = path.clone();
            let opts = ShareLinkOptions {
                expires_in_secs: expires,
                password,
                permissions: None,
            };
            let result = match execute_with_reset(pool, &server, move |p| {
                let path = path_for_call.clone();
                let opts = opts.clone();
                Box::pin(async move { p.create_share_link(&path, opts).await })
            })
            .await
            {
                Err(e) => err(e),
                Ok(link) => ok(
                    json!({ "url": link.url, "password": link.password, "expires_at": link.expires_at }),
                ),
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

            let from_for_call = from.clone();
            let to_for_call = to.clone();
            let result = match execute_with_reset(pool, &server, move |p| {
                let from = from_for_call.clone();
                let to = to_for_call.clone();
                Box::pin(async move { p.server_copy(&from, &to).await })
            })
            .await
            {
                Ok(()) => ok(json!({ "from": from, "to": to, "copied": true })),
                Err(e) => err(e),
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

            let path_for_call = path.clone();
            let result = match execute_with_reset(pool, &server, move |p| {
                let path = path_for_call.clone();
                Box::pin(async move { p.list_versions(&path).await })
            })
            .await
            {
                Err(e) => err(e),
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

            let path_for_call = path.clone();
            let result = match execute_with_reset(pool, &server, move |p| {
                let path = path_for_call.clone();
                Box::pin(async move { p.checksum(&path).await })
            })
            .await
            {
                Err(e) => err(e),
                Ok(checksums) => ok(json!({ "path": path, "checksums": checksums })),
            };
            finish(tool_name, Some(&server), Some(&path), result, start)
        }

        "aeroftp_close_connection" => {
            let server = match get_str(args, "server") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, None, None, err(e), start),
            };
            if let Err(e) = security::validate_server_query(&server) {
                return finish(tool_name, Some(&server), None, err(e), start);
            }
            let result = match pool.close_one(&server).await {
                Some(name) => ok(json!({
                    "server": server,
                    "closed": true,
                    "name": name,
                })),
                None => ok(json!({
                    "server": server,
                    "closed": false,
                    "reason": "no active pooled connection matched the query",
                })),
            };
            finish(tool_name, Some(&server), None, result, start)
        }

        "aeroftp_check_tree" => {
            use crate::sync_core::{compare_trees, scan_local_tree, scan_remote_tree, ScanOptions};

            let server = match get_str(args, "server") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, None, None, err(e), start),
            };
            let local_dir = match get_str(args, "local_dir") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, Some(&server), None, err(e), start),
            };
            let remote_dir = match get_str(args, "remote_dir") {
                Ok(s) => s,
                Err(e) => {
                    return finish(tool_name, Some(&server), Some(&local_dir), err(e), start);
                }
            };
            if let Err(e) = validate_sp(&server, Some(&remote_dir)) {
                return finish(tool_name, Some(&server), Some(&remote_dir), err(e), start);
            }
            if let Err(e) = security::validate_local_path(&local_dir) {
                return finish(tool_name, Some(&server), Some(&remote_dir), err(e), start);
            }
            let one_way = get_bool_opt(args, "one_way").unwrap_or(false);
            let checksum = get_bool_opt(args, "checksum").unwrap_or(false);
            let exclude = args
                .get("exclude")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let max_depth = args
                .get("max_depth")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            let cap = args
                .get("max_entries_reported")
                .and_then(|v| v.as_u64())
                .unwrap_or(200) as usize;

            if !std::path::Path::new(&local_dir).is_dir() {
                return finish(
                    tool_name,
                    Some(&server),
                    Some(&remote_dir),
                    err(format!("Local path is not a directory: {}", local_dir)),
                    start,
                );
            }

            // When `checksum=true`, request hashes on BOTH sides. The scan
            // silently falls back to size-only if the provider lacks server-
            // side checksum support — handled inside `scan_remote_tree`.
            let opts = ScanOptions {
                max_depth,
                exclude_patterns: exclude,
                compute_checksum: checksum,
                compute_remote_checksum: checksum,
                ..Default::default()
            };

            let result = match pool.get_provider(&server).await {
                Err(e) => err(e),
                Ok(arc) => {
                    let mut p = arc.lock().await;
                    let supports_remote_checksum = p.supports_checksum();
                    let locals = scan_local_tree(&local_dir, &opts);
                    let remotes = scan_remote_tree(&mut p, &remote_dir, &opts).await;
                    let diff = compare_trees(&locals, &remotes, one_way);
                    let entries_to_json = |entries: &[crate::sync_core::DiffEntry]| -> Vec<Value> {
                        entries
                            .iter()
                            .take(cap)
                            .map(|e| {
                                json!({
                                    "path": e.rel_path,
                                    "local_size": e.local_size,
                                    "remote_size": e.remote_size,
                                    "local_sha256": e.local_sha256,
                                    "remote_checksum_alg": e.remote_checksum_alg,
                                    "remote_checksum_hex": e.remote_checksum_hex,
                                    "compare_method": e.compare_method,
                                })
                            })
                            .collect()
                    };
                    ok(json!({
                        "server": server,
                        "local_dir": local_dir,
                        "remote_dir": remote_dir,
                        "checksum_requested": checksum,
                        "checksum_remote_supported": supports_remote_checksum,
                        "summary": {
                            "match": diff.match_count(),
                            "differ": diff.differ_count(),
                            "missing_local": diff.missing_local_count(),
                            "missing_remote": diff.missing_remote_count(),
                        },
                        "groups": {
                            "match": entries_to_json(&diff.matches),
                            "differ": entries_to_json(&diff.differ),
                            "missing_local": entries_to_json(&diff.missing_local),
                            "missing_remote": entries_to_json(&diff.missing_remote),
                        },
                        "has_differences": diff.has_differences(),
                        "truncated": diff.match_count() > cap
                            || diff.differ_count() > cap
                            || diff.missing_local_count() > cap
                            || diff.missing_remote_count() > cap,
                    }))
                }
            };
            finish(tool_name, Some(&server), Some(&remote_dir), result, start)
        }

        "aeroftp_sync_tree" => {
            use crate::sync_core::{
                sync_tree_core, ConflictMode, ScanOptions, SyncDirection, SyncOptions,
            };

            let server = match get_str(args, "server") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, None, None, err(e), start),
            };
            let local_dir = match get_str(args, "local_dir") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, Some(&server), None, err(e), start),
            };
            let remote_dir = match get_str(args, "remote_dir") {
                Ok(s) => s,
                Err(e) => {
                    return finish(tool_name, Some(&server), Some(&local_dir), err(e), start);
                }
            };
            let direction_raw = match get_str(args, "direction") {
                Ok(s) => s,
                Err(e) => {
                    return finish(tool_name, Some(&server), Some(&remote_dir), err(e), start);
                }
            };
            let direction = match SyncDirection::parse(&direction_raw) {
                Some(d) => d,
                None => {
                    return finish(
                        tool_name,
                        Some(&server),
                        Some(&remote_dir),
                        err(format!(
                            "Invalid direction '{}': expected upload, download, or both",
                            direction_raw
                        )),
                        start,
                    );
                }
            };
            if let Err(e) = validate_sp(&server, Some(&remote_dir)) {
                return finish(tool_name, Some(&server), Some(&remote_dir), err(e), start);
            }
            if let Err(e) = security::validate_local_path(&local_dir) {
                return finish(tool_name, Some(&server), Some(&remote_dir), err(e), start);
            }
            let dry_run = get_bool_opt(args, "dry_run").unwrap_or(false);
            let delete_orphans = get_bool_opt(args, "delete_orphans").unwrap_or(false);
            if delete_orphans && matches!(direction, SyncDirection::Both) {
                return finish(
                    tool_name,
                    Some(&server),
                    Some(&remote_dir),
                    err(
                        "delete_orphans is only supported with direction=upload or direction=download".into(),
                    ),
                    start,
                );
            }
            let conflict_raw = get_str_opt(args, "conflict_mode").unwrap_or_default();
            let conflict_mode = if conflict_raw.is_empty() {
                ConflictMode::Larger
            } else {
                match ConflictMode::parse(&conflict_raw) {
                    Some(c) => c,
                    None => {
                        return finish(
                            tool_name,
                            Some(&server),
                            Some(&remote_dir),
                            err(format!(
                                "Invalid conflict_mode '{}': expected larger, newer, or skip",
                                conflict_raw
                            )),
                            start,
                        );
                    }
                }
            };
            let exclude = args
                .get("exclude")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let max_depth = args
                .get("max_depth")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
            if !std::path::Path::new(&local_dir).is_dir() {
                return finish(
                    tool_name,
                    Some(&server),
                    Some(&remote_dir),
                    err(format!("Local path is not a directory: {}", local_dir)),
                    start,
                );
            }

            let opts = SyncOptions {
                direction,
                dry_run,
                delete_orphans,
                conflict_mode,
                scan: ScanOptions {
                    exclude_patterns: exclude,
                    max_depth,
                    ..Default::default()
                },
            };

            let result = match pool.get_provider(&server).await {
                Err(e) => err(e),
                Ok(arc) => {
                    let mut p = arc.lock().await;
                    let mut sink = NotifierSyncSink::new(notifier, dry_run);
                    sink.emit_started(&direction_raw, dry_run).await;
                    let report =
                        sync_tree_core(&mut p, &local_dir, &remote_dir, &opts, &mut sink).await;
                    sink.emit_finished(&report).await;
                    let errors: Vec<Value> = report
                        .errors
                        .iter()
                        .take(50)
                        .map(|e| {
                            json!({
                                "path": e.rel_path,
                                "operation": e.operation,
                                "error": e.message,
                            })
                        })
                        .collect();
                    // Plan is informational and only meaningful in dry-run.
                    // Cap at 1000 entries to keep the JSON payload bounded on
                    // huge syncs — the agent should use `aeroftp_check_tree`
                    // on a narrower subtree if it needs more.
                    let plan_cap = 1000usize;
                    let plan_full_len = if dry_run { sink.plan.len() } else { 0 };
                    let plan_json: Vec<Value> = if dry_run {
                        sink.plan
                            .iter()
                            .take(plan_cap)
                            .map(|p| {
                                json!({
                                    "op": p.op,
                                    "path": p.path,
                                    "reason": p.reason,
                                    "bytes": p.bytes,
                                })
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let mut payload = json!({
                        "server": server,
                        "local_dir": local_dir,
                        "remote_dir": remote_dir,
                        "direction": direction_raw,
                        "dry_run": report.dry_run,
                        "summary": {
                            "uploaded": report.uploaded,
                            "downloaded": report.downloaded,
                            "deleted": report.deleted,
                            "skipped": report.skipped,
                            "errors": report.error_count(),
                            "elapsed_secs": report.elapsed_secs,
                        },
                        "errors": errors,
                        "errors_truncated": report.error_count() > 50,
                    });
                    if dry_run {
                        // Project the sync_core skips back into action
                        // counters so the agent sees `planned.uploaded=N`
                        // instead of always-zero `summary.uploaded`.
                        let mut planned_uploaded = 0u32;
                        let mut planned_downloaded = 0u32;
                        let mut planned_deleted = 0u32;
                        let mut planned_skipped = 0u32;
                        for entry in &sink.plan {
                            if entry.reason == "dry-run" {
                                match entry.op.as_str() {
                                    "upload" => planned_uploaded += 1,
                                    "download" => planned_downloaded += 1,
                                    "delete_remote" | "delete_local" => planned_deleted += 1,
                                    _ => planned_skipped += 1,
                                }
                            } else {
                                planned_skipped += 1;
                            }
                        }
                        if let Some(obj) = payload.as_object_mut() {
                            obj.insert(
                                "planned".to_string(),
                                json!({
                                    "uploaded": planned_uploaded,
                                    "downloaded": planned_downloaded,
                                    "deleted": planned_deleted,
                                    "skipped": planned_skipped,
                                }),
                            );
                            obj.insert("plan".to_string(), json!(plan_json));
                            obj.insert("plan_total".to_string(), json!(plan_full_len));
                            obj.insert(
                                "plan_truncated".to_string(),
                                json!(plan_full_len > plan_cap),
                            );
                        }
                    }
                    ok(payload)
                }
            };
            finish(tool_name, Some(&server), Some(&remote_dir), result, start)
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

/// A single entry in the dry-run plan returned by `aeroftp_sync_tree`.
///
/// Distinct from `SyncError` in that it is informational: it describes the
/// action `sync_tree_core` would have taken. The `reason` field comes from
/// the `FileOutcome::Skipped { reason }` variant — for dry-run entries this
/// is the literal `"dry-run"`, for non-dry-run skips it is the decision
/// rationale (`"identical size"`, `"remote is larger"`, etc.).
#[derive(Debug, Clone)]
struct PlanEntry {
    op: String,
    path: String,
    reason: String,
    bytes: u64,
}

/// Progress sink for `aeroftp_sync_tree` that translates `SyncProgressSink`
/// events into MCP `notifications/progress` messages.
///
/// The sink keeps a running `processed` counter and a single synthetic `total`
/// of 100 so that clients that render a determinate progress bar still get
/// coherent output even when per-file byte progress is not available.
///
/// Also captures the dry-run plan: on `on_file_start` we stash the operation
/// label announced by `sync_core` and match it to the corresponding
/// `on_file_done`. This gives the agent the explicit `{op, path, reason}`
/// tuple that plain `summary.uploaded=0` could not.
struct NotifierSyncSink<'a> {
    notifier: Option<&'a McpNotifier>,
    processed: u32,
    failures: u32,
    /// Single-consumer progress drain. Constructed lazily on first use so
    /// sinks built without an active notifier pay no allocation cost.
    /// Replaces per-event `tokio::spawn` — each spawned no-op future was a
    /// task-level allocation plus scheduler entry; under a 500k-file sync
    /// those dominated the runtime.
    progress_tx: Option<mpsc::Sender<ProgressSample>>,
    /// True when `sync_tree_core` was called with `dry_run=true`. Used to
    /// short-circuit plan collection on real runs.
    dry_run: bool,
    /// Latched copy of the most recent `on_file_start` event. Cleared on
    /// the paired `on_file_done`. Since `sync_tree_core` is single-threaded
    /// per sink, there is always at most one in-flight operation.
    current_op: Option<(String, String, u64)>,
    /// Collected plan in emission order. Only populated when `dry_run`.
    plan: Vec<PlanEntry>,
}

impl<'a> NotifierSyncSink<'a> {
    fn new(notifier: Option<&'a McpNotifier>, dry_run: bool) -> Self {
        let progress_tx = notifier.cloned().map(|n| {
            let (tx, rx) = mpsc::channel::<ProgressSample>(64);
            spawn_progress_consumer(n, rx);
            tx
        });
        Self {
            notifier,
            processed: 0,
            failures: 0,
            progress_tx,
            dry_run,
            current_op: None,
            plan: Vec::new(),
        }
    }

    async fn emit_started(&self, direction: &str, dry_run: bool) {
        if let Some(n) = self.notifier {
            let msg = format!(
                "sync {} started{}",
                direction,
                if dry_run { " (dry run)" } else { "" }
            );
            n.send_progress(0, Some(100), Some(msg)).await;
        }
    }

    async fn emit_finished(&self, report: &crate::sync_core::SyncReport) {
        if let Some(n) = self.notifier {
            let msg = format!(
                "sync done: {} up, {} down, {} del, {} skip, {} err ({:.1}s)",
                report.uploaded,
                report.downloaded,
                report.deleted,
                report.skipped,
                report.error_count(),
                report.elapsed_secs,
            );
            n.send_progress_final(100, Some(100), Some(msg)).await;
        }
    }
}

impl crate::sync_core::SyncProgressSink for NotifierSyncSink<'_> {
    fn on_phase(&mut self, phase: crate::sync_core::SyncPhase) {
        if let Some(tx) = self.progress_tx.as_ref() {
            let label = match phase {
                crate::sync_core::SyncPhase::Scanning => "scanning",
                crate::sync_core::SyncPhase::Planning => "planning",
                crate::sync_core::SyncPhase::Executing => "executing",
                crate::sync_core::SyncPhase::Done => "done",
            };
            push_progress(tx, (0, Some(100), format!("phase: {}", label)));
        }
    }

    fn on_file_start(&mut self, rel: &str, total: u64, op: &'static str) {
        if self.dry_run {
            self.current_op = Some((op.to_string(), rel.to_string(), total));
        }
        if let Some(tx) = self.progress_tx.as_ref() {
            let msg = format!("{}: {}", op, rel);
            push_progress(tx, (self.processed.into(), None, msg));
        }
    }

    fn on_file_progress(&mut self, _rel: &str, _sent: u64, _total: u64) {
        // The sync orchestration does not (yet) propagate per-byte progress
        // from the provider callbacks. When it does, this hook is ready.
    }

    fn on_file_done(&mut self, rel: &str, outcome: &crate::sync_core::FileOutcome) {
        self.processed = self.processed.saturating_add(1);
        if matches!(outcome, crate::sync_core::FileOutcome::Failed { .. }) {
            self.failures = self.failures.saturating_add(1);
        }
        if self.dry_run {
            if let crate::sync_core::FileOutcome::Skipped { reason } = outcome {
                // Prefer the op announced via on_file_start (set for every
                // Copy decision, including dry-run upload/download/delete).
                // Without a paired start, the entry is a pre-decision skip —
                // still worth recording so the agent sees the full scan.
                let (op, bytes) = match self.current_op.take() {
                    Some((op, path, total)) if path == rel => (op, total),
                    Some((op, _, total)) => (op, total),
                    None => ("skip".to_string(), 0),
                };
                self.plan.push(PlanEntry {
                    op,
                    path: rel.to_string(),
                    reason: reason.clone(),
                    bytes,
                });
            } else {
                self.current_op = None;
            }
        }
        if let Some(tx) = self.progress_tx.as_ref() {
            let processed = self.processed;
            let failures = self.failures;
            let msg = match outcome {
                crate::sync_core::FileOutcome::Uploaded { bytes } => {
                    format!("uploaded {} ({} bytes)", rel, bytes)
                }
                crate::sync_core::FileOutcome::Downloaded { bytes } => {
                    format!("downloaded {} ({} bytes)", rel, bytes)
                }
                crate::sync_core::FileOutcome::Deleted => format!("deleted {}", rel),
                crate::sync_core::FileOutcome::Skipped { reason } => {
                    format!("skipped {}: {}", rel, reason)
                }
                crate::sync_core::FileOutcome::Failed { error } => {
                    format!("failed {}: {}", rel, error)
                }
            };
            let full_msg = format!(
                "{} (ok={}, err={})",
                msg,
                processed.saturating_sub(failures),
                failures
            );
            push_progress(tx, (processed.into(), None, full_msg));
        }
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
        let err = validate_read_preview_target(true, 0, 5 * 1024).unwrap_err();
        assert!(err.contains("Cannot read a directory"));
    }

    #[test]
    fn read_preview_rejects_file_larger_than_hard_cap() {
        // Only files above the absolute hard cap are rejected; smaller files
        // are truncated silently downstream.
        let err = validate_read_preview_target(false, MAX_READ_PREVIEW_BYTES + 1, 16 * 1024)
            .unwrap_err();
        assert!(err.contains("File too large for preview"));
        assert!(err.contains(&format!("Hard cap: {} KB", MAX_READ_PREVIEW_BYTES / 1024)));
    }

    #[test]
    fn read_preview_accepts_file_inside_window() {
        assert!(validate_read_preview_target(false, 4096, 5 * 1024).is_ok());
    }

    #[test]
    fn read_preview_accepts_file_over_window_but_under_hard_cap() {
        // 32 KB file with a 16 KB caller window is now accepted — callers
        // get a truncated preview instead of a hard error.
        assert!(validate_read_preview_target(false, 32 * 1024, 16 * 1024).is_ok());
    }

    #[test]
    fn read_preview_accepts_large_file_when_window_extended() {
        // With preview_kb=1024 (hard cap), a 900 KB file must pass.
        assert!(validate_read_preview_target(false, 900 * 1024, MAX_READ_PREVIEW_BYTES).is_ok());
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

    #[test]
    fn registry_exposes_parity_tools() {
        let tools = tool_definitions();
        let names: Vec<&str> = tools.iter().map(|t| t.name).collect();
        for required in [
            "aeroftp_close_connection",
            "aeroftp_check_tree",
            "aeroftp_sync_tree",
        ] {
            assert!(
                names.contains(&required),
                "missing tool {} from registry",
                required
            );
        }
    }

    #[test]
    fn transport_error_classifier_catches_variant_level_failures() {
        use super::is_transport_error;
        use crate::providers::ProviderError;
        assert!(is_transport_error(&ProviderError::NotConnected));
        assert!(is_transport_error(&ProviderError::ConnectionFailed(
            "refused".into(),
        )));
        assert!(is_transport_error(&ProviderError::Timeout));
        assert!(is_transport_error(&ProviderError::NetworkError(
            "dns".into(),
        )));
    }

    #[test]
    fn transport_error_classifier_catches_message_patterns() {
        use super::is_transport_error;
        use crate::providers::ProviderError;
        // The exact failure Lumo CMS hit on Aruba FTP after a 553 — the
        // control channel comes back with "Data connection is already open"
        // on every subsequent command until it is reset.
        assert!(is_transport_error(&ProviderError::TransferFailed(
            "Data connection is already open".into(),
        )));
        assert!(is_transport_error(&ProviderError::Other(
            "broken pipe while sending data".into(),
        )));
        assert!(is_transport_error(&ProviderError::ServerError(
            "connection reset by peer".into(),
        )));
    }

    #[test]
    fn transport_error_classifier_leaves_business_errors_alone() {
        use super::is_transport_error;
        use crate::providers::ProviderError;
        // Business-level failures must NOT invalidate the pool — doing so
        // would churn connections on every harmless 404/409 and defeat the
        // whole point of pooling.
        assert!(!is_transport_error(&ProviderError::NotFound(
            "/missing".into(),
        )));
        assert!(!is_transport_error(&ProviderError::AlreadyExists(
            "/exists".into(),
        )));
        assert!(!is_transport_error(&ProviderError::PermissionDenied(
            "read-only".into(),
        )));
        assert!(!is_transport_error(&ProviderError::AuthenticationFailed(
            "bad creds".into(),
        )));
    }

    #[test]
    fn parent_remote_dir_handles_common_shapes() {
        use super::parent_remote_dir;
        assert_eq!(parent_remote_dir("/a/b/c.txt").as_deref(), Some("/a/b"));
        assert_eq!(parent_remote_dir("/a/b/c/").as_deref(), Some("/a/b"));
        // Root-relative single segment → no parent to mkdir.
        assert_eq!(parent_remote_dir("/foo"), None);
        assert_eq!(parent_remote_dir("/"), None);
        assert_eq!(parent_remote_dir("bare.txt"), None);
    }

    #[test]
    fn delete_many_registry_entry_is_present_and_destructive() {
        use super::{tool_definitions, RateCategory};
        let entry = tool_definitions()
            .into_iter()
            .find(|t| t.name == "aeroftp_delete_many")
            .expect("delete_many tool should be registered");
        assert!(matches!(entry.category, RateCategory::Destructive));
        let paths = entry
            .input_schema
            .get("properties")
            .and_then(|p| p.get("paths"));
        assert!(paths.is_some(), "delete_many must expose a paths[] schema");
    }

    #[test]
    fn list_servers_registry_entry_exposes_filters() {
        use super::tool_definitions;
        let entry = tool_definitions()
            .into_iter()
            .find(|t| t.name == "aeroftp_list_servers")
            .expect("list_servers tool should be registered");
        for needed in ["name_contains", "protocol", "limit", "offset"] {
            assert!(
                entry
                    .input_schema
                    .get("properties")
                    .and_then(|p| p.get(needed))
                    .is_some(),
                "list_servers schema missing '{}'",
                needed
            );
        }
    }

    #[test]
    fn sync_tree_schema_declares_direction_enum() {
        let sync = tool_definitions()
            .into_iter()
            .find(|t| t.name == "aeroftp_sync_tree")
            .expect("sync_tree tool");
        let direction = sync
            .input_schema
            .get("properties")
            .and_then(|p| p.get("direction"))
            .expect("direction property");
        let en = direction
            .get("enum")
            .and_then(|v| v.as_array())
            .expect("enum array");
        let variants: Vec<&str> = en.iter().filter_map(|v| v.as_str()).collect();
        assert!(variants.contains(&"upload"));
        assert!(variants.contains(&"download"));
        assert!(variants.contains(&"both"));
    }
}
