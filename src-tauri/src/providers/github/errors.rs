//! GitHub-specific error taxonomy
//!
//! Maps GitHub API responses to structured errors with actionable user-facing
//! messages. Every variant tells the user *what happened* and *what to do next*.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use super::super::ProviderError;
use std::fmt;

/// GitHub-specific errors that provide richer context than the generic
/// [`ProviderError`] variants before being converted at the trait boundary.
#[derive(Debug)]
pub enum GitHubError {
    // ── Auth ────────────────────────────────────────────────────────
    /// 401 — token invalid or revoked.
    Unauthorized,
    /// Token present but expired (fine-grained PAT).
    TokenExpired,
    /// Token lacks the required scope/permission.
    InsufficientPermissions(String),
    /// Generic permission denied (e.g. GraphQL FORBIDDEN).
    PermissionDenied(String),

    // ── Repository ──────────────────────────────────────────────────
    /// 404 on the repo endpoint — wrong owner/repo or private without access.
    RepoNotFound,
    /// Named branch does not exist.
    BranchNotFound(String),
    /// File or directory path does not exist on the branch.
    PathNotFound(String),
    /// Generic not-found (used by GraphQL).
    NotFound(String),

    // ── Write policy ────────────────────────────────────────────────
    /// Branch is protected — direct pushes are blocked.
    ProtectedBranch(String),
    /// Repository rules require changes via pull request.
    RequiredPullRequest,
    /// Conflict: the file's SHA changed between read and write (struct form).
    StaleObject {
        path: String,
        expected_sha: String,
    },

    // ── Releases ────────────────────────────────────────────────────
    /// Attempted to upload an asset that already exists on the release.
    DuplicateReleaseAsset(String),
    /// Duplicate asset — alias used by releases_mode.
    DuplicateAsset(String),
    /// Release tag not found.
    ReleaseNotFound(String),

    // ── Rate limits ─────────────────────────────────────────────────
    /// Primary rate limit hit (X-RateLimit-Remaining = 0).
    PrimaryRateLimit {
        reset_at: u64,
    },
    /// Secondary (abuse) rate limit — Retry-After header present.
    SecondaryRateLimit {
        retry_after: u64,
    },

    // ── Transport ───────────────────────────────────────────────────
    /// DNS, TLS, or TCP-level failure.
    NetworkError(String),
    /// Non-classified REST API error.
    ApiError {
        status: u16,
        message: String,
    },
    /// Server-side error (5xx).
    ServerError(String),

    // ── Content ─────────────────────────────────────────────────────
    /// File exceeds the Contents API size limit (100 MB).
    FileTooLarge {
        size: u64,
        max: u64,
    },
    /// Payload too large (GraphQL).
    PayloadTooLarge(String),

    // ── GraphQL ─────────────────────────────────────────────────────
    /// GraphQL-level error with type and message.
    GraphQLError {
        error_type: String,
        message: String,
    },
    /// Parse/deserialization error.
    ParseError(String),
    /// Invalid input to a mutation.
    InvalidInput(String),
    /// Unprocessable entity (422 or GraphQL UNPROCESSABLE).
    Unprocessable(String),
}

