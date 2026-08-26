//! OpenStack Swift provider (Blomp, OVH, Rackspace)
//!
//! Keystone v2 / TempAuth v1 authentication + Swift Object Store REST API.
//! Version detection is self-healing: if the detected flow fails, the other is
//! attempted before surfacing an error (see `authenticate`).
//!
//! Blomp-specific behaviour (black-box verified against a live account,
//! 2026-07-28):
//!   - Auth is Keystone v2 with `tenantName = "storage"`; the object-store
//!     endpoint resolves to `http://swiftproxy.acs.ai.net:8080` (cleartext
//!     HTTP on 8080 — HTTPS is not offered on that port). The catalog is
//!     still trusted because Keystone itself is reached over HTTPS.
//!   - The account (container-list) operation `GET {storage_url}?format=json`
//!     is **forbidden at the proxy** — HTTP 403 for every request and every
//!     User-Agent, not a spurious/transient failure. This is what made Blomp
//!     look "broken" (rclone hits the same wall).
//!   - A single per-account container DOES exist and is fully usable
//!     (list/upload/download/rename/delete); its name is the login username
//!     (email). `discover_container` therefore falls back to the username
//!     container when the account listing is denied.
//!   - Single container per account (creating a second returns 403); large
//!     files use SLO segments under `.file-segments/`.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use log::{debug, info, warn};
use reqwest::{Client, Method, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use std::time::{Duration, Instant};

use super::{
    http_retry::{send_with_retry, HttpRetryConfig},
    ProviderError, ProviderType, RemoteEntry, StorageInfo, StorageProvider,
};

/// Per-request timeout for the (tiny) auth exchanges. The shared client keeps a
/// long read timeout for large object bodies; auth responses are small, so cap
/// them separately so an accepting-but-stalling endpoint cannot hang a connect
/// for the full body timeout.
const AUTH_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

// ─── Configuration ─────────────────────────────────────────────────

/// OpenStack Swift configuration (extracted from ProviderConfig)
pub struct SwiftConfig {
    pub auth_url: String,
    pub username: String,
    pub password: SecretString,
    pub verify_cert: bool,
    /// Per-profile opt-in to a cleartext object-store endpoint.
    ///
    /// Swift lets the catalog point object storage at a different host AND a
    /// different scheme, and some deployments (Blomp publishes
    /// `http://swiftproxy.acs.ai.net:8080`) offer no TLS there at all. That is a
    /// real deployment, and it is not a reason to let every Swift account
    /// downgrade in silence: the storage endpoint carries `X-Auth-Token`, which
    /// is a bearer credential for the whole account. So the guard fails closed
    /// and the user opts in on the one profile that needs it, exactly as
    /// `verify_cert` already works on this same struct.
    pub allow_cleartext_storage_endpoint: bool,
}

/// Presets whose object store genuinely offers no HTTPS, verified against the
/// live service rather than assumed. Keep this list to deployments someone has
/// actually connected to: it is a statement of fact about a service, and every
/// entry costs the confidentiality of that provider's traffic.
fn preset_publishes_cleartext_storage(provider_id: &str) -> bool {
    matches!(provider_id, "blomp")
}

impl SwiftConfig {
    pub fn from_provider_config(config: &super::ProviderConfig) -> Result<Self, ProviderError> {
        let auth_url = if config.host.starts_with("http") {
            config.host.clone()
        } else {
            format!("https://{}", config.host)
        };
        Ok(Self {
            auth_url,
            username: config.username.clone().unwrap_or_default(),
            password: SecretString::from(config.password.clone().unwrap_or_default()),
            verify_cert: config
                .extra
                .get("verify_cert")
                .map(|v| v != "false")
                .unwrap_or(true),
            // Two ways in, and the preset is the one that matters in practice.
            //
            // A per-profile flag covers the private OpenStack deployments whose
            // catalog publishes cleartext object storage. But a Blomp profile is
            // not expressing a preference: its object store has no TLS on 8080
            // as a fact about the service, so the preset declares it and every
            // Blomp profile keeps working, including the ones saved before this
            // build, and including the CLI, MCP, benchmark and AeroCloud paths
            // that never see the GUI form.
            //
            // This keys on the preset the USER picked, not on the hostname the
            // catalog returned. Trusting a returned hostname would let the thing
            // being validated choose its own exemption; the preset cannot.
            allow_cleartext_storage_endpoint: config
                .extra
                .get("allow_cleartext_storage_endpoint")
                .is_some_and(|v| v == "true")
                || config
                    .extra
                    .get("provider_id")
                    .map(String::as_str)
                    .is_some_and(preset_publishes_cleartext_storage),
        })
    }
}

// ─── Internal types ────────────────────────────────────────────────

/// Detected auth version
#[derive(Debug)]
enum AuthVersion {
    V1, // TempAuth
    V2, // Keystone v2
}

/// Auth state (works for both v1 and v2)
struct SwiftAuth {
    token: SecretString,
    storage_url: String,
    obtained_at: Instant,
}

impl SwiftAuth {
    fn is_valid(&self) -> bool {
        self.obtained_at.elapsed() < Duration::from_secs(23 * 3600)
    }
}

#[derive(Debug, Deserialize)]
struct ContainerEntry {
    name: String,
    #[allow(dead_code)]
    count: u64,
    #[allow(dead_code)]
    bytes: u64,
}

#[derive(Debug, Deserialize)]
struct ObjectEntry {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    bytes: Option<u64>,
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    hash: Option<String>,
    #[serde(default)]
    last_modified: Option<String>,
    #[serde(default)]
    subdir: Option<String>,
}

#[derive(serde::Serialize)]
struct SloSegment {
    path: String,
    etag: String,
    size_bytes: u64,
}

/// Compute MD5 hex digest for a byte slice
fn md5_hex(data: &[u8]) -> String {
    use md5::{Digest, Md5};
    let hash = Md5::digest(data);
    format!("{:x}", hash)
}

// ─── Provider ──────────────────────────────────────────────────────

pub struct SwiftProvider {
    config: SwiftConfig,
    client: Client,
    auth: Option<SwiftAuth>,
    container: String,
    current_path: String,
    connected: bool,
}

impl SwiftProvider {
    pub fn new(config: SwiftConfig) -> Self {
        let client = Client::builder()
            .user_agent(crate::providers::AEROFTP_USER_AGENT)
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(1800))
            // Swift requests carry the non-standard X-Auth-Token header.
            // reqwest strips a few standard credentials on cross-origin
            // redirects, but cannot know this provider-specific header is a
            // secret. Refuse redirects rather than forwarding the token.
            .redirect(reqwest::redirect::Policy::none())
            .danger_accept_invalid_certs(!config.verify_cert)
            .build()
            .unwrap_or_else(|_| Client::new());

        Self {
            config,
            client,
            auth: None,
            container: String::new(),
            current_path: "/".to_string(),
            connected: false,
        }
    }

    // ─── Auth ──────────────────────────────────────────────────

    /// Swift deliberately permits Identity and Object Storage to use different
    /// hosts *and* schemes, so plain same-origin with `auth_url` is not a valid
    /// requirement. Blomp is the concrete production case: Keystone is
    /// `https://authenticate.blomp.com` while the object-store publicURL is
    /// `http://swiftproxy.acs.ai.net:8080/...` (no TLS on that port).
    ///
    /// The TLS-authenticated Keystone/TempAuth response that *named* this
    /// endpoint establishes that the endpoint is AUTHENTIC. It does not make the
    /// connection to it CONFIDENTIAL, and those are different questions: an
    /// authentic endpoint reached over cleartext still hands `X-Auth-Token` and
    /// every object byte to any passive observer on the path. So a downgrade
    /// from an HTTPS auth session fails closed, and the deployments that really
    /// do publish cleartext object storage opt in per profile.
    ///
    /// Every token-bearing request stays bound to this origin through
    /// `validate_request_target`, and authenticated redirects are refused.
    fn validate_storage_endpoint(&self, candidate: &str) -> Result<String, ProviderError> {
        let auth = reqwest::Url::parse(&self.config.auth_url).map_err(|e| {
            ProviderError::AuthenticationFailed(format!("Invalid Swift auth URL: {e}"))
        })?;
        let endpoint = reqwest::Url::parse(candidate).map_err(|e| {
            ProviderError::AuthenticationFailed(format!("Invalid Swift storage URL: {e}"))
        })?;
        if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host_str().is_none() {
            return Err(ProviderError::AuthenticationFailed(
                "Swift storage endpoint must be an absolute HTTP(S) URL".into(),
            ));
        }
        if !endpoint.username().is_empty() || endpoint.password().is_some() {
            return Err(ProviderError::AuthenticationFailed(
                "Swift storage endpoint must not contain URL credentials".into(),
            ));
        }
        if auth.scheme() == "https" && endpoint.scheme() != "https" {
            if !self.config.allow_cleartext_storage_endpoint {
                // The switch now has a home, so the message names it. It stays
                // useful for the profiles that never see the form: the CLI, MCP
                // and AeroCloud carry the same key, which is why both are named
                // rather than only the checkbox.
                return Err(ProviderError::AuthenticationFailed(format!(
                    "This Swift catalog serves object storage from {} over plain HTTP, \
                     while authentication used HTTPS. The access token and every file \
                     would cross the network unencrypted, so the connection was refused. \
                     Accept it for this deployment with the cleartext object store option \
                     in the connection form, or set allow_cleartext_storage_endpoint on \
                     the profile.",
                    endpoint.host_str().unwrap_or("?")
                )));
            }
            warn!(
                "[SWIFT] object-store endpoint {} is CLEARTEXT HTTP: token and payload are \
                 unencrypted (opted in on this profile)",
                endpoint.host_str().unwrap_or("?")
            );
        }
        Ok(candidate.trim_end_matches('/').to_string())
    }

    /// Every token-bearing request must stay on the origin selected by the
    /// authenticated storage endpoint. This also re-runs after a 401 refresh,
    /// because a refreshed catalog is allowed to move the endpoint.
    fn validate_request_target(&self, target: &str) -> Result<(), ProviderError> {
        let storage = reqwest::Url::parse(self.storage_url()?).map_err(|e| {
            ProviderError::AuthenticationFailed(format!("Invalid Swift storage URL: {e}"))
        })?;
        let target = reqwest::Url::parse(target)
            .map_err(|e| ProviderError::InvalidPath(format!("Invalid Swift request URL: {e}")))?;
        let same_origin = storage.scheme() == target.scheme()
            && storage.host_str() == target.host_str()
            && storage.port_or_known_default() == target.port_or_known_default();
        if !same_origin {
            return Err(ProviderError::AuthenticationFailed(
                "Refusing to send X-Auth-Token outside the authenticated Swift storage origin"
                    .into(),
            ));
        }
        Ok(())
    }

    /// Authenticate against OpenStack Swift.
    /// Auto-detects auth version by probing the endpoint:
    ///   - If root JSON contains "v2.0" → Keystone v2 (POST /v2.0/tokens)
    ///   - Otherwise → TempAuth v1 (GET /auth/v1.0)
    async fn authenticate(&mut self) -> Result<(), ProviderError> {
        let base = self.config.auth_url.trim_end_matches('/').to_string();

        // Probe root to detect auth version.
        let version = self.detect_auth_version(&base).await;
        debug!("Swift auth version detected: {:?}", version);

        // Try the detected flow, then self-heal by falling back to the other
        // one. The probe is best-effort (a transient GET failure used to drop
        // us to V1, which then hits a non-existent /auth/v1.0 → 404 and a bogus
        // "credentials" error), so never let a single guess be terminal.
        let (primary, secondary) = match version {
            AuthVersion::V2 => (
                self.auth_keystone_v2(&base).await,
                AuthVersion::V1, // fallback flow to attempt on failure
            ),
            AuthVersion::V1 => (self.auth_tempauth_v1(&base).await, AuthVersion::V2),
        };
        if primary.is_ok() {
            return primary;
        }
        let primary_err = primary.expect_err("primary is Err after is_ok check");
        debug!("Swift primary auth failed; trying fallback flow {secondary:?}");
        let fallback = match secondary {
            AuthVersion::V2 => self.auth_keystone_v2(&base).await,
            AuthVersion::V1 => self.auth_tempauth_v1(&base).await,
        };
        match fallback {
            Ok(()) => Ok(()),
            Err(fallback_err) => Err(Self::prefer_swift_auth_error(primary_err, fallback_err)),
        }
    }

    /// Choose which of two failed auth flows to surface.
    ///
    /// TempAuth `/auth/v1.0` returns HTTP 404 on Keystone-only deployments
    /// (Blomp). If the probe guessed V1 first, that 404 used to mask the real
    /// Keystone diagnostic ("Invalid credentials", catalog problems, …). Prefer
    /// any non-TempAuth-404 error; only keep the 404 when both sides are 404.
    fn prefer_swift_auth_error(primary: ProviderError, fallback: ProviderError) -> ProviderError {
        let is_tempauth_404 = |e: &ProviderError| match e {
            ProviderError::AuthenticationFailed(msg) => {
                msg.contains("TempAuth failed: HTTP 404")
                    || msg.contains("TempAuth failed: HTTP 404 Not Found")
            }
            _ => false,
        };
        if is_tempauth_404(&primary) && !is_tempauth_404(&fallback) {
            fallback
        } else if is_tempauth_404(&fallback) && !is_tempauth_404(&primary) {
            primary
        } else {
            // Detected-flow error is the better default when both are real.
            primary
        }
    }

    /// Detect auth version from the root version document.
    ///
    /// Keystone endpoints advertise `v2.0` / `v3` in the root JSON (and often
    /// answer HTTP 300 Multiple Choices); legacy TempAuth (Rackspace-style)
    /// does not. On any probe failure we default to Keystone v2 — the modern
    /// norm and what Blomp serves — because `authenticate()` self-heals to
    /// TempAuth v1 if that guess is wrong.
    async fn detect_auth_version(&self, base: &str) -> AuthVersion {
        // Known Blomp/AIN Keystone hosts never speak TempAuth; skip the probe
        // so a transient HTML interstitial cannot push us down the V1 path.
        if let Ok(url) = reqwest::Url::parse(base) {
            if let Some(host) = url.host_str() {
                let host = host.to_ascii_lowercase();
                if host == "authenticate.blomp.com"
                    || host == "authenticate.ain.net"
                    || host.ends_with(".blomp.com")
                {
                    return AuthVersion::V2;
                }
            }
        }

        match self
            .client
            .get(base)
            .timeout(AUTH_REQUEST_TIMEOUT)
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                debug!(
                    "Swift auth probe: status={} body_len={}",
                    status.as_u16(),
                    text.len()
                );
                if text.contains("v2.0")
                    || text.contains("v3")
                    || text.contains("\"versions\"")
                    || status.as_u16() == 300
                {
                    return AuthVersion::V2;
                }
                if text.is_empty() {
                    // Empty body: do not assume TempAuth (that path 404s on
                    // Keystone and used to poison the user-facing error).
                    return AuthVersion::V2;
                }
                // Reachable root that names neither Keystone version → TempAuth.
                AuthVersion::V1
            }
            Err(e) => {
                debug!("Swift auth probe failed: {e}");
                AuthVersion::V2
            }
        }
    }

    /// TempAuth v1: GET {base}/auth/v1.0 with X-Auth-User + X-Auth-Key
    async fn auth_tempauth_v1(&mut self, base: &str) -> Result<(), ProviderError> {
        let url = format!("{base}/auth/v1.0");
        debug!("Swift TempAuth v1: {}", url);

        let resp = self
            .client
            .get(&url)
            .timeout(AUTH_REQUEST_TIMEOUT)
            .header("X-Auth-User", &self.config.username)
            .header("X-Auth-Key", self.config.password.expose_secret())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Auth request failed: {e}")))?;

        match resp.status() {
            StatusCode::OK | StatusCode::NO_CONTENT => {
                let token = resp
                    .headers()
                    .get("x-auth-token")
                    .or_else(|| resp.headers().get("x-storage-token"))
                    .ok_or_else(|| {
                        ProviderError::AuthenticationFailed("No X-Auth-Token in response".into())
                    })?
                    .to_str()
                    .map_err(|_| {
                        ProviderError::AuthenticationFailed("Invalid token header encoding".into())
                    })?
                    .to_string();

                let storage_url = resp
                    .headers()
                    .get("x-storage-url")
                    .ok_or_else(|| {
                        ProviderError::AuthenticationFailed("No X-Storage-Url in response".into())
                    })?
                    .to_str()
                    .map_err(|_| {
                        ProviderError::AuthenticationFailed(
                            "Invalid storage URL header encoding".into(),
                        )
                    })?
                    .to_string();
                let storage_url = self.validate_storage_endpoint(&storage_url)?;

                info!("Swift TempAuth OK: storage: {}", storage_url);
                self.auth = Some(SwiftAuth {
                    token: SecretString::from(token),
                    storage_url,
                    obtained_at: Instant::now(),
                });
                Ok(())
            }
            StatusCode::UNAUTHORIZED => Err(ProviderError::AuthenticationFailed(
                "Invalid credentials".into(),
            )),
            StatusCode::FORBIDDEN => Err(ProviderError::AuthenticationFailed(
                "Account suspended or forbidden".into(),
            )),
            status => Err(ProviderError::AuthenticationFailed(format!(
                "TempAuth failed: HTTP {status}"
            ))),
        }
    }

    /// Keystone v2: POST {base}/v2.0/tokens
    /// Body: {"auth":{"passwordCredentials":{"username":"...","password":"..."}}}
    /// Response: token in access.token.id, storage URL in access.serviceCatalog
    async fn auth_keystone_v2(&mut self, base: &str) -> Result<(), ProviderError> {
        let url = format!("{base}/v2.0/tokens");
        debug!("Swift Keystone v2: {}", url);

        // Blomp uses tenantName = "storage" (fixed for all accounts).
        // Other Swift providers may use the username or project name.
        let body = serde_json::json!({
            "auth": {
                "passwordCredentials": {
                    "username": self.config.username,
                    "password": self.config.password.expose_secret()
                },
                "tenantName": "storage"
            }
        });

        let resp = self
            .client
            .post(&url)
            .timeout(AUTH_REQUEST_TIMEOUT)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!("Keystone v2 request failed: {e}"))
            })?;

        match resp.status() {
            StatusCode::OK => {
                let json: serde_json::Value = resp.json().await.map_err(|e| {
                    ProviderError::AuthenticationFailed(format!("Invalid Keystone response: {e}"))
                })?;

                // Extract token
                let token = json["access"]["token"]["id"]
                    .as_str()
                    .ok_or_else(|| {
                        ProviderError::AuthenticationFailed("No token in Keystone response".into())
                    })?
                    .to_string();

                // Extract storage URL from service catalog
                // Try "object-store" first, fall back to any service with a publicURL
                let catalog = json["access"]["serviceCatalog"].as_array();
                let storage_url_str = catalog
                    .and_then(|cat| {
                        // First try object-store
                        cat.iter()
                            .find(|svc| svc["type"].as_str() == Some("object-store"))
                            .or_else(|| {
                                // Log available types for debugging
                                let types: Vec<&str> =
                                    cat.iter().filter_map(|s| s["type"].as_str()).collect();
                                debug!("Keystone catalog types: {:?}", types);
                                // Fall back to first service with a publicURL
                                cat.iter().find(|svc| {
                                    svc["endpoints"]
                                        .as_array()
                                        .and_then(|eps| eps.first())
                                        .and_then(|ep| ep["publicURL"].as_str())
                                        .is_some()
                                })
                            })
                    })
                    .and_then(|svc| svc["endpoints"].as_array())
                    .and_then(|endpoints| endpoints.first())
                    .and_then(|ep| {
                        ep["publicURL"]
                            .as_str()
                            .or_else(|| ep["internalURL"].as_str())
                    })
                    .ok_or_else(|| {
                        // Dump catalog for debugging
                        let cat_debug = catalog
                            .map(|c| {
                                c.iter()
                                    .map(|s| {
                                        format!(
                                            "type={}, endpoints={}",
                                            s["type"].as_str().unwrap_or("?"),
                                            s["endpoints"]
                                        )
                                    })
                                    .collect::<Vec<_>>()
                                    .join("; ")
                            })
                            .unwrap_or_else(|| "empty catalog".to_string());
                        warn!("Keystone catalog: {}", cat_debug);
                        ProviderError::AuthenticationFailed(
                            "No storage endpoint in Keystone service catalog".into(),
                        )
                    })?;
                let storage_url = self.validate_storage_endpoint(storage_url_str)?;

                info!("Swift Keystone v2 OK: storage: {}", storage_url);
                self.auth = Some(SwiftAuth {
                    token: SecretString::from(token),
                    storage_url,
                    obtained_at: Instant::now(),
                });
                Ok(())
            }
            StatusCode::UNAUTHORIZED => Err(ProviderError::AuthenticationFailed(
                "Invalid credentials".into(),
            )),
            StatusCode::FORBIDDEN => Err(ProviderError::AuthenticationFailed(
                "Account suspended or forbidden".into(),
            )),
            status => Err(ProviderError::AuthenticationFailed(format!(
                "Keystone v2 failed: HTTP {status}"
            ))),
        }
    }

    /// Ensure we have a valid token, re-auth if expired
    async fn ensure_auth(&mut self) -> Result<(), ProviderError> {
        if self.auth.as_ref().is_none_or(|a| !a.is_valid()) {
            self.authenticate().await?;
        }
        Ok(())
    }

    fn token(&self) -> Result<&str, ProviderError> {
        self.auth
            .as_ref()
            .map(|a| a.token.expose_secret())
            .ok_or_else(|| ProviderError::AuthenticationFailed("Not authenticated".into()))
    }

    fn storage_url(&self) -> Result<&str, ProviderError> {
        self.auth
            .as_ref()
            .map(|a| a.storage_url.as_str())
            .ok_or_else(|| ProviderError::AuthenticationFailed("Not authenticated".into()))
    }

    // ─── Container discovery ───────────────────────────────────

    /// Discover the working container.
    ///
    /// Standard Swift deployments (OVH, Rackspace, self-hosted) answer the
    /// account listing `GET {storage_url}?format=json` with the list of
    /// containers. Blomp, however, forbids the account-level operation at the
    /// proxy (HTTP 403 for every request and every User-Agent — verified
    /// black-box against a live account) while still serving a single
    /// per-account container named exactly after the login username/email.
    ///
    /// So: try the account listing first; on 403/401 (or an empty account),
    /// fall back to the deterministic Blomp container named `= username`,
    /// confirming it exists with a HEAD before committing.
    async fn discover_container(&mut self) -> Result<String, ProviderError> {
        let url = format!("{}?format=json", self.storage_url()?);
        debug!("Swift container discovery: {}", url);

        let resp = self.swift_request(Method::GET, &url, None, &[]).await?;
        let status = resp.status();

        // Blomp: account listing is forbidden — fall back to the username
        // container. Also cover 401 (some proxies mask it) and 404.
        if matches!(
            status,
            StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED | StatusCode::NOT_FOUND
        ) {
            debug!("Account listing HTTP {status}; trying username container fallback");
            return self.fallback_username_container().await;
        }

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Container list failed: HTTP {status}: {}",
                &body[..body.len().min(200)]
            )));
        }

        let text = resp
            .text()
            .await
            .map_err(|e| ProviderError::ServerError(format!("Read body failed: {e}")))?;
        debug!(
            "Container list (HTTP {}): {}",
            status,
            &text[..text.len().min(300)]
        );

        let containers: Vec<ContainerEntry> = serde_json::from_str(&text).map_err(|e| {
            ProviderError::ServerError(format!(
                "Invalid container JSON: {e}: body: {}",
                &text[..text.len().min(200)]
            ))
        })?;

        match containers.first() {
            Some(c) => {
                info!("Swift using container: {}", c.name);
                Ok(c.name.clone())
            }
            // Empty account: the container may exist but be hidden from the
            // account listing (Blomp-style). Try the username fallback.
            None => self.fallback_username_container().await,
        }
    }

    /// Blomp fallback: the single container is named after the login username.
    /// Confirm it with a HEAD so we fail with a clear message if absent.
    async fn fallback_username_container(&mut self) -> Result<String, ProviderError> {
        let name = self.config.username.trim().to_string();
        if name.is_empty() {
            return Err(ProviderError::ServerError(
                "No containers found and no username to derive the container name".into(),
            ));
        }

        // The container name is used verbatim throughout object_url(), so probe
        // it verbatim here too (Blomp accepts the raw email as the path segment).
        let url = format!("{}/{}", self.storage_url()?, name);
        let resp = self.swift_request(Method::HEAD, &url, None, &[]).await?;
        let status = resp.status();
        if status.is_success() {
            // Do not log `name`: the Blomp container name is the login email (PII).
            info!("Swift using per-account username container (Blomp fallback)");
            Ok(name)
        } else {
            Err(ProviderError::ServerError(format!(
                "Account listing denied and the per-account (username) container is not reachable (HTTP {status})"
            )))
        }
    }

    // ─── URL helpers ───────────────────────────────────────────

    fn object_url(&self, path: &str) -> Result<String, ProviderError> {
        let storage = self.storage_url()?;
        let clean = path.trim_start_matches('/');
        if clean.is_empty() {
            Ok(format!("{}/{}", storage, self.container))
        } else {
            Ok(format!(
                "{}/{}/{}",
                storage,
                self.container,
                urlencoding::encode(clean).replace("%2F", "/")
            ))
        }
    }

    /// Swift object keys are flat, so a path is only ever a prefix. This turns
    /// a UI path into that prefix, and the segment walk is the point: without
    /// it, `.` survives as a literal segment and the listing asks the server
    /// for the objects starting with `./`, which are none. The server answers
    /// 200 with an empty array, so the panel shows an empty container with no
    /// error at all, which is exactly how this hid.
    ///
    /// The GUI reaches it: `provider_list_files` defaults its path to `"."`, so
    /// the first listing after connecting sent `./` and every Swift account
    /// looked empty in the app while the CLI, which passes `/`, worked.
    fn normalize_path(path: &str) -> String {
        // Leading and trailing slashes are framing and go, exactly as the
        // original trim did. Repeated slashes INSIDE the path do not: a Swift
        // object name is an opaque key, so `a//b` and `a/b` name two different
        // objects and collapsing them would point rename, copy and delete at
        // the wrong one. Only `.` and `..` are resolved, because the GUI opens
        // a session on `.` and a relative segment has no meaning to the server.
        let mut segments: Vec<&str> = Vec::new();
        for segment in path.trim_matches('/').split('/') {
            match segment {
                "." => {}
                ".." => {
                    segments.pop();
                }
                other => segments.push(other),
            }
        }
        segments.join("/")
    }

    // ─── Request with 401 retry ────────────────────────────────

    async fn swift_request(
        &mut self,
        method: Method,
        url: &str,
        body: Option<Vec<u8>>,
        extra_headers: &[(String, String)],
    ) -> Result<reqwest::Response, ProviderError> {
        self.ensure_auth().await?;
        self.validate_request_target(url)?;

        let mut req = self
            .client
            .request(method.clone(), url)
            .header("X-Auth-Token", self.token()?);
        for (k, v) in extra_headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if let Some(ref data) = body {
            req = req.body(data.clone());
        }

        let request = req
            .build()
            .map_err(|e| ProviderError::NetworkError(format!("Failed to build request: {e}")))?;
        let resp = send_with_retry(&self.client, request, &HttpRetryConfig::default())
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Request failed: {e}")))?;

        if resp.status() == StatusCode::UNAUTHORIZED {
            // Re-auth and retry once
            self.authenticate().await?;
            self.validate_request_target(url)?;
            let mut req2 = self
                .client
                .request(method, url)
                .header("X-Auth-Token", self.token()?);
            for (k, v) in extra_headers {
                req2 = req2.header(k.as_str(), v.as_str());
            }
            if let Some(data) = body {
                req2 = req2.body(data);
            }
            let request2 = req2.build().map_err(|e| {
                ProviderError::NetworkError(format!("Failed to build request: {e}"))
            })?;
            send_with_retry(&self.client, request2, &HttpRetryConfig::default())
                .await
                .map_err(|e| ProviderError::ConnectionFailed(format!("Retry failed: {e}")))
        } else {
            Ok(resp)
        }
    }

    // ─── SLO upload ────────────────────────────────────────────

    /// Upload file >5GB via Static Large Objects.
    /// 1. Split into 1GiB chunks
    /// 2. PUT each to {container}/.file-segments/{object}/{seq:010}
    /// 3. PUT SLO manifest to {container}/{object}?multipart-manifest=put
    async fn upload_slo(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        use tokio::io::AsyncReadExt;

        let file = tokio::fs::File::open(local_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Open failed: {e}")))?;
        let file_size = file
            .metadata()
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Metadata failed: {e}")))?
            .len();

        let chunk_size: usize = 1024 * 1024 * 1024; // 1 GiB
        let object_name = Self::normalize_path(remote_path);
        let mut segments: Vec<SloSegment> = Vec::new();
        let mut reader = tokio::io::BufReader::new(file);
        let mut seq: u64 = 1;
        let mut uploaded: u64 = 0;

        loop {
            let mut buf = vec![0u8; chunk_size];
            let mut total_read = 0usize;

            loop {
                let n = reader.read(&mut buf[total_read..]).await.map_err(|e| {
                    ProviderError::TransferFailed(format!("Read chunk failed: {e}"))
                })?;
                if n == 0 {
                    break;
                }
                total_read += n;
                if total_read >= chunk_size {
                    break;
                }
            }

            if total_read == 0 {
                break;
            }
            buf.truncate(total_read);

            let segment_path = format!(".file-segments/{object_name}/{seq:010}");
            let segment_url = self.object_url(&segment_path)?;
            let digest = md5_hex(&buf);
            let segment_size = buf.len() as u64;

            let headers = vec![
                (
                    "Content-Type".to_string(),
                    "application/octet-stream".to_string(),
                ),
                ("ETag".to_string(), digest.clone()),
            ];

            let resp = self
                .swift_request(Method::PUT, &segment_url, Some(buf), &headers)
                .await?;
            if resp.status() != StatusCode::CREATED {
                return Err(ProviderError::ServerError(format!(
                    "Segment {seq} upload failed: HTTP {}",
                    resp.status()
                )));
            }

            let etag = resp
                .headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.trim_matches('"').to_string())
                .unwrap_or(digest);

            segments.push(SloSegment {
                path: format!("/{}/{}", self.container, segment_path),
                etag,
                size_bytes: segment_size,
            });

            uploaded += segment_size;
            if let Some(ref cb) = on_progress {
                cb(uploaded, file_size);
            }
            seq += 1;
        }

        // PUT SLO manifest
        let manifest_url = format!("{}?multipart-manifest=put", self.object_url(&object_name)?);
        let manifest_json = serde_json::to_vec(&segments)
            .map_err(|e| ProviderError::ServerError(format!("Manifest JSON failed: {e}")))?;

        let headers = vec![("Content-Type".to_string(), "application/json".to_string())];
        let resp = self
            .swift_request(Method::PUT, &manifest_url, Some(manifest_json), &headers)
            .await?;
        if !resp.status().is_success() {
            return Err(ProviderError::ServerError(format!(
                "SLO manifest upload failed: HTTP {}",
                resp.status()
            )));
        }

        info!(
            "SLO upload complete: {} ({} segments)",
            object_name,
            segments.len()
        );
        Ok(())
    }
}

