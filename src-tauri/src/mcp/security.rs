//! MCP security layer: path validation, rate limiting, input sanitization, audit logging
//!
//! - Path validation: no null bytes, no `..` traversal, max 4096 chars
//! - Rate limiting: token bucket per category (read/mutative/destructive)
//! - Input sanitization: max text lengths, reject control chars
//! - Audit logging: JSON on stderr, never logs file contents or passwords

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

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

/// Per-request MCP policy. Path checks precede I/O but are not atomic open capabilities.
#[derive(Debug)]
pub(crate) struct McpLocalPathPolicy {
    home: std::path::PathBuf,
    resolved_home: std::path::PathBuf,
    resolved_denied: Vec<std::path::PathBuf>,
    resolved_system: Vec<std::path::PathBuf>,
}

impl McpLocalPathPolicy {
    pub(crate) fn capture() -> Result<Self, String> {
        #[cfg(windows)]
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
        #[cfg(not(windows))]
        let home = std::env::var_os("HOME");
        Self::for_home(std::path::Path::new(&home.ok_or("MCP home unavailable")?))
    }

    pub(crate) fn for_home(home: &std::path::Path) -> Result<Self, String> {
        if !home.is_absolute() {
            return Err("MCP home must be absolute".into());
        }
        let resolved_home =
            std::fs::canonicalize(home).map_err(|_| "MCP home resolution refused")?;
        let resolved_denied = DENIED_HOME_DIRS
            .iter()
            .map(|dir| crate::sync_core::access::resolve_missing(&home.join(dir)))
            .collect::<Result<Vec<_>, _>>()?;
        let resolved_system = DENIED_SYSTEM_DIRS
            .iter()
            .map(|dir| crate::sync_core::access::resolve_missing(std::path::Path::new(dir)))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            home: home.to_path_buf(),
            resolved_home,
            resolved_denied,
            resolved_system,
        })
    }
}

fn policy_prefix(path: &std::path::Path, prefix: &std::path::Path) -> bool {
    #[cfg(not(windows))]
    {
        path.starts_with(prefix)
    }
    #[cfg(windows)]
    {
        fn comparison(path: &std::path::Path) -> Option<std::path::PathBuf> {
            let text = path.to_str()?.replace('/', "\\").to_lowercase();
            let text = if let Some(unc) = text.strip_prefix("\\\\?\\unc\\") {
                format!("\\\\{unc}")
            } else {
                text.strip_prefix("\\\\?\\").unwrap_or(&text).to_string()
            };
            Some(text.into())
        }
        match (comparison(path), comparison(prefix)) {
            (Some(path), Some(prefix)) => path.starts_with(prefix),
            _ => true, // Unrepresentable names fail closed.
        }
    }
}

impl crate::sync_core::access::LocalPathAuthorizer for McpLocalPathPolicy {
    fn validate(&self, path: &std::path::Path) -> Result<(), String> {
        let text = path.to_str().ok_or("unrepresentable local path")?;
        validate_remote_path(text)?;
        if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err("Path traversal is not allowed".into());
        }
        #[cfg(windows)]
        for part in path.components() {
            if let std::path::Component::Normal(name) = part {
                let name = name.to_str().ok_or("unrepresentable local path")?;
                if name.contains(':') || name.ends_with(['.', ' ']) {
                    return Err("Ambiguous Windows path refused".into());
                }
            }
        }
        let absolute = std::path::absolute(path).map_err(|_| "invalid local path")?;
        let check = |path: &std::path::Path| -> Result<(), String> {
            for prefix in DENIED_SYSTEM_DIRS {
                if policy_prefix(path, std::path::Path::new(prefix)) {
                    return Err("Access denied: system path".into());
                }
            }
            for denied in &self.resolved_system {
                if policy_prefix(path, denied) {
                    return Err("Access denied: resolved system path".into());
                }
            }
            for home in [&self.home, &self.resolved_home] {
                for dir in DENIED_HOME_DIRS {
                    if policy_prefix(path, &home.join(dir)) {
                        return Err(format!("Access denied: ~/{dir}"));
                    }
                }
            }
            Ok(())
        };
        check(path)?;
        check(&absolute)?;
        let effective = crate::sync_core::access::resolve_missing(&absolute)?;
        check(&effective)?;
        // A protected directory may itself be a symlink (for example an SSH
        // folder kept on another volume). Protect its effective root too, and
        // refresh it at admission if the protected link has changed since capture.
        for denied in &self.resolved_denied {
            if policy_prefix(&absolute, denied) || policy_prefix(&effective, denied) {
                return Err("Access denied: resolved home path".into());
            }
        }
        for dir in DENIED_HOME_DIRS {
            let denied = crate::sync_core::access::resolve_missing(&self.home.join(dir))?;
            if policy_prefix(&absolute, &denied) || policy_prefix(&effective, &denied) {
                return Err("Access denied: resolved home path".into());
            }
        }
        Ok(())
    }
}

