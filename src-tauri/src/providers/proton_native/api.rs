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

use super::error::{ProtonError, CODE_ALREADY_EXISTS, CODE_HUMAN_VERIFICATION_REQUIRED, CODE_OK};
use super::session::SessionTokens;

/// Official API host used by the Proton Drive apps and the SDK CLI
/// (`cli/src/config.ts`). Never `mail.proton.me/api`, which Proton keeps alive
/// only for old clients.
pub const DEFAULT_API_HOST: &str = "drive-api.proton.me";

/// Media type every SDK request accepts.
const ACCEPT_PROTON_JSON: &str = "application/vnd.protonmail.v1+json";

/// `apiClient.ts`: `DEFAULT_TIMEOUT_MS`.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// `apiService.ts`: storage (block) requests get 600 s.
const STORAGE_TIMEOUT: Duration = Duration::from_secs(600);

/// Refreshes one request may trigger: the first can adopt tokens another
/// process refreshed, the second refreshes when those were stale as well.
const MAX_REFRESH_ROUNDS: u32 = 2;

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

/// Where a session is kept between processes.
///
/// The desktop app, the MCP server and the CLI can each open the same saved
/// session. Proton hands out a new refresh token at every refresh, so two
/// processes refreshing independently would spend each other's token and one
/// of them would be signed out. The store makes the refresh single-owner: the
/// refresh runs under [`SessionStore::lock`], and a process that finds tokens
/// newer than the ones that were rejected adopts them instead of refreshing.
#[async_trait::async_trait]
pub trait SessionStore: Send + Sync {
    /// A guard held for the length of one refresh, so only one process
    /// refreshes at a time. `None` when exclusivity could not be had in a
    /// bounded wait; the refresh then goes ahead without it.
    async fn lock(&self) -> Option<Box<dyn Send>>;
    /// The tokens saved now, which another process may have refreshed.
    fn load(&self) -> Option<SessionTokens>;
    /// Saves freshly refreshed tokens.
    fn save(&self, tokens: &SessionTokens);
}

/// One part of a multipart form sent to the API host.
pub struct FormPart {
    pub name: &'static str,
    pub file_name: &'static str,
    pub mime: &'static str,
    pub data: bytes::Bytes,
}

