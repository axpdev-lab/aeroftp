//! HTTP client for the Proton API.
//!
//! Ported from the Proton Drive SDK at commit [`super::SDK_REFERENCE_COMMIT`]
//! (MIT, Copyright (c) 2025-2026 Proton AG, see `LICENSE-PROTON-DRIVE-SDK.md`):
//! - headers, session refresh and the "one refresh at a time" rule from
//!   `incubating/account/js/src/apiClient.ts`;
//! - retry and rate-limit policy from
//!   `client/js/src/internal/apiService/apiService.ts` (`DriveAPIService`).
//!
//! The README makes direct API calls acceptable only when they keep the SDK's
//! safety guarantees, so the policy below is the SDK's, number for number, and
//! must not become more aggressive: 429 waits for `Retry-After` (10 s when
//! absent) and 50 consecutive 429s stop all requests for 60 s; a 5xx is
//! retried once after 1 s and 10 consecutive 5xx stop requests for 60 s;
//! timeouts and network errors are retried up to 3 attempts.
//!
//! What we deliberately do not send: `x-pm-drive-sdk-version`. Proton asked
//! rclone to drop it (rclone PR #9260): only the real SDK may claim to be the
//! SDK.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use reqwest::{Method, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::{Mutex, RwLock};

use super::error::{ProtonError, CODE_HUMAN_VERIFICATION_REQUIRED, CODE_OK};
use super::session::SessionTokens;

/// Official API host used by the Proton Drive apps and the SDK CLI
/// (`cli/src/config.ts`). Never `mail.proton.me/api`, which Proton keeps alive
/// only for old clients.
pub const DEFAULT_API_HOST: &str = "drive-api.proton.me";

/// Media type every SDK request accepts.
const ACCEPT_PROTON_JSON: &str = "application/vnd.protonmail.v1+json";

/// `apiClient.ts`: `DEFAULT_TIMEOUT_MS`.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Proton success code for multi-item responses.
const CODE_MULTI_OK: i64 = 1001;

/// Retry and throttling numbers of `DriveAPIService`. Tests shrink the delays;
/// production always uses [`RetryPolicy::SDK`].
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_timeout_attempts: u32,
    pub max_network_attempts: u32,
    pub max_consecutive_429: u32,
    pub max_consecutive_5xx: u32,
    pub cool_down: Duration,
    pub server_error_delay: Duration,
    pub network_error_delay: Duration,
    pub default_429_delay: Duration,
    pub general_retry_delay: Duration,
}

impl RetryPolicy {
    pub const SDK: RetryPolicy = RetryPolicy {
        max_timeout_attempts: 3,
        max_network_attempts: 3,
        max_consecutive_429: 50,
        max_consecutive_5xx: 10,
        cool_down: Duration::from_secs(60),
        server_error_delay: Duration::from_secs(1),
        network_error_delay: Duration::from_secs(5),
        default_429_delay: Duration::from_secs(10),
        general_retry_delay: Duration::from_secs(1),
    };
}

#[derive(Debug, Default)]
struct Throttle {
    consecutive_429: u32,
    consecutive_5xx: u32,
    /// While set and in the future, every request fails fast instead of
    /// reaching Proton.
    blocked_until: Option<Instant>,
}

type TokensListener = Box<dyn Fn(&SessionTokens) + Send + Sync>;

/// The Proton API client: one per signed-in account.
pub struct ProtonApi {
    http: reqwest::Client,
    base_url: String,
    app_version: String,
    policy: RetryPolicy,
    tokens: RwLock<Option<SessionTokens>>,
    /// Held while a refresh runs, so concurrent 401s share one refresh.
    refresh_gate: Mutex<()>,
    throttle: StdMutex<Throttle>,
    /// Called after every refresh, so the caller can persist the new tokens.
    tokens_listener: StdMutex<Option<TokensListener>>,
}

/// Body of a Proton error response.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ErrorBody {
    #[serde(default)]
    code: Option<i64>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    details: Option<Value>,
}

