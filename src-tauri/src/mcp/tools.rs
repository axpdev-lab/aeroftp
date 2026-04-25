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

/// Timestamp when the MCP process first touched this module. Used by
/// `aeroftp_mcp_info` to report a stable `started_at` + `uptime_secs`.
/// Captured lazily on first access rather than at server-handshake time so
/// we do not require a boot hook — the gap between process start and first
/// tool call is sub-millisecond in practice.
static MCP_START: std::sync::LazyLock<chrono::DateTime<chrono::Utc>> =
    std::sync::LazyLock::new(chrono::Utc::now);

/// Hard cap for in-memory read previews.
const MAX_READ_PREVIEW_BYTES: u64 = 1_048_576;

/// Hard cap for `aeroftp_edit`. Ten times the read preview cap because edit
/// is the primary alternative to full download/modify/upload for agents —
/// being too restrictive pushes them back to that anti-pattern. Ten megabytes
/// still keeps a single tool call well inside reasonable in-memory bounds.
const MAX_EDIT_BYTES: u64 = 10 * 1_048_576;

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
///
/// The `421 control*` family was added after v3.5.10 deploy testing: Aruba's
/// proftpd (and most servers with an idle-timeout policy) drops the control
/// channel after ~5 minutes of silence, which happens routinely during long
/// remote scans where only data channels are active. The first subsequent
/// op hits `421 Control connection timed out`; without this classification
/// the error bubbles up to the agent as if the transfer itself failed, when
/// really the pool just needs a fresh connection.
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
        // 421 family — idle / timeout / control channel drop.
        "421 ",
        "control connection timed out",
        "control connection closed",
        "service not available",
        "idle timeout",
        "timeout waiting",
        "session timeout",
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
            description: "Upload a local file or inline text content to a remote server. Set create_parents=true to auto-mkdir missing parent directories (idempotent). Set no_clobber=true to skip the upload if the destination already exists (response returns `uploaded: false, skipped: true, reason: \"exists\"`).",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID" },
                "remote_path": { "type": "string", "description": "Destination path on the server" },
                "local_path": { "type": "string", "description": "Local file path to upload (mutually exclusive with content)" },
                "content": { "type": "string", "description": "Inline text content to upload (mutually exclusive with local_path)" },
                "create_parents": { "type": "boolean", "description": "Recursively mkdir the parent directories if missing (default: false)" },
                "no_clobber": { "type": "boolean", "description": "Skip the upload if the destination already exists — no overwrite. Default: false (overwrite)." }
            }, "required": ["server", "remote_path"] }),
            category: RateCategory::Mutative,
        },
        McpToolDef {
            name: "aeroftp_upload_many",
            description: "Upload multiple local files to a remote server in a single batch. Each item may specify its own `create_parents` and `no_clobber` flags. Applies a server-friendly backoff between operations (same contract as aeroftp_delete_many) and returns per-item results plus summary.totals. Serial-per-connection: one item at a time on the pooled connection, since most protocols (FTP, SFTP) do not parallelize uploads on a single session. Set continue_on_error=false to stop at the first failure.",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID" },
                "items": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "local_path": { "type": "string", "description": "Local file to upload" },
                            "remote_path": { "type": "string", "description": "Destination path on the server" },
                            "create_parents": { "type": "boolean", "description": "Auto-mkdir missing parent directories (default: false)" },
                            "no_clobber": { "type": "boolean", "description": "Skip this item if the destination already exists (default: false)" }
                        },
                        "required": ["local_path", "remote_path"]
                    },
                    "description": "Items to upload (max 100)"
                },
                "continue_on_error": { "type": "boolean", "description": "Keep going after a single-item failure (default: true)" },
                "delay_ms": { "type": "integer", "description": "Pause between uploads in milliseconds (default: 0, cap: 2000)" },
                "no_clobber": { "type": "boolean", "description": "Default no_clobber for every item that does not override it (default: false)" }
            }, "required": ["server", "items"] }),
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
            description: "Delete one or more remote entries. Pass 'path' for a single delete, or 'paths' (array, max 100) for batch mode. Batch mode applies a server-friendly backoff and returns per-item results plus an aggregate summary. Set recursive=true to remove non-empty directories. `aeroftp_delete_many` is a kept-for-compat alias of this tool in batch mode.",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID" },
                "path": { "type": "string", "description": "Remote path to delete (single-delete mode; mutually exclusive with 'paths')" },
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Remote paths to delete (batch mode; max 100). Mutually exclusive with 'path'."
                },
                "recursive": { "type": "boolean", "description": "Delete directories recursively (default: false)" },
                "continue_on_error": { "type": "boolean", "description": "Batch mode: keep going after a single-item failure (default: true)" },
                "delay_ms": { "type": "integer", "description": "Batch mode: pause between deletes in milliseconds (default: 200, cap: 2000)" }
            }, "required": ["server"] }),
            category: RateCategory::Destructive,
        },
        McpToolDef {
            name: "aeroftp_delete_many",
            description: "Deprecated alias of `aeroftp_delete` in batch mode. Prefer the unified `aeroftp_delete` tool with the `paths` array. Accepts the same arguments as the batch branch of `aeroftp_delete` and returns the same shape.",
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
            name: "aeroftp_edit",
            description: "Find-and-replace on a remote UTF-8 text file without downloading it locally. Replaces all occurrences by default, or only the first when `first=true`. Returns the number of replacements and bytes before/after. If no match is found, the file is NOT re-uploaded (no-op). Rejects binary files, files larger than 10 MB, and directories.",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID" },
                "path": { "type": "string", "description": "Remote file path (UTF-8 text)" },
                "find": { "type": "string", "description": "Literal string to search for (not a regex)" },
                "replace": { "type": "string", "description": "Replacement string" },
                "first": { "type": "boolean", "description": "Replace only the first occurrence (default: false — replace all)" }
            }, "required": ["server", "path", "find", "replace"] }),
            category: RateCategory::Mutative,
        },
        McpToolDef {
            name: "aeroftp_mcp_info",
            description: "Return diagnostics about the running MCP process itself: PID, binary path, binary mtime/size, version, uptime, and (on Linux) whether the executable file has been deleted while the process is still running. Agents should call this at the start of a test session to verify the binary matches the expected build — if `binary_deleted=true` or `binary_mtime` predates the last rebuild, the process is stale and the user should restart the MCP server before continuing.",
            input_schema: json!({ "type": "object", "properties": {}, "required": [] }),
            category: RateCategory::ReadOnly,
        },
        McpToolDef {
            name: "aeroftp_close_connection",
            description: "Close a pooled server connection explicitly. Useful when the agent wants to free resources or force a fresh authentication handshake on the next call. Returns {released, closed, was_active} so callers can distinguish a real close from a no-op (nothing pooled matched the query).",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID to disconnect" }
            }, "required": ["server"] }),
            category: RateCategory::Mutative,
        },
        McpToolDef {
            name: "aeroftp_check_tree",
            description: "Compare a local directory against a remote directory and report differences grouped as match, differ, missing_local, missing_remote. Each entry carries compare_method ('checksum' or 'size') so agents can tell how the decision was made. When checksum=true, SHA-256 is computed on local files AND on the remote when the provider supports server-side checksums; otherwise comparison falls back to size. Supports glob excludes and one-way mode. For large scopes set summary_only=true to get just the counters without the per-entry arrays.",
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
                "files": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Restrict the check to this exact set of relative paths. Useful when the agent already knows the candidate set and wants to skip a full tree scan."
                },
                "max_depth": { "type": "integer", "description": "Max recursion depth (default: 100)" },
                "max_entries_reported": { "type": "integer", "description": "Cap per-group entries returned (default: 200). Ignored when summary_only=true." },
                "summary_only": { "type": "boolean", "description": "Return only summary counters (match/differ/missing_local/missing_remote) and has_differences. Drops the groups arrays entirely. Use for large scopes where the response would exceed MCP size limits. Default: false." },
                "group_by_depth": { "type": "integer", "description": "When > 0, add `summary.by_top_level_dir` aggregating counts grouped by the first N path segments. 1 = top-level directory only, 2 = two levels deep. Files at root are aggregated under `_root`. Default: 0 (disabled)." }
            }, "required": ["server", "local_dir", "remote_dir"] }),
            category: RateCategory::ReadOnly,
        },
        McpToolDef {
            name: "aeroftp_sync_tree",
            description: "Synchronize a local directory with a remote directory. Direction: upload, download, or both. Supports dry_run, delete_orphans (upload/download only), conflict resolution (larger/newer/skip), explicit delta_policy (disabled/size_only/mtime/hash/delta), and glob excludes. Emits progress notifications when the caller supplies a progressToken. Output shape: dry_run=true returns {plan, plan_by_op, plan_by_op_totals, plan_total, planned, scan_stats, errors}; dry_run=false returns {summary (with summary.totals), errors}. The two fields never coexist. Each plan/error entry reports the per-file `decision_policy` actually used by the core. `plan_by_op` groups the plan per operation (upload/download/delete/skip) with per-bucket caps of 250 so an agent can see every candidate of a given type even when the total plan is huge. Set summary_only=true to drop the per-entry plan/plan_by_op/errors arrays when the response would otherwise exceed MCP size limits. When at least one file traveled through the rsync delta path, `summary.delta_savings` is added with {files_using_delta, total_bytes_sent, total_size, bytes_saved, average_speedup}; the block is OMITTED entirely when no file used the delta path. `bytes_saved` can be negative when rsync overhead exceeds the savings on small-file runs. `average_speedup` is omitted when `total_bytes_sent==0`. Alongside `delta_savings`, `summary.delta_files` carries a per-file breakdown as an array of `{path, bytes_sent, total_size, speedup}`, capped at 500 entries; on runs above the cap, `summary.delta_files_truncated: true` is added and the `delta_savings` aggregate keeps counting past the cap. Both keys are OMITTED on classic-only runs.",
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
                "delta_policy": {
                    "type": "string",
                    "enum": ["disabled", "size_only", "mtime", "hash", "delta"],
                    "description": "How the core decides whether a file changed before acting. Default: mtime. hash/delta also request checksums when the provider supports them."
                },
                "exclude": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Glob patterns to exclude"
                },
                "files": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Restrict the sync to this exact set of relative paths (e.g. ['app/foo.ts', 'static/bar.css']). Paths are relative to local_dir/remote_dir. Maps the CLI --files-from flag: use it when the agent already knows which 15 files need syncing and wants to skip the full-tree scan cost."
                },
                "max_depth": { "type": "integer", "description": "Max recursion depth (default: 100)" },
                "summary_only": { "type": "boolean", "description": "Return only summary/planned counters and drop the per-entry plan/errors arrays. Use for large scopes where the full response would exceed MCP size limits. Default: false." }
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

