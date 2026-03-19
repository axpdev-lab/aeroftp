//! GitHub HTTP client with authentication, API versioning, and rate-limit tracking
//!
//! Every request goes through [`GitHubHttpClient`] so that:
//! - The `Authorization` header is attached from [`SecretString`] without cloning
//!   the token into arbitrary heap buffers.
//! - The `X-GitHub-Api-Version` and `Accept` headers are always set.
//! - Rate-limit headers are extracted from **every** response.
//! - Non-2xx responses are classified into [`GitHubError`] variants.

use reqwest::{Client, Method, RequestBuilder, Response, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;

use super::errors::{classify_api_error, GitHubError};
use super::rate_limit::RateLimitState;
use crate::providers::sanitize_api_error;

/// Base URL for the GitHub REST API.
const API_BASE: &str = "https://api.github.com";

/// Base URL for the GitHub GraphQL API.
#[allow(dead_code)]
const GRAPHQL_URL: &str = "https://api.github.com/graphql";

/// Upload host for release assets.
#[allow(dead_code)]
const UPLOADS_BASE: &str = "https://uploads.github.com";

/// User-Agent sent with every request.
const USER_AGENT: &str = "AeroFTP/2.9.9";

/// GitHub API version header value.
const API_VERSION: &str = "2022-11-28";

/// HTTP client wrapper that handles auth, versioning, and rate-limit bookkeeping.
#[derive(Debug)]
#[allow(dead_code)]
pub struct GitHubHttpClient {
    pub(super) client: Client,
    pub(super) token: SecretString,
    rate_limit: RateLimitState,
}

#[allow(dead_code)]
impl GitHubHttpClient {
    /// Create a new client with the given personal access token.
    pub fn new(token: SecretString) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(USER_AGENT)
            .build()
            .expect("failed to build reqwest client");

        Self {
            client,
            token,
            rate_limit: RateLimitState::new(),
        }
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
                        return Err(GitHubError::SecondaryRateLimit {
                            retry_after: secs,
                        });
                    }
                }
            }
        }

        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }

        // Read the error body and classify.
        let body_text = response
            .text()
            .await
            .unwrap_or_else(|_| String::from("{}"));
        let body_json: serde_json::Value =
            serde_json::from_str(&body_text).unwrap_or_else(|_| {
                serde_json::json!({ "message": sanitize_api_error(&body_text) })
            });

        let mut err = classify_api_error(status.as_u16(), &body_json, path_hint);

        // Enrich rate-limit errors with the actual reset timestamp.
        if let GitHubError::PrimaryRateLimit { ref mut reset_at } = err {
            *reset_at = self.rate_limit.reset_at;
        }

        Err(err)
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
    pub async fn get_json<T: DeserializeOwned>(
        &mut self,
        url: &str,
    ) -> Result<T, GitHubError> {
        let full_url = self.resolve_url(url);
        let builder = self.request(Method::GET, &full_url);
        let resp = self.execute(builder, None).await?;
        resp.json::<T>().await.map_err(|e| GitHubError::ParseError(
            format!("JSON parse error: {}", e),
        ))
    }

    /// `PUT` with a JSON body; returns the response JSON.
    pub async fn put_json(
        &mut self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, GitHubError> {
        let full_url = self.resolve_url(url);
        let builder = self.request(Method::PUT, &full_url).json(body);
        let resp = self.execute(builder, None).await?;
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
        let full_url = self.resolve_url(url);
        let builder = self.request(Method::POST, &full_url).json(body);
        let resp = self.execute(builder, None).await?;
        resp.json::<T>()
            .await
            .map_err(|e| GitHubError::ParseError(format!("JSON parse error: {}", e)))
    }

    /// `PATCH` with a JSON body; returns the response JSON.
    pub async fn patch_json(
        &mut self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, GitHubError> {
        let full_url = self.resolve_url(url);
        let builder = self.request(Method::PATCH, &full_url).json(body);
        let resp = self.execute(builder, None).await?;
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
        let full_url = self.resolve_url(url);
        let builder = self.request(Method::DELETE, &full_url).json(body);
        self.execute(builder, None).await?;
        Ok(())
    }

    /// `DELETE` without body. Returns `()` on success.
    pub async fn delete(
        &mut self,
        url: &str,
    ) -> Result<(), GitHubError> {
        let full_url = self.resolve_url(url);
        let builder = self.request(Method::DELETE, &full_url);
        self.execute(builder, None).await?;
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
        let resp = self.execute(builder, None).await?;
        let json: serde_json::Value =
            resp.json().await.map_err(|e| GitHubError::ParseError(
                format!("GraphQL parse error: {}", e),
            ))?;

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
        let resp = self.execute(builder, None).await?;
        resp.json::<serde_json::Value>()
            .await
            .map_err(|e| GitHubError::ParseError(format!("GraphQL parse error: {}", e)))
    }

    /// Download raw file content (sets `Accept: application/octet-stream`).
    /// Returns the raw `Response` for streaming.
    pub async fn get_raw(
        &mut self,
        url: &str,
    ) -> Result<Response, GitHubError> {
        let full_url = self.resolve_url(url);
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

        self.execute(builder, None).await
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
                        return Err(GitHubError::SecondaryRateLimit {
                            retry_after: secs,
                        });
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
    fn resolve_url(&self, url: &str) -> String {
        if url.starts_with("http://") || url.starts_with("https://") {
            url.to_string()
        } else if url.starts_with('/') {
            format!("{}{}", API_BASE, url)
        } else {
            url.to_string()
        }
    }

    /// Expose the token as a string reference for submodules that build
    /// their own requests (e.g. `repo_mode.rs` raw GET).
    pub(super) fn token_str(&self) -> &str {
        self.token.expose_secret()
    }
}
