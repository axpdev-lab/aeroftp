//! GitHub-specific error taxonomy
//!
//! Maps GitHub API responses to structured errors with actionable user-facing
//! messages. Every variant tells the user *what happened* and *what to do next*.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use super::super::ProviderError;
use std::fmt;

/// GitHub-specific errors that provide richer context than the generic
/// [`ProviderError`] variants before being converted at the trait boundary.
#[derive(Debug)]
pub enum GitHubError {
    // ── Auth ────────────────────────────────────────────────────────
    /// 401: token invalid or revoked.
    Unauthorized,
    /// Token present but expired (fine-grained PAT).
    TokenExpired,
    /// Token lacks the required scope/permission.
    InsufficientPermissions(String),
    /// Generic permission denied (e.g. GraphQL FORBIDDEN).
    PermissionDenied(String),

    // ── Repository ──────────────────────────────────────────────────
    /// 404 on the repo endpoint: wrong owner/repo or private without access.
    RepoNotFound,
    /// Named branch does not exist.
    BranchNotFound(String),
    /// File or directory path does not exist on the branch.
    PathNotFound(String),
    /// Generic not-found (used by GraphQL).
    NotFound(String),

    // ── Write policy ────────────────────────────────────────────────
    /// Branch is protected: direct pushes are blocked.
    ProtectedBranch(String),
    /// Repository rules require changes via pull request.
    RequiredPullRequest,
    /// Conflict: the file's SHA changed between read and write (struct form).
    StaleObject { path: String, expected_sha: String },

    // ── Releases ────────────────────────────────────────────────────
    /// Attempted to upload an asset that already exists on the release.
    DuplicateAsset(String),
    /// Release tag not found.
    ReleaseNotFound(String),

    // ── Rate limits ─────────────────────────────────────────────────
    /// Primary rate limit hit (X-RateLimit-Remaining = 0).
    PrimaryRateLimit { reset_at: u64 },
    /// Secondary (abuse) rate limit: Retry-After header present.
    SecondaryRateLimit { retry_after: u64 },

    // ── Transport ───────────────────────────────────────────────────
    /// DNS, TLS, or TCP-level failure.
    NetworkError(String),
    /// Non-classified REST API error.
    ApiError { status: u16, message: String },
    /// Server-side error (5xx).
    ServerError(String),

    // ── Content ─────────────────────────────────────────────────────
    /// File exceeds the Contents API size limit (100 MB).
    FileTooLarge { size: u64, max: u64 },
    /// Payload too large (GraphQL).
    PayloadTooLarge(String),

