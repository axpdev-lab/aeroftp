//! MCP security layer — path validation, rate limiting, input sanitization, audit logging
//!
//! - Path validation: no null bytes, no `..` traversal, max 4096 chars
//! - Rate limiting: token bucket per category (read/mutative/destructive)
//! - Input sanitization: max text lengths, reject control chars
//! - Audit logging: JSON on stderr, never logs file contents or passwords

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

// ─── Path Validation ─────────────────────────────────────────────────

/// Maximum path length in characters.
const MAX_PATH_LEN: usize = 4096;

/// Maximum text content length for inline uploads.
const MAX_TEXT_LEN: usize = 10_000;

/// Validate a remote path for MCP tool arguments.
pub fn validate_remote_path(path: &str) -> Result<(), String> {
    if path.len() > MAX_PATH_LEN {
        return Err(format!("Path exceeds {} character limit", MAX_PATH_LEN));
    }
    if path.contains('\0') {
        return Err("Path contains null bytes".into());
    }
    if path.starts_with('-') {
        return Err("Path must not start with '-'".into());
    }
    // Reject .. components (path traversal)
    for component in path.split('/') {
        if component == ".." {
            return Err("Path traversal ('..') is not allowed".into());
        }
    }
    // Reject control characters (except common whitespace)
    if path.chars().any(|c| c.is_control() && c != '\t') {
        return Err("Path contains control characters".into());
    }
    Ok(())
}

/// Validate a local path for download targets.
/// Re-uses the CLI's deny-list approach with symlink resolution for TOCTOU safety.
pub fn validate_local_path(path: &str) -> Result<(), String> {
    validate_remote_path(path)?;

    // Deny sensitive system prefixes
    const DENIED_PREFIXES: &[&str] = &[
        "/etc", "/boot", "/proc", "/sys", "/dev",
        "/usr/sbin", "/sbin", "/root", "/run/secrets",
    ];

    // Check raw path first
    let normalized = path.replace('\\', "/");
    for prefix in DENIED_PREFIXES {
        if normalized.starts_with(prefix) {
            return Err(format!("Access denied: {}", prefix));
        }
    }

    // Resolve symlinks and re-check (prevents /tmp/evil -> /etc/shadow bypass).
    // For non-existent paths, canonicalize the parent to cover write targets.
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
        let resolved_str = canonical.to_string_lossy();
        for prefix in DENIED_PREFIXES {
            if resolved_str.starts_with(prefix) {
                return Err(format!("Access denied (resolved): {}", prefix));
            }
        }
        // Check resolved home-relative paths
        if let Ok(home) = std::env::var("HOME") {
            for dir in DENIED_HOME_DIRS {
                let full = format!("{}/{}", home, dir);
                if resolved_str.starts_with(&full) {
                    return Err(format!("Access denied: ~/{}", dir));
                }
            }
        }
    }

    // Deny sensitive home directories (raw path check)
    if let Ok(home) = std::env::var("HOME") {
        for dir in DENIED_HOME_DIRS {
            let full = format!("{}/{}", home, dir);
            if normalized.starts_with(&full) {
                return Err(format!("Access denied: ~/{}", dir));
            }
        }
    }

    Ok(())
}

const DENIED_HOME_DIRS: &[&str] = &[".ssh", ".gnupg", ".aws", ".config/aeroftp"];

/// Validate a server query string.
pub fn validate_server_query(query: &str) -> Result<(), String> {
    if query.is_empty() {
        return Err("Server name is required".into());
    }
    if query.len() > 256 {
        return Err("Server name exceeds 256 character limit".into());
    }
    if query.contains('\0') {
        return Err("Server name contains null bytes".into());
    }
    Ok(())
}

/// Validate inline text content length.
pub fn validate_text_content(text: &str) -> Result<(), String> {
    if text.len() > MAX_TEXT_LEN {
        return Err(format!("Text content exceeds {} character limit", MAX_TEXT_LEN));
    }
    Ok(())
}

// ─── Rate Limiting ───────────────────────────────────────────────────

/// Rate limit category for tool calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RateCategory {
    /// Read-only operations: list, read, info, search, quota, server_info, versions, checksum
    ReadOnly,
    /// Mutative operations: upload, mkdir, rename, create_share_link, server_copy
    Mutative,
    /// Destructive operations: delete
    Destructive,
}

impl RateCategory {
    /// Maximum requests per minute for this category.
    fn max_per_minute(self) -> u32 {
        match self {
            RateCategory::ReadOnly => 60,
            RateCategory::Mutative => 20,
            RateCategory::Destructive => 5,
        }
    }
}

/// Token bucket rate limiter.
pub struct RateLimiter {
    buckets: Mutex<HashMap<RateCategory, TokenBucket>>,
}

struct TokenBucket {
    tokens: f64,
    max_tokens: f64,
    refill_rate: f64, // tokens per second
    last_refill: Instant,
}

impl TokenBucket {
    fn new(max_per_minute: u32) -> Self {
        let max = max_per_minute as f64;
        Self {
            tokens: max,
            max_tokens: max,
            refill_rate: max / 60.0,
            last_refill: Instant::now(),
        }
    }

    fn try_consume(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.max_tokens);
        self.last_refill = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    fn retry_after(&self) -> Duration {
        if self.tokens >= 1.0 {
            return Duration::ZERO;
        }
        let needed = 1.0 - self.tokens;
        Duration::from_secs_f64(needed / self.refill_rate)
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimiter {
    pub fn new() -> Self {
        let mut buckets = HashMap::new();
        for cat in [RateCategory::ReadOnly, RateCategory::Mutative, RateCategory::Destructive] {
            buckets.insert(cat, TokenBucket::new(cat.max_per_minute()));
        }
        Self {
            buckets: Mutex::new(buckets),
        }
    }

    /// Try to consume a token. Returns Ok(()) if allowed, Err with retry-after seconds.
    pub fn check(&self, category: RateCategory) -> Result<(), f64> {
        let mut buckets = self.buckets.lock().unwrap();
        let bucket = buckets.get_mut(&category).unwrap();
        if bucket.try_consume() {
            Ok(())
        } else {
            Err(bucket.retry_after().as_secs_f64())
        }
    }
}

// ─── Audit Logging ───────────────────────────────────────────────────

/// Log a tool call to stderr as JSON. Never includes file contents or passwords.
pub fn audit_log(tool: &str, server: Option<&str>, path: Option<&str>, status: &str, duration_ms: u64) {
    let entry = serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "tool": tool,
        "server": server.unwrap_or("-"),
        "path": path.unwrap_or("-"),
        "status": status,
        "duration_ms": duration_ms,
    });
    eprintln!("{}", entry);
}
