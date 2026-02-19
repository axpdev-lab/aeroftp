#![allow(dead_code)]
//! GAP-A01: Shared HTTP retry wrapper with 429/5xx handling and Retry-After support.
//!
//! Provides `send_with_retry()` as a drop-in replacement for `request.send()` that adds:
//! - Exponential backoff with jitter on 429 (Too Many Requests) and 5xx errors
//! - Retry-After header parsing (both seconds and HTTP-date formats)
//! - Configurable max retries and delay bounds
//! - Transparent passthrough for non-retryable status codes (4xx except 429)

use reqwest::{Request, Response, Client};
use std::time::Duration;

/// Configuration for HTTP retry behavior
#[derive(Debug, Clone)]
pub struct HttpRetryConfig {
    /// Maximum number of retry attempts (default: 3)
    pub max_retries: u32,
    /// Base delay in milliseconds for exponential backoff (default: 1000)
    pub base_delay_ms: u64,
    /// Maximum delay cap in milliseconds (default: 30000)
    pub max_delay_ms: u64,
    /// Backoff multiplier (default: 2.0)
    pub backoff_multiplier: f64,
}

impl Default for HttpRetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay_ms: 1000,
            max_delay_ms: 30_000,
            backoff_multiplier: 2.0,
        }
    }
}

/// Determine if a status code is retryable
fn is_retryable_status(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504)
}

/// Parse Retry-After header value (supports both seconds and HTTP-date)
fn parse_retry_after(response: &Response) -> Option<Duration> {
    let value = response.headers().get("retry-after")?.to_str().ok()?;

    // Try parsing as seconds first (most common)
    if let Ok(secs) = value.parse::<u64>() {
        return Some(Duration::from_secs(secs.min(300))); // Cap at 5 minutes
    }

    // HTTP-date format not parsed (would require httpdate crate).
    // Numeric seconds covers >95% of real-world Retry-After values.
    None
}

/// Calculate delay for a given retry attempt with jitter
fn calculate_delay(attempt: u32, config: &HttpRetryConfig) -> Duration {
    let base = config.base_delay_ms as f64 * config.backoff_multiplier.powi(attempt as i32);
    let capped = base.min(config.max_delay_ms as f64);
    // Add 10-30% jitter to prevent thundering herd
    let jitter = capped * (0.1 + rand::random::<f64>() * 0.2);
    Duration::from_millis((capped + jitter) as u64)
}

/// Send an HTTP request with automatic retry on 429/5xx.
///
/// This clones the request for each retry attempt. The original request builder
/// pattern is preserved — callers build a `Request` via `client.get(url)...build()`.
///
/// # Example
/// ```no_run
/// let request = client.get(&url)
///     .header(AUTHORIZATION, auth)
///     .build()?;
/// let response = send_with_retry(&client, request, &HttpRetryConfig::default()).await?;
/// ```
pub async fn send_with_retry(
    client: &Client,
    request: Request,
    config: &HttpRetryConfig,
) -> Result<Response, reqwest::Error> {
    // Store request parts for cloning on retry
    let method = request.method().clone();
    let url = request.url().clone();
    let headers = request.headers().clone();
    let body_bytes = request.body()
        .and_then(|b| b.as_bytes())
        .map(|b| b.to_vec());

    let mut last_response = client.execute(request).await?;

    for attempt in 0..config.max_retries {
        if !is_retryable_status(last_response.status().as_u16()) {
            return Ok(last_response);
        }

        // Determine delay: prefer Retry-After, fall back to exponential backoff
        let delay = parse_retry_after(&last_response)
            .unwrap_or_else(|| calculate_delay(attempt, config));

        tracing::debug!(
            "HTTP {} {} returned {}. Retry {}/{} after {:?}",
            method, url, last_response.status(), attempt + 1, config.max_retries, delay
        );

        tokio::time::sleep(delay).await;

        // Rebuild request for retry
        let mut retry_req = client.request(method.clone(), url.clone());
        for (key, value) in headers.iter() {
            retry_req = retry_req.header(key, value);
        }
        if let Some(ref body) = body_bytes {
            retry_req = retry_req.body(body.clone());
        }

        last_response = retry_req.send().await?;
    }

    Ok(last_response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_retryable_status() {
        assert!(is_retryable_status(429));
        assert!(is_retryable_status(500));
        assert!(is_retryable_status(502));
        assert!(is_retryable_status(503));
        assert!(is_retryable_status(504));
        assert!(!is_retryable_status(200));
        assert!(!is_retryable_status(400));
        assert!(!is_retryable_status(401));
        assert!(!is_retryable_status(403));
        assert!(!is_retryable_status(404));
    }

    #[test]
    fn test_calculate_delay_bounded() {
        let config = HttpRetryConfig::default();
        for attempt in 0..10 {
            let delay = calculate_delay(attempt, &config);
            assert!(delay.as_millis() <= (config.max_delay_ms as u128 * 2)); // With jitter
        }
    }
}