    // ── GraphQL ─────────────────────────────────────────────────────
    /// GraphQL-level error with type and message.
    GraphQLError { error_type: String, message: String },
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
            Self::DuplicateAsset(name) => write!(
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

impl GitHubError {
    /// The message to hand to [`ProviderError`], with this type's own prefix
    /// removed where the target variant is about to write the same words.
    ///
    /// G93: `ProviderError` derives its text with `thiserror`, so every arm
    /// prepends a label of its own (`Path not found: {0}`, `Parse error: {0}`,
    /// ...). Six arms here already open with those same words, and the
    /// conversion used to pass `to_string()` straight through, so a missing
    /// file came back as `Path not found: Not found: ...` and a bad payload as
    /// `Parse error: Parse error: ...`. The duplication came from the pair of
    /// types, not from the payload, which is why no amount of fixing call
    /// sites removed it.
    ///
    /// Only those six are stripped. The arms that carry a sentence of their
    /// own (`Branch 'x' does not exist. Check the branch name or create it
    /// first.`, `Release 'x' not found.`) read correctly under their label and
    /// are left exactly as they are: the owner's decision was to remove the
    /// duplication, not to rewrite every GitHub error.
    ///
    /// The label the user finally reads still comes from `ProviderError`, so
    /// the not-found vocabulary that [`message_names_a_missing_path`] looks
    /// for is still in the text: `Path not found: <payload>` keeps the words
    /// that turn an error into an absence.
    ///
    /// [`message_names_a_missing_path`]: crate::providers::types::message_names_a_missing_path
    fn message_for_provider_error(&self) -> String {
        match self {
            // "Permission denied: " + "GitHub permission denied: ..."
            Self::PermissionDenied(msg) => msg.clone(),
            // "Path not found: " + "Path '...' not found on this branch."
            Self::PathNotFound(path) => format!("{} (on this branch)", path),
            // "Path not found: " + "Not found: ..."
            Self::NotFound(msg) => msg.clone(),
            // "Network error: " + "Network error: ..."
            Self::NetworkError(msg) => msg.clone(),
            // "Server error: " + "GitHub server error: ..."
            Self::ServerError(msg) => msg.clone(),
            // "Parse error: " + "Parse error: ..."
            Self::ParseError(msg) => msg.clone(),

            // Everything else keeps its own sentence. This arm is written out
            // variant by variant on purpose, with no `_` catch-all: a variant
            // added later that collides with a ProviderError label would be
            // absorbed in silence by a wildcard, which is exactly how the
            // doubling survived unnoticed in the first place. The compiler is
            // the only reader that will still be here.
            Self::Unauthorized
            | Self::TokenExpired
            | Self::InsufficientPermissions(_)
            | Self::RepoNotFound
            | Self::BranchNotFound(_)
            | Self::ProtectedBranch(_)
            | Self::RequiredPullRequest
            | Self::StaleObject { .. }
            | Self::DuplicateAsset(_)
            | Self::ReleaseNotFound(_)
            | Self::PrimaryRateLimit { .. }
            | Self::SecondaryRateLimit { .. }
            | Self::ApiError { .. }
            | Self::FileTooLarge { .. }
            | Self::PayloadTooLarge(_)
            | Self::GraphQLError { .. }
            | Self::InvalidInput(_)
            | Self::Unprocessable(_) => self.to_string(),
        }
    }
}

impl From<GitHubError> for ProviderError {
    fn from(e: GitHubError) -> Self {
        let text = e.message_for_provider_error();
        match e {
            // Auth
            GitHubError::Unauthorized | GitHubError::TokenExpired => {
                ProviderError::AuthenticationFailed(text)
            }
            GitHubError::InsufficientPermissions(_) | GitHubError::PermissionDenied(_) => {
                ProviderError::PermissionDenied(text)
            }

            // Not found
            GitHubError::RepoNotFound
            | GitHubError::PathNotFound(_)
            | GitHubError::BranchNotFound(_)
            | GitHubError::ReleaseNotFound(_)
            | GitHubError::NotFound(_) => ProviderError::NotFound(text),

            // Write policy
            GitHubError::ProtectedBranch(_) | GitHubError::RequiredPullRequest => {
                ProviderError::PermissionDenied(text)
            }

            // Conflict
            GitHubError::StaleObject { .. } => ProviderError::TransferFailed(text),

            // Duplicate
            GitHubError::DuplicateAsset(_) => ProviderError::AlreadyExists(text),

            // Rate limits
            GitHubError::PrimaryRateLimit { .. } | GitHubError::SecondaryRateLimit { .. } => {
                ProviderError::ServerError(text)
            }

            // Transport
            GitHubError::NetworkError(_) => ProviderError::NetworkError(text),
            GitHubError::ApiError { status, .. } if status == 408 || status == 504 => {
                ProviderError::Timeout
            }
            GitHubError::ApiError { .. } | GitHubError::ServerError(_) => {
                ProviderError::ServerError(text)
            }

            // Content
            GitHubError::FileTooLarge { .. } | GitHubError::PayloadTooLarge(_) => {
                ProviderError::TransferFailed(text)
            }

            // GraphQL / Parse / Input
            GitHubError::GraphQLError { .. } => ProviderError::ServerError(text),
            GitHubError::ParseError(_) => ProviderError::ParseError(text),
            GitHubError::InvalidInput(_) | GitHubError::Unprocessable(_) => {
                ProviderError::Other(text)
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
                GitHubError::ProtectedBranch(path_hint.unwrap_or("unknown").to_string())
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
                GitHubError::DuplicateAsset(path_hint.unwrap_or("unknown").to_string())
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
        // API-GH-004: Explicitly classify 5xx as ServerError for retry logic
        500..=599 => GitHubError::ServerError(format!("HTTP {}: {}", status, message)),
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

    /// G103, the twin of the test above, and the reason the hint has to reach
    /// this function: the same 404 body classifies as a missing REPOSITORY
    /// when no path is named. That is correct here (without a path there is
    /// nothing else this can be) and wrong at the call site that had a path
    /// and passed `None`, which is what `get_json_at` now fixes. Reading the
    /// two tests together says what the caller owes this function.
    #[test]
    fn a_404_without_a_path_hint_can_only_be_read_as_a_missing_repo() {
        let body = serde_json::json!({"message": "Not Found"});
        let err = classify_api_error(404, &body, None);
        assert!(
            matches!(err, GitHubError::RepoNotFound),
            "a 404 with no path named has nothing else to blame: {err}"
        );
        let with_path = classify_api_error(404, &body, Some("docs/missing.md"));
        assert!(
            matches!(with_path, GitHubError::PathNotFound(_)),
            "the same body names the path when the caller provides it: {with_path}"
        );
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

    // API-GH-004: 5xx explicitly classified as ServerError
    #[test]
    fn test_classify_500_server_error() {
        let body = serde_json::json!({"message": "Internal Server Error"});
        let err = classify_api_error(500, &body, None);
        assert!(matches!(err, GitHubError::ServerError(_)));
    }

    #[test]
    fn test_classify_502_server_error() {
        let body = serde_json::json!({"message": "Bad Gateway"});
        let err = classify_api_error(502, &body, None);
        assert!(matches!(err, GitHubError::ServerError(_)));
    }

    #[test]
    fn test_classify_503_server_error() {
        let body = serde_json::json!({"message": "Service Unavailable"});
        let err = classify_api_error(503, &body, None);
        assert!(matches!(err, GitHubError::ServerError(_)));
    }

    // Verify protected branch classification for structured matching
    #[test]
    fn test_classify_403_protected_branch() {
        let body = serde_json::json!({"message": "Cannot push to protected branch"});
        let err = classify_api_error(403, &body, None);
        assert!(matches!(err, GitHubError::ProtectedBranch(_)));
    }

    #[test]
    fn test_classify_403_primary_rate_limit_explicit() {
        let body = serde_json::json!({"message": "API rate limit exceeded for user"});
        let err = classify_api_error(403, &body, None);
        assert!(matches!(err, GitHubError::PrimaryRateLimit { .. }));
    }

    #[test]
    fn test_classify_422_pull_request_required() {
        let body = serde_json::json!({"message": "Required pull request reviews before merging"});
        let err = classify_api_error(422, &body, None);
        assert!(matches!(err, GitHubError::RequiredPullRequest));
    }

    /// G93: the six arms whose own prefix collided with the `ProviderError`
    /// label. Each assertion falsifies the defect by counting the words, not
    /// by reading the sentence: before the fix the text read
    /// `Path not found: Not found: gone.txt`.
    #[test]
    fn provider_error_does_not_repeat_the_label_of_the_variant() {
        let cases: Vec<(ProviderError, &str)> = vec![
            (GitHubError::NotFound("gone.txt".into()).into(), "not found"),
            (
                GitHubError::PathNotFound("docs/gone.md".into()).into(),
                "not found",
            ),
            (
                GitHubError::PermissionDenied("write access required".into()).into(),
                "permission denied",
            ),
            (
                GitHubError::NetworkError("dns failure".into()).into(),
                "network error",
            ),
            (
                GitHubError::ServerError("502 bad gateway".into()).into(),
                "server error",
            ),
            (
                GitHubError::ParseError("unexpected token".into()).into(),
                "parse error",
            ),
        ];
        for (err, label) in cases {
            let text = err.to_string().to_ascii_lowercase();
            assert_eq!(
                text.matches(label).count(),
                1,
                "the label `{label}` appears more than once in `{text}`"
            );
        }
    }

    /// The other half of the owner's decision: the arms that carry a sentence
    /// of their own are NOT rewritten, so their wording has to survive the
    /// conversion untouched.
    #[test]
    fn provider_error_keeps_the_sentences_that_were_already_good() {
        let err: ProviderError = GitHubError::BranchNotFound("gh-pages".into()).into();
        assert!(
            err.to_string()
                .contains("Branch 'gh-pages' does not exist. Check the branch name"),
            "branch guidance was rewritten: {err}"
        );

        let err: ProviderError = GitHubError::ReleaseNotFound("v4.2.0".into()).into();
        assert!(
            err.to_string().contains("Release 'v4.2.0' not found."),
            "release wording was rewritten: {err}"
        );

        let err: ProviderError = GitHubError::DuplicateAsset("app.deb".into()).into();
        assert!(
            err.to_string()
                .contains("Release asset 'app.deb' already exists."),
            "duplicate-asset guidance was rewritten: {err}"
        );
    }

    /// The guard named in the entry, and it is a guard on the VARIANT rather
    /// than on the words.
    ///
    /// `message_names_a_missing_path` reads the final text, and that text
    /// opens with the label `ProviderError::NotFound` derives, so as long as
    /// the conversion keeps choosing that variant the not-found vocabulary is
    /// there by construction. What this test can still catch is the change
    /// that would really lose an absence: a future edit routing these arms to
    /// `Other` or `ServerError`, whose labels say nothing about a missing
    /// path. Both halves are asserted so the reason is visible: the variant
    /// first, the text it produces second.
    #[test]
    fn a_stripped_not_found_still_maps_to_the_not_found_variant() {
        use crate::providers::types::message_names_a_missing_path_for;

        let err: ProviderError = GitHubError::PathNotFound("docs/gone.md".into()).into();
        assert!(
            matches!(err, ProviderError::NotFound(_)),
            "a missing path must stay a NotFound, or the absence is lost: {err}"
        );
        assert!(
            message_names_a_missing_path_for(&err.to_string(), "docs/gone.md"),
            "absence lost after stripping: {err}"
        );

        let err: ProviderError = GitHubError::NotFound("docs/gone.md".into()).into();
        assert!(
            matches!(err, ProviderError::NotFound(_)),
            "a missing path must stay a NotFound, or the absence is lost: {err}"
        );
        assert!(
            message_names_a_missing_path_for(&err.to_string(), "docs/gone.md"),
            "absence lost after stripping: {err}"
        );
    }

    /// The `Display` of `GitHubError` itself is deliberately untouched: it is
    /// what the logs print, and it is read without a `ProviderError` label in
    /// front of it.
    #[test]
    fn the_github_display_keeps_its_own_prefix() {
        assert_eq!(
            GitHubError::NotFound("gone.txt".into()).to_string(),
            "Not found: gone.txt"
        );
        assert_eq!(
            GitHubError::ParseError("unexpected token".into()).to_string(),
            "Parse error: unexpected token"
        );
    }
}