/// What a request carries.
#[derive(Clone, Copy)]
enum Body<'a> {
    Json(&'a Value),
    Form(&'a [FormPart]),
}

fn form_of(parts: &[FormPart]) -> Result<reqwest::multipart::Form, ProtonError> {
    let mut form = reqwest::multipart::Form::new();
    for part in parts {
        let field = reqwest::multipart::Part::stream(part.data.clone())
            .file_name(part.file_name)
            .mime_str(part.mime)
            .map_err(send_error)?;
        form = form.part(part.name, field);
    }
    Ok(form)
}

/// The Proton API client: one per signed-in account.
pub struct ProtonApi {
    http: reqwest::Client,
    /// Block transfers to the storage hosts, with the longer timeout.
    storage_http: reqwest::Client,
    base_url: String,
    app_version: String,
    policy: RetryPolicy,
    tokens: RwLock<Option<SessionTokens>>,
    /// Held while a refresh runs, so concurrent 401s share one refresh.
    refresh_gate: Mutex<()>,
    throttle: StdMutex<Throttle>,
    /// Shares the session with other processes and persists refreshes.
    session_store: StdMutex<Option<std::sync::Arc<dyn SessionStore>>>,
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
        let storage_http = reqwest::Client::builder()
            .user_agent(crate::providers::AEROFTP_USER_AGENT)
            .connect_timeout(REQUEST_TIMEOUT)
            .timeout(STORAGE_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            storage_http,
            base_url: base_url.trim_end_matches('/').to_string(),
            app_version,
            policy: RetryPolicy::SDK,
            tokens: RwLock::new(None),
            refresh_gate: Mutex::new(()),
            throttle: StdMutex::new(Throttle::default()),
            session_store: StdMutex::new(None),
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

    /// Installs the store that persists refreshed tokens and coordinates
    /// refreshes with other processes holding the same session.
    pub fn set_session_store(&self, store: std::sync::Arc<dyn SessionStore>) {
        if let Ok(mut slot) = self.session_store.lock() {
            *slot = Some(store);
        }
    }

    fn session_store(&self) -> Option<std::sync::Arc<dyn SessionStore>> {
        self.session_store
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().map(std::sync::Arc::clone))
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
        self.request(Method::POST, path, &[], Some(Body::Json(body)), true)
            .await
    }

    /// Authenticated `POST` of a multipart form to the API host
    /// (`apiService.ts::postFormData`), as the small-file upload sends it.
    pub async fn post_form<T: DeserializeOwned>(
        &self,
        path: &str,
        parts: &[FormPart],
    ) -> Result<T, ProtonError> {
        self.request(Method::POST, path, &[], Some(Body::Form(parts)), true)
            .await
    }

    /// Authenticated `PUT` with a JSON body.
    pub async fn put<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
    ) -> Result<T, ProtonError> {
        self.request(Method::PUT, path, &[], Some(Body::Json(body)), true)
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
        body: Option<Body<'_>>,
        authenticated: bool,
    ) -> Result<T, ProtonError> {
        let mut attempt: u32 = 0;
        let mut refreshes: u32 = 0;
        let what = format!("{method} {}", loggable_path(path));
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
            match body {
                Some(Body::Json(body)) => builder = builder.json(body),
                // A form is consumed by its send, so every attempt builds its own.
                Some(Body::Form(parts)) => builder = builder.multipart(form_of(parts)?),
                None => {}
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

            let started = Instant::now();
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
                        log_retry(&what, "network error", delay, attempt);
                        attempt += 1;
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return Err(send_error(error));
                }
            };

            let status = response.status();
            log_answer(&what, status, started);
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
                log_retry(&what, "rate limited (429)", wait, attempt);
                attempt += 1;
                tokio::time::sleep(wait).await;
                continue;
            }

            if status.is_server_error() {
                let blocked = self.note_5xx();
                if attempt == 0 && !blocked {
                    log_retry(
                        &what,
                        "server error",
                        self.policy.server_error_delay,
                        attempt,
                    );
                    attempt += 1;
                    tokio::time::sleep(self.policy.server_error_delay).await;
                    continue;
                }
            }

            if status == StatusCode::UNAUTHORIZED && authenticated && !skips_refresh(path) {
                // Two rounds: tokens adopted from another process may have
                // expired too, and then this process refreshes them itself.
                if refreshes >= MAX_REFRESH_ROUNDS {
                    return Err(ProtonError::SessionExpired);
                }
                refreshes += 1;
                let rejected = sent_access_token
                    .as_ref()
                    .map(|token| token.expose_secret().to_owned())
                    .unwrap_or_default();
                match self.refresh_if_possible(&rejected).await? {
                    Refreshed::Retry => continue,
                    Refreshed::Ended => return Err(ProtonError::SessionExpired),
                    Refreshed::Replaced => return Err(ProtonError::SessionReplaced),
                }
            }

            let bytes = response.bytes().await.map_err(send_error)?;
            return decode_answer(status, &bytes);
        }
    }

    /// `GET` of a block from a storage host: `url` is the `BareURL` the API
    /// returned, used verbatim, with its `pm-storage-token`. The session
    /// headers go along, as the official CLI's host client sends them. A 404
    /// is [`ProtonError::StorageTokenExpired`]; timeouts, network errors, 429
    /// and 5xx are retried like API calls.
    pub async fn storage_get(&self, url: &str, token: &str) -> Result<bytes::Bytes, ProtonError> {
        let mut attempt: u32 = 0;
        let what = "storage GET block";
        loop {
            self.fail_fast_if_blocked()?;
            let mut builder = self
                .storage_http
                .get(url)
                .header("pm-storage-token", token)
                .header("x-pm-appversion", &self.app_version)
                .header("Language", "en");
            if let Some(tokens) = self.tokens.read().await.clone() {
                builder = builder
                    .header("x-pm-uid", &tokens.uid)
                    .bearer_auth(tokens.access_token.expose_secret());
            }
            let started = Instant::now();
            let response = match builder.send().await {
                Ok(response) => response,
                Err(error) => {
                    let limit = if error.is_timeout() {
                        self.policy.max_timeout_attempts
                    } else {
                        self.policy.max_network_attempts
                    };
                    if attempt + 1 < limit {
                        log_retry(
                            what,
                            "network error",
                            self.policy.network_error_delay,
                            attempt,
                        );
                        attempt += 1;
                        tokio::time::sleep(self.policy.network_error_delay).await;
                        continue;
                    }
                    return Err(send_error(error));
                }
            };
            let status = response.status();
            log_answer(what, status, started);
            self.note_answer(status);
            if status == StatusCode::NOT_FOUND {
                return Err(ProtonError::StorageTokenExpired);
            }
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
                log_retry(what, "rate limited (429)", wait, attempt);
                attempt += 1;
                tokio::time::sleep(wait).await;
                continue;
            }
            if status.is_server_error() && !self.note_5xx() && attempt == 0 {
                log_retry(
                    what,
                    "server error",
                    self.policy.server_error_delay,
                    attempt,
                );
                attempt += 1;
                tokio::time::sleep(self.policy.server_error_delay).await;
                continue;
            }
            let bytes = response.bytes().await.map_err(send_error)?;
            if !status.is_success() {
                return decode_answer::<serde_json::Value>(status, &bytes).and(Err(
                    ProtonError::Api {
                        status: status.as_u16(),
                        code: 0,
                        message: "storage request failed".into(),
                    },
                ));
            }
            return Ok(bytes);
        }
    }

    /// `POST` of an encrypted block to a storage host
    /// (`upload/apiService.ts::uploadBlock`): multipart with one part named
    /// `Block`, filename `blob`, octet-stream. A 404 is
    /// [`ProtonError::StorageTokenExpired`]; transient failures are retried.
    pub async fn storage_post_block(
        &self,
        url: &str,
        token: &str,
        block: bytes::Bytes,
    ) -> Result<(), ProtonError> {
        let mut attempt: u32 = 0;
        let what = "storage POST block";
        loop {
            self.fail_fast_if_blocked()?;
            let part = reqwest::multipart::Part::stream(block.clone())
                .file_name("blob")
                .mime_str("application/octet-stream")
                .map_err(send_error)?;
            let form = reqwest::multipart::Form::new().part("Block", part);
            let mut builder = self
                .storage_http
                .post(url)
                .header("pm-storage-token", token)
                .header("x-pm-appversion", &self.app_version)
                .header("Language", "en")
                .multipart(form);
            if let Some(tokens) = self.tokens.read().await.clone() {
                builder = builder
                    .header("x-pm-uid", &tokens.uid)
                    .bearer_auth(tokens.access_token.expose_secret());
            }
            let started = Instant::now();
            let response = match builder.send().await {
                Ok(response) => response,
                Err(error) => {
                    let limit = if error.is_timeout() {
                        self.policy.max_timeout_attempts
                    } else {
                        self.policy.max_network_attempts
                    };
                    if attempt + 1 < limit {
                        log_retry(
                            what,
                            "network error",
                            self.policy.network_error_delay,
                            attempt,
                        );
                        attempt += 1;
                        tokio::time::sleep(self.policy.network_error_delay).await;
                        continue;
                    }
                    return Err(send_error(error));
                }
            };
            let status = response.status();
            log_answer(what, status, started);
            self.note_answer(status);
            if status == StatusCode::NOT_FOUND {
                return Err(ProtonError::StorageTokenExpired);
            }
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
                log_retry(what, "rate limited (429)", wait, attempt);
                attempt += 1;
                tokio::time::sleep(wait).await;
                continue;
            }
            if status.is_server_error() && !self.note_5xx() && attempt == 0 {
                log_retry(
                    what,
                    "server error",
                    self.policy.server_error_delay,
                    attempt,
                );
                attempt += 1;
                tokio::time::sleep(self.policy.server_error_delay).await;
                continue;
            }
            let bytes = response.bytes().await.map_err(send_error)?;
            if !status.is_success() {
                decode_answer::<serde_json::Value>(status, &bytes)?;
                return Err(ProtonError::Api {
                    status: status.as_u16(),
                    code: 0,
                    message: "block upload failed".into(),
                });
            }
            // A 200 can still carry a Proton error code.
            decode_answer::<serde_json::Value>(status, &bytes).map(|_| ())?;
            return Ok(());
        }
    }

    /// `apiClient.ts::refreshSessionIfPossible`: whether the caller retries
    /// with the current tokens or the session is over. With a
    /// [`SessionStore`] the refresh runs under its cross-process lock, and
    /// tokens another process saved since the rejected ones are adopted
    /// instead of refreshing again.
    async fn refresh_if_possible(
        &self,
        rejected_access_token: &str,
    ) -> Result<Refreshed, ProtonError> {
        let _gate = self.refresh_gate.lock().await;

        let Some(current) = self.tokens.read().await.clone() else {
            return Ok(Refreshed::Ended);
        };
        // Another request already refreshed while this one waited.
        if current.access_token.expose_secret() != rejected_access_token {
            return Ok(Refreshed::Retry);
        }
        let store = self.session_store();
        let _lease = match store.as_ref() {
            Some(store) => store.lock().await,
            None => None,
        };
        if let Some(saved) = self.newer_saved_tokens(store.as_deref(), &current) {
            *self.tokens.write().await = Some(saved);
            return Ok(Refreshed::Retry);
        }
        let Some(refresh_token) = current.refresh_token.clone() else {
            log::warn!("Proton session refresh impossible: no refresh token");
            return Ok(Refreshed::Ended);
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
            .map_err(send_error)?;

        let status = response.status();
        if !status.is_success() {
            log::warn!(
                "Proton session refresh failed with HTTP {}",
                status.as_u16()
            );
            // A 4xx other than 429 means the refresh token is dead: sign out,
            // as apiClient.ts does, unless another process saved a newer pair
            // meanwhile (its lock can time out). A 429 or 5xx keeps the
            // session for later.
            if status.is_client_error() && status != StatusCode::TOO_MANY_REQUESTS {
                if let Some(saved) = self.newer_saved_tokens(store.as_deref(), &current) {
                    *self.tokens.write().await = Some(saved);
                    return Ok(Refreshed::Retry);
                }
                *self.tokens.write().await = None;
                // Another session under this vault key means a new sign-in
                // replaced this one (and revoked it): a reconnect opens that
                // session with its own keys. Its tokens are not adopted here:
                // it may belong to another account, whose keys this session
                // has not unlocked.
                let replaced = store
                    .as_deref()
                    .and_then(|store| store.load())
                    .is_some_and(|saved| saved.uid != current.uid);
                return Ok(if replaced {
                    Refreshed::Replaced
                } else {
                    Refreshed::Ended
                });
            }
            return Ok(Refreshed::Ended);
        }

        let body = response.bytes().await.map_err(send_error)?;
        let refreshed: RefreshResponse = serde_json::from_slice(&body)
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
        if let Some(store) = store.as_ref() {
            store.save(&tokens);
        }
        Ok(Refreshed::Retry)
    }

    /// The saved tokens when they belong to the same session and differ from
    /// `current`: another process refreshed them.
    fn newer_saved_tokens(
        &self,
        store: Option<&dyn SessionStore>,
        current: &SessionTokens,
    ) -> Option<SessionTokens> {
        let saved = store?.load()?;
        let same_session = saved.uid == current.uid;
        let newer = saved.access_token.expose_secret() != current.access_token.expose_secret();
        (same_session && newer).then_some(saved)
    }
}

