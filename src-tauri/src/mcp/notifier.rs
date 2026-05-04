//! MCP progress notifier: emits `notifications/progress` during long-running
//! operations so the client can render transfer progress and detect stalls.
//!
//! The MCP spec (2024-11-05) lets callers attach a `progressToken` to any
//! request via `params._meta.progressToken`. The server replies with
//! `notifications/progress` messages referencing that token. Without progress
//! notifications, agents block for minutes on multi-MB transfers with no
//! feedback: this module closes that gap.
//!
//! Rate-limiting: the notifier throttles outbound messages to roughly
//! 10 Hz (one every 100 ms) to avoid flooding the client on fast transfers.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use crate::mcp::transport::StdoutWriter;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const DEFAULT_THROTTLE: Duration = Duration::from_millis(100);

/// Emits `notifications/progress` messages for a single tool invocation.
///
/// Constructed per-request by `McpServerCore` when the incoming JSON-RPC
/// request carries a `progressToken`. Tools that do not need progress
/// updates can simply ignore the notifier handle.
#[derive(Clone)]
pub struct McpNotifier {
    writer: Arc<StdoutWriter>,
    progress_token: Option<Value>,
    last_emit_nanos: Arc<AtomicU64>,
    started_at: Instant,
    throttle: Duration,
}

impl McpNotifier {
    /// Build a notifier that will emit progress notifications on the shared
    /// stdout writer. If `progress_token` is `None` the notifier becomes a
    /// no-op (the client did not ask for progress).
    pub fn new(writer: Arc<StdoutWriter>, progress_token: Option<Value>) -> Self {
        Self {
            writer,
            progress_token,
            last_emit_nanos: Arc::new(AtomicU64::new(0)),
            started_at: Instant::now(),
            throttle: DEFAULT_THROTTLE,
        }
    }

    /// `true` when the caller attached a progress token.
    pub fn is_active(&self) -> bool {
        self.progress_token.is_some()
    }

    /// Send a progress notification subject to a ~10 Hz throttle.
    ///
    /// `progress` and `total` follow the MCP convention: either both are raw
    /// byte/item counts, or `progress` is a percentage (0..=100) and `total`
    /// is `Some(100)`. `message` is a short human-readable status.
    pub async fn send_progress(&self, progress: u64, total: Option<u64>, message: Option<String>) {
        if !self.is_active() {
            return;
        }
        if !self.take_throttle_slot(false) {
            return;
        }
        self.emit(progress, total, message).await;
    }

    /// Force-send the final progress notification regardless of throttle.
    ///
    /// Always-flush on completion so the client always sees 100% before the
    /// tool result arrives.
    pub async fn send_progress_final(
        &self,
        progress: u64,
        total: Option<u64>,
        message: Option<String>,
    ) {
        if !self.is_active() {
            return;
        }
        self.take_throttle_slot(true);
        self.emit(progress, total, message).await;
    }

    fn take_throttle_slot(&self, force: bool) -> bool {
        let now_nanos = self.started_at.elapsed().as_nanos() as u64;
        let last = self.last_emit_nanos.load(Ordering::Relaxed);
        let elapsed = now_nanos.saturating_sub(last);
        if !force && last != 0 && elapsed < self.throttle.as_nanos() as u64 {
            return false;
        }
        self.last_emit_nanos.store(now_nanos, Ordering::Relaxed);
        true
    }

    async fn emit(&self, progress: u64, total: Option<u64>, message: Option<String>) {
        let mut params = serde_json::Map::new();
        if let Some(token) = &self.progress_token {
            params.insert("progressToken".to_string(), token.clone());
        }
        params.insert("progress".to_string(), json!(progress));
        if let Some(total) = total {
            params.insert("total".to_string(), json!(total));
        }
        if let Some(message) = message {
            params.insert("message".to_string(), json!(message));
        }
        let msg = json!({
            "jsonrpc": "2.0",
            "method": "notifications/progress",
            "params": Value::Object(params),
        });
        let _ = self.writer.write_message(&msg).await;
    }
}

/// Extract the progress token from a JSON-RPC request's `params._meta`.
///
/// Accepts both the MCP-spec `params._meta.progressToken` location and the
/// legacy `params.progressToken` fallback used by some clients.
pub fn extract_progress_token(req: &Value) -> Option<Value> {
    let params = req.get("params")?;
    if let Some(token) = params
        .get("_meta")
        .and_then(|m| m.get("progressToken"))
        .cloned()
    {
        if !token.is_null() {
            return Some(token);
        }
    }
    params
        .get("progressToken")
        .cloned()
        .filter(|v| !v.is_null())
}

/// Format a human-readable progress message without em-dashes.
/// Example: `"42%: 12.3 MB / 29.1 MB: 3.2 MB/s"` is NOT produced;
/// we use regular hyphens per project style guide.
pub fn format_transfer_message(pct: u64, sent: u64, total: u64, bps: u64) -> String {
    let sent_fmt = format_bytes(sent);
    let total_fmt = format_bytes(total);
    let bps_fmt = format_bytes(bps);
    format!("{}% - {} / {} - {}/s", pct, sent_fmt, total_fmt, bps_fmt)
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    if bytes == 0 {
        return "0 B".to_string();
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", value as u64, UNITS[unit])
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_progress_token_handles_mcp_spec_location() {
        let req = json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "_meta": { "progressToken": "upload-42" }
            }
        });
        assert_eq!(extract_progress_token(&req), Some(json!("upload-42")));
    }

    #[test]
    fn extract_progress_token_handles_legacy_location() {
        let req = json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": { "progressToken": 7 }
        });
        assert_eq!(extract_progress_token(&req), Some(json!(7)));
    }

    #[test]
    fn extract_progress_token_returns_none_when_missing() {
        let req = json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {}
        });
        assert_eq!(extract_progress_token(&req), None);
    }

    #[test]
    fn extract_progress_token_rejects_null() {
        let req = json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "_meta": { "progressToken": null }
            }
        });
        assert_eq!(extract_progress_token(&req), None);
    }

    #[test]
    fn format_transfer_message_uses_hyphen_separator() {
        let msg = format_transfer_message(45, 45 * 1024 * 1024, 100 * 1024 * 1024, 2 * 1024 * 1024);
        assert!(!msg.contains('\u{2014}'), "message must not use em-dash");
        assert!(msg.contains("45%"));
        assert!(msg.contains("MB"));
        assert!(msg.contains("/s"));
    }

    #[test]
    fn format_bytes_scales_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn inactive_notifier_is_noop() {
        let writer = Arc::new(StdoutWriter::new());
        let notifier = McpNotifier::new(writer, None);
        assert!(!notifier.is_active());
    }
}