impl fmt::Display for GitHubError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Auth
            Self::Unauthorized => write!(
                f,
                "GitHub token is invalid or revoked. Generate a new token at github.com/settings/tokens."
            ),
            Self::TokenExpired => write!(
                f,
                "GitHub token has expired. Refresh or regenerate it at github.com/settings/tokens."
            ),
            Self::InsufficientPermissions(scope) => write!(
                f,
                "Token lacks the '{}' permission. Edit your token scopes at github.com/settings/tokens.",
                scope
            ),
            Self::PermissionDenied(msg) => write!(f, "GitHub permission denied: {}", msg),

            // Repository
            Self::RepoNotFound => write!(
                f,
                "Repository not found. Check that owner/repo are correct and the token has access."
            ),
            Self::BranchNotFound(branch) => write!(
                f,
                "Branch '{}' does not exist. Check the branch name or create it first.",
                branch
            ),
            Self::PathNotFound(path) => write!(f, "Path '{}' not found on this branch.", path),
            Self::NotFound(msg) => write!(f, "Not found: {}", msg),

            // Write policy
            Self::ProtectedBranch(branch) => write!(
                f,
                "Branch '{}' is protected. Create a new branch to make changes.",
                branch
            ),
            Self::RequiredPullRequest => write!(
                f,
                "This repository requires changes via pull request."
            ),
            Self::StaleObject { path, .. } => write!(
                f,
                "File '{}' was modified since last read. Refresh and retry.",
                path
            ),

            // Releases
            Self::DuplicateReleaseAsset(name) | Self::DuplicateAsset(name) => write!(
                f,
                "Release asset '{}' already exists. Delete it first or use a different name.",
                name
            ),
            Self::ReleaseNotFound(tag) => write!(f, "Release '{}' not found.", tag),

            // Rate limits
            Self::PrimaryRateLimit { reset_at } => write!(
                f,
                "GitHub API rate limit reached. Resets at {}.",
                format_reset_timestamp(*reset_at)
            ),
            Self::SecondaryRateLimit { retry_after } => write!(
                f,
                "GitHub secondary rate limit hit. Retry after {} seconds.",
                retry_after
            ),

            // Transport
            Self::NetworkError(msg) => write!(f, "Network error: {}", msg),
            Self::ApiError { status, message } => write!(
                f,
                "GitHub API error (HTTP {}): {}",
                status, message
            ),
            Self::ServerError(msg) => write!(f, "GitHub server error: {}", msg),

            // Content
            Self::FileTooLarge { size, max } => write!(
                f,
                "File size ({}) exceeds GitHub limit ({}). Use Releases for large files.",
                format_bytes(*size),
                format_bytes(*max),
            ),
            Self::PayloadTooLarge(msg) => write!(f, "Payload too large: {}", msg),

            // GraphQL
            Self::GraphQLError { error_type, message } => write!(
                f,
                "GitHub GraphQL error ({}): {}",
                error_type, message
            ),
            Self::ParseError(msg) => write!(f, "Parse error: {}", msg),
            Self::InvalidInput(msg) => write!(f, "Invalid input: {}", msg),
            Self::Unprocessable(msg) => write!(f, "Unprocessable: {}", msg),
        }
    }
}

impl std::error::Error for GitHubError {}

impl From<GitHubError> for ProviderError {
    fn from(e: GitHubError) -> Self {
        match e {
            // Auth
            GitHubError::Unauthorized | GitHubError::TokenExpired => {
                ProviderError::AuthenticationFailed(e.to_string())
            }
            GitHubError::InsufficientPermissions(_) | GitHubError::PermissionDenied(_) => {
                ProviderError::PermissionDenied(e.to_string())
            }

            // Not found
            GitHubError::RepoNotFound
            | GitHubError::PathNotFound(_)
            | GitHubError::BranchNotFound(_)
            | GitHubError::ReleaseNotFound(_)
            | GitHubError::NotFound(_) => ProviderError::NotFound(e.to_string()),

            // Write policy
            GitHubError::ProtectedBranch(_) | GitHubError::RequiredPullRequest => {
                ProviderError::PermissionDenied(e.to_string())
            }

            // Conflict
            GitHubError::StaleObject { .. } => ProviderError::TransferFailed(e.to_string()),

            // Duplicate
            GitHubError::DuplicateReleaseAsset(_) | GitHubError::DuplicateAsset(_) => {
                ProviderError::AlreadyExists(e.to_string())
            }

            // Rate limits
            GitHubError::PrimaryRateLimit { .. } | GitHubError::SecondaryRateLimit { .. } => {
                ProviderError::ServerError(e.to_string())
            }

            // Transport
            GitHubError::NetworkError(_) => ProviderError::NetworkError(e.to_string()),
            GitHubError::ApiError { status, .. } if status == 408 || status == 504 => {
                ProviderError::Timeout
            }
            GitHubError::ApiError { .. } | GitHubError::ServerError(_) => {
                ProviderError::ServerError(e.to_string())
            }

            // Content
            GitHubError::FileTooLarge { .. } | GitHubError::PayloadTooLarge(_) => {
                ProviderError::TransferFailed(e.to_string())
            }

            // GraphQL / Parse / Input
            GitHubError::GraphQLError { .. } => ProviderError::ServerError(e.to_string()),
            GitHubError::ParseError(_) => ProviderError::ParseError(e.to_string()),
            GitHubError::InvalidInput(_) | GitHubError::Unprocessable(_) => {
                ProviderError::Other(e.to_string())
            }
        }
    }
}