const DENIED_SYSTEM_DIRS: &[&str] = &[
    "/etc",
    "/boot",
    "/proc",
    "/sys",
    "/dev",
    "/usr/sbin",
    "/sbin",
    "/root",
    "/run/secrets",
];

pub fn validate_local_path(path: &str) -> Result<(), String> {
    use crate::sync_core::access::LocalPathAuthorizer;
    validate_remote_path(path)?;
    McpLocalPathPolicy::capture()?.validate(std::path::Path::new(path))
}

const DENIED_HOME_DIRS: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".kube",
    ".config/gcloud",
    ".docker",
    ".config/aeroftp",
    ".vault-token",
];

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
        return Err(format!(
            "Text content exceeds {} character limit",
            MAX_TEXT_LEN
        ));
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
            RateCategory::ReadOnly => 1200,
            RateCategory::Mutative => 400,
            RateCategory::Destructive => 100,
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
        for cat in [
            RateCategory::ReadOnly,
            RateCategory::Mutative,
            RateCategory::Destructive,
        ] {
            buckets.insert(cat, TokenBucket::new(cat.max_per_minute()));
        }
        Self {
            buckets: Mutex::new(buckets),
        }
    }

    /// Try to consume a token. Returns Ok(()) if allowed, Err with retry-after seconds.
    /// Bypassed when `AEROFTP_MCP_RATE_LIMIT_DISABLED` is set to `1` or `true`.
    pub fn check(&self, category: RateCategory) -> Result<(), f64> {
        if rate_limit_disabled() {
            return Ok(());
        }
        let mut buckets = self.buckets.lock().unwrap();
        let bucket = buckets.get_mut(&category).unwrap();
        if bucket.try_consume() {
            Ok(())
        } else {
            Err(bucket.retry_after().as_secs_f64())
        }
    }
}

fn rate_limit_disabled() -> bool {
    match std::env::var("AEROFTP_MCP_RATE_LIMIT_DISABLED") {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes" || v == "on"
        }
        Err(_) => false,
    }
}

// ─── Audit Logging ───────────────────────────────────────────────────

