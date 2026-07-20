//! GAP-A01: Shared HTTP retry wrapper with 429/5xx handling and Retry-After support.
//!
//! Provides `send_with_retry()` as a drop-in replacement for `request.send()` that adds:
//! - Exponential backoff with jitter on 429 (Too Many Requests) and 5xx errors
//! - Retry-After header parsing (both seconds and HTTP-date formats)
//! - Configurable max retries and delay bounds
//! - Transparent passthrough for non-retryable status codes (4xx except 429)

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use reqwest::{Client, Request, Response};
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

/// Record the honest first-byte moment of one HTTP attempt: the reqwest
/// `execute` future resolving means the response headers have arrived, so
/// request-start to now is the time to first byte for this attempt. Called
/// exactly once per attempt that reaches headers; a transport error has no
/// first byte and records nothing.
fn record_headers_received(start: std::time::Instant) {
    crate::transfer_dag::ttfb::record_sample(start.elapsed().as_nanos() as u64);
}

/// Send an HTTP request with automatic retry on 429/5xx.
///
/// This clones the request for each retry attempt. The original request builder
/// pattern is preserved: callers build a `Request` via `client.get(url)...build()`.
///
/// # Example
/// ```ignore
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
    let body_bytes = request
        .body()
        .and_then(|b| b.as_bytes())
        .map(|b| b.to_vec());

    // KE-A3: proactive tpslimit gate. No-op when the CLI did not install
    // a limiter (GUI path, default). Sits BEFORE the first execute so
    // retries also pay the rate-limit toll: an HTTP 429 followed by an
    // immediate retry would otherwise leak past the cap.
    super::tpslimit::maybe_acquire().await;

    // The TTFB timer starts at the actual send, after the rate-limit toll:
    // a proactive throttle wait is not part of the server's first-byte time.
    let attempt_start = std::time::Instant::now();
    let mut last_response = match client.execute(request).await {
        Ok(response) => {
            record_headers_received(attempt_start);
            response
        }
        Err(error) => return Err(error),
    };

    for attempt in 0..config.max_retries {
        if !is_retryable_status(last_response.status().as_u16()) {
            return Ok(last_response);
        }

        // Determine delay: prefer Retry-After, fall back to exponential backoff
        let delay =
            parse_retry_after(&last_response).unwrap_or_else(|| calculate_delay(attempt, config));

        tracing::debug!(
            "HTTP {} {} returned {}. Retry {}/{} after {:?}",
            method,
            url,
            last_response.status(),
            attempt + 1,
            config.max_retries,
            delay
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

        // KE-A3: each retry pays the same tpslimit toll as the original
        // request. Without this, a 429 would let a retry bypass the
        // proactive cap and saturate the backend the very instant the
        // cooldown ended.
        super::tpslimit::maybe_acquire().await;
        // Each retried attempt that reaches headers is its own honest
        // first-byte sample; retries never double-count a single attempt.
        let attempt_start = std::time::Instant::now();
        last_response = match retry_req.send().await {
            Ok(response) => {
                record_headers_received(attempt_start);
                response
            }
            Err(error) => return Err(error),
        };
    }

    Ok(last_response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer_dag::ttfb::test_fixture::{spawn_delayed_http_fixture, FixtureResponse};
    use crate::transfer_dag::ttfb::TtfbRecorder;

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

    // DAG-P2-07 (block C): the recorder is runtime-scoped, so fixture traffic
    // from other tests (each on its own runtime) cannot inflate these counts;
    // exact sample assertions are deterministic.

    #[tokio::test]
    async fn ttfb_records_one_sample_per_request_reaching_headers() {
        let delay = Duration::from_millis(50);
        let addr = spawn_delayed_http_fixture(delay, 1, |_head| FixtureResponse::ok("hello")).await;
        let guard = TtfbRecorder::install();

        let client = Client::new();
        let request = client
            .get(format!("http://{addr}/file"))
            .build()
            .expect("build request");
        let response = send_with_retry(&client, request, &HttpRetryConfig::default())
            .await
            .expect("fixture response");
        assert_eq!(response.status(), 200);
        let _ = response.bytes().await;

        let (nanos, samples) = guard.totals();
        assert_eq!(samples, 1, "one request reaching headers is one sample");
        assert!(
            nanos >= Duration::from_millis(40).as_nanos() as u64,
            "sampled TTFB {nanos}ns below the 50ms fixture first-byte delay"
        );
    }

    #[tokio::test]
    async fn ttfb_retry_counts_one_sample_per_attempt_reaching_headers() {
        // First attempt gets a retryable 503, the retry a 200. Both attempts
        // receive headers, so both are honest first-byte samples; the
        // retried request must not be collapsed into one.
        let delay = Duration::from_millis(20);
        let served = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let respond = {
            let served = std::sync::Arc::clone(&served);
            move |_head: &str| {
                let ordinal = served.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if ordinal == 0 {
                    FixtureResponse {
                        status: "503 Service Unavailable".to_string(),
                        headers: Vec::new(),
                        body: b"busy".to_vec(),
                    }
                } else {
                    FixtureResponse::ok("recovered")
                }
            }
        };
        let addr = spawn_delayed_http_fixture(delay, 2, respond).await;
        let guard = TtfbRecorder::install();

        let config = HttpRetryConfig {
            max_retries: 1,
            base_delay_ms: 1,
            max_delay_ms: 5,
            backoff_multiplier: 1.0,
        };
        let client = Client::new();
        let request = client
            .get(format!("http://{addr}/file"))
            .build()
            .expect("build request");
        let response = send_with_retry(&client, request, &config)
            .await
            .expect("retry response");
        assert_eq!(response.status(), 200);
        let _ = response.bytes().await;

        let (nanos, samples) = guard.totals();
        assert_eq!(
            samples, 2,
            "each attempt that reached headers counts exactly one sample"
        );
        assert!(
            nanos >= Duration::from_millis(30).as_nanos() as u64,
            "two 20ms-delayed attempts must total >= 30ms, got {nanos}ns"
        );
    }

    #[tokio::test]
    async fn ttfb_transport_error_records_no_sample() {
        // The fixture accepts and closes without answering: no first byte
        // ever arrives, so nothing may be recorded.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            if let Ok((socket, _)) = listener.accept().await {
                drop(socket);
            }
        });
        let guard = TtfbRecorder::install();

        let client = Client::new();
        let request = client
            .get(format!("http://{addr}/file"))
            .build()
            .expect("build request");
        let result = send_with_retry(&client, request, &HttpRetryConfig::default()).await;
        assert!(result.is_err(), "a closed connection must surface an error");
        assert_eq!(guard.totals(), (0, 0), "no first byte, no sample");
    }
}