/// What [`ProtonApi::refresh_if_possible`] leaves the rejected request with.
enum Refreshed {
    /// Retry with the current tokens (refreshed here or by another request
    /// or process).
    Retry,
    /// The session is over: the user signs in again.
    Ended,
    /// A new sign-in saved another session under this vault key and ended
    /// this one: a reconnect continues with it.
    Replaced,
}

/// One answer, for the debug log: the method and path (volume and link IDs,
/// no secret), never the query, a header or a body.
fn log_answer(what: &str, status: StatusCode, started: Instant) {
    log::debug!(
        "Proton {what} -> {} in {} ms",
        status.as_u16(),
        started.elapsed().as_millis()
    );
}

/// A wait before a retry, as a warning: a rate limit or a server answer that
/// slows a transfer down is otherwise invisible.
fn log_retry(what: &str, reason: &str, wait: Duration, attempt: u32) {
    log::warn!(
        "Proton {what}: {reason}, retry {} in {} ms",
        attempt + 1,
        wait.as_millis()
    );
}

/// A failed request or body read. A timeout is its own error, so the
/// provider reports it as one (`ProviderError::Timeout`); anything else is a
/// network error.
fn send_error(error: reqwest::Error) -> ProtonError {
    if error.is_timeout() {
        ProtonError::Timeout
    } else {
        // Without the URL: a storage URL or a fork selector would otherwise
        // ride along into the message, the log and the UI.
        ProtonError::Network(error.without_url().to_string())
    }
}

