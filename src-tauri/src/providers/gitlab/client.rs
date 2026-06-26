//! GitLab HTTP client with PRIVATE-TOKEN auth and rate-limit tracking
//!
//! Simplified version of the GitHub client, adapted for GitLab API v4.
//! Key differences:
//! - Auth via `PRIVATE-TOKEN` header (not Bearer)
//! - API base is configurable (self-hosted instances)
//! - Pagination via `x-next-page` header (not Link header)
//! - No API version header required

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use reqwest::{Client, Method, RequestBuilder, Response, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;

use crate::providers::{
    http_retry::{send_with_retry, HttpRetryConfig},
    sanitize_api_error, ProviderError,
};

/// User-Agent sent with every request.
const USER_AGENT: &str = concat!("AeroFTP/", env!("CARGO_PKG_VERSION"));

/// Defensive cap for paginated REST listings.
const MAX_PAGINATION_PAGES: usize = 100;

/// Rate-limit state extracted from GitLab response headers.
#[derive(Debug)]
struct RateLimitState {
    remaining: Option<u64>,
}

impl RateLimitState {
    fn new() -> Self {
        Self { remaining: None }
    }

    fn update_from_headers(&mut self, headers: &reqwest::header::HeaderMap) {
        if let Some(val) = headers.get("ratelimit-remaining") {
            if let Ok(s) = val.to_str() {
                self.remaining = s.parse().ok();
            }
        }
    }
}

/// HTTP client wrapper for GitLab API v4.
#[derive(Debug)]
pub struct GitLabHttpClient {
    client: Client,
    token: SecretString,
    api_base: String,
    rate_limit: RateLimitState,
}

impl GitLabHttpClient {
    /// Create a new client.
    ///
    /// `api_base` should be e.g. `https://gitlab.com/api/v4` or
    /// `https://self-hosted.example.com/api/v4`.
    pub fn new(
        token: SecretString,
        api_base: String,
        accept_invalid_certs: bool,
    ) -> Result<Self, ProviderError> {
        let client = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .read_timeout(std::time::Duration::from_secs(1800))
            .user_agent(USER_AGENT)
            .danger_accept_invalid_certs(accept_invalid_certs)
            .build()
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!("Failed to build HTTP client: {}", e))
            })?;

        Ok(Self {
            client,
            token,
            api_base,
            rate_limit: RateLimitState::new(),
        })
    }

    /// Build a request with auth header.
    fn request(&self, method: Method, url: &str) -> RequestBuilder {
        self.client
            .request(method, url)
            .header("PRIVATE-TOKEN", self.token.expose_secret())
    }

    /// Resolve a URL path to a full URL.
    fn resolve_url(&self, path: &str) -> String {
        if path.starts_with("https://") || path.starts_with("http://") {
            path.to_string()
        } else {
            format!("{}{}", self.api_base, path)
        }
    }

    /// Send a request, update rate-limit, classify errors.
    async fn execute(&mut self, builder: RequestBuilder) -> Result<Response, ProviderError> {
        let request = builder
            .build()
            .map_err(|e| ProviderError::NetworkError(format!("Failed to build request: {}", e)))?;
        let response = send_with_retry(&self.client, request, &HttpRetryConfig::default())
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::Timeout
                } else {
                    ProviderError::ConnectionFailed(format!("GitLab network error: {}", e))
                }
            })?;

        self.rate_limit.update_from_headers(response.headers());

        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }

        // Rate limit: classified without reading the body (the message is fixed),
        // preserving the original early return that never consumed the body.
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(Self::classify_gitlab_error(status.as_u16(), ""));
        }

        let body: String = response.text().await.unwrap_or_else(|_| String::from("{}"));
        Err(Self::classify_gitlab_error(status.as_u16(), &body))
    }

    /// Pure status+body -> ProviderError classifier (extracted from `execute` so
    /// it can be unit-tested without a live `reqwest::Response`). Mirrors the
    /// original inline match exactly; `execute` short-circuits 429 before reading
    /// the body but routes through here so the rate-limit message stays identical.
    fn classify_gitlab_error(status: u16, body: &str) -> ProviderError {
        if status == 429 {
            return ProviderError::Other(
                "GitLab rate limit exceeded. Please wait and try again.".to_string(),
            );
        }
        let sanitized = sanitize_api_error(body);
        match status {
            401 => {
                ProviderError::AuthenticationFailed("GitLab: Invalid or expired token".to_string())
            }
            403 => {
                ProviderError::PermissionDenied(format!("GitLab: Access denied - {}", sanitized))
            }
            404 => ProviderError::NotFound(format!("GitLab: Resource not found - {}", sanitized)),
            _ => ProviderError::Other(format!("GitLab API error ({}): {}", status, sanitized)),
        }
    }

    /// Execute with single retry for 5xx errors.
    async fn execute_with_retry(
        &mut self,
        builder: RequestBuilder,
    ) -> Result<Response, ProviderError> {
        let retry_builder = builder.try_clone();
        match self.execute(builder).await {
            Ok(resp) => Ok(resp),
            Err(ref e) if is_server_error(e) => {
                log::info!("GitLab: server error, retrying after 1s");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                if let Some(rb) = retry_builder {
                    self.execute(rb).await
                } else {
                    Err(ProviderError::Other(
                        "GitLab: server error (retry failed)".into(),
                    ))
                }
            }
            Err(e) => Err(e),
        }
    }

    // ── High-level convenience methods ─────────────────────────────

    /// GET a JSON endpoint.
    pub async fn get_json<T: DeserializeOwned>(&mut self, path: &str) -> Result<T, ProviderError> {
        let url = self.resolve_url(path);
        let builder = self.request(Method::GET, &url);
        let resp = self.execute_with_retry(builder).await?;
        resp.json::<T>()
            .await
            .map_err(|e| ProviderError::Other(format!("GitLab JSON parse error: {}", e)))
    }

    /// GET paginated array using `x-next-page` header.
    pub async fn get_paginated<T: DeserializeOwned>(
        &mut self,
        path: &str,
        per_page: u32,
    ) -> Result<Vec<T>, ProviderError> {
        let mut all_items = Vec::new();
        let mut page = 1u32;

        for _ in 0..MAX_PAGINATION_PAGES {
            let separator = if path.contains('?') { '&' } else { '?' };
            let url = self.resolve_url(&format!(
                "{}{}per_page={}&page={}",
                path, separator, per_page, page
            ));
            let builder = self.request(Method::GET, &url);
            let resp = self.execute_with_retry(builder).await?;

            let next_page = resp
                .headers()
                .get("x-next-page")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u32>().ok());

            let mut items: Vec<T> = resp
                .json()
                .await
                .map_err(|e| ProviderError::Other(format!("GitLab JSON parse error: {}", e)))?;

            all_items.append(&mut items);

            match next_page {
                Some(np) if np > page => page = np,
                _ => break,
            }
        }

        Ok(all_items)
    }

    /// POST JSON body, return typed response.
    pub async fn post_json<T: DeserializeOwned>(
        &mut self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<T, ProviderError> {
        let url = self.resolve_url(path);
        let builder = self.request(Method::POST, &url).json(body);
        let resp = self.execute_with_retry(builder).await?;
        resp.json::<T>()
            .await
            .map_err(|e| ProviderError::Other(format!("GitLab JSON parse error: {}", e)))
    }

    /// GET raw bytes (file download).
    pub async fn get_raw_bytes(&mut self, path: &str) -> Result<Vec<u8>, ProviderError> {
        let url = self.resolve_url(path);
        let builder = self.request(Method::GET, &url);
        let resp = self.execute_with_retry(builder).await?;
        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| ProviderError::Other(format!("GitLab download error: {}", e)))
    }

    /// GET raw response (for streaming downloads with progress).
    pub async fn get_raw_response(&mut self, path: &str) -> Result<Response, ProviderError> {
        let url = self.resolve_url(path);
        let builder = self.request(Method::GET, &url);
        self.execute_with_retry(builder).await
    }

    /// HEAD request: returns true if 2xx, false if 404, error otherwise.
    pub async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        let url = self.resolve_url(path);
        let builder = self.request(Method::HEAD, &url);
        match self.execute(builder).await {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// PUT raw bytes (for Generic Packages upload).
    /// No retry: body bytes cannot be cloned by reqwest, so retry would fail silently.
    pub async fn put_bytes(
        &mut self,
        path: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> Result<Response, ProviderError> {
        let url = self.resolve_url(path);
        let builder = self
            .request(Method::PUT, &url)
            .header("Content-Type", content_type)
            .body(bytes);
        self.execute(builder).await
    }

    /// DELETE request.
    pub async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let url = self.resolve_url(path);
        let builder = self.request(Method::DELETE, &url);
        self.execute_with_retry(builder).await?;
        Ok(())
    }

    /// API base URL.
    pub fn api_base(&self) -> &str {
        &self.api_base
    }
}

