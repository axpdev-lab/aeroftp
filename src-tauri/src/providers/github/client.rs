//! GitHub HTTP client with authentication, API versioning, and rate-limit tracking
//!
//! Every request goes through [`GitHubHttpClient`] so that:
//! - The `Authorization` header is attached from [`SecretString`] without cloning
//!   the token into arbitrary heap buffers.
//! - The `X-GitHub-Api-Version` and `Accept` headers are always set.
//! - Rate-limit headers are extracted from **every** response.
//! - Non-2xx responses are classified into [`GitHubError`] variants.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use reqwest::{Client, Method, RequestBuilder, Response, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;

use super::errors::{classify_api_error, GitHubError};
use super::rate_limit::RateLimitState;
use crate::providers::sanitize_api_error;

/// Base URL for the GitHub REST API.
pub(super) const API_BASE: &str = "https://api.github.com";

/// Base URL for the GitHub GraphQL API.
const GRAPHQL_URL: &str = "https://api.github.com/graphql";

/// Upload host for release assets.
const UPLOADS_BASE: &str = "https://uploads.github.com";

/// User-Agent sent with every request (auto-derived from Cargo.toml version).
const USER_AGENT: &str = concat!("AeroFTP/", env!("CARGO_PKG_VERSION"));

/// GitHub API version header value.
const API_VERSION: &str = "2022-11-28";

/// Defensive cap for paginated REST listings.
const MAX_PAGINATION_PAGES: usize = 100;

/// HTTP client wrapper that handles auth, versioning, and rate-limit bookkeeping.
#[derive(Debug)]
pub struct GitHubHttpClient {
    pub(super) client: Client,
    pub(super) token: SecretString,
    rate_limit: RateLimitState,
}

impl GitHubHttpClient {
    /// Create a new client with the given personal access token.
    /// QA-GH-004: Returns Result instead of panicking on TLS init failure.
    pub fn new(token: SecretString) -> Result<Self, GitHubError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(USER_AGENT)
            .build()
            .map_err(|e| {
                GitHubError::NetworkError(format!("Failed to build HTTP client: {}", e))
            })?;