/// `path` as the log may show it. The selector of a session fork is a
/// capability: until AeroFTP collects the session, whoever holds it can, so
/// it is masked. Volume, share and link IDs are identifiers and stay.
fn loggable_path(path: &str) -> std::borrow::Cow<'_, str> {
    const FORKS: &str = "auth/v4/sessions/forks/";
    match path.find(FORKS) {
        Some(at) if path.len() > at + FORKS.len() => {
            std::borrow::Cow::Owned(format!("{}{FORKS}<selector>", &path[..at]))
        }
        _ => std::borrow::Cow::Borrowed(path),
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

    if code == CODE_ALREADY_EXISTS {
        let details = error_body.details.as_ref();
        let text = |key: &str| {
            details
                .and_then(|d| d.get(key))
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        let draft_revision = text("ConflictDraftRevisionID");
        return Err(ProtonError::AlreadyExists {
            link_id: text("ConflictLinkID"),
            draft_client_uid: text("ConflictDraftClientUID"),
            is_draft: draft_revision.is_some(),
        });
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

    /// An in-memory [`SessionStore`] standing in for the vault.
    #[derive(Default)]
    struct MemoryStore {
        saved: StdMutex<Option<SessionTokens>>,
        locks: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl SessionStore for MemoryStore {
        async fn lock(&self) -> Option<Box<dyn Send>> {
            self.locks.fetch_add(1, Ordering::SeqCst);
            None
        }
        fn load(&self) -> Option<SessionTokens> {
            self.saved.lock().unwrap().clone()
        }
        fn save(&self, tokens: &SessionTokens) {
            *self.saved.lock().unwrap() = Some(tokens.clone());
        }
    }

    impl MemoryStore {
        fn saved_access(&self) -> Option<String> {
            self.saved
                .lock()
                .unwrap()
                .as_ref()
                .map(|t| t.access_token.expose_secret().to_owned())
        }
    }

    /// `/core/v4/users` that accepts only `Bearer access-2`, and a refresh
    /// endpoint answering `refresh` and counting its calls.
    fn users_app(refreshes: Arc<AtomicUsize>, refresh: (u16, &'static str)) -> axum::Router {
        axum::Router::new()
            .route(
                "/auth/v4/refresh",
                axum::routing::post(move || {
                    refreshes.fetch_add(1, Ordering::SeqCst);
                    async move {
                        (
                            axum::http::StatusCode::from_u16(refresh.0).unwrap(),
                            refresh.1,
                        )
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
                        (
                            axum::http::StatusCode::UNAUTHORIZED,
                            r#"{"Code":401,"Error":"Invalid access token"}"#,
                        )
                    }
                }),
            )
    }

    const REFRESH_OK: (u16, &str) = (
        200,
        r#"{"Code":1000,"UID":"uid-1","AccessToken":"access-2","RefreshToken":"refresh-2"}"#,
    );
    const REFRESH_DEAD: (u16, &str) = (400, r#"{"Code":10013,"Error":"Invalid refresh token"}"#);

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

    /// A 401 refreshes the session once under the store's lock, saves the
    /// new tokens in the store and replays the request with them.
    #[tokio::test]
    async fn a_401_refreshes_once_and_replays_with_the_new_token() {
        let refreshes = Arc::new(AtomicUsize::new(0));
        let api = ProtonApi::with_base_url(
            serve(users_app(Arc::clone(&refreshes), REFRESH_OK)).await,
            "external-drive-aeroftp@1.2.3-alpha".into(),
        )
        .with_policy(FAST);
        api.set_tokens(Some(tokens("access-1"))).await;
        let store = Arc::new(MemoryStore::default());
        store.save(&tokens("access-1"));
        api.set_session_store(Arc::clone(&store) as Arc<dyn SessionStore>);

        let answer: Value = api.get("/core/v4/users", &[]).await.expect("replayed");
        assert_eq!(answer["Ok"], Value::Bool(true));
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(store.locks.load(Ordering::SeqCst), 1);
        assert_eq!(store.saved_access().as_deref(), Some("access-2"));
    }

    /// Another process refreshed the shared session: its saved tokens are
    /// adopted and the spent refresh token is not sent again.
    #[tokio::test]
    async fn tokens_refreshed_by_another_process_are_adopted_without_a_refresh() {
        let refreshes = Arc::new(AtomicUsize::new(0));
        let api = ProtonApi::with_base_url(
            serve(users_app(Arc::clone(&refreshes), REFRESH_DEAD)).await,
            "external-drive-aeroftp@1.2.3-alpha".into(),
        )
        .with_policy(FAST);
        api.set_tokens(Some(tokens("access-1"))).await;
        let store = Arc::new(MemoryStore::default());
        store.save(&tokens("access-2"));
        api.set_session_store(Arc::clone(&store) as Arc<dyn SessionStore>);

        let answer: Value = api.get("/core/v4/users", &[]).await.expect("adopted");
        assert_eq!(answer["Ok"], Value::Bool(true));
        assert_eq!(
            refreshes.load(Ordering::SeqCst),
            0,
            "no refresh may be sent"
        );
        let adopted = api.tokens().await.expect("tokens kept");
        assert_eq!(adopted.access_token.expose_secret(), "access-2");
    }

    /// Saved tokens of another session (another sign-in under the same
    /// profile key) are never adopted by this one: the request reads as
    /// replaced, and the reconnect that follows opens that session itself.
    #[tokio::test]
    async fn saved_tokens_of_another_session_are_not_adopted() {
        let refreshes = Arc::new(AtomicUsize::new(0));
        let api = ProtonApi::with_base_url(
            serve(users_app(Arc::clone(&refreshes), REFRESH_DEAD)).await,
            "external-drive-aeroftp@1.2.3-alpha".into(),
        )
        .with_policy(FAST);
        api.set_tokens(Some(tokens("access-1"))).await;
        let store = Arc::new(MemoryStore::default());
        let mut other = tokens("access-2");
        other.uid = "uid-other".into();
        store.save(&other);
        api.set_session_store(Arc::clone(&store) as Arc<dyn SessionStore>);

        let error = api.get::<Value>("/core/v4/users", &[]).await.unwrap_err();
        assert!(matches!(error, ProtonError::SessionReplaced), "{error:?}");
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
        assert!(
            api.tokens().await.is_none(),
            "another session's tokens were adopted"
        );
    }

    /// A refused refresh with nothing else saved, or the same session saved,
    /// ends the session: only another session under the key reads as replaced.
    #[tokio::test]
    async fn a_dead_refresh_without_another_saved_session_ends_it() {
        for saved in [None, Some(tokens("access-1"))] {
            let refreshes = Arc::new(AtomicUsize::new(0));
            let api = ProtonApi::with_base_url(
                serve(users_app(Arc::clone(&refreshes), REFRESH_DEAD)).await,
                "external-drive-aeroftp@1.2.3-alpha".into(),
            )
            .with_policy(FAST);
            api.set_tokens(Some(tokens("access-1"))).await;
            let store = Arc::new(MemoryStore::default());
            if let Some(saved) = &saved {
                store.save(saved);
            }
            api.set_session_store(Arc::clone(&store) as Arc<dyn SessionStore>);

            let error = api.get::<Value>("/core/v4/users", &[]).await.unwrap_err();
            assert!(
                matches!(error, ProtonError::SessionExpired),
                "saved {}: {error:?}",
                saved.is_some()
            );
        }
    }

    #[test]
    fn the_log_masks_a_fork_selector_and_keeps_ids() {
        assert_eq!(
            loggable_path("/auth/v4/sessions/forks/7dkqwlvdgyi3v2dvuu4bgqmpnsscp47z"),
            "/auth/v4/sessions/forks/<selector>"
        );
        assert_eq!(
            loggable_path("/auth/v4/sessions/forks"),
            "/auth/v4/sessions/forks"
        );
        assert_eq!(
            loggable_path("drive/v2/volumes/vol-1/links/link-2"),
            "drive/v2/volumes/vol-1/links/link-2"
        );
    }

    /// Adopted tokens that are stale too get one real refresh, then the
    /// request goes through; a third 401 would end the session.
    #[tokio::test]
    async fn adopted_tokens_that_are_stale_too_are_refreshed() {
        let refreshes = Arc::new(AtomicUsize::new(0));
        let api = ProtonApi::with_base_url(
            serve(users_app(Arc::clone(&refreshes), REFRESH_OK)).await,
            "external-drive-aeroftp@1.2.3-alpha".into(),
        )
        .with_policy(FAST);
        api.set_tokens(Some(tokens("access-1"))).await;
        let store = Arc::new(MemoryStore::default());
        store.save(&tokens("access-0"));
        api.set_session_store(Arc::clone(&store) as Arc<dyn SessionStore>);

        let answer: Value = api.get("/core/v4/users", &[]).await.expect("refreshed");
        assert_eq!(answer["Ok"], Value::Bool(true));
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(store.saved_access().as_deref(), Some("access-2"));
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

    /// The small-file upload posts a multipart form to the API host with the
    /// session headers. A form is consumed by its send, so the retry after a
    /// 5xx must carry a full form of its own, not an empty body.
    #[tokio::test]
    async fn a_form_reaches_the_api_host_whole_on_every_attempt() {
        // What each attempt carried: its session uid and its raw body.
        type Attempts = Arc<std::sync::Mutex<Vec<(Option<String>, Vec<u8>)>>>;
        let seen: Attempts = Arc::default();
        let record = Arc::clone(&seen);
        let app = axum::Router::new().route(
            "/drive/v2/volumes/vol/files/small",
            axum::routing::post(
                move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
                    let record = Arc::clone(&record);
                    async move {
                        let uid = headers
                            .get("x-pm-uid")
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_owned);
                        let mut seen = record.lock().unwrap();
                        seen.push((uid, body.to_vec()));
                        if seen.len() == 1 {
                            (axum::http::StatusCode::BAD_GATEWAY, String::new())
                        } else {
                            (
                                axum::http::StatusCode::OK,
                                r#"{"Code":1000,"LinkID":"l","RevisionID":"r"}"#.to_string(),
                            )
                        }
                    }
                },
            ),
        );
        let api = ProtonApi::with_base_url(
            serve(app).await,
            "external-drive-aeroftp@1.2.3-alpha".into(),
        )
        .with_policy(FAST);
        api.set_tokens(Some(tokens("access-1"))).await;

        let parts = [
            FormPart {
                name: "Metadata",
                file_name: "Metadata",
                mime: "application/json",
                data: bytes::Bytes::from_static(br#"{"ParentLinkID":"p"}"#),
            },
            FormPart {
                name: "ContentBlock",
                file_name: "ContentBlock",
                mime: "application/octet-stream",
                data: bytes::Bytes::from_static(b"encrypted-block"),
            },
        ];
        let answer: Value = api
            .post_form("drive/v2/volumes/vol/files/small", &parts)
            .await
            .unwrap();
        assert_eq!(answer["LinkID"], serde_json::json!("l"));

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "one retry after the 502");
        for (uid, body) in seen.iter() {
            assert_eq!(uid.as_deref(), Some("uid-1"));
            let body = String::from_utf8_lossy(body);
            assert!(
                body.contains(r#"name="Metadata"; filename="Metadata""#),
                "{body}"
            );
            assert!(body.contains(r#"{"ParentLinkID":"p"}"#), "{body}");
            assert!(
                body.contains(r#"name="ContentBlock"; filename="ContentBlock""#),
                "{body}"
            );
            assert!(body.contains("encrypted-block"), "{body}");
        }
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