/// Format bytes into a human-readable string (e.g., `"14.2 MB"`).
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Format a Unix timestamp into `HH:MM UTC`.
fn format_reset_timestamp(ts: u64) -> String {
    let secs_in_day = ts % 86400;
    let hours = secs_in_day / 3600;
    let minutes = (secs_in_day % 3600) / 60;
    format!("{:02}:{:02} UTC", hours, minutes)
}

/// Classify a GitHub REST API JSON error response into a typed error.
///
/// Inspects `status`, the `message` field, and optional `errors[].code` to
/// pick the most specific [`GitHubError`] variant.
pub fn classify_api_error(
    status: u16,
    body: &serde_json::Value,
    path_hint: Option<&str>,
) -> GitHubError {
    let message = body
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown error")
        .to_string();

    let error_code = body
        .get("errors")
        .and_then(|e| e.as_array())
        .and_then(|arr| arr.first())
        .and_then(|e| e.get("code"))
        .and_then(|c| c.as_str())
        .unwrap_or("");

    match status {
        401 => {
            if message.contains("token expired") || message.contains("expir") {
                GitHubError::TokenExpired
            } else {
                GitHubError::Unauthorized
            }
        }
        403 => {
            if message.contains("rate limit") {
                GitHubError::PrimaryRateLimit { reset_at: 0 }
            } else if message.contains("abuse") || message.contains("secondary") {
                GitHubError::SecondaryRateLimit { retry_after: 60 }
            } else if message.contains("push") || message.contains("protected") {
                GitHubError::ProtectedBranch(
                    path_hint.unwrap_or("unknown").to_string(),
                )
            } else {
                GitHubError::InsufficientPermissions(message)
            }
        }
        404 => {
            if let Some(path) = path_hint {
                GitHubError::PathNotFound(path.to_string())
            } else {
                GitHubError::RepoNotFound
            }
        }
        409 => {
            if error_code == "already_exists" {
                GitHubError::DuplicateReleaseAsset(
                    path_hint.unwrap_or("unknown").to_string(),
                )
            } else {
                GitHubError::StaleObject {
                    path: path_hint.unwrap_or("unknown").to_string(),
                    expected_sha: String::new(),
                }
            }
        }
        422 => {
            if message.contains("too_large") || error_code == "too_large" {
                GitHubError::FileTooLarge {
                    size: 0,
                    max: 100 * 1024 * 1024,
                }
            } else if message.contains("pull request") {
                GitHubError::RequiredPullRequest
            } else {
                GitHubError::ApiError { status, message }
            }
        }
        _ => GitHubError::ApiError { status, message },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(500), "500 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(10 * 1024 * 1024), "10.0 MB");
    }

    #[test]
    fn test_classify_401() {
        let body = serde_json::json!({"message": "Bad credentials"});
        let err = classify_api_error(401, &body, None);
        assert!(matches!(err, GitHubError::Unauthorized));
    }

    #[test]
    fn test_classify_404_with_path() {
        let body = serde_json::json!({"message": "Not Found"});
        let err = classify_api_error(404, &body, Some("src/main.rs"));
        assert!(matches!(err, GitHubError::PathNotFound(_)));
    }

    #[test]
    fn test_classify_403_rate_limit() {
        let body = serde_json::json!({"message": "API rate limit exceeded"});
        let err = classify_api_error(403, &body, None);
        assert!(matches!(err, GitHubError::PrimaryRateLimit { .. }));
    }

    #[test]
    fn test_provider_error_conversion() {
        let err: ProviderError = GitHubError::Unauthorized.into();
        assert!(matches!(err, ProviderError::AuthenticationFailed(_)));

        let err: ProviderError = GitHubError::PathNotFound("foo".into()).into();
        assert!(matches!(err, ProviderError::NotFound(_)));

        let err: ProviderError = GitHubError::DuplicateAsset("app.deb".into()).into();
        assert!(matches!(err, ProviderError::AlreadyExists(_)));
    }
}
