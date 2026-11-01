//! GAP-A01: Shared HTTP retry wrapper with 429/5xx handling and Retry-After support.
//!
//! Provides `send_with_retry()` as a drop-in replacement for `request.send()` that adds:
//! - Exponential backoff on 429, and on 5xx only when replay is safe
//! - Retry-After header parsing (both seconds and HTTP-date formats)
//! - Configurable max retries and delay bounds
//! - Transparent passthrough for non-retryable status codes (4xx except 429)

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use reqwest::{Client, Request, Response};
use std::time::Duration;

/// A replayable body is not proof that repeating the operation is safe: a
/// gateway may return 5xx after the service committed a create. RPC-style
/// APIs can mutate through GET, while a POST can also be a read-only query.
#[derive(Debug, Clone, Copy, Default)]
pub enum ServerErrorRetries {
    /// Conservative default for ordinary HTTP APIs. Writes need an explicit
    /// operation-level decision, even PUT (which can create another revision).
    #[default]
    SafeMethods,
    /// The caller has established that repeating this operation is safe.
    SafeOperation,
    /// Only a refusal (429) may be retried, regardless of the HTTP method.
    Never,
}

/// The audited shared-HTTP providers own request replay decisions. A batch
/// executor must not start the entire upload again after an uncertain write,
/// including a timeout or a malformed acknowledgement. Overlays forward this
/// policy to the underlying provider. Protocol-specific recovery stays there.
pub(super) fn manages_upload_retries_for(provider: super::ProviderType) -> bool {
    use super::ProviderType;
    matches!(
        provider,
        ProviderType::S3
            | ProviderType::Azure
            | ProviderType::Swift
            | ProviderType::Backblaze
            | ProviderType::FileLu
            | ProviderType::PCloud
            | ProviderType::FourShared
            | ProviderType::Jottacloud
            | ProviderType::KDrive
            | ProviderType::Koofr
            | ProviderType::Internxt
            | ProviderType::Twake
            | ProviderType::GitLab
            | ProviderType::Filen
            | ProviderType::ZohoWorkdrive
            | ProviderType::WebDav
    )
}

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
    /// Whether a server error permits repeating the operation. This never
    /// overrides the requirement that the request body can actually be replayed.
    pub server_error_retries: ServerErrorRetries,
}

impl Default for HttpRetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay_ms: 1000,
            max_delay_ms: 30_000,
            backoff_multiplier: 2.0,
            server_error_retries: ServerErrorRetries::SafeMethods,
        }
    }
}

impl HttpRetryConfig {
    /// Opt in only for a read-only or otherwise proven safe operation, not
    /// merely because its request body can be rebuilt.
    pub fn for_safe_operation(mut self) -> Self {
        self.server_error_retries = ServerErrorRetries::SafeOperation;
        self
    }

    /// Use for state-changing RPCs sent as GET and other ambiguous writes.
    pub fn refused_only(mut self) -> Self {
        self.server_error_retries = ServerErrorRetries::Never;
        self
    }
}

/// Determine if a status code is retryable
fn is_retryable_status(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504)
}

fn can_retry_response(status: u16, method: &reqwest::Method, config: &HttpRetryConfig) -> bool {
    if !is_retryable_status(status) {
        return false;
    }
    if status == 429 {
        return true;
    }
    match config.server_error_retries {
        ServerErrorRetries::SafeMethods => {
            matches!(
                *method,
                reqwest::Method::GET | reqwest::Method::HEAD | reqwest::Method::OPTIONS
            )
        }
        ServerErrorRetries::SafeOperation => true,
        ServerErrorRetries::Never => false,
    }
}

/// Parse Retry-After header value (supports both seconds and HTTP-date)
pub(crate) fn parse_retry_after(response: &Response) -> Option<Duration> {
    let value = response.headers().get("retry-after")?.to_str().ok()?;
    parse_retry_after_value(value, std::time::SystemTime::now())
}