/// Log a tool call to stderr as JSON. Never includes file contents or passwords.
pub fn audit_log(
    tool: &str,
    server: Option<&str>,
    path: Option<&str>,
    status: &str,
    duration_ms: u64,
) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync_core::access::{LocalAccessScope, LocalPathAuthorizer};
    use std::{path::Path, sync::Arc};

    #[test]
    fn requested103_home_prefixes_and_missing_tails() {
        let home = tempfile::tempdir().unwrap();
        let policy = McpLocalPathPolicy::for_home(home.path()).unwrap();
        for rel in DENIED_HOME_DIRS {
            assert!(policy.validate(&home.path().join(rel)).is_err(), "{rel}");
            assert!(
                policy
                    .validate(&home.path().join(rel).join("new/key"))
                    .is_err(),
                "{rel}"
            );
            assert!(
                policy
                    .validate(&home.path().join(format!("{rel}-backup/new/key")))
                    .is_ok(),
                "{rel}"
            );
        }
        assert!(policy.validate(Path::new("/etc/shadow")).is_err());
        #[cfg(unix)]
        assert!(policy.validate(Path::new("/etcetera/new/key")).is_ok());
    }

    #[test]
    fn requested103_scope_checks_prefixes_root_and_native_names() {
        let home = tempfile::tempdir().unwrap();
        let scope = LocalAccessScope::new(
            home.path(),
            Arc::new(McpLocalPathPolicy::for_home(home.path()).unwrap()),
        )
        .unwrap();
        for (rel, denied) in [
            (".ssh/new/key", ".ssh"),
            (".config/aeroftp/db", ".config/aeroftp"),
            (".vault-token", ".vault-token"),
        ] {
            assert_eq!(scope.validate_relative(rel).unwrap_err().rel_path, denied);
        }
        assert!(scope.validate_relative("../escape").is_err());
        assert!(scope.validate_relative("sub/..notes").is_ok());
        #[cfg(unix)]
        assert!(scope.validate_relative(r"sub\literal").is_ok());
        assert!(scope.validate_root(&home.path().join("other")).is_err());
        assert!(LocalAccessScope::new(
            &home.path().join(".ssh"),
            Arc::new(McpLocalPathPolicy::for_home(home.path()).unwrap())
        )
        .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn requested103_aliases_symlinked_home_and_dangling_links() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let real_home = tmp.path().join("real-home");
        std::fs::create_dir_all(real_home.join(".ssh")).unwrap();
        let home = tmp.path().join("home");
        symlink(&real_home, &home).unwrap();
        let policy = McpLocalPathPolicy::for_home(&home).unwrap();
        symlink(real_home.join(".ssh"), tmp.path().join("alias")).unwrap();
        symlink(tmp.path().join("missing"), tmp.path().join("dangling")).unwrap();
        for path in [
            home.join(".ssh/key"),
            real_home.join(".ssh/key"),
            tmp.path().join("alias/new/key"),
            tmp.path().join("dangling/new/key"),
        ] {
            assert!(policy.validate(&path).is_err(), "{}", path.display());
        }
        symlink(tmp.path().join("loop"), tmp.path().join("loop")).unwrap();
        assert!(policy.validate(&tmp.path().join("loop/new")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn requested103_swap_before_admission_and_unrepresentable_names() {
        use std::os::unix::{ffi::OsStrExt, fs::symlink};
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("safe")).unwrap();
        std::fs::create_dir(home.path().join(".ssh")).unwrap();
        symlink(home.path().join("safe"), home.path().join("alias")).unwrap();
        let scope = LocalAccessScope::new(
            home.path(),
            Arc::new(McpLocalPathPolicy::for_home(home.path()).unwrap()),
        )
        .unwrap();
        assert!(scope.validate_relative("alias/new/key").is_ok());
        std::fs::remove_file(home.path().join("alias")).unwrap();
        symlink(home.path().join(".ssh"), home.path().join("alias")).unwrap();
        assert_eq!(
            scope
                .validate_relative("alias/new/key")
                .unwrap_err()
                .rel_path,
            "alias"
        );
        let bad = home.path().join(std::ffi::OsStr::from_bytes(b"bad\xff"));
        assert!(scope.validate_local(&bad).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn requested103_protected_directory_itself_is_a_symlink() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let secrets = tmp.path().join("secrets");
        let replacement = tmp.path().join("replacement");
        for path in [&home, &secrets, &replacement] {
            std::fs::create_dir(path).unwrap();
        }
        symlink(&secrets, home.join(".ssh")).unwrap();
        let policy = McpLocalPathPolicy::for_home(&home).unwrap();
        assert!(policy.validate(&secrets.join("new/key")).is_err());
        symlink(&secrets, tmp.path().join("alias")).unwrap();
        assert!(policy.validate(&tmp.path().join("alias/new/key")).is_err());
        std::fs::remove_file(home.join(".ssh")).unwrap();
        symlink(&replacement, home.join(".ssh")).unwrap();
        assert!(policy.validate(&replacement.join("new/key")).is_err());
        assert!(policy.validate(&secrets.join("old/key")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn requested103_permission_failure_does_not_become_missing_parent() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let opaque = home.path().join("opaque");
        std::fs::create_dir(&opaque).unwrap();
        let policy = McpLocalPathPolicy::for_home(home.path()).unwrap();
        std::fs::set_permissions(&opaque, std::fs::Permissions::from_mode(0)).unwrap();
        let result = policy.validate(&opaque.join("new/key"));
        std::fs::set_permissions(&opaque, std::fs::Permissions::from_mode(0o700)).unwrap();
        // A root test runner can traverse mode 0000; do not claim a permission proof there.
        if unsafe { libc::geteuid() } != 0 {
            assert!(result.is_err());
        }
    }

    #[cfg(windows)]
    #[test]
    fn requested103_windows_case_verbatim_ads_and_trailing_aliases() {
        let home = tempfile::tempdir().unwrap();
        let policy = McpLocalPathPolicy::for_home(home.path()).unwrap();
        let canonical = std::fs::canonicalize(home.path()).unwrap();
        assert!(policy.validate(&canonical.join(".SSH/new/key")).is_err());
        for rel in [
            ".ssh./new/key",
            ".ssh /new/key",
            ".ssh:stream",
            ".ssh::$INDEX_ALLOCATION/new/key",
        ] {
            assert!(policy.validate(&home.path().join(rel)).is_err(), "{rel}");
        }
        assert!(policy
            .validate(&canonical.join(".ssh-backup/new/key"))
            .is_ok());
    }
}