/// Parse the optional `files` array into the `ScanOptions.files_from` set.
/// Empty or missing → `None` (no filter). Any non-string entry is skipped
/// silently — the agent will see the ones that actually matched reflected
/// in `scan_stats` / `summary`, and a typo on one entry does not kill the
/// whole tool call.
fn parse_files_from(args: &Value) -> Option<std::collections::HashSet<String>> {
    let arr = args.get("files").and_then(|v| v.as_array())?;
    if arr.is_empty() {
        return None;
    }
    let mut set = std::collections::HashSet::with_capacity(arr.len());
    for v in arr {
        if let Some(s) = v.as_str() {
            if !s.is_empty() {
                set.insert(s.to_string());
            }
        }
    }
    if set.is_empty() {
        None
    } else {
        Some(set)
    }
}

/// Gate `aeroftp_read_file` against directories and oversized files.
///
/// Only rejects files that exceed the hard in-memory cap
/// (`MAX_READ_PREVIEW_BYTES`). Files larger than the caller's `preview_bytes`
/// window but within the hard cap are accepted here and truncated downstream
/// with `truncated:true` so agents can get a tail-free preview without having
/// to retry.
fn validate_read_preview_target(
    is_dir: bool,
    size: u64,
    _preview_bytes: u64,
) -> Result<(), String> {
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

/// Collapse the `"[NNN] NNN ..."` doubled-status-code pattern that suppaftp
/// emits when concatenating its internal `[code]` tag with a server reply
/// that already begins with the same code. Scans the whole message (not just
/// the prefix) so prefixed wrappings like `"Transfer failed: Invalid
/// response: [553] 553 Can't open..."` are also collapsed. Leaves
/// non-matching strings untouched.
///
/// UTF-8 safe: only inspects ASCII bytes for the pattern and copies segments
/// of the original `&str` verbatim via index slicing so multi-byte characters
/// in the non-matching regions are preserved untouched.
fn collapse_duplicate_ftp_code(msg: &str) -> String {
    let bytes = msg.as_bytes();
    let n = bytes.len();
    // Minimum match length: "[NNN] NNN " = 10 bytes (all ASCII).
    if n < 10 {
        return msg.to_string();
    }
    let mut out = String::with_capacity(n);
    let mut last_emit = 0usize;
    let mut i = 0usize;
    while i + 10 <= n {
        let c = bytes[i];
        // Fast reject: only `[` is a candidate start. Safe on multi-byte
        // UTF-8 because `[` (0x5B) is never a continuation byte.
        if c != b'[' {
            i += 1;
            continue;
        }
        if bytes[i + 1].is_ascii_digit()
            && bytes[i + 2].is_ascii_digit()
            && bytes[i + 3].is_ascii_digit()
            && bytes[i + 4] == b']'
            && bytes[i + 5].is_ascii_whitespace()
            && bytes[i + 6].is_ascii_digit()
            && bytes[i + 7].is_ascii_digit()
            && bytes[i + 8].is_ascii_digit()
            && bytes[i + 9].is_ascii_whitespace()
            && bytes[i + 1] == bytes[i + 6]
            && bytes[i + 2] == bytes[i + 7]
            && bytes[i + 3] == bytes[i + 8]
        {
            // Flush everything before the match verbatim, then emit the
            // tail-side code + its trailing whitespace. Skip over the whole
            // 10-byte doubled pattern.
            out.push_str(&msg[last_emit..i]);
            out.push_str(&msg[i + 6..i + 10]);
            i += 10;
            last_emit = i;
            continue;
        }
        i += 1;
    }
    out.push_str(&msg[last_emit..]);
    out
}

/// Detect parent-missing errors across providers. FTP returns `553` (some
/// servers) or `550`; SFTP/WebDAV surface `ENOENT` or `No such file or
/// directory`. Match on substrings so we don't miss provider-specific wording.
fn looks_like_parent_missing_upload(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    let has_enoent = lower.contains("no such file or directory")
        || lower.contains("enoent")
        || lower.contains("does not exist");
    let has_ftp_code = lower.contains("553") || lower.contains("550");
    // "can't open that file" is the exact suppaftp wording for missing parent
    // on vsftpd/Pure-FTPd. Keep it as a standalone trigger because the FTP
    // code may have been stripped by upstream sanitization.
    let has_suppa = lower.contains("can't open that file");
    has_enoent || has_ftp_code || has_suppa
}

/// Wrap an upload failure with a hint that points at `create_parents=true`
/// when the failure smells like a missing parent directory. The hint mentions
/// the full remote_path rather than the specific missing component because
/// the server reply does not always identify which component is missing.
fn augment_upload_error(
    sanitized: &str,
    remote_path: &str,
    create_parents: bool,
    had_parent: bool,
) -> String {
    let collapsed = collapse_duplicate_ftp_code(sanitized);
    // Only augment when the agent did NOT ask for auto-mkdir AND there is a
    // parent to create. If `create_parents=true` already failed, the message
    // would be misleading — the mkdir itself blew up elsewhere.
    if !create_parents && had_parent && looks_like_parent_missing_upload(&collapsed) {
        format!(
            "Upload failed: parent directory does not exist for '{}'. Retry with create_parents=true to auto-mkdir. Server said: {}",
            remote_path, collapsed
        )
    } else {
        collapsed
    }
}

/// Maximum number of paths accepted by a single batch delete call. Hard-coded
/// here so both `aeroftp_delete` (batch branch) and `aeroftp_delete_many`
/// report an identical error message to the caller.
const DELETE_BATCH_MAX: usize = 100;

/// Maximum number of items accepted by a single batch upload call. Same
/// rationale as `DELETE_BATCH_MAX`: batches larger than this should be split
/// client-side so one failure does not cost the whole round-trip.
const UPLOAD_BATCH_MAX: usize = 100;

/// One parsed entry in the `aeroftp_upload_many` request. Parsed once up
/// front so the execution loop can operate on fully-validated structs.
#[derive(Debug, Clone)]
struct UploadBatchItem {
    local_path: String,
    remote_path: String,
    create_parents: bool,
    no_clobber: bool,
}

/// Shared implementation of `aeroftp_upload_many`. Mirrors
/// `dispatch_delete_batch` in shape: validation first, then a sequential
/// loop that records per-item results and aggregates a summary with
/// `totals` for uniform reporting.
///
/// Serial by design: FTP, SFTP, and WebDAV over HTTP/1.1 do not pipeline
/// uploads over a single control channel, and the pool holds one
/// connection per profile. Parallelism would require a per-profile pool
/// with multiple slots; that is a future refactor tracked in APPENDIX-08.
async fn dispatch_upload_batch(
    args: &Value,
    server: &str,
    pool: &ConnectionPool,
    notifier: Option<&McpNotifier>,
) -> (Value, bool) {
    let items_val = match args.get("items").and_then(|v| v.as_array()) {
        Some(a) => a.clone(),
        None => {
            return err(
                "'items' must be a non-empty array of {local_path, remote_path} objects".into(),
            );
        }
    };
    if items_val.is_empty() {
        return err("'items' must contain at least one entry".into());
    }
    if items_val.len() > UPLOAD_BATCH_MAX {
        return err(format!(
            "items exceeds max ({}): got {}",
            UPLOAD_BATCH_MAX,
            items_val.len()
        ));
    }

    // Parse + validate up front. Agents pass dozens of items; catching a
    // bad entry after 15 successful uploads wastes time.
    let mut items: Vec<UploadBatchItem> = Vec::with_capacity(items_val.len());
    for (idx, v) in items_val.iter().enumerate() {
        let obj = match v.as_object() {
            Some(o) => o,
            None => {
                return err(format!(
                    "items[{}] must be an object with local_path and remote_path",
                    idx
                ));
            }
        };
        let local = obj
            .get("local_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let remote = obj
            .get("remote_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if local.is_empty() || remote.is_empty() {
            return err(format!(
                "items[{}] must have non-empty local_path and remote_path",
                idx
            ));
        }
        if let Err(e) = security::validate_local_path(&local) {
            return err(format!("items[{}]: {}", idx, e));
        }
        if let Err(e) = security::validate_remote_path(&remote) {
            return err(format!("items[{}]: {}", idx, e));
        }
        let cp = obj
            .get("create_parents")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        // Per-item no_clobber can be overridden; if absent it falls back to
        // the request-level `no_clobber` default (if any).
        let nc_default = get_bool_opt(args, "no_clobber").unwrap_or(false);
        let nc = obj
            .get("no_clobber")
            .and_then(|v| v.as_bool())
            .unwrap_or(nc_default);
        items.push(UploadBatchItem {
            local_path: local,
            remote_path: remote,
            create_parents: cp,
            no_clobber: nc,
        });
    }

    let continue_on_error = get_bool_opt(args, "continue_on_error").unwrap_or(true);
    // Default 0 ms — batch upload rarely needs the anti-rate-limit pacing
    // that delete needs, because uploads already take seconds each. Keep
    // the knob for cases where an agent wants to gentle-walk a rate-limited
    // provider.
    let delay_ms = args
        .get("delay_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        .min(2_000);

    let mut results: Vec<Value> = Vec::with_capacity(items.len());
    let mut uploaded_ok: u32 = 0;
    let mut skipped_ok: u32 = 0;
    let mut errors: u32 = 0;
    let mut aborted_after: Option<usize> = None;
    let mut bytes_total: u64 = 0;

    for (idx, item) in items.iter().enumerate() {
        if idx > 0 && delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        }

        // no_clobber short-circuit per-item. stat probe; `Ok` means the
        // file exists and we skip the upload. `Err` assumes not-found and
        // proceeds — same semantics as the single-file path.
        if item.no_clobber {
            let probe_path = item.remote_path.clone();
            let exists = execute_with_reset(pool, server, move |p| {
                let path = probe_path.clone();
                Box::pin(async move { p.stat(&path).await })
            })
            .await
            .is_ok();
            if exists {
                skipped_ok += 1;
                results.push(json!({
                    "local_path": item.local_path,
                    "remote_path": item.remote_path,
                    "uploaded": false,
                    "skipped": true,
                    "reason": "exists",
                    "no_clobber": true,
                }));
                continue;
            }
        }

        // Create parents up front (if requested) so the upload itself only
        // needs one shot. Mirrors the single-file aeroftp_upload_file flow.
        if item.create_parents {
            if let Some(parent) = parent_remote_dir(&item.remote_path) {
                if let Err(e) = ensure_remote_parents(pool, server, &parent).await {
                    errors += 1;
                    let err_obj = json!({ "message": e.clone(), "path": item.remote_path });
                    results.push(json!({
                        "local_path": item.local_path,
                        "remote_path": item.remote_path,
                        "uploaded": false,
                        "error": e,
                        "errors": [err_obj],
                    }));
                    if !continue_on_error {
                        aborted_after = Some(idx + 1);
                        break;
                    }
                    continue;
                }
            }
        }

        // Retry loop with the same contract as the single-file upload path:
        // up to MAX_UPLOAD_ATTEMPTS tries on transport-level errors, each
        // attempt gets a freshly built progress callback.
        let mut attempt: u8 = 0;
        let outcome = loop {
            attempt += 1;
            let arc = match pool.get_provider(server).await {
                Ok(a) => a,
                Err(e) => break Err((e, attempt)),
            };
            let bytes = std::fs::metadata(&item.local_path)
                .map(|m| m.len())
                .unwrap_or(0);
            let upload_result = {
                let mut p = arc.lock().await;
                let cb = build_progress_callback(notifier, "upload");
                p.upload(&item.local_path, &item.remote_path, cb).await
            };
            match upload_result {
                Ok(()) => break Ok((bytes, attempt)),
                Err(e) => {
                    let transport = is_transport_error(&e);
                    let sanitized = sanitize_error(e);
                    drop(arc);
                    if transport {
                        let _ = pool.invalidate(server).await;
                        if attempt < MAX_UPLOAD_ATTEMPTS {
                            continue;
                        }
                    }
                    break Err((
                        augment_upload_error(
                            &sanitized,
                            &item.remote_path,
                            item.create_parents,
                            parent_remote_dir(&item.remote_path).is_some(),
                        ),
                        attempt,
                    ));
                }
            }
        };

        match outcome {
            Ok((bytes, attempts)) => {
                uploaded_ok += 1;
                bytes_total = bytes_total.saturating_add(bytes);
                results.push(json!({
                    "local_path": item.local_path,
                    "remote_path": item.remote_path,
                    "uploaded": true,
                    "bytes": bytes,
                    "attempts": attempts,
                }));
            }
            Err((msg, attempts)) => {
                errors += 1;
                let err_obj = json!({ "message": msg.clone(), "path": item.remote_path });
                results.push(json!({
                    "local_path": item.local_path,
                    "remote_path": item.remote_path,
                    "uploaded": false,
                    "error": msg,
                    "errors": [err_obj],
                    "attempts": attempts,
                }));
                if !continue_on_error {
                    aborted_after = Some(idx + 1);
                    break;
                }
            }
        }
    }

    let total_planned = items.len();
    let processed = results.len();
    let errors_array: Vec<Value> = results
        .iter()
        .filter_map(|r| {
            let msg = r.get("error").and_then(|v| v.as_str())?;
            let path = r
                .get("remote_path")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            Some(json!({ "message": msg, "path": path }))
        })
        .collect();
    ok(json!({
        "server": server,
        "results": results,
        "summary": {
            "planned": total_planned,
            "processed": processed,
            "uploaded": uploaded_ok,
            "skipped": skipped_ok,
            "errors": errors,
            "aborted_after": aborted_after,
            "delay_ms": delay_ms,
            "bytes_uploaded": bytes_total,
            "totals": {
                "requested": total_planned,
                "succeeded": uploaded_ok,
                "failed": errors,
                "skipped": skipped_ok,
                "elapsed_secs": 0u64,
            },
        },
        "errors": errors_array,
    }))
}

/// Maximum retry attempts for single-file upload and batch upload paths.
/// Rationale — suppaftp can leave a pooled FTP control channel with a
/// half-open data connection after a successful STOR on some servers
/// (Aruba FTPS, vsftpd under load). The NEXT op on that connection hits
/// "Data connection is already open" even though the previous op succeeded.
/// Three attempts cover the full recovery cycle: initial fail → pool
/// invalidate → fresh connection → retry → success.
const MAX_UPLOAD_ATTEMPTS: u8 = 3;

/// Shared implementation of the batch delete branch. Called from both
/// `aeroftp_delete` (when `paths` is provided) and `aeroftp_delete_many`.
///
/// Returns `(json_payload, is_error)` in the same shape `execute_tool`
/// expects, so callers only need to wrap it with `finish()`. Validation of
/// `server` is the caller's responsibility — the dispatcher assumes it has
/// already been performed.
async fn dispatch_delete_batch(args: &Value, server: &str, pool: &ConnectionPool) -> (Value, bool) {
    let paths_val = match args.get("paths").and_then(|v| v.as_array()) {
        Some(a) => a.clone(),
        None => {
            return err("'paths' must be a non-empty array of strings".into());
        }
    };
    if paths_val.is_empty() {
        return err("'paths' must contain at least one entry".into());
    }
    if paths_val.len() > DELETE_BATCH_MAX {
        return err(format!(
            "paths exceeds max ({}): got {}",
            DELETE_BATCH_MAX,
            paths_val.len()
        ));
    }
    let mut paths: Vec<String> = Vec::with_capacity(paths_val.len());
    for v in paths_val {
        match v.as_str() {
            Some(s) if !s.is_empty() => paths.push(s.to_string()),
            _ => {
                return err("every entry of 'paths' must be a non-empty string".into());
            }
        }
    }
    for p in &paths {
        if let Err(e) = security::validate_remote_path(p) {
            return err(e);
        }
    }
    let recursive = get_bool_opt(args, "recursive").unwrap_or(false);
    let continue_on_error = get_bool_opt(args, "continue_on_error").unwrap_or(true);
    // Default 200 ms — matches the rhythm Lumo had to simulate with external
    // sleeps. Cap at 2 s so an agent cannot stall the tool indefinitely by
    // passing a huge value.
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
        let outcome = execute_with_reset(pool, server, move |p| {
            let path = path_for_call.clone();
            Box::pin(async move {
                let entry = p.stat(&path).await?;
                match delete_kind(entry.is_dir, recursive) {
                    DeleteKind::Directory => p.rmdir(&path).await.map(|_| true),
                    DeleteKind::DirectoryRecursive => p.rmdir_recursive(&path).await.map(|_| true),
                    DeleteKind::File => p.delete(&path).await.map(|_| false),
                }
            })
        })
        .await;
        match outcome {
            Ok(is_dir) => {
                deleted_ok += 1;
                // No per-item `recursive` field: it is an input knob applied
                // globally to the batch, not a property of each result. The
                // caller already knows what they passed; echoing it back would
                // be noise and mislead consumers into thinking it's a per-item
                // outcome.
                results.push(json!({
                    "path": path,
                    "deleted": true,
                    "is_dir": is_dir,
                }));
            }
            Err(msg) => {
                errors += 1;
                let err_obj = json!({ "message": msg.clone(), "path": path });
                results.push(json!({
                    "path": path,
                    "deleted": false,
                    "error": msg,
                    "errors": [err_obj],
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
    // Collect error messages for the aggregate envelope. The per-item
    // entries keep the detail; this mirror is a convenience for consumers
    // that want to check `errors.length > 0` at the top level.
    let errors_array: Vec<Value> = results
        .iter()
        .filter_map(|r| {
            let msg = r.get("error").and_then(|v| v.as_str())?;
            let path = r.get("path").and_then(|v| v.as_str()).unwrap_or_default();
            Some(json!({ "message": msg, "path": path }))
        })
        .collect();
    ok(json!({
        "server": server,
        "results": results,
        "summary": {
            "planned": total_planned,
            "processed": processed,
            "deleted": deleted_ok,
            "errors": errors,
            "aborted_after": aborted_after,
            "delay_ms": delay_ms,
            "recursive": recursive,
            "totals": {
                "requested": total_planned,
                "succeeded": deleted_ok,
                "failed": errors,
                "skipped": 0u32,
                "elapsed_secs": 0u64,
            },
        },
        "errors": errors_array,
    }))
}

/// Group a flat sync plan into per-operation buckets (`upload`, `download`,
/// `delete`, `skip`). Each bucket is capped at `per_op_cap` so an agent
/// inspecting one type of op gets every candidate of that type, not just
/// the early-alphabet sample that flat truncation used to return.
///
/// Returns `(by_op, truncated_flags, totals)` — three JSON objects keyed by
/// the same bucket names. `totals` reports the uncapped count per bucket so
/// the agent can tell a 300-upload plan from a 3-upload one even when both
/// come back with 250 visible entries.
fn build_plan_by_op(
    plan: &[PlanEntry],
    per_op_cap: usize,
) -> (
    serde_json::Map<String, Value>,
    serde_json::Map<String, Value>,
    serde_json::Map<String, Value>,
) {
    let mut uploads: Vec<Value> = Vec::new();
    let mut downloads: Vec<Value> = Vec::new();
    let mut deletes: Vec<Value> = Vec::new();
    let mut skips: Vec<Value> = Vec::new();
    let mut total_uploads = 0u32;
    let mut total_downloads = 0u32;
    let mut total_deletes = 0u32;
    let mut total_skips = 0u32;

    for p in plan {
        let (bucket, total) = match p.op.as_str() {
            "upload" => (&mut uploads, &mut total_uploads),
            "download" => (&mut downloads, &mut total_downloads),
            "delete_remote" | "delete_local" => (&mut deletes, &mut total_deletes),
            _ => (&mut skips, &mut total_skips),
        };
        *total += 1;
        if bucket.len() < per_op_cap {
            bucket.push(json!({
                "op": p.op,
                "path": p.path,
                "reason": p.reason,
                "bytes": p.bytes,
                "decision_policy": p.decision_policy,
            }));
        }
    }

    let mut by_op = serde_json::Map::new();
    by_op.insert("upload".to_string(), Value::Array(uploads));
    by_op.insert("download".to_string(), Value::Array(downloads));
    by_op.insert("delete".to_string(), Value::Array(deletes));
    by_op.insert("skip".to_string(), Value::Array(skips));

    let mut totals = serde_json::Map::new();
    totals.insert("upload".to_string(), json!(total_uploads));
    totals.insert("download".to_string(), json!(total_downloads));
    totals.insert("delete".to_string(), json!(total_deletes));
    totals.insert("skip".to_string(), json!(total_skips));

    let mut truncated = serde_json::Map::new();
    truncated.insert(
        "upload".to_string(),
        json!(total_uploads as usize > per_op_cap),
    );
    truncated.insert(
        "download".to_string(),
        json!(total_downloads as usize > per_op_cap),
    );
    truncated.insert(
        "delete".to_string(),
        json!(total_deletes as usize > per_op_cap),
    );
    truncated.insert("skip".to_string(), json!(total_skips as usize > per_op_cap));

    (by_op, truncated, totals)
}

/// Group the rel_path of a `DiffEntry` into a bucket key based on the first
/// `depth` path segments. `_root` is reserved for entries that have no
/// component before the last segment (e.g. `README.md`) — falling through
/// to the root prevents them from colliding with a directory literally
/// named empty-string.
fn top_level_bucket(rel_path: &str, depth: usize) -> String {
    if depth == 0 {
        return "_root".to_string();
    }
    let trimmed = rel_path.trim_start_matches('/');
    let parts: Vec<&str> = trimmed.split('/').filter(|p| !p.is_empty()).collect();
    // The final segment is the file name — ignore it. Anything with no
    // parent directory is `_root`.
    if parts.len() <= 1 {
        return "_root".to_string();
    }
    let dirs_available = parts.len() - 1;
    let take = depth.min(dirs_available);
    if take == 0 {
        return "_root".to_string();
    }
    parts[..take].join("/")
}

/// Aggregate a `DiffReport` into per-bucket counters grouped by the first
/// `depth` path segments. Returned as a JSON object keyed by bucket name
/// so the agent can drill down surgically instead of running multiple
/// scoped `check_tree` calls.
fn aggregate_by_top_level(
    diff: &crate::sync_core::DiffReport,
    depth: usize,
) -> serde_json::Map<String, Value> {
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Bucket {
        match_count: u32,
        differ: u32,
        missing_local: u32,
        missing_remote: u32,
    }
    // BTreeMap for deterministic output order (alphabetical) — agents do
    // structural diffs against this response and non-stable keys would
    // break caching.
    let mut buckets: BTreeMap<String, Bucket> = BTreeMap::new();
    let bump = |buckets: &mut BTreeMap<String, Bucket>,
                entry: &crate::sync_core::DiffEntry,
                pick: fn(&mut Bucket)| {
        let key = top_level_bucket(&entry.rel_path, depth);
        let b = buckets.entry(key).or_default();
        pick(b);
    };
    for e in &diff.matches {
        bump(&mut buckets, e, |b| b.match_count += 1);
    }
    for e in &diff.differ {
        bump(&mut buckets, e, |b| b.differ += 1);
    }
    for e in &diff.missing_local {
        bump(&mut buckets, e, |b| b.missing_local += 1);
    }
    for e in &diff.missing_remote {
        bump(&mut buckets, e, |b| b.missing_remote += 1);
    }
    let mut out = serde_json::Map::new();
    for (k, v) in buckets {
        out.insert(
            k,
            json!({
                "match": v.match_count,
                "differ": v.differ,
                "missing_local": v.missing_local,
                "missing_remote": v.missing_remote,
            }),
        );
    }
    out
}

/// Linux-only probe for the "executable file has been deleted since this
/// process started" condition. The kernel appends " (deleted)" to the
/// target of `/proc/self/exe` when the on-disk file no longer exists —
/// typically because the agent rebuilt the binary while an older MCP
/// process was still running. Agents that see `binary_deleted=true` know
/// the MCP they are talking to is stale and should be restarted.
///
/// Returns `None` on non-Linux (no reliable equivalent) and on read errors
/// so the response can omit the field rather than lie.
#[allow(unused_variables)]
fn detect_binary_deleted() -> Option<bool> {
    #[cfg(target_os = "linux")]
    {
        match std::fs::read_link("/proc/self/exe") {
            Ok(target) => Some(target.to_string_lossy().ends_with(" (deleted)")),
            Err(_) => None,
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Build the payload returned by `aeroftp_mcp_info`. Kept as a standalone
/// function so unit tests can exercise the field shape without going
/// through the full tool dispatch.
fn build_mcp_info() -> Value {
    let pid = std::process::id();
    let now = chrono::Utc::now();
    let started_at = *MCP_START;
    let uptime_secs = (now - started_at).num_seconds().max(0);
    let version = env!("CARGO_PKG_VERSION");

    // Binary metadata is best-effort: if `current_exe()` or `metadata`
    // fails (e.g. restricted fs), we emit `null` for that field rather
    // than failing the whole call.
    let (binary_path, binary_mtime, binary_size) = match std::env::current_exe() {
        Ok(path) => {
            let path_str = path.to_string_lossy().into_owned();
            let (mtime, size) = match std::fs::metadata(&path) {
                Ok(meta) => {
                    let mtime_iso = meta.modified().ok().map(|t| {
                        let dt: chrono::DateTime<chrono::Utc> = t.into();
                        dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                    });
                    (mtime_iso, Some(meta.len()))
                }
                Err(_) => (None, None),
            };
            (Some(path_str), mtime, size)
        }
        Err(_) => (None, None, None),
    };

    let binary_deleted = detect_binary_deleted();

    let mut payload = json!({
        "pid": pid,
        "version": version,
        "uptime_secs": uptime_secs,
        "started_at": started_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    });
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("binary_path".to_string(), json!(binary_path));
        obj.insert("binary_mtime".to_string(), json!(binary_mtime));
        obj.insert("binary_size".to_string(), json!(binary_size));
        // Only emit `binary_deleted` on platforms where the probe is
        // meaningful. Omit on macOS/Windows so clients don't misread a
        // `null` as "definitely not deleted".
        if let Some(deleted) = binary_deleted {
            obj.insert("binary_deleted".to_string(), json!(deleted));
        }
    }
    payload
}

/// Pure find/replace applied to the file text of `aeroftp_edit`.
///
/// Returns `(new_content, replacements)`. Uses literal-string semantics
/// (not regex) to match what the CLI does. When `first_only` is true, at
/// most one replacement is performed.
///
/// Callers are responsible for deciding whether to re-upload: `replacements
/// == 0` means the file is byte-identical to the input and there is no
/// point spending a round-trip on a no-op upload.
fn apply_replacements(
    original: &str,
    find: &str,
    replace: &str,
    first_only: bool,
) -> (String, u32) {
    if find.is_empty() {
        return (original.to_string(), 0);
    }
    if first_only {
        if let Some(idx) = original.find(find) {
            let mut out = String::with_capacity(original.len() + replace.len());
            out.push_str(&original[..idx]);
            out.push_str(replace);
            out.push_str(&original[idx + find.len()..]);
            (out, 1)
        } else {
            (original.to_string(), 0)
        }
    } else {
        let count = original.matches(find).count() as u32;
        if count == 0 {
            (original.to_string(), 0)
        } else {
            (original.replace(find, replace), count)
        }
    }
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
///
/// Emits BOTH the legacy scalar `error` field (for back-compat with existing
/// consumers that read `result.error`) AND a uniform `errors` array so
/// generic reporting code can treat every tool identically — one-element
/// array on single failure, longer on aggregate failures. The element shape
/// is `{message, code?, path?}` as agreed for the uniform envelope.
fn err(msg: String) -> (Value, bool) {
    let errors = json!([{ "message": msg.clone() }]);
    (json!({ "error": msg, "errors": errors }), true)
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
    pool: &Arc<ConnectionPool>,
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

    // T3 Gate 2 Area C: prefer the unified core dispatcher for the remote
    // tools that have been migrated. Advanced MCP-only tools still fall
    // through to the mature legacy match below.
    {
        let ctx = crate::ai_core::mcp_impl::McpToolCtx::new(Arc::clone(pool), notifier.cloned());
        match crate::ai_core::tools::dispatch_tool(&ctx, tool_name, args).await {
            Ok(value) => return finish(tool_name, None, None, ok(value), start),
            Err(crate::ai_core::tools::ToolError::Unknown(_))
            | Err(crate::ai_core::tools::ToolError::NotMigrated(_)) => {}
            Err(e) => return finish(tool_name, None, None, err(e.to_string()), start),
        }
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
            let no_clobber = get_bool_opt(args, "no_clobber").unwrap_or(false);
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

            // no_clobber short-circuit. Use `stat` as a liveness probe; if it
            // succeeds, the file exists and we abort before touching the
            // upload machinery. `stat` errors are ignored — they almost
            // always mean "not found", which is exactly the case we want to
            // proceed. NotSupported providers fall through to an attempted
            // upload, which is the same behavior a CLI `--no-clobber` gets.
            if no_clobber {
                let probe_path = remote_path.clone();
                let exists = execute_with_reset(pool, &server, move |p| {
                    let path = probe_path.clone();
                    Box::pin(async move { p.stat(&path).await })
                })
                .await
                .is_ok();
                if exists {
                    return finish(
                        tool_name,
                        Some(&server),
                        Some(&remote_path),
                        ok(json!({
                            "remote_path": remote_path,
                            "uploaded": false,
                            "skipped": true,
                            "reason": "exists",
                            "no_clobber": true,
                        })),
                        start,
                    );
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

            let had_parent = parent_remote_dir(&remote_path).is_some();

            // Resolve the local source. Inline text content is materialized
            // into a tmp file once here so the retry loop below uses the same
            // path on each attempt. tmp is cleaned up unconditionally at the
            // end of the tool dispatch.
            let (upload_src, declared_bytes, tmp_cleanup): (
                String,
                u64,
                Option<std::path::PathBuf>,
            ) = if let Some(ref lp) = local_path {
                let b = std::fs::metadata(lp).map(|m| m.len()).unwrap_or(0);
                (lp.clone(), b, None)
            } else {
                let text = content.as_deref().unwrap_or_default();
                let b = text.len() as u64;
                let nonce = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                let tmp = std::env::temp_dir().join(format!("aeroftp_mcp_upload_{}", nonce));
                if let Err(e) = std::fs::write(&tmp, text) {
                    return finish(
                        tool_name,
                        Some(&server),
                        Some(&remote_path),
                        err(sanitize_error(e)),
                        start,
                    );
                }
                (tmp.to_string_lossy().into_owned(), b, Some(tmp))
            };
            let has_progress = local_path.is_some();

            // Retry loop: up to MAX_UPLOAD_ATTEMPTS tries on transport-level
            // errors. Rationale — suppaftp can leave a pooled FTP control
            // channel with a half-open data connection after a successful
            // STOR on some servers (Aruba FTPS, vsftpd under load). The
            // NEXT op on that connection hits "Data connection is already
            // open" even though the previous op succeeded. Without this
            // retry the failure surfaces to the agent as a hard error; with
            // it we transparently open a fresh connection and the second
            // attempt succeeds.
            let mut attempt: u8 = 0;
            let result = loop {
                attempt += 1;
                let arc = match pool.get_provider(&server).await {
                    Ok(a) => a,
                    Err(e) => break err(e),
                };
                let upload_result = {
                    let mut p = arc.lock().await;
                    let cb = if has_progress {
                        build_progress_callback(notifier, "upload")
                    } else {
                        None
                    };
                    p.upload(&upload_src, &remote_path, cb).await
                };
                match upload_result {
                    Ok(()) => {
                        if has_progress {
                            if let Some(n) = notifier {
                                n.send_progress_final(
                                    declared_bytes,
                                    Some(declared_bytes),
                                    Some(format!("upload complete: {} bytes", declared_bytes)),
                                )
                                .await;
                            }
                        }
                        break ok(json!({
                            "remote_path": remote_path,
                            "uploaded": true,
                            "bytes": declared_bytes,
                            "attempts": attempt,
                        }));
                    }
                    Err(e) => {
                        let transport = is_transport_error(&e);
                        let sanitized = sanitize_error(e);
                        drop(arc);
                        if transport {
                            let _ = pool.invalidate(&server).await;
                            if attempt < MAX_UPLOAD_ATTEMPTS {
                                // Try again on a freshly-opened connection.
                                continue;
                            }
                        }
                        break err(augment_upload_error(
                            &sanitized,
                            &remote_path,
                            create_parents,
                            had_parent,
                        ));
                    }
                }
            };
            if let Some(tmp) = tmp_cleanup {
                let _ = std::fs::remove_file(&tmp);
            }
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
            if let Err(e) = security::validate_server_query(&server) {
                return finish(tool_name, Some(&server), None, err(e), start);
            }
            // Batch mode when `paths` is provided. `path` and `paths` are
            // mutually exclusive — rejecting combined input prevents ambiguity
            // about which argument wins.
            let has_paths = args.get("paths").is_some();
            let has_path = args.get("path").is_some();
            if has_paths && has_path {
                return finish(
                    tool_name,
                    Some(&server),
                    None,
                    err(
                        "Provide either 'path' (single delete) or 'paths' (batch) — not both"
                            .into(),
                    ),
                    start,
                );
            }
            if has_paths {
                let result = dispatch_delete_batch(args, &server, pool).await;
                return finish(tool_name, Some(&server), None, result, start);
            }

            let path = match get_str(args, "path") {
                Ok(s) => s,
                Err(_) => return finish(
                    tool_name,
                    Some(&server),
                    None,
                    err(
                        "Provide either 'path' (single delete) or 'paths' (batch array of strings)"
                            .into(),
                    ),
                    start,
                ),
            };
            let recursive = get_bool_opt(args, "recursive").unwrap_or(false);
            if let Err(e) = security::validate_remote_path(&path) {
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
            // Thin alias kept for back-compat. All logic lives in the shared
            // batch dispatcher so the two tools never drift.
            let result = dispatch_delete_batch(args, &server, pool).await;
            finish(tool_name, Some(&server), None, result, start)
        }

        "aeroftp_upload_many" => {
            let server = match get_str(args, "server") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, None, None, err(e), start),
            };
            if let Err(e) = security::validate_server_query(&server) {
                return finish(tool_name, Some(&server), None, err(e), start);
            }
            let result = dispatch_upload_batch(args, &server, pool, notifier).await;
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

        "aeroftp_edit" => {
            let server = match get_str(args, "server") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, None, None, err(e), start),
            };
            let path = match get_str(args, "path") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, Some(&server), None, err(e), start),
            };
            let find = match get_str(args, "find") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, Some(&server), Some(&path), err(e), start),
            };
            // `replace` is a required STRING but may be empty (delete-match
            // semantics). `get_str` rejects empty-missing distinction, so we
            // accept a present-but-empty value explicitly here.
            let replace = match args.get("replace").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => {
                    return finish(
                        tool_name,
                        Some(&server),
                        Some(&path),
                        err("Missing required argument: replace".into()),
                        start,
                    )
                }
            };
            let first_only = get_bool_opt(args, "first").unwrap_or(false);
            if let Err(e) = validate_sp(&server, Some(&path)) {
                return finish(tool_name, Some(&server), Some(&path), err(e), start);
            }
            if find.is_empty() {
                return finish(
                    tool_name,
                    Some(&server),
                    Some(&path),
                    err("`find` must not be empty".into()),
                    start,
                );
            }
            if let Err(e) = security::validate_text_content(&find) {
                return finish(tool_name, Some(&server), Some(&path), err(e), start);
            }
            if let Err(e) = security::validate_text_content(&replace) {
                return finish(tool_name, Some(&server), Some(&path), err(e), start);
            }

            // Stat first so we can reject directories and oversize files
            // without paying for an unnecessary download.
            let path_for_stat = path.clone();
            let stat_result = execute_with_reset(pool, &server, move |p| {
                let path = path_for_stat.clone();
                Box::pin(async move { p.stat(&path).await })
            })
            .await;
            let result = match stat_result {
                Err(e) => err(e),
                Ok(entry) => {
                    if entry.is_dir {
                        err("Cannot edit a directory.".into())
                    } else if entry.size > MAX_EDIT_BYTES {
                        err(format!(
                            "File too large for in-place edit ({:.1} KB). Hard cap: {} KB. Download, edit locally, then upload.",
                            entry.size as f64 / 1024.0,
                            MAX_EDIT_BYTES / 1024,
                        ))
                    } else {
                        // Fetch → UTF-8 decode → apply. `apply_replacements`
                        // is a pure function; the surrounding I/O is the
                        // only side-effect path.
                        let path_for_dl = path.clone();
                        match execute_with_reset(pool, &server, move |p| {
                            let path = path_for_dl.clone();
                            Box::pin(async move { p.download_to_bytes(&path).await })
                        })
                        .await
                        {
                            Err(e) => err(e),
                            Ok(data) => match String::from_utf8(data) {
                                Err(_) => err(
                                    "File is not valid UTF-8. aeroftp_edit operates only on text files; use aeroftp_download_file + aeroftp_upload_file for binary content.".into(),
                                ),
                                Ok(original) => {
                                    let bytes_before = original.len() as u64;
                                    let (new_content, replacements) =
                                        apply_replacements(&original, &find, &replace, first_only);
                                    if replacements == 0 {
                                        // No-op: do NOT re-upload. Saves a
                                        // round-trip and avoids churning
                                        // mtime on a file that did not need
                                        // to change.
                                        ok(json!({
                                            "path": path,
                                            "find": find,
                                            "replacements": 0u32,
                                            "modified": false,
                                            "bytes_before": bytes_before,
                                            "bytes_after": bytes_before,
                                        }))
                                    } else {
                                        let bytes_after = new_content.len() as u64;
                                        // Write to a tmp file and upload.
                                        // Matches the path used by the
                                        // `content`-mode branch of
                                        // aeroftp_upload_file — shared
                                        // disk-backed upload keeps the
                                        // surface uniform.
                                        let nonce = std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .map(|d| d.as_nanos())
                                            .unwrap_or(0);
                                        let tmp = std::env::temp_dir()
                                            .join(format!("aeroftp_mcp_edit_{}", nonce));
                                        if let Err(e) = std::fs::write(&tmp, &new_content) {
                                            err(sanitize_error(e))
                                        } else {
                                            let tmp_str = tmp.to_string_lossy().into_owned();
                                            let up_result = match pool.get_provider(&server).await {
                                                Err(e) => Err(e),
                                                Ok(arc) => {
                                                    let res = {
                                                        let mut p = arc.lock().await;
                                                        p.upload(&tmp_str, &path, None)
                                                            .await
                                                            .map_err(sanitize_error)
                                                    };
                                                    drop(arc);
                                                    res
                                                }
                                            };
                                            let _ = std::fs::remove_file(&tmp);
                                            match up_result {
                                                Err(e) => err(e),
                                                Ok(()) => ok(json!({
                                                    "path": path,
                                                    "find": find,
                                                    "replacements": replacements,
                                                    "modified": true,
                                                    "bytes_before": bytes_before,
                                                    "bytes_after": bytes_after,
                                                })),
                                            }
                                        }
                                    }
                                }
                            },
                        }
                    }
                }
            };
            finish(tool_name, Some(&server), Some(&path), result, start)
        }

        "aeroftp_mcp_info" => {
            // No server/path validation: this tool has no external I/O.
            let result = ok(build_mcp_info());
            finish(tool_name, None, None, result, start)
        }

        "aeroftp_close_connection" => {
            let server = match get_str(args, "server") {
                Ok(s) => s,
                Err(e) => return finish(tool_name, None, None, err(e), start),
            };
            if let Err(e) = security::validate_server_query(&server) {
                return finish(tool_name, Some(&server), None, err(e), start);
            }
            // The response distinguishes three states an agent cares about:
            //   - was_active=true,  closed=true  → there was a pooled entry
            //     and we successfully tore it down
            //   - was_active=false, closed=false → noop: nothing matched the
            //     query. This is NOT a failure, just "nothing to do"
            //   - (reserved) was_active=true,  closed=false → we matched an
            //     entry but failed to release it — currently unreachable with
            //     the fire-and-forget disconnect path, but the shape is in
            //     place for future expansion
            // `released: true` is a uniform success flag so generic reporting
            // code can check a single field ("the operation completed, no
            // agent action needed"). `closed` is kept for back-compat.
            let result = match pool.close_one(&server).await {
                Some(name) => ok(json!({
                    "server": server,
                    "closed": true,
                    "was_active": true,
                    "released": true,
                    "name": name,
                })),
                None => ok(json!({
                    "server": server,
                    "closed": false,
                    "was_active": false,
                    "released": true,
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
            let summary_only = get_bool_opt(args, "summary_only").unwrap_or(false);
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
            let group_by_depth = args
                .get("group_by_depth")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize)
                .unwrap_or(0);
            let files_from = parse_files_from(args);

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
                files_from,
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
                    if summary_only {
                        // Skip the per-entry arrays entirely. Agents call
                        // check_tree on large scopes (thousands of files) just
                        // to get the drift counters; the arrays push the
                        // response past the MCP harness's max-tokens limit and
                        // the whole call aborts. `summary_only` keeps the
                        // useful counters and drops the noisy payload.
                        let mut summary = json!({
                            "match": diff.match_count(),
                            "differ": diff.differ_count(),
                            "missing_local": diff.missing_local_count(),
                            "missing_remote": diff.missing_remote_count(),
                        });
                        if group_by_depth > 0 {
                            if let Some(obj) = summary.as_object_mut() {
                                obj.insert(
                                    "by_top_level_dir".to_string(),
                                    Value::Object(aggregate_by_top_level(&diff, group_by_depth)),
                                );
                            }
                        }
                        ok(json!({
                            "server": server,
                            "local_dir": local_dir,
                            "remote_dir": remote_dir,
                            "checksum_requested": checksum,
                            "checksum_remote_supported": supports_remote_checksum,
                            "summary_only": true,
                            "summary": summary,
                            "has_differences": diff.has_differences(),
                        }))
                    } else {
                        let entries_to_json =
                            |entries: &[crate::sync_core::DiffEntry]| -> Vec<Value> {
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
                        let mut summary = json!({
                            "match": diff.match_count(),
                            "differ": diff.differ_count(),
                            "missing_local": diff.missing_local_count(),
                            "missing_remote": diff.missing_remote_count(),
                        });
                        if group_by_depth > 0 {
                            if let Some(obj) = summary.as_object_mut() {
                                obj.insert(
                                    "by_top_level_dir".to_string(),
                                    Value::Object(aggregate_by_top_level(&diff, group_by_depth)),
                                );
                            }
                        }
                        ok(json!({
                            "server": server,
                            "local_dir": local_dir,
                            "remote_dir": remote_dir,
                            "checksum_requested": checksum,
                            "checksum_remote_supported": supports_remote_checksum,
                            "summary": summary,
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
                }
            };
            finish(tool_name, Some(&server), Some(&remote_dir), result, start)
        }

        "aeroftp_sync_tree" => {
            use crate::sync_core::{
                sync_tree_core, ConflictMode, DeltaPolicy, ScanOptions, SyncDirection, SyncOptions,
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
            let delta_policy_raw = get_str_opt(args, "delta_policy").unwrap_or_default();
            let delta_policy = if delta_policy_raw.is_empty() {
                DeltaPolicy::default()
            } else {
                match DeltaPolicy::parse(&delta_policy_raw) {
                    Some(policy) => policy,
                    None => {
                        return finish(
                            tool_name,
                            Some(&server),
                            Some(&remote_dir),
                            err(format!(
                                "Invalid delta_policy '{}': expected disabled, size_only, mtime, hash, or delta",
                                delta_policy_raw
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
            let summary_only = get_bool_opt(args, "summary_only").unwrap_or(false);
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
                delta_policy,
                dry_run,
                delete_orphans,
                conflict_mode,
                scan: ScanOptions {
                    exclude_patterns: exclude,
                    files_from: parse_files_from(args),
                    max_depth,
                    compute_checksum: delta_policy.wants_checksums(),
                    compute_remote_checksum: delta_policy.wants_checksums(),
                    ..Default::default()
                },
            };

            // Entry-time pool invalidate. Wave-1 added an end-of-run
            // invalidate so the NEXT tool call would start fresh; wave-2
            // stress testing showed that wasn't enough for the "preview
            // then apply" flow (sync_tree dry_run=true → sync_tree
            // dry_run=false). Even though the first run ends clean, the
            // second run's internal LIST→STOR sequence can still fail
            // with "Data connection is already open" if the underlying
            // session carries any residual state. Paying one extra
            // reconnect on every sync_tree entry guarantees the core
            // operates on a fresh control channel regardless of what
            // preceded it. See APPENDIX-08 §Gap 7 for the repro.
            let _ = pool.invalidate(&server).await;

            let result = match pool.get_provider(&server).await {
                Err(e) => err(e),
                Ok(arc) => {
                    let mut p = arc.lock().await;
                    let mut sink = NotifierSyncSink::new(notifier, dry_run);
                    sink.emit_started(&direction_raw, dry_run).await;
                    let report =
                        sync_tree_core(&mut p, &local_dir, &remote_dir, &opts, &mut sink).await;
                    sink.emit_finished(&report).await;
                    // Uniform error envelope: `errors: [{message, path?, operation?}]`.
                    // Also expose a lightweight `error` scalar for back-compat with
                    // consumers that treat the old single-string shape.
                    // When summary_only=true, drop the array entirely — the
                    // `summary.errors` counter is preserved so drift counts
                    // stay visible.
                    let errors: Vec<Value> = if summary_only {
                        Vec::new()
                    } else {
                        report
                            .errors
                            .iter()
                            .take(50)
                            .map(|e| {
                                json!({
                                    "message": e.message,
                                    "path": e.rel_path,
                                    "operation": e.operation,
                                    "decision_policy": e.decision_policy.as_str(),
                                })
                            })
                            .collect()
                    };
                    // Plan is informational and only meaningful in dry-run.
                    // `plan_cap` caps the flat array; `per_op_cap` caps each
                    // bucket of `plan_by_op`. Rationale: an agent running a
                    // sync with thousands of skip entries and a handful of
                    // real uploads can still see every upload candidate in
                    // `plan_by_op.upload`, whereas the flat `plan` truncation
                    // used to hide them all behind earlier-alphabet skips.
                    // summary_only drops both arrays while keeping totals.
                    let plan_cap = 1000usize;
                    let per_op_cap = 250usize;
                    let plan_full_len = if dry_run { sink.plan.len() } else { 0 };
                    let plan_json: Vec<Value> = if dry_run && !summary_only {
                        sink.plan
                            .iter()
                            .take(plan_cap)
                            .map(|p| {
                                json!({
                                    "op": p.op,
                                    "path": p.path,
                                    "reason": p.reason,
                                    "bytes": p.bytes,
                                    "decision_policy": p.decision_policy,
                                })
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    // Group the plan by op bucket so an agent inspecting, say,
                    // upload candidates gets them all (up to per_op_cap) even
                    // when the overall plan count is huge.
                    let (plan_by_op, plan_by_op_truncated, plan_by_op_totals) =
                        if dry_run && !summary_only {
                            build_plan_by_op(&sink.plan, per_op_cap)
                        } else {
                            (
                                serde_json::Map::new(),
                                serde_json::Map::new(),
                                serde_json::Map::new(),
                            )
                        };

                    // Split the output by mode: in dry-run there is no
                    // meaningful `summary` (no writes happened), and in a real
                    // run there is no meaningful `plan`/`planned` block. This
                    // removes the old ambiguity where both fields coexisted
                    // with contradicting counters.
                    let payload = if dry_run {
                        // Fold the sink plan into action counters. `reason ==
                        // "dry-run"` means the core would have acted on the
                        // file; anything else (e.g. `"identical size"`) is a
                        // genuine skip the planner would have made anyway.
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
                        let planned_total = planned_uploaded
                            + planned_downloaded
                            + planned_deleted
                            + planned_skipped;
                        json!({
                            "server": server,
                            "local_dir": local_dir,
                            "remote_dir": remote_dir,
                            "direction": direction_raw,
                            "delta_policy": delta_policy.as_str(),
                            "dry_run": true,
                            "summary_only": summary_only,
                            "planned": {
                                "uploaded": planned_uploaded,
                                "downloaded": planned_downloaded,
                                "deleted": planned_deleted,
                                "skipped": planned_skipped,
                                "totals": {
                                    "requested": planned_total,
                                    "actionable": planned_uploaded
                                        + planned_downloaded
                                        + planned_deleted,
                                    "skipped": planned_skipped,
                                },
                            },
                            "plan": plan_json,
                            "plan_total": plan_full_len,
                            "plan_truncated": plan_full_len > plan_cap,
                            "plan_by_op": plan_by_op,
                            "plan_by_op_truncated": plan_by_op_truncated,
                            "plan_by_op_totals": plan_by_op_totals,
                            "scan_stats": {
                                "elapsed_secs": report.elapsed_secs,
                                "errors": report.error_count(),
                            },
                            "errors": errors,
                            "errors_truncated": report.error_count() > 50,
                        })
                    } else {
                        let total_processed =
                            report.uploaded + report.downloaded + report.deleted + report.skipped;
                        let total_errors = report.error_count();
                        let succeeded = report.uploaded + report.downloaded + report.deleted;
                        // Build the summary as a Map so `delta_savings` can be
                        // OMITTED (not emitted as `null`) when no file used the
                        // delta path. `serde_json::json!({…})` with an
                        // Option::None field serialises to `null`, which is
                        // noise that an MCP agent has to filter; absence is
                        // the cleaner signal.
                        let mut summary = serde_json::Map::new();
                        summary.insert("uploaded".into(), report.uploaded.into());
                        summary.insert("downloaded".into(), report.downloaded.into());
                        summary.insert("deleted".into(), report.deleted.into());
                        summary.insert("skipped".into(), report.skipped.into());
                        summary.insert("errors".into(), total_errors.into());
                        summary.insert("elapsed_secs".into(), report.elapsed_secs.into());
                        summary.insert(
                            "totals".into(),
                            json!({
                                "requested": total_processed + total_errors as u32,
                                "succeeded": succeeded,
                                "failed": total_errors as u32,
                                "skipped": report.skipped,
                                "elapsed_secs": report.elapsed_secs,
                            }),
                        );
                        if let Some(savings) = report.delta_savings.as_ref() {
                            // bytes_saved surfaced as a signed integer so
                            // rsync overhead (total_bytes_sent > total_size,
                            // rare but real on tiny files with inefficient
                            // delta) shows up as negative instead of being
                            // clipped to zero. Caller can still display
                            // max(0, bytes_saved) if desired.
                            let bytes_saved: i64 =
                                savings.total_size as i64 - savings.total_bytes_sent as i64;
                            let mut block = serde_json::Map::new();
                            block.insert(
                                "files_using_delta".into(),
                                savings.files_using_delta.into(),
                            );
                            block
                                .insert("total_bytes_sent".into(), savings.total_bytes_sent.into());
                            block.insert("total_size".into(), savings.total_size.into());
                            block.insert("bytes_saved".into(), bytes_saved.into());
                            // Omit `average_speedup` entirely when None — same
                            // absence-vs-null contract as the parent block.
                            if let Some(avg) = savings.average_speedup {
                                block.insert("average_speedup".into(), avg.into());
                            }
                            summary.insert("delta_savings".into(), Value::Object(block));
                        }
                        // PR-T03: per-file breakdown, capped at
                        // DELTA_FILES_CAP (500) so large syncs don't
                        // explode the response. Presence mirrors
                        // `delta_savings` — both driven by the same
                        // accumulator. Emit nothing when the classic
                        // path served every file, keeping the
                        // absence-vs-null contract consistent.
                        if !report.delta_files.is_empty() {
                            let files_json: Vec<Value> = report
                                .delta_files
                                .iter()
                                .map(|f| {
                                    json!({
                                        "path": f.path,
                                        "bytes_sent": f.bytes_sent,
                                        "total_size": f.total_size,
                                        "speedup": f.speedup,
                                    })
                                })
                                .collect();
                            summary.insert("delta_files".into(), Value::Array(files_json));
                            if report.delta_files_truncated {
                                summary.insert("delta_files_truncated".into(), Value::Bool(true));
                            }
                        }
                        json!({
                            "server": server,
                            "local_dir": local_dir,
                            "remote_dir": remote_dir,
                            "direction": direction_raw,
                            "delta_policy": delta_policy.as_str(),
                            "dry_run": false,
                            "summary_only": summary_only,
                            "summary": Value::Object(summary),
                            "errors": errors,
                            "errors_truncated": total_errors > 50,
                        })
                    };
                    // Release the provider lock and pool Arc BEFORE invalidate
                    // so the pool sees `strong_count == 1` and actually closes
                    // the underlying socket on the detached disconnect.
                    drop(p);
                    drop(arc);
                    // Preventive invalidate after every sync_tree run. Reason:
                    // a sync_tree (even a dry_run) issues a long chain of LIST
                    // and STOR/RETR commands that can leave the suppaftp data
                    // channel in an ambiguous state — the NEXT tool call on
                    // that pooled connection fails with "Data connection is
                    // already open" even though every intra-sync op succeeded.
                    // Cost: one reconnect per sync_tree. Acceptable given the
                    // overall duration of a sync, and it guarantees the next
                    // call (whether another sync or an upload_file) starts on
                    // a pristine control channel.
                    let _ = pool.invalidate(&server).await;
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
    decision_policy: String,
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
    current_op: Option<(String, String, u64, String)>,
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

    fn on_file_start(
        &mut self,
        rel: &str,
        total: u64,
        op: &'static str,
        decision_policy: crate::sync_core::DeltaPolicy,
    ) {
        if self.dry_run {
            self.current_op = Some((
                op.to_string(),
                rel.to_string(),
                total,
                decision_policy.as_str().to_string(),
            ));
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
                let (op, bytes, decision_policy) = match self.current_op.take() {
                    Some((op, path, total, decision_policy)) if path == rel => {
                        (op, total, decision_policy)
                    }
                    Some((op, _, total, decision_policy)) => (op, total, decision_policy),
                    None => (
                        "skip".to_string(),
                        0,
                        crate::sync_core::DeltaPolicy::default()
                            .as_str()
                            .to_string(),
                    ),
                };
                self.plan.push(PlanEntry {
                    op,
                    path: rel.to_string(),
                    reason: reason.clone(),
                    bytes,
                    decision_policy,
                });
            } else {
                self.current_op = None;
            }
        }
        if let Some(tx) = self.progress_tx.as_ref() {
            let processed = self.processed;
            let failures = self.failures;
            let msg = match outcome {
                crate::sync_core::FileOutcome::Uploaded { bytes, .. } => {
                    format!("uploaded {} ({} bytes)", rel, bytes)
                }
                crate::sync_core::FileOutcome::Downloaded { bytes, .. } => {
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
        let err =
            validate_read_preview_target(false, MAX_READ_PREVIEW_BYTES + 1, 16 * 1024).unwrap_err();
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
    fn transport_error_classifier_catches_421_idle_timeout_family() {
        use super::is_transport_error;
        use crate::providers::ProviderError;
        // Aruba proftpd after a ~5min idle window: the control channel is
        // dropped and the first subsequent op surfaces `421`. Without this
        // classifier entry the error bubbles up as a hard transfer failure.
        assert!(is_transport_error(&ProviderError::TransferFailed(
            "Invalid response: 421 Control connection timed out.".into(),
        )));
        assert!(is_transport_error(&ProviderError::ServerError(
            "421 Idle timeout".into(),
        )));
        assert!(is_transport_error(&ProviderError::Other(
            "421 Service not available, closing control connection".into(),
        )));
        assert!(is_transport_error(&ProviderError::TransferFailed(
            "control connection closed".into(),
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

    #[test]
    fn collapse_duplicate_ftp_code_removes_leading_bracket_pair() {
        let input = "[553] 553 Can't open that file: No such file or directory.";
        let out = super::collapse_duplicate_ftp_code(input);
        assert_eq!(out, "553 Can't open that file: No such file or directory.");
    }

    #[test]
    fn collapse_duplicate_ftp_code_leaves_single_code_alone() {
        let input = "553 Can't open that file";
        assert_eq!(super::collapse_duplicate_ftp_code(input), input);
    }

    #[test]
    fn collapse_duplicate_ftp_code_leaves_non_matching_messages_alone() {
        let input = "some random error message";
        assert_eq!(super::collapse_duplicate_ftp_code(input), input);
    }

    #[test]
    fn collapse_duplicate_ftp_code_handles_wrapped_prefix() {
        // Real-world shape: provider error wraps the suppaftp response.
        let input = "Transfer failed: Invalid response: [553] 553 Can't open that file: No such file or directory.";
        let out = super::collapse_duplicate_ftp_code(input);
        assert_eq!(
            out,
            "Transfer failed: Invalid response: 553 Can't open that file: No such file or directory."
        );
    }

    #[test]
    fn collapse_duplicate_ftp_code_preserves_multibyte_context() {
        // Non-ASCII content on either side of the doubled code must survive.
        let input = "Errore à livello: [550] 550 File non esiste — fine.";
        let out = super::collapse_duplicate_ftp_code(input);
        assert_eq!(out, "Errore à livello: 550 File non esiste — fine.");
    }

    #[test]
    fn augment_upload_error_hint_does_not_retain_bracket_double() {
        // Reproduces the agent-reported residual: even the Server-said tail
        // must be free of the `[NNN] NNN` doubled pattern.
        let wrapped = "Transfer failed: Invalid response: [553] 553 Can't open that file: No such file or directory.";
        let msg = super::augment_upload_error(wrapped, "/etna/_no_dir/t.sql", false, true);
        assert!(msg.contains("create_parents=true"), "expected hint: {msg}");
        assert!(
            !msg.contains("[553] 553"),
            "duplicated code should be collapsed even inside Server-said tail: {msg}"
        );
    }

    #[test]
    fn parent_missing_classifier_matches_common_phrases() {
        assert!(super::looks_like_parent_missing_upload(
            "553 Can't open that file: No such file or directory."
        ));
        assert!(super::looks_like_parent_missing_upload(
            "SFTP upload: ENOENT (No such file or directory)"
        ));
        assert!(super::looks_like_parent_missing_upload(
            "WebDAV: 404 Not Found - parent does not exist"
        ));
    }

    #[test]
    fn parent_missing_classifier_ignores_unrelated_failures() {
        assert!(!super::looks_like_parent_missing_upload(
            "Permission denied"
        ));
        assert!(!super::looks_like_parent_missing_upload(
            "Connection reset by peer"
        ));
    }

    #[test]
    fn augment_upload_error_hints_create_parents_when_relevant() {
        let msg = super::augment_upload_error(
            "[553] 553 Can't open that file: No such file or directory.",
            "/etna/_no_dir/_sub/t.sql",
            false, // create_parents
            true,  // had_parent
        );
        assert!(
            msg.contains("create_parents=true"),
            "expected hint in: {msg}"
        );
        assert!(msg.contains("/etna/_no_dir/_sub/t.sql"));
        // The duplicated `[553] 553` pair should be collapsed.
        assert!(!msg.contains("[553] 553"));
    }

    #[test]
    fn augment_upload_error_skips_hint_when_create_parents_already_true() {
        let input = "553 No such file or directory";
        let msg = super::augment_upload_error(input, "/a/b/c", true, true);
        assert!(!msg.contains("create_parents=true"));
    }

    #[test]
    fn augment_upload_error_skips_hint_when_error_is_unrelated() {
        let msg = super::augment_upload_error("Permission denied", "/a/b/c", false, true);
        assert!(!msg.contains("create_parents=true"));
        assert_eq!(msg, "Permission denied");
    }

    #[test]
    fn err_envelope_exposes_both_scalar_and_array() {
        let (payload, is_error) = super::err("boom".to_string());
        assert!(is_error);
        assert_eq!(payload.get("error").and_then(|v| v.as_str()), Some("boom"));
        let arr = payload
            .get("errors")
            .and_then(|v| v.as_array())
            .expect("errors array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].get("message").and_then(|v| v.as_str()), Some("boom"));
    }

    #[test]
    fn apply_replacements_replaces_all_by_default() {
        let (out, n) = super::apply_replacements("aa bb aa bb aa", "aa", "X", false);
        assert_eq!(out, "X bb X bb X");
        assert_eq!(n, 3);
    }

    #[test]
    fn apply_replacements_honors_first_only() {
        let (out, n) = super::apply_replacements("aa bb aa", "aa", "X", true);
        assert_eq!(out, "X bb aa");
        assert_eq!(n, 1);
    }

    #[test]
    fn apply_replacements_returns_zero_on_no_match() {
        let (out, n) = super::apply_replacements("nothing here", "xyz", "X", false);
        assert_eq!(out, "nothing here");
        assert_eq!(n, 0);
    }

    #[test]
    fn apply_replacements_supports_empty_replace() {
        // Delete-match semantics: replacing "foo" with "" removes matches.
        let (out, n) = super::apply_replacements("foobar foo baz", "foo", "", false);
        assert_eq!(out, "bar  baz");
        assert_eq!(n, 2);
    }

    #[test]
    fn apply_replacements_handles_multibyte_utf8() {
        // UTF-8 safety check: é (2 bytes), 中 (3 bytes), 😀 (4 bytes) all survive.
        let (out, n) = super::apply_replacements("café中😀café", "café", "CAFE", false);
        assert_eq!(out, "CAFE中😀CAFE");
        assert_eq!(n, 2);
    }

    #[test]
    fn apply_replacements_rejects_empty_find() {
        // Empty find would be an infinite match; we short-circuit to 0.
        let (out, n) = super::apply_replacements("anything", "", "x", false);
        assert_eq!(out, "anything");
        assert_eq!(n, 0);
    }

    #[test]
    fn mcp_info_exposes_expected_fields() {
        let info = super::build_mcp_info();
        let obj = info.as_object().expect("mcp_info is a JSON object");
        // Always-present fields.
        for key in ["pid", "version", "uptime_secs", "started_at"] {
            assert!(obj.contains_key(key), "missing required field: {key}");
        }
        // Version must match the crate version that was compiled in.
        assert_eq!(
            obj.get("version").and_then(|v| v.as_str()),
            Some(env!("CARGO_PKG_VERSION"))
        );
        // pid must be non-zero. `u32` on Unix, still > 0 in any real process.
        assert!(obj.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) > 0);
        // uptime must be non-negative. (Captured lazily on first access,
        // so in a unit test this may be 0.)
        assert!(
            obj.get("uptime_secs")
                .and_then(|v| v.as_i64())
                .unwrap_or(-1)
                >= 0
        );
        // Best-effort fields are present but may be null on some platforms.
        for key in ["binary_path", "binary_mtime", "binary_size"] {
            assert!(obj.contains_key(key), "missing best-effort field: {key}");
        }
    }

    #[test]
    fn mcp_info_registry_entry_is_read_only_and_requires_nothing() {
        let t = tool_definitions()
            .into_iter()
            .find(|t| t.name == "aeroftp_mcp_info")
            .expect("aeroftp_mcp_info tool registered");
        assert_eq!(t.category, super::RateCategory::ReadOnly);
        let required = t
            .input_schema
            .get("required")
            .and_then(|v| v.as_array())
            .expect("required array");
        assert!(required.is_empty(), "tool should take no arguments");
    }

    #[test]
    fn build_plan_by_op_groups_and_caps_per_bucket() {
        let entries = vec![
            super::PlanEntry {
                op: "upload".into(),
                path: "a".into(),
                reason: "dry-run".into(),
                bytes: 1,
                decision_policy: "mtime".into(),
            },
            super::PlanEntry {
                op: "upload".into(),
                path: "b".into(),
                reason: "dry-run".into(),
                bytes: 2,
                decision_policy: "mtime".into(),
            },
            super::PlanEntry {
                op: "upload".into(),
                path: "c".into(),
                reason: "dry-run".into(),
                bytes: 3,
                decision_policy: "mtime".into(),
            },
            super::PlanEntry {
                op: "delete_remote".into(),
                path: "x".into(),
                reason: "dry-run".into(),
                bytes: 0,
                decision_policy: "size_only".into(),
            },
            super::PlanEntry {
                op: "skip".into(),
                path: "y".into(),
                reason: "identical size".into(),
                bytes: 0,
                decision_policy: "size_only".into(),
            },
        ];
        // Cap at 2 to exercise truncation.
        let (by_op, truncated, totals) = super::build_plan_by_op(&entries, 2);
        let uploads = by_op.get("upload").and_then(|v| v.as_array()).unwrap();
        assert_eq!(uploads.len(), 2);
        assert_eq!(totals.get("upload").and_then(|v| v.as_u64()), Some(3));
        assert_eq!(
            truncated.get("upload").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(totals.get("delete").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(
            truncated.get("delete").and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            uploads[0].get("decision_policy").and_then(|v| v.as_str()),
            Some("mtime")
        );
        // `skip` bucket gets the unknown-op entry.
        assert_eq!(totals.get("skip").and_then(|v| v.as_u64()), Some(1));
    }

    #[test]
    fn sync_tree_schema_exposes_plan_by_op_hint_in_description() {
        let t = tool_definitions()
            .into_iter()
            .find(|t| t.name == "aeroftp_sync_tree")
            .unwrap();
        assert!(
            t.description.contains("plan_by_op"),
            "sync_tree description should mention plan_by_op"
        );
    }

    #[test]
    fn sync_tree_schema_exposes_delta_policy_enum() {
        let t = tool_definitions()
            .into_iter()
            .find(|t| t.name == "aeroftp_sync_tree")
            .unwrap();
        let props = t
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("properties");
        let delta_policy = props
            .get("delta_policy")
            .and_then(|v| v.as_object())
            .expect("delta_policy property");
        let variants = delta_policy
            .get("enum")
            .and_then(|v| v.as_array())
            .expect("delta_policy enum");
        let variants: Vec<&str> = variants.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(
            variants,
            vec!["disabled", "size_only", "mtime", "hash", "delta"]
        );
    }

    #[test]
    fn sync_tree_description_documents_delta_files_breakdown() {
        // PR-T03 contract: the tool description must tell consuming
        // agents about `summary.delta_files[]`, its 500-entry cap, and
        // the `summary.delta_files_truncated` flag. This is the MCP
        // surface that downstream tooling (ultrareview, mcp-inspector,
        // future UIs) reads to decide whether to parse the breakdown.
        let t = tool_definitions()
            .into_iter()
            .find(|t| t.name == "aeroftp_sync_tree")
            .expect("aeroftp_sync_tree registered");
        let desc = t.description;
        assert!(
            desc.contains("delta_files"),
            "description must mention delta_files breakdown; was: {desc}"
        );
        assert!(
            desc.contains("500"),
            "description must mention the 500-entry cap; was: {desc}"
        );
        assert!(
            desc.contains("delta_files_truncated"),
            "description must mention the truncation flag; was: {desc}"
        );
    }

    #[test]
    fn upload_tools_expose_no_clobber() {
        for name in ["aeroftp_upload_file", "aeroftp_upload_many"] {
            let t = tool_definitions()
                .into_iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("{name} registered"));
            let props = t
                .input_schema
                .get("properties")
                .and_then(|v| v.as_object())
                .expect("properties");
            // upload_file exposes it at the top level; upload_many exposes
            // both at the top level (default) and inside the item schema.
            if name == "aeroftp_upload_file" {
                assert!(props.contains_key("no_clobber"));
            } else {
                assert!(props.contains_key("no_clobber"));
                let items = props.get("items").and_then(|v| v.as_object()).unwrap();
                let item_schema = items.get("items").and_then(|v| v.as_object()).unwrap();
                let item_props = item_schema
                    .get("properties")
                    .and_then(|v| v.as_object())
                    .unwrap();
                assert!(item_props.contains_key("no_clobber"));
            }
        }
    }

    #[test]
    fn top_level_bucket_puts_root_files_under_root() {
        assert_eq!(super::top_level_bucket("README.md", 1), "_root");
        assert_eq!(super::top_level_bucket("/README.md", 1), "_root");
    }

    #[test]
    fn top_level_bucket_respects_depth() {
        assert_eq!(super::top_level_bucket("app/sub/inner/file.ts", 1), "app");
        assert_eq!(
            super::top_level_bucket("app/sub/inner/file.ts", 2),
            "app/sub"
        );
        assert_eq!(
            super::top_level_bucket("app/sub/inner/file.ts", 3),
            "app/sub/inner"
        );
    }

    #[test]
    fn top_level_bucket_clamps_depth_to_available_dirs() {
        // Only one directory level, asking for two → take what's available.
        assert_eq!(super::top_level_bucket("app/file.ts", 2), "app");
    }

    #[test]
    fn top_level_bucket_depth_zero_always_root() {
        assert_eq!(super::top_level_bucket("app/sub/file.ts", 0), "_root");
    }

    #[test]
    fn aggregate_by_top_level_produces_per_bucket_counts() {
        use crate::sync_core::{DiffEntry, DiffReport};
        let make = |rel: &str| DiffEntry {
            rel_path: rel.to_string(),
            local_size: None,
            remote_size: None,
            local_sha256: None,
            remote_checksum_alg: None,
            remote_checksum_hex: None,
            compare_method: None,
        };
        let diff = DiffReport {
            matches: vec![make("app/a.ts"), make("front/b.css")],
            differ: vec![make("app/sub/c.ts")],
            missing_local: vec![make("README.md")],
            missing_remote: vec![make("app/d.ts"), make("app/e.ts")],
        };
        let agg = super::aggregate_by_top_level(&diff, 1);
        let app = agg.get("app").and_then(|v| v.as_object()).expect("app");
        assert_eq!(app.get("match").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(app.get("differ").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(app.get("missing_remote").and_then(|v| v.as_u64()), Some(2));
        let front = agg.get("front").and_then(|v| v.as_object()).expect("front");
        assert_eq!(front.get("match").and_then(|v| v.as_u64()), Some(1));
        let root = agg.get("_root").and_then(|v| v.as_object()).expect("_root");
        assert_eq!(root.get("missing_local").and_then(|v| v.as_u64()), Some(1));
    }

    #[test]
    fn check_tree_and_sync_tree_expose_files_param() {
        for name in ["aeroftp_check_tree", "aeroftp_sync_tree"] {
            let t = tool_definitions()
                .into_iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("{name} registered"));
            let files = t
                .input_schema
                .get("properties")
                .and_then(|p| p.get("files"))
                .unwrap_or_else(|| panic!("{name} exposes `files`"));
            assert_eq!(files.get("type").and_then(|v| v.as_str()), Some("array"));
        }
    }

    #[test]
    fn check_tree_exposes_group_by_depth() {
        let t = tool_definitions()
            .into_iter()
            .find(|t| t.name == "aeroftp_check_tree")
            .expect("check_tree");
        let gbd = t
            .input_schema
            .get("properties")
            .and_then(|p| p.get("group_by_depth"))
            .expect("group_by_depth");
        assert_eq!(gbd.get("type").and_then(|v| v.as_str()), Some("integer"));
    }

    #[test]
    fn upload_many_registry_entry_is_present_and_mutative() {
        let t = tool_definitions()
            .into_iter()
            .find(|t| t.name == "aeroftp_upload_many")
            .expect("aeroftp_upload_many tool registered");
        let props = t
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("properties");
        for key in ["server", "items", "continue_on_error", "delay_ms"] {
            assert!(props.contains_key(key), "missing property: {key}");
        }
        // items schema must be an array of objects with local_path + remote_path.
        let items = props.get("items").expect("items schema");
        assert_eq!(items.get("type").and_then(|v| v.as_str()), Some("array"));
        let item_schema = items.get("items").expect("items.items");
        let item_props = item_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("item properties");
        for key in ["local_path", "remote_path", "create_parents"] {
            assert!(item_props.contains_key(key), "item missing: {key}");
        }
        let required = t
            .input_schema
            .get("required")
            .and_then(|v| v.as_array())
            .expect("required");
        let req: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(req.contains(&"server"));
        assert!(req.contains(&"items"));
        assert_eq!(t.category, super::RateCategory::Mutative);
    }

    #[test]
    fn check_tree_and_sync_tree_expose_summary_only() {
        for name in ["aeroftp_check_tree", "aeroftp_sync_tree"] {
            let t = tool_definitions()
                .into_iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("tool {name} registered"));
            let so = t
                .input_schema
                .get("properties")
                .and_then(|p| p.get("summary_only"))
                .unwrap_or_else(|| panic!("{name} exposes summary_only"));
            assert_eq!(
                so.get("type").and_then(|v| v.as_str()),
                Some("boolean"),
                "{name}.summary_only must be boolean"
            );
        }
    }

    #[test]
    fn edit_tool_registry_entry_is_present_and_mutative() {
        let t = tool_definitions()
            .into_iter()
            .find(|t| t.name == "aeroftp_edit")
            .expect("aeroftp_edit tool should be registered");
        let props = t
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("properties");
        for key in ["server", "path", "find", "replace", "first"] {
            assert!(props.contains_key(key), "missing property: {key}");
        }
        let required = t
            .input_schema
            .get("required")
            .and_then(|v| v.as_array())
            .expect("required");
        let req: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        for key in ["server", "path", "find", "replace"] {
            assert!(req.contains(&key), "missing required: {key}");
        }
        assert!(!req.contains(&"first"));
        assert_eq!(t.category, super::RateCategory::Mutative);
    }

    #[test]
    fn delete_tool_schema_accepts_path_and_paths() {
        let t = tool_definitions()
            .into_iter()
            .find(|t| t.name == "aeroftp_delete")
            .expect("delete tool");
        let props = t
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("properties");
        assert!(props.contains_key("path"));
        assert!(props.contains_key("paths"));
        // Neither `path` nor `paths` should be in `required`.
        let required = t
            .input_schema
            .get("required")
            .and_then(|v| v.as_array())
            .expect("required");
        let req: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(req.contains(&"server"));
        assert!(!req.contains(&"path"));
        assert!(!req.contains(&"paths"));
    }
}