fn parse_retry_after_value(value: &str, now: std::time::SystemTime) -> Option<Duration> {
    let value = value.trim();
    if let Ok(secs) = value.parse::<u64>() {
        return Some(Duration::from_secs(secs.min(300)));
    }
    let date = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    // A date already reached (or a client clock ahead of the server's) asks
    // for no wait at all: callers then apply their own backoff instead of
    // retrying at once against a server that has just refused.
    let remaining = date
        .signed_duration_since(chrono::DateTime::<chrono::Utc>::from(now))
        .to_std()
        .ok()
        .filter(|remaining| !remaining.is_zero())?;
    Some(remaining.min(Duration::from_secs(300)))
}

/// Calculate delay for a given retry attempt with jitter
pub(crate) fn calculate_delay(attempt: u32, config: &HttpRetryConfig) -> Duration {
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
    // Saturate like the executor's duration_nanos: u128 nanos beyond u64
    // range clamp to u64::MAX instead of wrapping.
    let nanos = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
    crate::transfer_dag::ttfb::record_sample(nanos);
}

/// Send a replayable request with retries on refusals and safe server errors.
/// A 5xx on a write is returned unchanged by default: it may have committed.
/// The caller can reconcile it, but the transport must not blindly repeat it.
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
    // Store request parts for rebuilding on retry. The body is copied once
    // into refcounted `Bytes`, so every retry clones a pointer, not the
    // payload. Callers that own their payload as `Bytes` already should use
    // [`send_with_retry_replayable`] and skip this copy altogether.
    let method = request.method().clone();
    let url = request.url().clone();
    let headers = request.headers().clone();
    let body_bytes = request
        .body()
        .and_then(|b| b.as_bytes())
        .map(bytes::Bytes::copy_from_slice);
    // The first attempt sends the request exactly as built, so a streaming
    // body (a multipart form over a file, for instance) goes out intact; only
    // a retry rebuilds from the captured parts. A body that could not be
    // captured as bytes is consumed by that first attempt and gets no retry:
    // rebuilt from method, URL and headers alone it would go out empty under
    // the original Content-Type and Content-Length, and a server could store
    // that empty upload and acknowledge it.
    let replayable = request.body().is_none() || body_bytes.is_some();
    let single_attempt;
    let config = if replayable {
        config
    } else {
        single_attempt = HttpRetryConfig {
            max_retries: 0,
            ..config.clone()
        };
        &single_attempt
    };
    let mut first = Some(request);
    send_with_retry_replayable(
        client,
        move || {
            if let Some(request) = first.take() {
                return reqwest::RequestBuilder::from_parts(client.clone(), request);
            }
            let mut req = client.request(method.clone(), url.clone());
            for (key, value) in headers.iter() {
                req = req.header(key, value);
            }
            if let Some(body) = &body_bytes {
                req = req.body(body.clone());
            }
            req
        },
        config,
    )
    .await
}