        Ok(Self {
            client,
            token,
            rate_limit: RateLimitState::new(),
        })
    }

    // ── Low-level helpers ──────────────────────────────────────────

    /// Build a [`RequestBuilder`] with auth and API-version headers.
    pub fn request(&self, method: Method, url: &str) -> RequestBuilder {
        self.client
            .request(method, url)
            .header(
                "Authorization",
                format!("Bearer {}", self.token.expose_secret()),
            )
            .header("X-GitHub-Api-Version", API_VERSION)
            .header("Accept", "application/vnd.github+json")
    }

    /// Send a request, extract rate-limit headers, and classify errors.
    ///
    /// Returns the raw [`Response`] on 2xx; maps everything else to
    /// [`GitHubError`].
    pub async fn execute(
        &mut self,
        builder: RequestBuilder,
        path_hint: Option<&str>,
    ) -> Result<Response, GitHubError> {
        let response = self.execute_unchecked(builder).await?;

        // Check for secondary rate limit (Retry-After header).
        if let Some(retry_after) = response.headers().get("retry-after") {
            if let Ok(s) = retry_after.to_str() {
                if let Ok(secs) = s.parse::<u64>() {
                    if response.status() == StatusCode::FORBIDDEN
                        || response.status() == StatusCode::TOO_MANY_REQUESTS
                    {
                        return Err(GitHubError::SecondaryRateLimit { retry_after: secs });
                    }
                }
            }
        }

        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }

        // Read the error body and classify.
        let body_text = response.text().await.unwrap_or_else(|_| String::from("{}"));
        let body_json: serde_json::Value = serde_json::from_str(&body_text)
            .unwrap_or_else(|_| serde_json::json!({ "message": sanitize_api_error(&body_text) }));

        let mut err = classify_api_error(status.as_u16(), &body_json, path_hint);

        // Enrich rate-limit errors with the actual reset timestamp.
        if let GitHubError::PrimaryRateLimit { ref mut reset_at } = err {
            *reset_at = self.rate_limit.reset_at;
        }

        Err(err)
    }

    /// API-GH-011: Execute with bounded retry for secondary rate limits and transient 5xx.
    /// - SecondaryRateLimit: sleep `retry_after` seconds, retry once
    /// - 5xx ServerError: sleep 1s, retry once
    /// - All other errors: return immediately
    pub async fn execute_with_retry(
        &mut self,
        builder: RequestBuilder,
        path_hint: Option<&str>,
    ) -> Result<Response, GitHubError> {
        // First attempt — try_clone() the builder so we can retry
        let retry_builder = builder.try_clone();
        match self.execute(builder, path_hint).await {
            Ok(resp) => Ok(resp),
            Err(GitHubError::SecondaryRateLimit { retry_after }) => {
                let wait = retry_after.min(120); // cap at 2 minutes
                log::info!(
                    "GitHub: secondary rate limit — waiting {}s before retry",
                    wait
                );
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                if let Some(rb) = retry_builder {
                    self.execute(rb, path_hint).await
                } else {
                    Err(GitHubError::SecondaryRateLimit { retry_after })
                }
            }
            Err(GitHubError::ServerError(ref msg)) => {
                let msg_owned = msg.clone();
                log::info!("GitHub: server error — retrying after 1s: {}", msg_owned);
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                if let Some(rb) = retry_builder {
                    self.execute(rb, path_hint).await
                } else {
                    Err(GitHubError::ServerError(msg_owned))
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Send a request and update rate-limit state without interpreting status.
    pub async fn execute_unchecked(
        &mut self,
        builder: RequestBuilder,
    ) -> Result<Response, GitHubError> {
        let response = builder.send().await.map_err(|e| {
            if e.is_timeout() {
                GitHubError::ApiError {
                    status: 408,
                    message: "request timed out".to_string(),
                }
            } else {
                GitHubError::NetworkError(e.to_string())
            }
        })?;

        self.rate_limit.update_from_headers(response.headers());
        Ok(response)
    }

    // ── High-level convenience methods ─────────────────────────────

    /// `GET` a JSON endpoint and deserialize the response body.
    ///
    /// `url` can be either a full URL or a path (e.g. `/repos/o/r/releases`).
    /// If it starts with `/`, the API base is prepended automatically.
    pub async fn get_json<T: DeserializeOwned>(&mut self, url: &str) -> Result<T, GitHubError> {
        let full_url = self.resolve_url(url)?;
        let builder = self.request(Method::GET, &full_url);
        let resp = self.execute_with_retry(builder, None).await?;
        resp.json::<T>()
            .await
            .map_err(|e| GitHubError::ParseError(format!("JSON parse error: {}", e)))
    }

    /// `GET` a paginated JSON array endpoint by following `Link: ... rel="next"`.
    pub async fn get_paginated_json_array<T: DeserializeOwned>(
        &mut self,
        url: &str,
    ) -> Result<Vec<T>, GitHubError> {
        let mut next_url = Some(self.resolve_url(url)?);
        let mut pages = 0usize;
        let mut all_items = Vec::new();

        while let Some(current_url) = next_url.take() {
            if pages >= MAX_PAGINATION_PAGES {
                return Err(GitHubError::ApiError {
                    status: 400,
                    message: format!(
                        "Pagination exceeded defensive cap of {} pages",
                        MAX_PAGINATION_PAGES
                    ),
                });
            }

            let builder = self.request(Method::GET, &current_url);
            let resp = self.execute_with_retry(builder, None).await?;
            let next_link = parse_next_link(
                resp.headers()
                    .get(reqwest::header::LINK)
                    .and_then(|value| value.to_str().ok()),
            );
            let mut page_items = resp
                .json::<Vec<T>>()
                .await
                .map_err(|e| GitHubError::ParseError(format!("JSON parse error: {}", e)))?;

            all_items.append(&mut page_items);
            next_url = next_link;
            pages += 1;
        }

        Ok(all_items)
    }

    /// `PUT` with a JSON body; returns the response JSON.
    pub async fn put_json(
        &mut self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, GitHubError> {
        let full_url = self.resolve_url(url)?;
        let builder = self.request(Method::PUT, &full_url).json(body);
        let resp = self.execute_with_retry(builder, None).await?;
        resp.json::<serde_json::Value>()
            .await
            .map_err(|e| GitHubError::ParseError(format!("JSON parse error: {}", e)))
    }

    /// `POST` with a JSON body; returns the response JSON.
    pub async fn post_json<T: DeserializeOwned>(
        &mut self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<T, GitHubError> {
        let full_url = self.resolve_url(url)?;
        let builder = self.request(Method::POST, &full_url).json(body);
        let resp = self.execute_with_retry(builder, None).await?;
        resp.json::<T>()
            .await
            .map_err(|e| GitHubError::ParseError(format!("JSON parse error: {}", e)))
    }

    /// `POST` with empty body. Returns `()` on 2xx.
    /// Used for Actions re-run / cancel endpoints that return 201/202.
    pub async fn post_empty(&mut self, url: &str) -> Result<(), GitHubError> {
        let full_url = self.resolve_url(url)?;
        let builder = self.request(Method::POST, &full_url);
        self.execute_with_retry(builder, None).await?;
        Ok(())
    }

    /// `PATCH` with a JSON body; returns the response JSON.
    pub async fn patch_json(
        &mut self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, GitHubError> {
        let full_url = self.resolve_url(url)?;
        let builder = self.request(Method::PATCH, &full_url).json(body);
        let resp = self.execute_with_retry(builder, None).await?;
        resp.json::<serde_json::Value>()
            .await
            .map_err(|e| GitHubError::ParseError(format!("JSON parse error: {}", e)))
    }

    /// `DELETE` with a JSON body. Returns `()` on success.
    pub async fn delete_json(
        &mut self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<(), GitHubError> {
        let full_url = self.resolve_url(url)?;
        let builder = self.request(Method::DELETE, &full_url).json(body);
        self.execute_with_retry(builder, None).await?;
        Ok(())
    }

    /// `DELETE` without body. Returns `()` on success.
    pub async fn delete(&mut self, url: &str) -> Result<(), GitHubError> {
        let full_url = self.resolve_url(url)?;
        let builder = self.request(Method::DELETE, &full_url);
        self.execute_with_retry(builder, None).await?;
        Ok(())
    }

    /// `POST` to the GraphQL endpoint. Returns the full JSON response.
    pub async fn graphql(
        &mut self,
        query: &str,
        variables: &serde_json::Value,
    ) -> Result<serde_json::Value, GitHubError> {
        let body = serde_json::json!({
            "query": query,
            "variables": variables,
        });
        let builder = self.request(Method::POST, GRAPHQL_URL).json(&body);
        let resp = self.execute_with_retry(builder, None).await?;
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| GitHubError::ParseError(format!("GraphQL parse error: {}", e)))?;

        // Check for GraphQL-level errors.
        if let Some(errors) = json.get("errors") {
            if let Some(arr) = errors.as_array() {
                if let Some(first) = arr.first() {
                    let msg = first
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("unknown GraphQL error");
                    return Err(GitHubError::ApiError {
                        status: 200,
                        message: format!("GraphQL error: {}", msg),
                    });
                }
            }
        }

        Ok(json)
    }

    /// `POST` raw GraphQL body (pre-built JSON). Returns the full JSON response
    /// without checking for GraphQL errors (caller handles them).
    pub async fn graphql_raw(
        &mut self,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, GitHubError> {
        let builder = self.request(Method::POST, GRAPHQL_URL).json(body);
        let resp = self.execute_with_retry(builder, None).await?;
        resp.json::<serde_json::Value>()
            .await
            .map_err(|e| GitHubError::ParseError(format!("GraphQL parse error: {}", e)))
    }

    /// Download raw file content (sets `Accept: application/octet-stream`).
    /// Returns the raw `Response` for streaming.
    pub async fn get_raw(&mut self, url: &str) -> Result<Response, GitHubError> {
        let full_url = self.resolve_url(url)?;
        let builder = self
            .client
            .get(&full_url)
            .header(
                "Authorization",
                format!("Bearer {}", self.token.expose_secret()),
            )
            .header("X-GitHub-Api-Version", API_VERSION)
            .header("Accept", "application/octet-stream")
            .header("User-Agent", USER_AGENT);

        self.execute_with_retry(builder, None).await
    }

    /// Download raw bytes (convenience wrapper over `get_raw`).
    pub async fn get_raw_bytes(&mut self, url: &str) -> Result<Vec<u8>, GitHubError> {
        let resp = self.get_raw(url).await?;
        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| GitHubError::NetworkError(e.to_string()))
    }

    /// Upload raw binary data (for release assets).
    /// Returns the raw `Response` for caller to handle.
    pub async fn upload_raw(
        &mut self,
        url: &str,
        content_type: &str,
        data: Vec<u8>,
    ) -> Result<Response, GitHubError> {
        let builder = self
            .client
            .post(url)
            .header(
                "Authorization",
                format!("Bearer {}", self.token.expose_secret()),
            )
            .header("X-GitHub-Api-Version", API_VERSION)
            .header("Content-Type", content_type)
            .header("User-Agent", USER_AGENT)
            .body(data);

        let response = self.execute_unchecked(builder).await?;

        // Check for secondary rate limit (Retry-After header) on upload responses.
        if let Some(retry_after) = response.headers().get("retry-after") {
            if let Ok(s) = retry_after.to_str() {
                if let Ok(secs) = s.parse::<u64>() {
                    if response.status() == StatusCode::FORBIDDEN
                        || response.status() == StatusCode::TOO_MANY_REQUESTS
                    {
                        return Err(GitHubError::SecondaryRateLimit { retry_after: secs });
                    }
                }
            }
        }

        Ok(response)
    }

    // ── Accessors ──────────────────────────────────────────────────

    /// Current rate-limit state snapshot.
    pub fn rate_limit(&self) -> &RateLimitState {
        &self.rate_limit
    }

    /// The API base URL.
    pub fn api_base(&self) -> &'static str {
        API_BASE
    }

    /// The uploads base URL (for release assets).
    pub fn uploads_base(&self) -> &'static str {
        UPLOADS_BASE
    }

    /// Resolve a URL: if it starts with `/`, prepend the API base.
    /// SEC-GH-004/005: Only allowlisted GitHub domains accepted for absolute URLs.
    /// Rejects http:// entirely (TLS enforcement). Unknown URLs are errors.
    fn resolve_url(&self, url: &str) -> Result<String, GitHubError> {
        if is_allowed_github_url(url) {
            Ok(url.to_string())
        } else if url.starts_with('/') {
            Ok(format!("{}{}", API_BASE, url))
        } else {
            log::warn!("resolve_url: rejected non-GitHub URL: {}", url);
            Err(GitHubError::InvalidInput(format!(
                "URL not on GitHub domain allowlist: {}",
                url
            )))
        }
    }

    /// Expose the token as a string reference for submodules that build
    /// their own requests (e.g. `repo_mode.rs` raw GET).
    pub(super) fn token_str(&self) -> &str {
        self.token.expose_secret()
    }
}

/// GitHub domain allowlist for absolute URLs.
/// SEC-GH-004/014: Any URL not matching these prefixes is rejected.
const ALLOWED_URL_PREFIXES: &[&str] = &[
    "https://api.github.com",
    "https://uploads.github.com",
    "https://raw.githubusercontent.com",
    "https://codeload.github.com",
    "https://github.com",
];

/// Check if a URL matches the GitHub domain allowlist.
/// Ensures the prefix is followed by `/` or `?` or end-of-string
/// to prevent subdomain spoofing (e.g., api.github.com.evil.com).
fn is_allowed_github_url(url: &str) -> bool {
    ALLOWED_URL_PREFIXES.iter().any(|prefix| {
        if !url.starts_with(prefix) {
            return false;
        }
        let rest = &url[prefix.len()..];
        rest.is_empty() || rest.starts_with('/') || rest.starts_with('?')
    })
}

fn parse_next_link(link_header: Option<&str>) -> Option<String> {
    let header = link_header?;

    for part in header.split(',') {
        let mut segments = part.split(';').map(str::trim);
        let url_part = segments.next()?;
        let is_next = segments.any(|segment| segment == "rel=\"next\"");

        if !is_next {
            continue;
        }

        if let Some(url) = url_part.strip_prefix('<').and_then(|s| s.strip_suffix('>')) {
            // SEC-GH-014: Validate pagination URL against GitHub domain allowlist
            if is_allowed_github_url(url) {
                return Some(url.to_string());
            }
            log::warn!(
                "parse_next_link: rejected non-GitHub pagination URL: {}",
                url
            );
            return None;
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::{is_allowed_github_url, parse_next_link};

    #[test]
    fn test_parse_next_link_extracts_next_url() {
        let header = concat!(
            "<https://api.github.com/repositories/1/branches?per_page=100&page=2>; rel=\"next\", ",
            "<https://api.github.com/repositories/1/branches?per_page=100&page=4>; rel=\"last\""
        );

        assert_eq!(
            parse_next_link(Some(header)).as_deref(),
            Some("https://api.github.com/repositories/1/branches?per_page=100&page=2")
        );
    }

    #[test]
    fn test_parse_next_link_returns_none_without_next() {
        let header =
            "<https://api.github.com/repositories/1/branches?per_page=100&page=4>; rel=\"last\"";
        assert_eq!(parse_next_link(Some(header)), None);
        assert_eq!(parse_next_link(None), None);
    }

    // SEC-GH-014: Pagination URL must be on allowlisted GitHub domain
    #[test]
    fn test_parse_next_link_rejects_non_github_url() {
        let header = "<https://evil.example.com/steal?token=xxx>; rel=\"next\"";
        assert_eq!(parse_next_link(Some(header)), None);
    }

    #[test]
    fn test_parse_next_link_rejects_http_url() {
        let header = "<http://api.github.com/repos/o/r?page=2>; rel=\"next\"";
        assert_eq!(parse_next_link(Some(header)), None);
    }

    // SEC-GH-004/005: URL allowlist validation
    #[test]
    fn test_is_allowed_github_url_accepts_valid() {
        assert!(is_allowed_github_url("https://api.github.com/repos/o/r"));
        assert!(is_allowed_github_url(
            "https://uploads.github.com/repos/o/r/releases/1/assets"
        ));
        assert!(is_allowed_github_url(
            "https://raw.githubusercontent.com/o/r/main/file"
        ));
        assert!(is_allowed_github_url(
            "https://codeload.github.com/o/r/tar.gz/v1"
        ));
        assert!(is_allowed_github_url(
            "https://github.com/login/device/code"
        ));
    }

    #[test]
    fn test_is_allowed_github_url_rejects_invalid() {
        assert!(!is_allowed_github_url("https://evil.com/api.github.com"));
        assert!(!is_allowed_github_url("http://api.github.com/repos"));
        assert!(!is_allowed_github_url("https://api.github.com.evil.com/"));
        assert!(!is_allowed_github_url("ftp://api.github.com/"));
        assert!(!is_allowed_github_url(""));
    }
}