/// `POST /auth/v4/refresh` answer.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RefreshResponse {
    #[serde(rename = "UID", default)]
    uid: Option<String>,
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
}

impl ProtonApi {
    /// A client for the official API host.
    pub fn new(app_version: String) -> Self {
        Self::with_base_url(format!("https://{DEFAULT_API_HOST}"), app_version)
    }

    /// A client for another base URL. Production code uses [`Self::new`];
    /// this exists for the local test doubles.
    pub fn with_base_url(base_url: String, app_version: String) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(crate::providers::AEROFTP_USER_AGENT)
            .connect_timeout(REQUEST_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            app_version,
            policy: RetryPolicy::SDK,
            tokens: RwLock::new(None),
            refresh_gate: Mutex::new(()),
            throttle: StdMutex::new(Throttle::default()),
            tokens_listener: StdMutex::new(None),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_policy(mut self, policy: RetryPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn app_version(&self) -> &str {
        &self.app_version
    }

    /// Installs (or replaces) the session tokens used by authenticated calls.
    pub async fn set_tokens(&self, tokens: Option<SessionTokens>) {
        *self.tokens.write().await = tokens;
    }

    pub async fn tokens(&self) -> Option<SessionTokens> {
        self.tokens.read().await.clone()
    }

    /// Registers the function told about every refreshed token pair.
    pub fn on_tokens_refreshed(&self, listener: impl Fn(&SessionTokens) + Send + Sync + 'static) {
        if let Ok(mut slot) = self.tokens_listener.lock() {
            *slot = Some(Box::new(listener));
        }
    }

    /// Authenticated `GET`.
    pub async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, ProtonError> {
        self.request(Method::GET, path, query, None, true).await
    }

    /// Unauthenticated `GET` (session fork endpoints).
    pub async fn get_unauthenticated<T: DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<T, ProtonError> {
        self.request(Method::GET, path, &[], None, false).await
    }

    /// Authenticated `POST` with a JSON body.
    pub async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
    ) -> Result<T, ProtonError> {
        self.request(Method::POST, path, &[], Some(body), true)
            .await
    }

    /// Authenticated `DELETE`.
    pub async fn delete<T: DeserializeOwned>(&self, path: &str) -> Result<T, ProtonError> {
        self.request(Method::DELETE, path, &[], None, true).await
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }

    fn fail_fast_if_blocked(&self) -> Result<(), ProtonError> {
        let Ok(mut throttle) = self.throttle.lock() else {
            return Ok(());
        };
        if let Some(until) = throttle.blocked_until {
            let now = Instant::now();
            if now < until {
                return Err(ProtonError::RateLimited {
                    retry_after_secs: until.saturating_duration_since(now).as_secs().max(1),
                });
            }
            throttle.blocked_until = None;
            throttle.consecutive_429 = 0;
            throttle.consecutive_5xx = 0;
        }
        Ok(())
    }

    /// Records a 429. Returns true when the consecutive limit was reached and
    /// requests are now blocked for the cool-down.
    fn note_429(&self) -> bool {
        let Ok(mut throttle) = self.throttle.lock() else {
            return false;
        };
        throttle.consecutive_429 += 1;
        if throttle.consecutive_429 >= self.policy.max_consecutive_429 {
            throttle.blocked_until = Some(Instant::now() + self.policy.cool_down);
            return true;
        }
        false
    }

    fn note_5xx(&self) -> bool {
        let Ok(mut throttle) = self.throttle.lock() else {
            return false;
        };
        throttle.consecutive_5xx += 1;
        if throttle.consecutive_5xx >= self.policy.max_consecutive_5xx {
            throttle.blocked_until = Some(Instant::now() + self.policy.cool_down);
            return true;
        }
        false
    }

    fn note_answer(&self, status: StatusCode) {
        if let Ok(mut throttle) = self.throttle.lock() {
            if status != StatusCode::TOO_MANY_REQUESTS {
                throttle.consecutive_429 = 0;
            }
            if !status.is_server_error() {
                throttle.consecutive_5xx = 0;
            }
        }
    }

    async fn request<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
        authenticated: bool,
    ) -> Result<T, ProtonError> {
        let mut attempt: u32 = 0;
        let mut refreshed = false;
        loop {
            self.fail_fast_if_blocked()?;

            let mut builder = self
                .http
                .request(method.clone(), self.url(path))
                .header("x-pm-appversion", &self.app_version)
                .header(reqwest::header::ACCEPT, ACCEPT_PROTON_JSON)
                .header("Language", "en");
            if !query.is_empty() {
                builder = builder.query(query);
            }
            if let Some(body) = body {
                builder = builder.json(body);
            }
            let sent_access_token = if authenticated {
                let tokens = self.tokens.read().await.clone();
                let Some(tokens) = tokens else {
                    return Err(ProtonError::NotSignedIn);
                };
                builder = builder
                    .header("x-pm-uid", &tokens.uid)
                    .bearer_auth(tokens.access_token.expose_secret());
                Some(tokens.access_token)
            } else {
                None
            };

            let response = match builder.send().await {
                Ok(response) => response,
                Err(error) => {
                    // DriveAPIService: timeouts and network errors get up to
                    // three attempts, anything else is retried once.
                    let (limit, delay) = if error.is_timeout() {
                        (
                            self.policy.max_timeout_attempts,
                            self.policy.server_error_delay,
                        )
                    } else if error.is_connect() || error.is_request() {
                        (
                            self.policy.max_network_attempts,
                            self.policy.network_error_delay,
                        )
                    } else {
                        (1, self.policy.general_retry_delay)
                    };
                    if attempt + 1 < limit || attempt == 0 {
                        attempt += 1;
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return Err(ProtonError::Network(error.to_string()));
                }
            };

            let status = response.status();
            self.note_answer(status);

            if status == StatusCode::TOO_MANY_REQUESTS {
                let wait = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(crate::providers::retry_after::parse_retry_after_seconds)
                    .filter(|wait| !wait.is_zero())
                    .unwrap_or(self.policy.default_429_delay);
                if self.note_429() {
                    return Err(ProtonError::RateLimited {
                        retry_after_secs: self.policy.cool_down.as_secs(),
                    });
                }
                attempt += 1;
                tokio::time::sleep(wait).await;
                continue;
            }

            if status.is_server_error() {
                let blocked = self.note_5xx();
                if attempt == 0 && !blocked {
                    attempt += 1;
                    tokio::time::sleep(self.policy.server_error_delay).await;
                    continue;
                }
            }

            if status == StatusCode::UNAUTHORIZED
                && authenticated
                && !refreshed
                && !skips_refresh(path)
            {
                refreshed = true;
                let rejected = sent_access_token
                    .as_ref()
                    .map(|token| token.expose_secret().to_owned())
                    .unwrap_or_default();
                if self.refresh_if_possible(&rejected).await? {
                    continue;
                }
                return Err(ProtonError::SessionExpired);
            }

            let bytes = response
                .bytes()
                .await
                .map_err(|error| ProtonError::Network(error.to_string()))?;
            return decode_answer(status, &bytes);
        }
    }

    /// `apiClient.ts::refreshSessionIfPossible`. Returns true when the caller
    /// should retry with the current tokens.
    async fn refresh_if_possible(&self, rejected_access_token: &str) -> Result<bool, ProtonError> {
        let _gate = self.refresh_gate.lock().await;

        let Some(current) = self.tokens.read().await.clone() else {
            return Ok(false);
        };
        // Another request already refreshed while this one waited.
        if current.access_token.expose_secret() != rejected_access_token {
            return Ok(true);
        }
        let Some(refresh_token) = current.refresh_token.clone() else {
            log::warn!("Proton session refresh impossible: no refresh token");
            return Ok(false);
        };

        let response = self
            .http
            .post(self.url("/auth/v4/refresh"))
            .header("x-pm-appversion", &self.app_version)
            .header(reqwest::header::ACCEPT, ACCEPT_PROTON_JSON)
            .header("x-pm-uid", &current.uid)
            .bearer_auth(current.access_token.expose_secret())
            .json(&serde_json::json!({
                "ResponseType": "token",
                "GrantType": "refresh_token",
                "RefreshToken": refresh_token.expose_secret(),
            }))
            .send()
            .await
            .map_err(|error| ProtonError::Network(error.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            log::warn!(
                "Proton session refresh failed with HTTP {}",
                status.as_u16()
            );
            // A 4xx other than 429 means the refresh token is dead: sign out,
            // as apiClient.ts does. A 429 or 5xx keeps the session for later.
            if status.is_client_error() && status != StatusCode::TOO_MANY_REQUESTS {
                *self.tokens.write().await = None;
            }
            return Ok(false);
        }

        let refreshed: RefreshResponse = response
            .json()
            .await
            .map_err(|error| ProtonError::Decode(format!("refresh answer: {error}")))?;
        let Some(access_token) = refreshed.access_token else {
            return Err(ProtonError::Decode(
                "refresh answer without AccessToken".into(),
            ));
        };
        let tokens = SessionTokens {
            uid: refreshed.uid.unwrap_or(current.uid),
            access_token: SecretString::from(access_token),
            refresh_token: Some(
                refreshed
                    .refresh_token
                    .map(SecretString::from)
                    .unwrap_or(refresh_token),
            ),
        };
        *self.tokens.write().await = Some(tokens.clone());
        if let Ok(listener) = self.tokens_listener.lock() {
            if let Some(listener) = listener.as_ref() {
                listener(&tokens);
            }
        }
        Ok(true)
    }
}

/// `apiClient.ts::shouldSkipAuthRefreshForUrl`: the auth endpoints themselves
/// never trigger a refresh.
fn skips_refresh(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.contains("/auth/v4/refresh")
        || path.contains("/auth/v4/sessions")
        || path.contains("/core/v4/auth")
}

/// Turns an HTTP answer into the typed body or a [`ProtonError`].
fn decode_answer<T: DeserializeOwned>(status: StatusCode, bytes: &[u8]) -> Result<T, ProtonError> {
    let error_body: ErrorBody = serde_json::from_slice(bytes).unwrap_or_default();
    let code = error_body.code.unwrap_or(0);
    let success_code = error_body.code.is_none() || code == CODE_OK || code == CODE_MULTI_OK;

    if status.is_success() && success_code {
        return serde_json::from_slice(bytes)
            .map_err(|error| ProtonError::Decode(error.to_string()));
    }

    if code == CODE_HUMAN_VERIFICATION_REQUIRED {
        let methods = error_body
            .details
            .as_ref()
            .and_then(|details| details.get("HumanVerificationMethods"))
            .and_then(Value::as_array)
            .map(|methods| {
                methods
                    .iter()
                    .filter_map(|method| method.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        return Err(ProtonError::HumanVerificationRequired { methods });
    }

    Err(ProtonError::Api {
        status: status.as_u16(),
        code,
        message: error_body
            .error
            .unwrap_or_else(|| status.canonical_reason().unwrap_or("error").to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const FAST: RetryPolicy = RetryPolicy {
        max_timeout_attempts: 3,
        max_network_attempts: 3,
        max_consecutive_429: 3,
        max_consecutive_5xx: 10,
        cool_down: Duration::from_secs(60),
        server_error_delay: Duration::from_millis(1),
        network_error_delay: Duration::from_millis(1),
        default_429_delay: Duration::from_millis(1),
        general_retry_delay: Duration::from_millis(1),
    };

    fn tokens(access: &str) -> SessionTokens {
        SessionTokens {
            uid: "uid-1".into(),
            access_token: SecretString::from(access.to_string()),
            refresh_token: Some(SecretString::from("refresh-1".to_string())),
        }
    }

    async fn serve(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        format!("http://{addr}")
    }

    #[test]
    fn auth_endpoints_never_trigger_a_refresh() {
        assert!(skips_refresh("/auth/v4/refresh"));
        assert!(skips_refresh("/auth/v4/sessions/forks/abc"));
        assert!(skips_refresh("/core/v4/auth/info"));
        assert!(!skips_refresh("/core/v4/users"));
        assert!(!skips_refresh("/drive/shares"));
    }

    #[test]
    fn human_verification_is_its_own_error() {
        let body = br#"{"Code":9001,"Error":"Human verification required","Details":{"HumanVerificationMethods":["captcha","email"],"HumanVerificationToken":"t"}}"#;
        let error = decode_answer::<Value>(StatusCode::UNPROCESSABLE_ENTITY, body).unwrap_err();
        match error {
            ProtonError::HumanVerificationRequired { methods } => {
                assert_eq!(methods, vec!["captcha".to_string(), "email".to_string()]);
            }
            other => panic!("expected HumanVerificationRequired, got {other:?}"),
        }
    }

    #[test]
    fn an_http_200_with_an_error_code_is_an_error() {
        let body = br#"{"Code":2501,"Error":"File or folder not found"}"#;
        let error = decode_answer::<Value>(StatusCode::OK, body).unwrap_err();
        assert!(
            matches!(
                error,
                ProtonError::Api {
                    code: 2501,
                    status: 200,
                    ..
                }
            ),
            "{error:?}"
        );
    }

    /// Every request carries the honest app version and the session headers,
    /// and never the SDK-only header.
    #[tokio::test]
    async fn requests_carry_app_version_and_session_headers() {
        let seen: Arc<StdMutex<Vec<(String, String)>>> = Arc::default();
        let record = Arc::clone(&seen);
        let app = axum::Router::new().fallback(axum::routing::any(
            move |headers: axum::http::HeaderMap| {
                let mut pairs: Vec<(String, String)> = headers
                    .iter()
                    .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
                    .collect();
                pairs.sort();
                *record.lock().unwrap() = pairs;
                async { r#"{"Code":1000}"# }
            },
        ));
        let api = ProtonApi::with_base_url(
            serve(app).await,
            "external-drive-aeroftp@1.2.3-alpha".into(),
        )
        .with_policy(FAST);
        api.set_tokens(Some(tokens("access-1"))).await;
        let _: Value = api.get("/core/v4/users", &[]).await.expect("ok");

        let headers = seen.lock().unwrap().clone();
        let get = |name: &str| {
            headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(
            get("x-pm-appversion").as_deref(),
            Some("external-drive-aeroftp@1.2.3-alpha")
        );
        assert_eq!(get("x-pm-uid").as_deref(), Some("uid-1"));
        assert_eq!(get("authorization").as_deref(), Some("Bearer access-1"));
        assert_eq!(get("accept").as_deref(), Some(ACCEPT_PROTON_JSON));
        assert_eq!(
            get("x-pm-drive-sdk-version"),
            None,
            "only the real SDK may send it"
        );
    }

    /// A 401 refreshes the session once, persists the new tokens through the
    /// listener and replays the request with them.
    #[tokio::test]
    async fn a_401_refreshes_once_and_replays_with_the_new_token() {
        let refreshes = Arc::new(AtomicUsize::new(0));
        let refresh_count = Arc::clone(&refreshes);
        let app = axum::Router::new()
            .route(
                "/auth/v4/refresh",
                axum::routing::post(move || {
                    refresh_count.fetch_add(1, Ordering::SeqCst);
                    async {
                        r#"{"Code":1000,"UID":"uid-1","AccessToken":"access-2","RefreshToken":"refresh-2"}"#
                    }
                }),
            )
            .route(
                "/core/v4/users",
                axum::routing::get(|headers: axum::http::HeaderMap| async move {
                    let auth = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    if auth == "Bearer access-2" {
                        (axum::http::StatusCode::OK, r#"{"Code":1000,"Ok":true}"#)
                    } else {
                        (axum::http::StatusCode::UNAUTHORIZED, r#"{"Code":401,"Error":"Invalid access token"}"#)
                    }
                }),
            );
        let api = ProtonApi::with_base_url(
            serve(app).await,
            "external-drive-aeroftp@1.2.3-alpha".into(),
        )
        .with_policy(FAST);
        api.set_tokens(Some(tokens("access-1"))).await;
        let persisted: Arc<StdMutex<Option<String>>> = Arc::default();
        let persist = Arc::clone(&persisted);
        api.on_tokens_refreshed(move |tokens| {
            *persist.lock().unwrap() = Some(tokens.access_token.expose_secret().to_owned());
        });

        let answer: Value = api.get("/core/v4/users", &[]).await.expect("replayed");
        assert_eq!(answer["Ok"], Value::Bool(true));
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(persisted.lock().unwrap().as_deref(), Some("access-2"));
    }

    /// A refresh refused with a 4xx ends the session instead of looping.
    #[tokio::test]
    async fn a_dead_refresh_token_signs_out() {
        let app = axum::Router::new()
            .route(
                "/auth/v4/refresh",
                axum::routing::post(|| async {
                    (
                        axum::http::StatusCode::BAD_REQUEST,
                        r#"{"Code":10013,"Error":"Invalid refresh token"}"#,
                    )
                }),
            )
            .fallback(axum::routing::any(|| async {
                (axum::http::StatusCode::UNAUTHORIZED, r#"{"Code":401}"#)
            }));
        let api = ProtonApi::with_base_url(
            serve(app).await,
            "external-drive-aeroftp@1.2.3-alpha".into(),
        )
        .with_policy(FAST);
        api.set_tokens(Some(tokens("access-1"))).await;

        let error = api.get::<Value>("/core/v4/users", &[]).await.unwrap_err();
        assert!(matches!(error, ProtonError::SessionExpired), "{error:?}");
        assert!(api.tokens().await.is_none(), "tokens must be dropped");
    }

    /// Consecutive 429s past the SDK limit stop requests for the cool-down,
    /// and the next call fails without reaching the server.
    #[tokio::test]
    async fn too_many_429_block_further_requests() {
        let hits = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&hits);
        let app = axum::Router::new().fallback(axum::routing::any(move || {
            count.fetch_add(1, Ordering::SeqCst);
            async {
                (
                    axum::http::StatusCode::TOO_MANY_REQUESTS,
                    r#"{"Code":2028}"#,
                )
            }
        }));
        let api = ProtonApi::with_base_url(
            serve(app).await,
            "external-drive-aeroftp@1.2.3-alpha".into(),
        )
        .with_policy(FAST);
        api.set_tokens(Some(tokens("access-1"))).await;

        let error = api.get::<Value>("/drive/shares", &[]).await.unwrap_err();
        assert!(
            matches!(error, ProtonError::RateLimited { .. }),
            "{error:?}"
        );
        assert_eq!(
            hits.load(Ordering::SeqCst),
            FAST.max_consecutive_429 as usize
        );

        let again = api.get::<Value>("/drive/shares", &[]).await.unwrap_err();
        assert!(
            matches!(again, ProtonError::RateLimited { .. }),
            "{again:?}"
        );
        assert_eq!(
            hits.load(Ordering::SeqCst),
            FAST.max_consecutive_429 as usize,
            "a blocked client must not reach the server"
        );
    }

    /// A 5xx is retried exactly once.
    #[tokio::test]
    async fn a_server_error_is_retried_once() {
        let hits = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&hits);
        let app = axum::Router::new().fallback(axum::routing::any(move || {
            count.fetch_add(1, Ordering::SeqCst);
            async { (axum::http::StatusCode::BAD_GATEWAY, "") }
        }));
        let api = ProtonApi::with_base_url(
            serve(app).await,
            "external-drive-aeroftp@1.2.3-alpha".into(),
        )
        .with_policy(FAST);
        api.set_tokens(Some(tokens("access-1"))).await;

        let error = api.get::<Value>("/drive/shares", &[]).await.unwrap_err();
        assert!(
            matches!(error, ProtonError::Api { status: 502, .. }),
            "{error:?}"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }
}
