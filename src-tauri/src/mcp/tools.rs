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

use crate::mcp::notifier::McpNotifier;
use crate::mcp::pool::ConnectionPool;
use crate::mcp::security::{self, RateCategory};
use serde_json::{json, Value};
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

// Hard cap for in-memory read previews. Used by `validate_read_preview_target`
// which is now `#[cfg(test)]`, so this constant follows the same gating to
// keep the production lib build warning-free.
#[cfg(test)]
const MAX_READ_PREVIEW_BYTES: u64 = 1_048_576;

// `MAX_EDIT_BYTES`, `DeleteKind`, `sanitize_error`, `is_transport_error`,
// `message_implies_broken_session` and `execute_with_reset` were the
// transport-failure / pool-invalidation helpers used by the legacy per-tool
// match arms. T3 Gate 3 moved every remote tool body into
// `ai_core::remote_tools` which talks to the pool through the
// McpRemoteBackend abstraction; the pool itself owns
// invalidation today, so the helpers no longer have callers and were
// removed wholesale.

/// MCP tool definition for `tools/list`.
pub struct McpToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub category: RateCategory,
}

/// Get all 16 curated tool definitions.
pub fn tool_definitions() -> Vec<McpToolDef> {
    let mut defs = vec![
        McpToolDef {
            name: "aeroftp_mcp_info",
            description: "Return diagnostics about the running MCP process itself...",
            input_schema: json!({ "type": "object", "properties": {}, "required": [] }),
            category: RateCategory::ReadOnly,
        },
        McpToolDef {
            name: "aeroftp_close_connection",
            description: "Close a pooled server connection explicitly...",
            input_schema: json!({ "type": "object", "properties": {
                "server": { "type": "string", "description": "Server name or ID to disconnect" }
            }, "required": ["server"] }),
            category: RateCategory::Mutative,
        },
        McpToolDef {
            name: "aeroftp_check_tree",
            description: "Compare a local directory against a remote directory and report differences...",
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
        // upload_many + edit live in MCP_TOOL_DEFS to keep the rich
        // descriptions / nested item-schema (no_clobber inside items[],
        // first-only flag) that downstream agents and the parity tests
        // depend on. The unified core registry has lighter shapes for
        // these tools — the dynamic injection below skips them via the
        // already-registered name (vec push order wins on `find`).
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
    ];

    // Inject dynamic tools from the unified core registry (T3 Gate 3).
    // Skip names that the legacy MCP_TOOL_DEFS already provides (richer
    // descriptions and nested schemas the unified registry intentionally
    // simplifies — see aeroftp_upload_many / aeroftp_edit / aeroftp_sync_tree).
    use crate::ai_core::tools::{Surfaces, DangerLevel, TOOL_DEFINITIONS};
    let already_defined: std::collections::HashSet<&'static str> =
        defs.iter().map(|d| d.name).collect();
    for core_def in TOOL_DEFINITIONS.iter() {
        if !core_def.surfaces.contains(Surfaces::MCP) {
            continue;
        }
        if already_defined.contains(core_def.name) {
            continue;
        }
        let cat = match core_def.danger {
            DangerLevel::Safe | DangerLevel::ReadOnly => RateCategory::ReadOnly,
            DangerLevel::Medium => RateCategory::Mutative,
            DangerLevel::High => RateCategory::Destructive,
        };
        defs.push(McpToolDef {
            name: core_def.name,
            description: core_def.description,
            input_schema: core_def.input_schema.clone(),
            category: cat,
        });
    }

    defs
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
///
/// Kept for the existing unit test coverage on the read-preview cap.
/// The production read path lives in `ai_core::remote_tools::read_file`
/// after T3 Gate 3 — this helper is no longer wired into a tool dispatch.
#[cfg(test)]
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

// `parent_remote_dir`, `ensure_remote_parents`, `collapse_duplicate_ftp_code`,
// `looks_like_parent_missing_upload`, `augment_upload_error`,
// `dispatch_upload_batch`, `dispatch_delete_batch`, `UploadBatchItem`,
// DELETE_BATCH_MAX / UPLOAD_BATCH_MAX / MAX_UPLOAD_ATTEMPTS were removed
// in T3 Gate 3. Every remote tool now flows through
// `ai_core::remote_tools::dispatch_remote_tool`, which talks to the pool
// via the McpRemoteBackend abstraction and emits progress through
// EventSink. ~600 LOC of duplicated transport / error / batching logic
// gone for good.

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

// `apply_replacements` and `delete_kind` were used by the legacy
// per-tool match arms removed in T3 Gate 3. The unified dispatcher
// (ai_core::remote_tools::dispatch_remote_tool) handles edits and
// deletions via the McpRemoteBackend abstraction.

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

// `build_progress_callback` powered the legacy upload/download arms.
// Removed in T3 Gate 3 — `ai_core::remote_tools` now emits progress via
// `EventSink::emit_tool_progress` directly.

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
        _ => finish(tool_name, None, None, err(format!("Unknown tool: {}", tool_name)), start),
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
    use super::{tool_definitions, validate_read_preview_target, MAX_READ_PREVIEW_BYTES};

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