// ─── StorageProvider trait ──────────────────────────────────────────

#[async_trait]
impl StorageProvider for SwiftProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn provider_type(&self) -> ProviderType {
        ProviderType::Swift
    }
    fn display_name(&self) -> String {
        format!("Swift ({})", self.config.username)
    }
    fn account_email(&self) -> Option<String> {
        Some(self.config.username.clone())
    }
    fn is_connected(&self) -> bool {
        self.connected
    }

    /// Connect: authenticate + discover default container
    async fn connect(&mut self) -> Result<(), ProviderError> {
        self.authenticate().await?;
        self.container = self.discover_container().await?;
        self.connected = true;
        info!("Swift connected: container: {}", self.container);
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.auth = None;
        self.connected = false;
        Ok(())
    }

    /// List objects with virtual directory simulation.
    /// GET {storage_url}/{container}?prefix={path}/&delimiter=/&format=json&limit=10000
    /// Paginates via marker parameter.
    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let prefix = Self::normalize_path(path);
        let prefix_query = if prefix.is_empty() {
            String::new()
        } else {
            format!("{prefix}/")
        };

        let mut all_entries = Vec::new();
        let mut marker = String::new();

        loop {
            let base = format!("{}/{}", self.storage_url()?, self.container);
            let mut url = format!("{base}?format=json&delimiter=/&limit=10000");
            if !prefix_query.is_empty() {
                url.push_str(&format!("&prefix={}", urlencoding::encode(&prefix_query)));
            }
            if !marker.is_empty() {
                url.push_str(&format!("&marker={}", urlencoding::encode(&marker)));
            }

            let resp = self.swift_request(Method::GET, &url, None, &[]).await?;
            if !resp.status().is_success() {
                return Err(ProviderError::ServerError(format!(
                    "List failed: HTTP {}",
                    resp.status()
                )));
            }

            let entries: Vec<ObjectEntry> = resp
                .json()
                .await
                .map_err(|e| ProviderError::ServerError(format!("List JSON parse failed: {e}")))?;

            if entries.is_empty() {
                break;
            }

            let last_name = entries
                .last()
                .and_then(|e| e.name.as_ref().or(e.subdir.as_ref()))
                .cloned()
                .unwrap_or_default();

            for entry in &entries {
                if let Some(ref subdir) = entry.subdir {
                    // Virtual directory pseudo-entry
                    let dir_name = subdir
                        .trim_end_matches('/')
                        .rsplit('/')
                        .next()
                        .unwrap_or(subdir);
                    if !dir_name.is_empty() {
                        all_entries.push(RemoteEntry::directory(
                            dir_name.to_string(),
                            format!("/{}", subdir.trim_end_matches('/')),
                        ));
                    }
                } else if let Some(ref name) = entry.name {
                    // Skip .file-segments/ (SLO internals)
                    if name.contains("/.file-segments/") || name.starts_with(".file-segments/") {
                        continue;
                    }

                    if name.ends_with('/') && entry.bytes.unwrap_or(0) == 0 {
                        // Directory marker object
                        let dir_name = name
                            .trim_end_matches('/')
                            .rsplit('/')
                            .next()
                            .unwrap_or(name);
                        if !dir_name.is_empty() {
                            all_entries.push(RemoteEntry::directory(
                                dir_name.to_string(),
                                format!("/{}", name.trim_end_matches('/')),
                            ));
                        }
                    } else {
                        // Regular file
                        let file_name = name.rsplit('/').next().unwrap_or(name);
                        let mut re = RemoteEntry::file(
                            file_name.to_string(),
                            format!("/{name}"),
                            entry.bytes.unwrap_or(0),
                        );
                        re.modified = entry.last_modified.clone();
                        re.mime_type = entry.content_type.clone();
                        if let Some(ref hash) = entry.hash {
                            re.metadata.insert("etag".to_string(), hash.clone());
                        }
                        all_entries.push(re);
                    }
                }
            }

            if entries.len() < 10000 {
                break;
            }
            marker = last_name;
        }

        Ok(all_entries)
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        self.current_path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("{}/{}", self.current_path.trim_end_matches('/'), path)
        };
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        if self.current_path != "/" {
            if let Some((parent, _)) = self.current_path.rsplit_once('/') {
                self.current_path = if parent.is_empty() {
                    "/".to_string()
                } else {
                    parent.to_string()
                };
            }
        }
        Ok(())
    }

    /// GET {storage_url}/{container}/{object}
    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let url = self.object_url(remote_path)?;
        let resp = self.swift_request(Method::GET, &url, None, &[]).await?;

        if !resp.status().is_success() {
            return Err(ProviderError::ServerError(format!(
                "Download failed: HTTP {}",
                resp.status()
            )));
        }

        let total = resp.content_length().unwrap_or(0);

        // Read full response body
        let bytes = resp.bytes().await.map_err(|e| {
            ProviderError::TransferFailed(format!("Download body read failed: {e}"))
        })?;
        let bytes_written = bytes.len() as u64;

        // Ensure parent directory exists
        if let Some(parent) = std::path::Path::new(local_path).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| ProviderError::TransferFailed(format!("Create dir failed: {e}")))?;
        }

        tokio::fs::write(local_path, &bytes)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Write file failed: {e}")))?;

        if let Some(cb) = on_progress {
            cb(bytes_written, total.max(bytes_written));
        }

        Ok(())
    }

    fn supports_resume(&self) -> bool {
        true
    }

    async fn resume_download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        _offset: u64,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        self.ensure_auth().await?;
        let url = self.object_url(remote_path)?;
        let token = self.token()?.to_string();

        super::http_resumable_download(
            local_path,
            |range_header| {
                let mut req = self.client.get(&url).header("X-Auth-Token", &token);
                if let Some(range) = range_header {
                    req = req.header("Range", range);
                }
                req
            },
            on_progress,
        )
        .await
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let url = self.object_url(remote_path)?;
        let resp = self.swift_request(Method::GET, &url, None, &[]).await?;

        if !resp.status().is_success() {
            return Err(ProviderError::ServerError(format!(
                "Download failed: HTTP {}",
                resp.status()
            )));
        }

        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| ProviderError::TransferFailed(format!("Read bytes failed: {e}")))
    }

    /// PUT {storage_url}/{container}/{object}
    /// Headers: Content-Type, ETag (MD5), X-Object-Meta-Mtime
    /// For files >5GB, delegates to upload_slo()
    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let metadata = tokio::fs::metadata(local_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Stat local file failed: {e}")))?;
        let file_size = metadata.len();

        // SLO for files > 5 GiB
        if file_size > 5 * 1024 * 1024 * 1024 {
            return self.upload_slo(local_path, remote_path, on_progress).await;
        }

        let data = tokio::fs::read(local_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Read local file failed: {e}")))?;

        let url = self.object_url(remote_path)?;

        // Content-Type from filename
        let mime = mime_guess::from_path(remote_path)
            .first_or_octet_stream()
            .to_string();

        // MD5 for integrity
        let digest = md5_hex(&data);

        // Preserve local mtime
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| format!("{}.000000", d.as_secs()));

        let mut headers = vec![
            ("Content-Type".to_string(), mime),
            ("ETag".to_string(), digest),
        ];
        if let Some(mt) = mtime {
            headers.push(("X-Object-Meta-Mtime".to_string(), mt));
        }

        let resp = self
            .swift_request(Method::PUT, &url, Some(data), &headers)
            .await?;

        match resp.status() {
            StatusCode::CREATED => {
                if let Some(cb) = on_progress {
                    cb(file_size, file_size);
                }
                Ok(())
            }
            StatusCode::UNPROCESSABLE_ENTITY => Err(ProviderError::ServerError(
                "ETag mismatch: data corrupted in transit".into(),
            )),
            status => Err(ProviderError::ServerError(format!(
                "Upload failed: HTTP {status}"
            ))),
        }
    }

    /// PUT zero-byte object with trailing / as directory marker
    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let dir_path = format!("{}/", Self::normalize_path(path));
        let url = self.object_url(&dir_path)?;

        let headers = vec![
            (
                "Content-Type".to_string(),
                "application/directory".to_string(),
            ),
            ("Content-Length".to_string(), "0".to_string()),
        ];

        let resp = self
            .swift_request(Method::PUT, &url, Some(vec![]), &headers)
            .await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(ProviderError::ServerError(format!(
                "Mkdir failed: HTTP {}",
                resp.status()
            )))
        }
    }

    /// DELETE {storage_url}/{container}/{object}
    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let url = self.object_url(path)?;
        let resp = self.swift_request(Method::DELETE, &url, None, &[]).await?;

        match resp.status() {
            StatusCode::NO_CONTENT | StatusCode::OK => Ok(()),
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound(path.to_string())),
            status => Err(ProviderError::ServerError(format!(
                "Delete failed: HTTP {status}"
            ))),
        }
    }

    /// Delete directory marker
    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        // Try with trailing slash (directory marker convention)
        let dir_path = format!("{}/", Self::normalize_path(path));
        let url = self.object_url(&dir_path)?;
        let resp = self.swift_request(Method::DELETE, &url, None, &[]).await?;
        match resp.status() {
            StatusCode::NO_CONTENT | StatusCode::OK | StatusCode::NOT_FOUND => Ok(()),
            status => Err(ProviderError::ServerError(format!(
                "Rmdir failed: HTTP {status}"
            ))),
        }
    }

    /// Recursive delete via bulk-delete.
    /// POST {storage_url}?bulk-delete
    ///   Content-Type: text/plain
    ///   Body: /{container}/path1\n/{container}/path2\n...
    /// Max 10000 per request.
    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        if path.trim_matches('/').is_empty() {
            return Err(ProviderError::InvalidPath(
                "Refusing to recursively delete root '/'. This would erase the entire container."
                    .into(),
            ));
        }
        let prefix = Self::normalize_path(path);

        // List all objects under prefix (no delimiter = flat recursive listing)
        let base = format!("{}/{}", self.storage_url()?, self.container);
        let url = format!(
            "{}?format=json&prefix={}/&limit=10000",
            base,
            urlencoding::encode(&prefix)
        );

        let resp = self.swift_request(Method::GET, &url, None, &[]).await?;
        let entries: Vec<ObjectEntry> = resp
            .json()
            .await
            .map_err(|e| ProviderError::ServerError(format!("List for delete failed: {e}")))?;

        if entries.is_empty() {
            let _ = self.rmdir(path).await;
            return Ok(());
        }

        // Collect all object paths for bulk delete
        let mut object_paths: Vec<String> = entries
            .iter()
            .filter_map(|e| e.name.as_ref())
            .map(|n| format!("/{}/{n}", self.container))
            .collect();

        // Also delete the directory marker itself
        object_paths.push(format!("/{}/{prefix}/", self.container));

        // Bulk delete in chunks of 10000
        for chunk in object_paths.chunks(10000) {
            let body = chunk.join("\n");
            let bulk_url = format!("{}?bulk-delete", self.storage_url()?);

            let headers = vec![
                ("Content-Type".to_string(), "text/plain".to_string()),
                ("Accept".to_string(), "application/json".to_string()),
            ];

            let resp = self
                .swift_request(Method::POST, &bulk_url, Some(body.into_bytes()), &headers)
                .await?;

            if !resp.status().is_success() {
                return Err(ProviderError::ServerError(format!(
                    "Bulk delete failed: HTTP {}",
                    resp.status()
                )));
            }

            // Swift bulk-delete can return HTTP 200 with per-object failures in
            // the JSON body (`Errors` array). Accept is already application/json;
            // parse it so we do not report a full success when objects remain.
            // Spec: https://docs.openstack.org/swift/latest/middleware.html#bulk-delete
            let body_bytes = resp.bytes().await.map_err(|e| {
                ProviderError::ServerError(format!("Bulk delete body read failed: {e}"))
            })?;
            if !body_bytes.is_empty() {
                if let Ok(report) = serde_json::from_slice::<serde_json::Value>(&body_bytes) {
                    let errors = report
                        .get("Errors")
                        .or_else(|| report.get("errors"))
                        .and_then(|e| e.as_array());
                    if let Some(errors) = errors {
                        if !errors.is_empty() {
                            let sample = errors
                                .iter()
                                .take(3)
                                .map(|e| e.to_string())
                                .collect::<Vec<_>>()
                                .join("; ");
                            return Err(ProviderError::ServerError(format!(
                                "Bulk delete reported {} object failure(s): {sample}",
                                errors.len()
                            )));
                        }
                    }
                    // Some proxies put the overall status in the body even when
                    // the HTTP status is 200.
                    if let Some(rs) = report
                        .get("Response Status")
                        .or_else(|| report.get("response_status"))
                        .and_then(|v| v.as_str())
                    {
                        let ok = rs.starts_with('2')
                            || rs.to_ascii_lowercase().contains("200")
                            || rs.eq_ignore_ascii_case("ok");
                        if !ok {
                            return Err(ProviderError::ServerError(format!(
                                "Bulk delete response status: {rs}"
                            )));
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Rename via server-side COPY + DELETE (Swift has no atomic rename).
    /// PUT {dest_url} with X-Copy-From: /{container}/{source}
    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from_clean = Self::normalize_path(from);
        let to_clean = Self::normalize_path(to);

        let dest_url = self.object_url(&to_clean)?;
        let copy_from = format!("/{}/{from_clean}", self.container);

        let headers = vec![
            ("X-Copy-From".to_string(), copy_from),
            ("Content-Length".to_string(), "0".to_string()),
        ];

        let resp = self
            .swift_request(Method::PUT, &dest_url, Some(vec![]), &headers)
            .await?;
        if !resp.status().is_success() {
            return Err(ProviderError::ServerError(format!(
                "Copy for rename failed: HTTP {}",
                resp.status()
            )));
        }

        self.delete(from).await
    }

    /// HEAD {storage_url}/{container}/{object}
    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let url = self.object_url(path)?;
        let resp = self.swift_request(Method::HEAD, &url, None, &[]).await?;

        match resp.status() {
            StatusCode::OK | StatusCode::NO_CONTENT => {
                let size = resp
                    .headers()
                    .get("content-length")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0);

                let modified = resp
                    .headers()
                    .get("last-modified")
                    .and_then(|v| v.to_str().ok())
                    .map(|v| v.to_string());

                let mime = resp
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .map(|v| v.to_string());

                let is_dir =
                    mime.as_deref() == Some("application/directory") || path.ends_with('/');
                let name = path
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or(path)
                    .to_string();

                let mut entry = if is_dir {
                    RemoteEntry::directory(name, format!("/{}", path.trim_matches('/')))
                } else {
                    RemoteEntry::file(name, format!("/{}", path.trim_matches('/')), size)
                };
                entry.modified = modified;
                entry.mime_type = mime;

                if let Some(etag) = resp.headers().get("etag").and_then(|v| v.to_str().ok()) {
                    entry
                        .metadata
                        .insert("etag".to_string(), etag.trim_matches('"').to_string());
                }
                if let Some(mtime) = resp
                    .headers()
                    .get("x-object-meta-mtime")
                    .and_then(|v| v.to_str().ok())
                {
                    entry
                        .metadata
                        .insert("mtime".to_string(), mtime.to_string());
                }

                Ok(entry)
            }
            StatusCode::NOT_FOUND => Err(ProviderError::NotFound(path.to_string())),
            status => Err(ProviderError::ServerError(format!(
                "Stat failed: HTTP {status}"
            ))),
        }
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        self.stat(path).await.map(|e| e.size)
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        match self.stat(path).await {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// HEAD {storage_url}: lightweight, validates token
    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        self.ensure_auth().await?;
        let url = self.storage_url()?.to_string();
        let resp = self.swift_request(Method::HEAD, &url, None, &[]).await?;
        if resp.status().is_success() || resp.status() == StatusCode::NO_CONTENT {
            Ok(())
        } else {
            Err(ProviderError::ConnectionFailed("Keep-alive failed".into()))
        }
    }

    /// HEAD {storage_url} -> account info headers
    async fn server_info(&mut self) -> Result<String, ProviderError> {
        let url = self.storage_url()?.to_string();
        let resp = self.swift_request(Method::HEAD, &url, None, &[]).await?;

        let get_header = |name: &str| -> String {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("?")
                .to_string()
        };

        Ok(format!(
            "OpenStack Swift\nContainer: {}\nContainers: {}\nObjects: {}\nStorage used: {} bytes",
            self.container,
            get_header("x-account-container-count"),
            get_header("x-account-object-count"),
            get_header("x-account-bytes-used"),
        ))
    }

    fn supports_server_copy(&self) -> bool {
        true
    }

    fn supports_server_side_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        // Legacy alias kept so CLI / MCP / provider_commands callers keep
        // working. The real `X-Copy-From` PUT lives on `server_side_copy`
        // (S3-T10 migration, v4.0.0).
        StorageProvider::server_side_copy(self, from, to).await
    }

    async fn server_side_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let from_clean = Self::normalize_path(from);
        let to_clean = Self::normalize_path(to);
        let dest_url = self.object_url(&to_clean)?;
        let copy_from = format!("/{}/{from_clean}", self.container);

        let headers = vec![
            ("X-Copy-From".to_string(), copy_from),
            ("Content-Length".to_string(), "0".to_string()),
        ];

        let resp = self
            .swift_request(Method::PUT, &dest_url, Some(vec![]), &headers)
            .await?;
        if resp.status() == StatusCode::CREATED || resp.status().is_success() {
            Ok(())
        } else {
            Err(ProviderError::ServerError(format!(
                "Server copy failed: HTTP {}",
                resp.status()
            )))
        }
    }

    /// HEAD {storage_url} -> X-Account-Bytes-Used + X-Account-Meta-Quota-Bytes.
    /// Default 40GB for Blomp free tier if quota header absent on a successful
    /// response. Non-success statuses (403 on account-level ops, 5xx, …) must
    /// not fabricate "0 used / 40 GB" as if they were real readings.
    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        let url = self.storage_url()?.to_string();
        let resp = self.swift_request(Method::HEAD, &url, None, &[]).await?;

        if !resp.status().is_success() {
            return Err(ProviderError::ServerError(format!(
                "Account HEAD failed: HTTP {}",
                resp.status()
            )));
        }

        let used: u64 = resp
            .headers()
            .get("x-account-bytes-used")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);

        let total: u64 = resp
            .headers()
            .get("x-account-meta-quota-bytes")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(42_949_672_960); // 40 GB default

        Ok(StorageInfo {
            used,
            total,
            free: total.saturating_sub(used),
            versioning_bytes: None,
        })
    }

    fn transfer_optimization_hints(&self) -> super::TransferOptimizationHints {
        super::TransferOptimizationHints {
            supports_resume_download: true,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_provider() -> SwiftProvider {
        SwiftProvider::new(SwiftConfig {
            auth_url: "https://keystone.example.com:5000/v3".to_string(),
            username: "user".to_string(),
            password: SecretString::from("pw".to_string()),
            verify_cert: true,
            // The shipping default: the downgrade guard is armed.
            allow_cleartext_storage_endpoint: false,
        })
    }

    /// A profile that has explicitly accepted a cleartext object-store endpoint,
    /// which is what the Blomp preset ships with.
    fn cleartext_opted_in_provider() -> SwiftProvider {
        SwiftProvider::new(SwiftConfig {
            auth_url: "https://authenticate.blomp.com".to_string(),
            username: "user".to_string(),
            password: SecretString::from("pw".to_string()),
            verify_cert: true,
            allow_cleartext_storage_endpoint: true,
        })
    }

    /// The GUI's default listing path is `"."`, not `"/"`, and Swift turned that
    /// into the prefix `./`, which matches no object. The container listed as
    /// empty in the app with a 200 and no error, while the CLI (which passes
    /// `/`) listed it correctly. Live-reproduced against Blomp: `ls /` returned
    /// ten entries and `ls .` returned zero.
    #[test]
    fn a_dot_path_is_the_container_root_like_a_slash() {
        assert_eq!(SwiftProvider::normalize_path("."), "");
        assert_eq!(SwiftProvider::normalize_path("./"), "");
        assert_eq!(SwiftProvider::normalize_path("/."), "");
        assert_eq!(
            SwiftProvider::normalize_path("."),
            SwiftProvider::normalize_path("/"),
            "the GUI default and the CLI default must address the same place"
        );
        // A dot inside a path is a segment to drop, not part of a name.
        assert_eq!(SwiftProvider::normalize_path("foo/./bar"), "foo/bar");
        // And `..` walks up rather than surviving into a prefix the server
        // would match literally.
        assert_eq!(SwiftProvider::normalize_path("foo/../bar"), "bar");
        assert_eq!(SwiftProvider::normalize_path("/foo/bar/../"), "foo");
        // A leading `..` cannot escape the container: there is nothing above it.
        assert_eq!(SwiftProvider::normalize_path("../secrets"), "secrets");
        // A file whose name merely contains a dot is untouched.
        assert_eq!(SwiftProvider::normalize_path("/a.txt"), "a.txt");
        assert_eq!(SwiftProvider::normalize_path("dir/.hidden"), "dir/.hidden");
    }

    #[test]
    fn normalize_path_strips_leading_and_trailing_slashes() {
        assert_eq!(SwiftProvider::normalize_path(""), "");
        assert_eq!(SwiftProvider::normalize_path("/"), "");
        assert_eq!(SwiftProvider::normalize_path("///"), "");
        assert_eq!(SwiftProvider::normalize_path("foo"), "foo");
        assert_eq!(SwiftProvider::normalize_path("/foo/bar/"), "foo/bar");
        assert_eq!(SwiftProvider::normalize_path("/a/b/c"), "a/b/c");
    }

    /// A Swift object name is an opaque key, so a repeated slash inside it is
    /// part of the name and not punctuation to tidy away. The first cut of the
    /// dot-path fix dropped every empty segment, which silently turned `a//b`
    /// into `a/b`: listing kept the server's name while rename, copy and delete
    /// went through the normalizer and addressed a different object. Raised by
    /// CodeRabbit on the pull request that introduced it.
    #[test]
    fn normalize_path_keeps_repeated_slashes_inside_the_name() {
        assert_eq!(SwiftProvider::normalize_path("a//b"), "a//b");
        assert_eq!(SwiftProvider::normalize_path("/a//b/"), "a//b");
        assert_eq!(
            SwiftProvider::normalize_path("dir//sub///file.txt"),
            "dir//sub///file.txt"
        );
        // Framing still goes, and the dot segments still resolve around them.
        assert_eq!(SwiftProvider::normalize_path("//a//b//"), "a//b");
        assert_eq!(SwiftProvider::normalize_path("a//./b"), "a//b");
    }

    #[test]
    fn md5_hex_produces_stable_hex_digest() {
        let digest = md5_hex(b"hello");
        assert_eq!(digest, "5d41402abc4b2a76b9719d911017c592");
        // md5 of empty
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn object_url_returns_error_when_not_authenticated() {
        let p = test_provider();
        // Without having authenticated, storage_url is unavailable → error
        assert!(p.object_url("some/object").is_err());
    }

    #[test]
    fn swift_auth_is_valid_within_ttl() {
        let auth = SwiftAuth {
            token: SecretString::from("t".to_string()),
            storage_url: "https://obj.example.com/v1/AUTH_a".to_string(),
            obtained_at: Instant::now(),
        };
        assert!(auth.is_valid());
    }

    #[test]
    fn storage_endpoint_rejects_credentials_and_malformed_urls() {
        let p = test_provider();
        assert_eq!(
            p.validate_storage_endpoint("https://objects.example.net/v1/AUTH_a/")
                .unwrap(),
            "https://objects.example.net/v1/AUTH_a"
        );
        assert!(p
            .validate_storage_endpoint("https://user:pw@objects.example.net/v1/AUTH_a")
            .is_err());
        assert!(p
            .validate_storage_endpoint("ftp://objects.example.net/v1/AUTH_a")
            .is_err());
        assert!(p.validate_storage_endpoint("not-a-url").is_err());
    }

    /// An HTTPS-authenticated session must not silently continue over cleartext.
    /// The catalog naming the endpoint proves it is authentic, not that the path
    /// to it is private, and the endpoint carries `X-Auth-Token`, which is a
    /// bearer credential for the whole account. v4.1.7 refused this; v4.1.8
    /// briefly accepted it for every Swift account, which is what this pins.
    #[test]
    fn an_https_session_refuses_a_cleartext_storage_endpoint_by_default() {
        let p = test_provider();
        let err = p
            .validate_storage_endpoint("http://objects.example.net/v1/AUTH_a")
            .expect_err("a downgrade must fail closed");
        let msg = err.to_string();
        assert!(
            msg.contains("plain HTTP") && msg.contains("unencrypted"),
            "the refusal has to say what is at stake, got: {msg}"
        );
        // The opt-in now has a control, so the refusal points at it. Both names
        // are required: the form covers the GUI, and the profile key covers the
        // CLI, MCP and AeroCloud paths that never see a form. This assertion was
        // the inverse until the control existed, and it passed by accident for a
        // moment afterwards because the new wording happened to avoid the two
        // words it forbade.
        assert!(
            msg.contains("connection form"),
            "the refusal must name the control that lifts it: {msg}"
        );
        assert!(
            msg.contains("allow_cleartext_storage_endpoint"),
            "a profile without a form needs the key named too: {msg}"
        );
    }

    /// The one deployment that motivated the change still works, because the
    /// profile opts in rather than because the guard was removed for everyone.
    /// Blomp's Keystone is HTTPS while its object store is cleartext on 8080.
    #[test]
    fn an_opted_in_profile_still_accepts_its_cleartext_endpoint() {
        let p = cleartext_opted_in_provider();
        assert_eq!(
            p.validate_storage_endpoint(
                "http://swiftproxy.acs.ai.net:8080/v1/AUTH_8b989f118e624ca6957e102775583f6f"
            )
            .unwrap(),
            "http://swiftproxy.acs.ai.net:8080/v1/AUTH_8b989f118e624ca6957e102775583f6f"
        );
        // Opting in relaxes the scheme, and nothing else.
        assert!(p
            .validate_storage_endpoint("http://user:pw@swiftproxy.acs.ai.net:8080/v1/AUTH_a")
            .is_err());
    }

    /// The regression this nearly shipped with: arming the guard broke every
    /// Blomp profile that already existed, because nothing backfills a new
    /// per-profile flag onto profiles saved before the flag existed, and the
    /// CLI, MCP, benchmark and AeroCloud paths never see the GUI form that
    /// would have set it. The preset declaration is what makes those work, so
    /// this pins a profile carrying only `provider_id`, which is what a saved
    /// Blomp profile actually carries.
    #[test]
    fn a_saved_blomp_profile_keeps_working_without_any_stored_flag() {
        use std::collections::HashMap;
        let mut extra = HashMap::new();
        extra.insert("provider_id".to_string(), "blomp".to_string());
        let config = super::super::ProviderConfig {
            name: "Blomp".to_string(),
            provider_type: ProviderType::Swift,
            host: "https://authenticate.blomp.com".to_string(),
            port: None,
            username: Some("user".to_string()),
            password: Some("pw".to_string()),
            initial_path: None,
            extra,
        };
        let swift = SwiftConfig::from_provider_config(&config).expect("swift config");
        assert!(
            swift.allow_cleartext_storage_endpoint,
            "the Blomp preset has to carry its own exemption"
        );

        // And it is the preset, not Swift in general: another Swift profile with
        // no flag stays armed.
        let mut other = HashMap::new();
        other.insert("provider_id".to_string(), "custom-swift".to_string());
        let config = super::super::ProviderConfig {
            name: "Private cloud".to_string(),
            provider_type: ProviderType::Swift,
            host: "https://keystone.example.com".to_string(),
            port: None,
            username: Some("user".to_string()),
            password: Some("pw".to_string()),
            initial_path: None,
            extra: other,
        };
        let swift = SwiftConfig::from_provider_config(&config).expect("swift config");
        assert!(!swift.allow_cleartext_storage_endpoint);
    }

    /// A cleartext Keystone was never an HTTPS session to begin with, so there is
    /// no downgrade to refuse: the guard must not start rejecting plain-HTTP
    /// deployments that were consistent all along.
    #[test]
    fn a_cleartext_auth_session_is_not_a_downgrade() {
        let p = SwiftProvider::new(SwiftConfig {
            auth_url: "http://keystone.internal:5000/v3".to_string(),
            username: "user".to_string(),
            password: SecretString::from("pw".to_string()),
            verify_cert: true,
            allow_cleartext_storage_endpoint: false,
        });
        assert_eq!(
            p.validate_storage_endpoint("http://objects.internal/v1/AUTH_a")
                .unwrap(),
            "http://objects.internal/v1/AUTH_a"
        );
    }

    #[test]
    fn prefer_swift_auth_error_masks_tempauth_404_behind_keystone() {
        let tempauth_404 =
            || ProviderError::AuthenticationFailed("TempAuth failed: HTTP 404 Not Found".into());
        let invalid = || ProviderError::AuthenticationFailed("Invalid credentials".into());
        match SwiftProvider::prefer_swift_auth_error(tempauth_404(), invalid()) {
            ProviderError::AuthenticationFailed(msg) => assert_eq!(msg, "Invalid credentials"),
            other => panic!("unexpected {other:?}"),
        }
        match SwiftProvider::prefer_swift_auth_error(invalid(), tempauth_404()) {
            ProviderError::AuthenticationFailed(msg) => assert_eq!(msg, "Invalid credentials"),
            other => panic!("unexpected {other:?}"),
        }
        // Both 404 → keep primary
        match SwiftProvider::prefer_swift_auth_error(tempauth_404(), tempauth_404()) {
            ProviderError::AuthenticationFailed(msg) => {
                assert!(msg.contains("TempAuth failed: HTTP 404"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn token_bearing_requests_stay_on_authenticated_storage_origin() {
        let mut p = test_provider();
        p.auth = Some(SwiftAuth {
            token: SecretString::from("t".to_string()),
            storage_url: "https://objects.example.net/v1/AUTH_a".to_string(),
            obtained_at: Instant::now(),
        });
        assert!(p
            .validate_request_target("https://objects.example.net/v1/AUTH_a/container/file")
            .is_ok());
        assert!(p
            .validate_request_target("https://other.example.net/v1/AUTH_a/container/file")
            .is_err());
    }

    // Live smoke (ignored): confirms Blomp host forces Keystone and accepts HTTP storage URL shape.
    #[tokio::test]
    #[ignore = "network; requires BLOMP_USER + BLOMP_PASS"]
    async fn live_blomp_keystone_accepts_http_storage_url() {
        let user = std::env::var("BLOMP_USER").expect("BLOMP_USER");
        let pass = std::env::var("BLOMP_PASS").expect("BLOMP_PASS");
        let mut p = SwiftProvider::new(SwiftConfig {
            auth_url: "https://authenticate.blomp.com".into(),
            username: user,
            password: SecretString::from(pass),
            verify_cert: true,
            // Blomp's catalog publishes cleartext object storage, so the preset
            // opts in; without this the downgrade guard refuses the endpoint.
            allow_cleartext_storage_endpoint: true,
        });
        p.authenticate().await.expect("blomp auth");
        let url = p.storage_url().unwrap();
        assert!(
            url.starts_with("http://swiftproxy.acs.ai.net:8080/"),
            "got {url}"
        );
    }
}