/// [`send_with_retry`] for callers that can rebuild the request on demand.
///
/// `build` is called once per attempt and must return an equivalent request
/// each time (same method, URL, headers and payload). This is the zero-copy
/// path for large bodies: a caller holding its payload as `bytes::Bytes`
/// clones a refcount per attempt instead of the bytes, and a paced
/// (rate-limited) streaming body, which cannot be replayed from a built
/// `Request`, is simply built again. Every attempt pays the tpslimit toll and
/// records its own honest TTFB sample, exactly as the built-request variant.
pub async fn send_with_retry_replayable<F>(
    client: &Client,
    mut build: F,
    config: &HttpRetryConfig,
) -> Result<Response, reqwest::Error>
where
    F: FnMut() -> reqwest::RequestBuilder,
{
    let _ = client;
    // KE-A3: proactive tpslimit gate. No-op when the CLI did not install
    // a limiter (GUI path, default). Sits BEFORE the first execute so
    // retries also pay the rate-limit toll: an HTTP 429 followed by an
    // immediate retry would otherwise leak past the cap.
    super::tpslimit::maybe_acquire().await;

    // The TTFB timer starts at the actual send, after the rate-limit toll:
    // a proactive throttle wait is not part of the server's first-byte time.
    // Keep the builder's own client, including its transport settings. A
    // built request also exposes the actual method without cloning a stream.
    let (attempt_client, request) = build().build_split();
    let request = request?;
    let mut method = request.method().clone();
    let attempt_start = std::time::Instant::now();
    let mut last_response = match attempt_client.execute(request).await {
        Ok(response) => {
            record_headers_received(attempt_start);
            response
        }
        Err(error) => return Err(error),
    };

    for attempt in 0..config.max_retries {
        if !can_retry_response(last_response.status().as_u16(), &method, config) {
            if is_retryable_status(last_response.status().as_u16()) {
                tracing::warn!(method = %method, status = %last_response.status(),
                    "Not repeating an operation after an ambiguous server error; it may have completed. Check remote state before retrying.");
            }
            return Ok(last_response);
        }

        // Determine delay: prefer Retry-After, fall back to exponential backoff
        let delay =
            parse_retry_after(&last_response).unwrap_or_else(|| calculate_delay(attempt, config));

        tracing::debug!(
            "HTTP {} returned {}. Retry {}/{} after {:?}",
            method,
            last_response.status(),
            attempt + 1,
            config.max_retries,
            delay
        );

        tokio::time::sleep(delay).await;

        // KE-A3: each retry pays the same tpslimit toll as the original
        // request. Without this, a 429 would let a retry bypass the
        // proactive cap and saturate the backend the very instant the
        // cooldown ended.
        super::tpslimit::maybe_acquire().await;
        // Each retried attempt that reaches headers is its own honest
        // first-byte sample; retries never double-count a single attempt.
        let (attempt_client, request) = build().build_split();
        let request = request?;
        method = request.method().clone();
        let attempt_start = std::time::Instant::now();
        last_response = match attempt_client.execute(request).await {
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
    fn retry_after_parses_seconds_and_http_dates_with_a_finite_cap() {
        let now = std::time::UNIX_EPOCH + Duration::from_secs(1_445_412_480);
        assert_eq!(
            parse_retry_after_value(" 12 ", now),
            Some(Duration::from_secs(12))
        );
        assert_eq!(
            parse_retry_after_value("999", now),
            Some(Duration::from_secs(300))
        );
        assert_eq!(
            parse_retry_after_value("Wed, 21 Oct 2015 07:28:10 GMT", now),
            Some(Duration::from_secs(10))
        );
        // A date in the past, or exactly now, is no explicit wait.
        assert_eq!(
            parse_retry_after_value("Wed, 21 Oct 2015 07:27:00 GMT", now),
            None
        );
        assert_eq!(
            parse_retry_after_value("Wed, 21 Oct 2015 07:28:00 GMT", now),
            None
        );
        assert_eq!(
            parse_retry_after_value("Thu, 22 Oct 2015 07:28:00 GMT", now),
            Some(Duration::from_secs(300))
        );
        for invalid in ["", "-1", "1.5", "later"] {
            assert_eq!(parse_retry_after_value(invalid, now), None);
        }
    }

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
            ..Default::default()
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

    /// A server that reads every request body to the end, answers 503 to the
    /// first request and 200 to the others, and records the body length of
    /// each request it received.
    async fn busy_once_server() -> (String, std::sync::Arc<std::sync::Mutex<Vec<usize>>>) {
        refusal_once_server(axum::http::StatusCode::SERVICE_UNAVAILABLE).await
    }

    async fn refusal_once_server(
        first_status: axum::http::StatusCode,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<usize>>>) {
        use futures_util::StreamExt;
        let seen: std::sync::Arc<std::sync::Mutex<Vec<usize>>> = Default::default();
        let recorded = std::sync::Arc::clone(&seen);
        let app = axum::Router::new().route(
            "/up",
            axum::routing::any(move |body: axum::body::Body| {
                let recorded = std::sync::Arc::clone(&recorded);
                async move {
                    let mut stream = body.into_data_stream();
                    let mut len = 0;
                    while let Some(Ok(chunk)) = stream.next().await {
                        len += chunk.len();
                    }
                    let mut seen = recorded.lock().unwrap();
                    seen.push(len);
                    if seen.len() == 1 {
                        first_status
                    } else {
                        axum::http::StatusCode::OK
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        (format!("http://{addr}/up"), seen)
    }

    fn fast_retries() -> HttpRetryConfig {
        HttpRetryConfig {
            max_retries: 2,
            base_delay_ms: 1,
            max_delay_ms: 5,
            backoff_multiplier: 1.0,
            ..Default::default()
        }
    }

    /// A streamed body cannot be sent twice. The 503 is returned as it came,
    /// after one request that carried the whole body; it used to be followed
    /// by a retry with the same headers and no body, which the server
    /// answered 200.
    #[tokio::test]
    async fn a_streamed_body_is_sent_once_and_never_retried_empty() {
        let (url, seen) = busy_once_server().await;
        let client = Client::new();
        let chunks = futures_util::stream::iter([
            Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"stre")),
            Ok(bytes::Bytes::from_static(b"amed")),
        ]);
        let request = client
            .post(&url)
            .body(reqwest::Body::wrap_stream(chunks))
            .build()
            .unwrap();
        let response = send_with_retry(&client, request, &fast_retries())
            .await
            .unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [8],
            "requests and their body lengths"
        );
        assert_eq!(response.status(), 503);
    }

    /// A replayable body does not authorize creating another item after a
    /// server error: the first response may follow a committed write.
    #[tokio::test]
    async fn a_bytes_create_is_not_retried_after_a_server_error() {
        let (url, seen) = busy_once_server().await;
        let client = Client::new();
        let request = client.post(&url).body("payload").build().unwrap();
        let response = send_with_retry(&client, request, &fast_retries())
            .await
            .unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [7],
            "requests and their body lengths"
        );
        assert_eq!(response.status(), 503);
    }

    #[tokio::test]
    async fn an_explicitly_safe_post_keeps_its_payload_when_retried() {
        let (url, seen) = busy_once_server().await;
        let client = Client::new();
        let request = client.post(&url).body("query").build().unwrap();
        let response = send_with_retry(&client, request, &fast_retries().for_safe_operation())
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(*seen.lock().unwrap(), [5, 5]);
    }

    #[tokio::test]
    async fn a_rebuilt_create_is_not_retried_after_it_may_have_committed() {
        let (url, seen) = busy_once_server().await;
        let client = Client::new();
        let response = send_with_retry_replayable(
            &client,
            || client.post(&url).body("create"),
            &fast_retries(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), 503);
        assert_eq!(*seen.lock().unwrap(), [6]);
    }

    #[tokio::test]
    async fn a_refused_create_can_retry_but_a_get_rpc_cannot_replay_a_server_error() {
        let client = Client::new();
        let (url, seen) = refusal_once_server(axum::http::StatusCode::TOO_MANY_REQUESTS).await;
        let request = client.post(&url).body("create").build().unwrap();
        let response = send_with_retry(&client, request, &fast_retries().refused_only())
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(*seen.lock().unwrap(), [6, 6]);

        let (url, seen) = busy_once_server().await;
        let request = client.get(&url).build().unwrap();
        let response = send_with_retry(&client, request, &fast_retries().refused_only())
            .await
            .unwrap();
        assert_eq!(response.status(), 503);
        assert_eq!(*seen.lock().unwrap(), [0]);
    }

    #[tokio::test]
    async fn a_rebuilt_stream_is_whole_on_each_explicit_refusal_attempt() {
        let client = Client::new();
        let (url, seen) = refusal_once_server(axum::http::StatusCode::TOO_MANY_REQUESTS).await;
        let response = send_with_retry_replayable(
            &client,
            || {
                let stream = futures_util::stream::once(async {
                    Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"entire-payload"))
                });
                client.post(&url).body(reqwest::Body::wrap_stream(stream))
            },
            &fast_retries().refused_only(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(*seen.lock().unwrap(), [14, 14]);
    }

    #[test]
    fn replay_decision_is_about_the_operation_and_the_response() {
        use reqwest::Method;
        let defaults = fast_retries();
        for status in [500, 502, 503, 504] {
            for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
                assert!(!can_retry_response(status, &method, &defaults));
                assert!(can_retry_response(
                    status,
                    &method,
                    &defaults.clone().for_safe_operation()
                ));
            }
            assert!(can_retry_response(status, &Method::GET, &defaults));
            assert!(!can_retry_response(
                status,
                &Method::GET,
                &defaults.clone().refused_only()
            ));
        }
        assert!(can_retry_response(
            429,
            &Method::POST,
            &defaults.clone().refused_only()
        ));
        for status in [200, 400, 401, 403, 404, 409] {
            assert!(!can_retry_response(
                status,
                &Method::POST,
                &defaults.clone().for_safe_operation()
            ));
        }
    }
}