fn is_server_error(err: &ProviderError) -> bool {
    matches!(err, ProviderError::Other(msg) if msg.contains("API error (5"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Row 4 (#347): the GitLab `execute` status match, now a pure classifier.
    #[test]
    fn classify_gitlab_error_maps_status_to_variant() {
        assert!(matches!(
            GitLabHttpClient::classify_gitlab_error(401, "{}"),
            ProviderError::AuthenticationFailed(_)
        ));
        assert!(matches!(
            GitLabHttpClient::classify_gitlab_error(403, r#"{"message":"403 Forbidden"}"#),
            ProviderError::PermissionDenied(_)
        ));
        assert!(matches!(
            GitLabHttpClient::classify_gitlab_error(404, r#"{"message":"404 Not Found"}"#),
            ProviderError::NotFound(_)
        ));
        // 429 keeps its fixed rate-limit message and ignores the body.
        match GitLabHttpClient::classify_gitlab_error(429, "anything") {
            ProviderError::Other(msg) => assert!(msg.contains("rate limit"), "got: {msg}"),
            e => panic!("expected Other, got {e:?}"),
        }
    }

    // The 5xx catch-all must carry "API error (5..)" so `execute_with_retry`'s
    // `is_server_error` gate fires and retries the request once.
    #[test]
    fn classify_gitlab_error_5xx_is_retryable_via_is_server_error() {
        for code in [500u16, 502, 503] {
            let err = GitLabHttpClient::classify_gitlab_error(code, "boom");
            match &err {
                ProviderError::Other(msg) => {
                    assert!(msg.contains(&format!("API error ({code})")), "got: {msg}");
                }
                e => panic!("expected Other, got {e:?}"),
            }
            assert!(
                is_server_error(&err),
                "HTTP {code} error must be classified as retryable"
            );
        }
        // A 4xx (not 401/403/404) is Other but NOT retryable.
        let four = GitLabHttpClient::classify_gitlab_error(418, "teapot");
        assert!(matches!(four, ProviderError::Other(_)));
        assert!(
            !is_server_error(&four),
            "4xx must not be retried as a server error"
        );
    }
}
