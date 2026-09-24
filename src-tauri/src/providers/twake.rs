//! Twake Drive Storage Provider: native cozy-stack files API
//!
//! Twake Workplace (Linagora, formerly Cozy Cloud) runs cozy-stack on a
//! per-user instance such as `https://alice.twake.app`. The same code serves
//! self-hosted cozy-stack and legacy `*.mycozy.cloud` instances.
//!
//! Auth: OAuth2 authorization code + PKCE, with the client obtained at sign-in
//! through RFC 7591 dynamic client registration (`POST /auth/register`) on the
//! user's own instance. Nothing is registered with the vendor and no secret is
//! compiled into the binary. The result of a sign-in ([`TwakeCredentials`]) is
//! serialized into the profile password, so export, duplicate, delete and the
//! MCP / CLI profile paths carry it like any other stored secret.
//!
//! API: `docs/files.md` and `docs/auth.md` of <https://github.com/linagora/cozy-stack>.
//! Files are addressed by id; paths resolve through `GET /files/metadata?Path=`.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use base64::Engine;
use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::time::Duration;

use super::{
    response_bytes_with_limit, sanitize_api_error, send_with_retry, HttpRetryConfig,
    ProviderConfig, ProviderError, ProviderType, RemoteEntry, StorageInfo, StorageProvider,
    AEROFTP_USER_AGENT, MAX_DOWNLOAD_TO_BYTES,
};

/// Permission asked at sign-in: full access to the files doctype. Disk usage is
/// readable with it too (cozy-stack allows it to any client that can write a
/// directory), so no settings scope is needed.
pub const TWAKE_SCOPE: &str = "io.cozy.files";

/// Fixed loopback port for the OAuth redirect. cozy-stack compares the
/// `redirect_uri` byte for byte with the registered one, so the port is part of
/// the client registration and must not change between sign-ins.
pub const TWAKE_CALLBACK_PORT: u16 = 19856;

/// `software_id` sent at registration: identical for every AeroFTP install,
/// while the `client_id` differs per instance.
const TWAKE_SOFTWARE_ID: &str = "github.com/axpdev-lab/aeroftp";

const ROOT_DIR_ID: &str = "io.cozy.files.root-dir";
const TRASH_DIR_ID: &str = "io.cozy.files.trash-dir";

/// Entries asked per page of a directory listing (the server default is 30).
const LIST_PAGE_LIMIT: u32 = 100;
/// Stop following `links.next` rather than loop for ever on a broken server.
const LIST_MAX_PAGES: u32 = 10_000;
/// Up to this size an upload tries the create first and resolves a conflict
/// afterwards; above it the target is looked up first (see `upload`).
const EAGER_CREATE_MAX_BYTES: u64 = 1024 * 1024;
/// Parallel transfer workers (clones sharing the HTTP client and token).
const TWAKE_TRANSFER_MAX_SESSIONS: u16 = 4;

/// Hosted Twake / Cozy domains whose apps live on flat subdomains
/// (`<slug>-<app>.<domain>`) next to the instance (`<slug>.<domain>`).
const FLAT_SUBDOMAIN_HOSTS: &[&str] = &["twake.app", "mycozy.cloud", "cozy.works"];

#[cfg(debug_assertions)]
fn twake_log(msg: &str) {
    eprintln!("[twake] {}", msg);
}

#[cfg(not(debug_assertions))]
fn twake_log(_msg: &str) {}

// ─── Instance URL ───

/// Normalize what a user pastes into the instance field to the instance origin.
///
/// Accepts a bare host (`alice.twake.app`), a full URL, or the URL of any app
/// of the instance copied from the browser (`https://alice-drive.twake.app/#/folder`),
/// and returns `https://alice.twake.app`. Plain `http` is refused except for a
/// loopback host (a local cozy-stack for development).
pub fn normalize_instance_url(input: &str) -> Result<String, ProviderError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ProviderError::InvalidConfig(
            "Twake instance address is required".into(),
        ));
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{}", trimmed)
    };
    let url = url::Url::parse(&with_scheme).map_err(|e| {
        ProviderError::InvalidConfig(format!("Invalid Twake instance address '{trimmed}': {e}"))
    })?;
    let host = url
        .host_str()
        .ok_or_else(|| {
            ProviderError::InvalidConfig(format!("Twake instance address '{trimmed}' has no host"))
        })?
        .to_ascii_lowercase();
    let loopback = host == "localhost" || host == "127.0.0.1" || host == "[::1]";
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        other => {
            return Err(ProviderError::InvalidConfig(format!(
                "Unsupported scheme '{other}' for a Twake instance: use https://"
            )))
        }
    }
    let host = instance_host_from_app_host(&host);
    let origin = match url.port() {
        Some(port) => format!("{}://{}:{}", url.scheme(), host, port),
        None => format!("{}://{}", url.scheme(), host),
    };
    Ok(origin)
}

/// `alice-drive.twake.app` -> `alice.twake.app` on the hosted flat-subdomain
/// domains. Instance slugs are alphanumeric, so the first `-` of the first label
/// separates the slug from the app. Any other host is returned unchanged.
fn instance_host_from_app_host(host: &str) -> String {
    for domain in FLAT_SUBDOMAIN_HOSTS {
        let Some(label) = host.strip_suffix(domain).and_then(|h| h.strip_suffix('.')) else {
            continue;
        };
        if label.contains('.') {
            continue;
        }
        if let Some((slug, _app)) = label.split_once('-') {
            if !slug.is_empty() {
                return format!("{}.{}", slug, domain);
            }
        }
    }
    host.to_string()
}

// ─── Credentials ───

/// Everything a sign-in produces, stored as the profile password.
///
/// The stored form is an opaque token, `twake1:` + base64url(JSON), never a
/// bare JSON object: the CLI and MCP profile loaders read a JSON-object
/// password as the legacy `{username, password}` credential shape and would
/// hand the provider an empty password.
///
/// `client_id` / `client_secret` / `registration_access_token` come from the
/// dynamic registration on this instance; the refresh token from the code
/// exchange. cozy-stack does not rotate refresh tokens, so the blob stays valid
/// for the life of the client and never needs to be written back.
#[derive(Clone, Serialize, Deserialize)]
pub struct TwakeCredentials {
    pub instance: String,
    pub client_id: String,
    pub client_secret: String,
    pub registration_access_token: String,
    pub refresh_token: String,
}

impl std::fmt::Debug for TwakeCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TwakeCredentials")
            .field("instance", &self.instance)
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("registration_access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .finish()
    }
}

const STORED_PREFIX: &str = "twake1:";

impl TwakeCredentials {
    /// Serialize to the opaque token stored as the profile password.
    pub fn to_stored(&self) -> String {
        let json = serde_json::to_vec(self).unwrap_or_default();
        format!(
            "{}{}",
            STORED_PREFIX,
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
        )
    }

    pub fn from_stored(raw: &str) -> Result<Self, ProviderError> {
        let not_signed_in = || {
            ProviderError::AuthenticationFailed(
                "Twake Drive is not signed in for this profile: use Sign in with Twake".into(),
            )
        };
        let encoded = raw
            .trim()
            .strip_prefix(STORED_PREFIX)
            .ok_or_else(not_signed_in)?;
        let json = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| not_signed_in())?;
        let creds: Self = serde_json::from_slice(&json).map_err(|_| not_signed_in())?;
        if creds.client_id.is_empty()
            || creds.client_secret.is_empty()
            || creds.refresh_token.is_empty()
        {
            return Err(ProviderError::AuthenticationFailed(
                "Twake Drive credentials are incomplete: sign in with Twake again".into(),
            ));
        }
        Ok(creds)
    }
}

// ─── Sign-in (dynamic registration + PKCE) ───

#[derive(Debug, Deserialize)]
struct RegistrationResponse {
    client_id: String,
    client_secret: String,
    registration_access_token: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
}

/// State kept between opening the browser and receiving the callback.
pub struct TwakePendingSignIn {
    instance: String,
    redirect_uri: String,
    client_id: String,
    client_secret: SecretString,
    registration_access_token: SecretString,
    code_verifier: SecretString,
    /// CSRF `state` the callback must echo.
    pub state: String,
    /// URL to open in the system browser.
    pub auth_url: String,
}

fn auth_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(AEROFTP_USER_AGENT)
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap_or_default()
}

fn random_urlsafe(bytes: usize) -> String {
    use rand::RngCore;
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

fn pkce_challenge(verifier: &str) -> String {
    use sha2::{Digest, Sha256};
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// `application/x-www-form-urlencoded` body (reqwest is built without `form`).
fn form_urlencode(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// The loopback redirect URI registered for the client.
pub fn redirect_uri_for_port(port: u16) -> String {
    format!("http://127.0.0.1:{}/callback", port)
}

/// Register a client on the instance and build the authorization URL.
///
/// The caller binds the callback listener on `port` first (so the port is known
/// to be free), then opens [`TwakePendingSignIn::auth_url`] in the SYSTEM
/// browser: Twake refuses embedded webviews.
pub async fn begin_sign_in(
    instance_input: &str,
    port: u16,
    app_version: &str,
) -> Result<TwakePendingSignIn, ProviderError> {
    let instance = normalize_instance_url(instance_input)?;
    let redirect_uri = redirect_uri_for_port(port);
    let client = auth_http_client();
    let body = serde_json::json!({
        "redirect_uris": [redirect_uri],
        "client_name": "AeroFTP",
        "client_kind": "desktop",
        "software_id": TWAKE_SOFTWARE_ID,
        "software_version": app_version,
        "client_uri": "https://www.aeroftp.app",
        "logo_uri": "https://www.aeroftp.app/icons/aeroftp-512.png",
        "policy_uri": "https://github.com/axpdev-lab/aeroftp/blob/main/PRIVACY.md",
    });
    let resp = client
        .post(format!("{}/auth/register", instance))
        .header(ACCEPT, "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            ProviderError::ConnectionFailed(format!("Cannot reach Twake instance {instance}: {e}"))
        })?;
    let status = resp.status();
    if status.as_u16() != 201 && !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ProviderError::AuthenticationFailed(format!(
            "Twake refused the client registration on {instance} (HTTP {}): {}",
            status.as_u16(),
            sanitize_api_error(&text)
        )));
    }
    let reg: RegistrationResponse = resp.json().await.map_err(|e| {
        ProviderError::ParseError(format!("Unexpected registration response from Twake: {e}"))
    })?;

    let code_verifier = random_urlsafe(64);
    let state = random_urlsafe(24);
    let mut auth_url = url::Url::parse(&format!("{}/auth/authorize", instance))
        .map_err(|e| ProviderError::InvalidConfig(e.to_string()))?;
    auth_url
        .query_pairs_mut()
        .append_pair("client_id", &reg.client_id)
        .append_pair("response_type", "code")
        .append_pair("scope", TWAKE_SCOPE)
        .append_pair("state", &state)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("code_challenge", &pkce_challenge(&code_verifier))
        .append_pair("code_challenge_method", "S256");

    twake_log(&format!(
        "registered client {} on {}",
        reg.client_id, instance
    ));
    Ok(TwakePendingSignIn {
        instance,
        redirect_uri,
        client_id: reg.client_id,
        client_secret: SecretString::from(reg.client_secret),
        registration_access_token: SecretString::from(reg.registration_access_token),
        code_verifier: SecretString::from(code_verifier),
        state,
        auth_url: auth_url.to_string(),
    })
}

/// Exchange the authorization code for tokens and return the credentials to
/// store on the profile.
pub async fn finish_sign_in(
    pending: TwakePendingSignIn,
    code: &str,
) -> Result<TwakeCredentials, ProviderError> {
    let client = auth_http_client();
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("client_id", pending.client_id.as_str()),
        ("client_secret", pending.client_secret.expose_secret()),
        ("code_verifier", pending.code_verifier.expose_secret()),
        ("redirect_uri", pending.redirect_uri.as_str()),
    ];
    let resp = client
        .post(format!("{}/auth/access_token", pending.instance))
        .header(ACCEPT, "application/json")
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(form_urlencode(&form))
        .send()
        .await
        .map_err(|e| ProviderError::ConnectionFailed(format!("Token exchange failed: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ProviderError::AuthenticationFailed(format!(
            "Twake refused the authorization code (HTTP {}): {}",
            status.as_u16(),
            sanitize_api_error(&text)
        )));
    }
    let tokens: TokenResponse = resp
        .json()
        .await
        .map_err(|e| ProviderError::ParseError(format!("Unexpected token response: {e}")))?;
    let refresh_token = tokens
        .refresh_token
        .filter(|t| !t.is_empty())
        .ok_or_else(|| {
            ProviderError::AuthenticationFailed("Twake returned no refresh token".into())
        })?;
    Ok(TwakeCredentials {
        instance: pending.instance,
        client_id: pending.client_id,
        client_secret: pending.client_secret.expose_secret().to_string(),
        registration_access_token: pending
            .registration_access_token
            .expose_secret()
            .to_string(),
        refresh_token,
    })
}

/// Delete the OAuth client from the instance (it disappears from the user's
/// Connected devices). Used when a profile signs in again, so re-authorizing
/// does not leave orphan clients behind. Not called on profile removal: a
/// duplicated profile shares the client, and revoking it would silently sign
/// the copy out.
pub async fn revoke_client(creds: &TwakeCredentials) -> Result<(), ProviderError> {
    let resp = auth_http_client()
        .delete(format!(
            "{}/auth/register/{}",
            creds.instance,
            urlencoding::encode(&creds.client_id)
        ))
        .bearer_auth(&creds.registration_access_token)
        .send()
        .await
        .map_err(|e| ProviderError::ConnectionFailed(format!("Client revocation failed: {e}")))?;
    match resp.status().as_u16() {
        200..=299 | 404 => Ok(()),
        code => Err(ProviderError::ServerError(format!(
            "Twake refused to delete the OAuth client (HTTP {code})"
        ))),
    }
}

// ─── Configuration ───

#[derive(Clone)]
pub struct TwakeConfig {
    pub credentials: TwakeCredentials,
    pub initial_path: Option<String>,
}

impl TwakeConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let raw = config
            .password
            .as_deref()
            .filter(|p| !p.trim().is_empty())
            .ok_or_else(|| {
                ProviderError::AuthenticationFailed(
                    "Twake Drive is not signed in for this profile: use Sign in with Twake".into(),
                )
            })?;
        let mut credentials = TwakeCredentials::from_stored(raw)?;
        // The host field is the source of truth for WHICH instance the profile
        // points at; the blob must have been issued by that instance.
        if !config.host.trim().is_empty() {
            let host_instance = normalize_instance_url(&config.host)?;
            if host_instance != credentials.instance {
                return Err(ProviderError::AuthenticationFailed(format!(
                    "The stored Twake sign-in belongs to {} but the profile points at {}: sign in again",
                    credentials.instance, host_instance
                )));
            }
        }
        credentials.instance = normalize_instance_url(&credentials.instance)?;
        Ok(Self {
            credentials,
            initial_path: config.initial_path.clone(),
        })
    }
}

// ─── API structures ───

fn de_u64_lenient<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumOrStr {
        Num(u64),
        Str(String),
    }
    Ok(match Option::<NumOrStr>::deserialize(d)? {
        Some(NumOrStr::Num(n)) => Some(n),
        Some(NumOrStr::Str(s)) => s.trim().parse().ok(),
        None => None,
    })
}

#[derive(Debug, Clone, Deserialize)]
struct FileAttributes {
    #[serde(rename = "type")]
    kind: String,
    name: String,
    #[serde(default, deserialize_with = "de_u64_lenient")]
    size: Option<u64>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    md5sum: Option<String>,
    #[serde(default)]
    mime: Option<String>,
    #[serde(default)]
    trashed: Option<bool>,
    #[serde(default)]
    executable: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
struct FileDoc {
    id: String,
    attributes: FileAttributes,
}

impl FileDoc {
    fn is_dir(&self) -> bool {
        self.attributes.kind == "directory"
    }
}

#[derive(Debug, Deserialize)]
struct SingleDoc {
    data: FileDoc,
}

#[derive(Debug, Deserialize, Default)]
struct Links {
    #[serde(default)]
    next: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DirListing {
    #[serde(default)]
    included: Vec<FileDoc>,
    #[serde(default)]
    links: Links,
}

#[derive(Debug, Deserialize)]
struct DiskUsageAttrs {
    #[serde(default, deserialize_with = "de_u64_lenient")]
    used: Option<u64>,
    #[serde(default, deserialize_with = "de_u64_lenient")]
    quota: Option<u64>,
    #[serde(default, deserialize_with = "de_u64_lenient")]
    versions: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct DiskUsageData {
    attributes: DiskUsageAttrs,
}

#[derive(Debug, Deserialize)]
struct DiskUsage {
    data: DiskUsageData,
}

#[derive(Debug, Deserialize)]
struct JsonApiErrors {
    #[serde(default)]
    errors: Vec<JsonApiError>,
}

#[derive(Debug, Deserialize)]
struct JsonApiError {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    detail: Option<String>,
}

/// cozy-stack stores `md5sum` as base64 of the raw digest; AeroFTP compares
/// lowercase hex.
fn md5_base64_to_hex(b64: &str) -> Option<String> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .ok()?;
    (raw.len() == 16).then(|| hex::encode(raw))
}

// ─── Provider ───

pub struct TwakeProvider {
    config: TwakeConfig,
    client: reqwest::Client,
    access_token: SecretString,
    connected: bool,
    current_path: String,
    server_version: Option<String>,
}

impl TwakeProvider {
    pub fn new(config: TwakeConfig) -> Self {
        // Bearer is stripped by reqwest on a cross-origin redirect; keep every
        // redirect on the instance anyway, like the other self-hosted backends.
        let client = reqwest::Client::builder()
            .user_agent(AEROFTP_USER_AGENT)
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(1800))
            .redirect(super::redirect_policy::same_origin_redirect_policy())
            .build()
            .unwrap_or_default();
        Self {
            config,
            client,
            access_token: SecretString::from(String::new()),
            connected: false,
            current_path: "/".into(),
            server_version: None,
        }
    }

    /// An independent, already connected worker for parallel transfers: it
    /// shares the HTTP client and the current access token, and refreshes on
    /// its own after a 401 (cozy-stack does not rotate refresh tokens, so
    /// concurrent refreshes from several workers never invalidate each other).
    fn clone_worker(&self) -> Self {
        Self {
            config: TwakeConfig {
                credentials: self.config.credentials.clone(),
                initial_path: None,
            },
            client: self.client.clone(),
            access_token: self.access_token.clone(),
            connected: self.connected,
            current_path: self.current_path.clone(),
            server_version: self.server_version.clone(),
        }
    }

    fn instance(&self) -> &str {
        &self.config.credentials.instance
    }

    fn url(&self, path_and_query: &str) -> String {
        format!("{}{}", self.instance(), path_and_query)
    }

    fn bearer(&self) -> Result<HeaderValue, ProviderError> {
        HeaderValue::from_str(&format!("Bearer {}", self.access_token.expose_secret()))
            .map_err(|_| ProviderError::AuthenticationFailed("Invalid Twake access token".into()))
    }

    /// Trade the stored refresh token for a fresh access token.
    async fn refresh_access_token(&mut self) -> Result<(), ProviderError> {
        let creds = &self.config.credentials;
        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", creds.refresh_token.as_str()),
            ("client_id", creds.client_id.as_str()),
            ("client_secret", creds.client_secret.as_str()),
        ];
        let resp = self
            .client
            .post(self.url("/auth/access_token"))
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(form_urlencode(&form))
            .send()
            .await
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!(
                    "Cannot reach Twake instance {}: {e}",
                    self.instance()
                ))
            })?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            // 400 invalid_grant / 401: the client was removed from Connected
            // devices or the refresh token is dead. Only a new sign-in helps.
            return Err(ProviderError::AuthenticationFailed(format!(
                "Twake refused the stored sign-in (HTTP {}): {}. Sign in with Twake again.",
                status.as_u16(),
                sanitize_api_error(&text)
            )));
        }
        let tokens: TokenResponse = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(format!("Unexpected token response: {e}")))?;
        self.access_token = SecretString::from(tokens.access_token);
        Ok(())
    }

    /// Send a replayable request, refreshing the access token once on 401.
    async fn send<F>(&mut self, build: F) -> Result<reqwest::Response, ProviderError>
    where
        F: Fn(&reqwest::Client) -> reqwest::RequestBuilder,
    {
        for attempt in 0..2 {
            let request = build(&self.client)
                .header(AUTHORIZATION, self.bearer()?)
                .header(ACCEPT, "application/vnd.api+json")
                .build()
                .map_err(|e| ProviderError::NetworkError(format!("Build request failed: {e}")))?;
            let (method, path) = (request.method().clone(), request.url().path().to_string());
            let resp = send_with_retry(&self.client, request, &HttpRetryConfig::default())
                .await
                .map_err(|e| ProviderError::NetworkError(format!("Request failed: {e}")))?;
            twake_log(&format!(
                "{} {} -> {}",
                method,
                path,
                resp.status().as_u16()
            ));
            if resp.status().as_u16() == 401 && attempt == 0 {
                twake_log("401, refreshing access token");
                self.refresh_access_token().await?;
                continue;
            }
            return Ok(resp);
        }
        unreachable!("the loop returns on its second attempt")
    }

    /// Map a non-success response to a ProviderError.
    async fn error_from(resp: reqwest::Response, what: &str) -> ProviderError {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        Self::classify_error(status, &body, what)
    }

    fn classify_error(status: u16, body: &str, what: &str) -> ProviderError {
        let detail = serde_json::from_str::<JsonApiErrors>(body)
            .ok()
            .and_then(|e| e.errors.into_iter().next())
            .and_then(|e| e.detail.or(e.title))
            .unwrap_or_else(|| sanitize_api_error(body));
        match status {
            401 => ProviderError::AuthenticationFailed(format!(
                "Twake rejected the access token: {detail}. Sign in with Twake again."
            )),
            403 => ProviderError::PermissionDenied(format!("{what}: {detail}")),
            404 => ProviderError::NotFound(what.to_string()),
            409 => ProviderError::AlreadyExists(what.to_string()),
            413 => ProviderError::TransferFailed(format!(
                "{what}: not enough space left on the Twake instance ({detail})"
            )),
            429 => ProviderError::ServerError(format!("{what}: rate limited by Twake ({detail})")),
            _ => ProviderError::ServerError(format!("{what}: HTTP {status}: {detail}")),
        }
    }

    // ─── Paths ───

    fn normalize_path(path: &str) -> String {
        let replaced = path.trim().replace('\\', "/");
        let mut parts: Vec<&str> = Vec::new();
        for seg in replaced.split('/') {
            match seg {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                s => parts.push(s),
            }
        }
        if parts.is_empty() {
            "/".into()
        } else {
            format!("/{}", parts.join("/"))
        }
    }

    fn resolve_path(&self, path: &str) -> String {
        let trimmed = path.trim();
        if trimmed.is_empty() || trimmed == "." {
            return self.current_path.clone();
        }
        if trimmed.starts_with('/') {
            Self::normalize_path(trimmed)
        } else {
            Self::normalize_path(&format!("{}/{}", self.current_path, trimmed))
        }
    }

    /// Split "/a/b/file.txt" -> ("/a/b", "file.txt").
    fn split_path(path: &str) -> (String, String) {
        match path.rfind('/') {
            Some(0) => ("/".into(), path[1..].to_string()),
            Some(pos) => (path[..pos].to_string(), path[pos + 1..].to_string()),
            None => ("/".into(), path.to_string()),
        }
    }

    fn join_path(parent: &str, name: &str) -> String {
        if parent == "/" {
            format!("/{}", name)
        } else {
            format!("{}/{}", parent.trim_end_matches('/'), name)
        }
    }

    /// Resolve an absolute path to its file document.
    async fn lookup(&mut self, abs_path: &str) -> Result<FileDoc, ProviderError> {
        let url = self.url(&format!(
            "/files/metadata?Path={}",
            urlencoding::encode(abs_path)
        ));
        let resp = self.send(|c| c.get(&url)).await?;
        if !resp.status().is_success() {
            return Err(Self::error_from(resp, abs_path).await);
        }
        let doc: SingleDoc = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(format!("Unexpected metadata response: {e}")))?;
        if doc.data.attributes.trashed.unwrap_or(false) {
            return Err(ProviderError::NotFound(abs_path.to_string()));
        }
        Ok(doc.data)
    }

    async fn dir_id(&mut self, abs_path: &str) -> Result<String, ProviderError> {
        if abs_path == "/" {
            return Ok(ROOT_DIR_ID.to_string());
        }
        let doc = self.lookup(abs_path).await?;
        if !doc.is_dir() {
            return Err(ProviderError::InvalidPath(format!(
                "'{}' is not a directory",
                abs_path
            )));
        }
        Ok(doc.id)
    }

    fn doc_to_entry(doc: &FileDoc, path: String) -> RemoteEntry {
        let attrs = &doc.attributes;
        let is_dir = doc.is_dir();
        let mut metadata = HashMap::new();
        metadata.insert("id".to_string(), doc.id.clone());
        if let Some(md5) = attrs.md5sum.as_deref().and_then(md5_base64_to_hex) {
            metadata.insert("md5".to_string(), md5);
        }
        RemoteEntry {
            name: attrs.name.clone(),
            path,
            is_dir,
            size: if is_dir { 0 } else { attrs.size.unwrap_or(0) },
            modified: attrs.updated_at.clone(),
            permissions: attrs
                .executable
                .filter(|x| *x && !is_dir)
                .map(|_| "rwxr-xr-x".to_string()),
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            mime_type: if is_dir { None } else { attrs.mime.clone() },
            metadata,
        }
    }

    /// Every entry of a directory, following the cursor pagination.
    async fn list_dir(&mut self, dir_id: &str) -> Result<Vec<FileDoc>, ProviderError> {
        let mut docs = Vec::new();
        let mut next = Some(format!(
            "/files/{}?page%5Blimit%5D={}",
            urlencoding::encode(dir_id),
            LIST_PAGE_LIMIT
        ));
        let mut pages = 0;
        while let Some(path_and_query) = next.take() {
            pages += 1;
            if pages > LIST_MAX_PAGES {
                return Err(ProviderError::ServerError(
                    "Twake listing did not terminate".into(),
                ));
            }
            let url = self.url(&path_and_query);
            let resp = self.send(|c| c.get(&url)).await?;
            if !resp.status().is_success() {
                return Err(Self::error_from(resp, dir_id).await);
            }
            let page: DirListing = resp.json().await.map_err(|e| {
                ProviderError::ParseError(format!("Unexpected listing response: {e}"))
            })?;
            docs.extend(
                page.included
                    .into_iter()
                    .filter(|d| d.id != TRASH_DIR_ID && !d.attributes.trashed.unwrap_or(false)),
            );
            next = page
                .links
                .next
                .filter(|n| n.starts_with('/'))
                .map(|n| ensure_page_limit(&n));
        }
        Ok(docs)
    }

    /// Stream a local file as a request body, reporting progress and hashing
    /// what was sent so the result can be checked against the server's md5sum.
    async fn upload_body(
        local_path: &str,
        total: u64,
        progress: Option<tokio::sync::mpsc::UnboundedSender<(u64, u64)>>,
        hasher: std::sync::Arc<std::sync::Mutex<md5::Md5>>,
    ) -> Result<reqwest::Body, ProviderError> {
        use futures_util::StreamExt;
        use md5::Digest;
        let file = tokio::fs::File::open(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let mut sent: u64 = 0;
        let stream = crate::transfer_dag::throttle::throttle_stream(
            tokio_util::io::ReaderStream::with_capacity(file, 256 * 1024),
            crate::transfer_dag::governor::TransferDirection::Upload,
        )
        .map(move |chunk| {
            if let Ok(bytes) = &chunk {
                sent += bytes.len() as u64;
                if let Ok(mut h) = hasher.lock() {
                    h.update(bytes);
                }
                if let Some(tx) = &progress {
                    let _ = tx.send((sent, total));
                }
            }
            chunk
        });
        Ok(reqwest::Body::wrap_stream(stream))
    }
}

/// `links.next` carries only the cursor; keep the page size on every page.
fn ensure_page_limit(next: &str) -> String {
    if next.contains("page%5Blimit%5D") || next.contains("page[limit]") {
        next.to_string()
    } else {
        let sep = if next.contains('?') { '&' } else { '?' };
        format!("{next}{sep}page%5Blimit%5D={LIST_PAGE_LIMIT}")
    }
}

/// The later of two RFC 3339 timestamps (an unparsable one loses; both
/// unparsable gives `None`).
fn latest_timestamp<'a>(a: Option<&'a str>, b: Option<&'a str>) -> Option<&'a str> {
    let parse = |s: &str| chrono::DateTime::parse_from_rfc3339(s).ok();
    match (
        a.and_then(|x| parse(x).map(|t| (x, t))),
        b.and_then(|x| parse(x).map(|t| (x, t))),
    ) {
        (Some((xa, ta)), Some((xb, tb))) => Some(if tb > ta { xb } else { xa }),
        (Some((xa, _)), None) => Some(xa),
        (None, Some((xb, _))) => Some(xb),
        (None, None) => None,
    }
}

fn local_mtime_rfc3339(local_path: &str) -> Option<String> {
    let modified = std::fs::metadata(local_path).ok()?.modified().ok()?;
    let dt: chrono::DateTime<chrono::Utc> = modified.into();
    Some(dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

#[async_trait]
impl StorageProvider for TwakeProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Twake
    }

    fn display_name(&self) -> String {
        let host = self
            .instance()
            .split_once("://")
            .map(|(_, h)| h)
            .unwrap_or(self.instance());
        format!("Twake Drive ({host})")
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        twake_log(&format!("connecting to {}", self.instance()));
        // One round trip: a successful refresh proves the client, its secret
        // and the refresh token are all valid. The transfer engine opens a
        // provider per file, so connect stays as cheap as possible.
        self.refresh_access_token().await?;
        self.connected = true;
        self.current_path = "/".into();
        if let Some(initial) = self.config.initial_path.clone() {
            if !initial.trim().is_empty() && initial.trim() != "/" {
                self.cd(&initial).await?;
            }
        }
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        self.access_token = SecretString::from(String::new());
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let abs = self.resolve_path(path);
        let dir_id = self.dir_id(&abs).await?;
        let docs = self.list_dir(&dir_id).await?;
        Ok(docs
            .iter()
            .map(|d| Self::doc_to_entry(d, Self::join_path(&abs, &d.attributes.name)))
            .collect())
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let abs = self.resolve_path(path);
        self.dir_id(&abs).await?;
        self.current_path = abs;
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        if self.current_path != "/" {
            self.current_path = Self::split_path(&self.current_path).0;
        }
        Ok(())
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let abs = self.resolve_path(remote_path);
        let doc = self.lookup(&abs).await?;
        if doc.is_dir() {
            return Err(ProviderError::InvalidPath(format!(
                "'{abs}' is a directory"
            )));
        }
        let url = self.url(&format!("/files/download/{}", urlencoding::encode(&doc.id)));
        let resp = self.send(|c| c.get(&url)).await?;
        if !resp.status().is_success() {
            return Err(Self::error_from(resp, &abs).await);
        }

        use futures_util::StreamExt;
        let total = resp.content_length().or(doc.attributes.size).unwrap_or(0);
        let mut stream = Box::pin(crate::transfer_dag::throttle::throttle_stream(
            resp.bytes_stream(),
            crate::transfer_dag::governor::TransferDirection::Download,
        ));
        let mut atomic = super::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Create file failed: {e}")))?;
        let mut downloaded: u64 = 0;
        if let Some(ref cb) = on_progress {
            cb(0, total);
        }
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|e| ProviderError::TransferFailed(format!("Stream error: {e}")))?;
            atomic
                .write_all(&chunk)
                .await
                .map_err(|e| ProviderError::TransferFailed(format!("Write error: {e}")))?;
            downloaded += chunk.len() as u64;
            if let Some(ref cb) = on_progress {
                cb(downloaded, total);
            }
        }
        atomic.commit().await.map_err(|e| {
            ProviderError::TransferFailed(format!("Failed to finalize download: {e}"))
        })?;
        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let abs = self.resolve_path(remote_path);
        let doc = self.lookup(&abs).await?;
        if doc.is_dir() {
            return Err(ProviderError::InvalidPath(format!(
                "'{abs}' is a directory"
            )));
        }
        let url = self.url(&format!("/files/download/{}", urlencoding::encode(&doc.id)));
        let resp = self.send(|c| c.get(&url)).await?;
        if !resp.status().is_success() {
            return Err(Self::error_from(resp, &abs).await);
        }
        response_bytes_with_limit(resp, MAX_DOWNLOAD_TO_BYTES).await
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        use md5::Digest;
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let abs = self.resolve_path(remote_path);
        let (parent, name) = Self::split_path(&abs);
        if name.is_empty() {
            return Err(ProviderError::InvalidPath(abs));
        }
        let total = tokio::fs::metadata(local_path)
            .await
            .map_err(ProviderError::IoError)?
            .len();
        let mtime = local_mtime_rfc3339(local_path);

        // Overwrite in place when the file exists (keeps its id, shares and
        // versions); create it in the parent otherwise. The transfer engine
        // opens a fresh provider per file, so round trips dominate small
        // transfers: a small file tries the create first and only looks the
        // target up on 409. A large one looks it up first, so a conflict never
        // streams the whole body twice.
        let mut existing = if total > EAGER_CREATE_MAX_BYTES {
            match self.lookup(&abs).await {
                Ok(doc) => Some(doc),
                Err(ProviderError::NotFound(_)) => None,
                Err(e) => return Err(e),
            }
        } else {
            None
        };

        let progress_tx = on_progress.map(|cb| {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(u64, u64)>();
            tokio::spawn(async move {
                while let Some((sent, t)) = rx.recv().await {
                    cb(sent, t);
                }
            });
            tx
        });
        if let Some(tx) = &progress_tx {
            let _ = tx.send((0, total));
        }

        let mut refreshed = false;
        let mut conflict_seen = false;
        let resp = loop {
            if let Some(doc) = &existing {
                if doc.is_dir() {
                    return Err(ProviderError::AlreadyExists(format!(
                        "'{abs}' is a directory"
                    )));
                }
            }
            // cozy-stack refuses any later metadata change (rename, move) of a
            // doc whose updated_at is before its created_at ("Invalid time
            // given"), and it does not clamp that at upload time. A new file
            // therefore gets the local mtime as BOTH dates; an overwrite never
            // goes below the created_at the file already has.
            let url = match &existing {
                Some(doc) => {
                    let mut url = self.url(&format!("/files/{}", urlencoding::encode(&doc.id)));
                    if let Some(ts) = &mtime {
                        let floor = doc.attributes.created_at.as_deref();
                        let ts = latest_timestamp(Some(ts.as_str()), floor).unwrap_or(ts.as_str());
                        url.push_str(&format!("?UpdatedAt={}", urlencoding::encode(ts)));
                    }
                    url
                }
                None => {
                    let parent_id = self.dir_id(&parent).await?;
                    let mut url = self.url(&format!(
                        "/files/{}?Type=file&Name={}",
                        urlencoding::encode(&parent_id),
                        urlencoding::encode(&name)
                    ));
                    if let Some(ts) = &mtime {
                        url.push_str(&format!(
                            "&CreatedAt={0}&UpdatedAt={0}",
                            urlencoding::encode(ts)
                        ));
                    }
                    url
                }
            };

            // The body is a stream, so every retry reopens the file.
            let hasher = std::sync::Arc::new(std::sync::Mutex::new(md5::Md5::new()));
            let body =
                Self::upload_body(local_path, total, progress_tx.clone(), hasher.clone()).await?;
            let builder = if existing.is_some() {
                self.client.put(&url)
            } else {
                self.client.post(&url)
            };
            let resp = builder
                .header(AUTHORIZATION, self.bearer()?)
                .header(ACCEPT, "application/vnd.api+json")
                .header(CONTENT_TYPE, "application/octet-stream")
                .header(CONTENT_LENGTH, total)
                .body(body)
                .send()
                .await
                .map_err(|e| ProviderError::TransferFailed(format!("Upload failed: {e}")))?;
            twake_log(&format!(
                "{} upload {} -> {}",
                if existing.is_some() { "PUT" } else { "POST" },
                abs,
                resp.status().as_u16()
            ));
            match resp.status().as_u16() {
                401 if !refreshed => {
                    refreshed = true;
                    self.refresh_access_token().await?;
                    continue;
                }
                409 if existing.is_none() && !conflict_seen => {
                    conflict_seen = true;
                    existing = Some(self.lookup(&abs).await?);
                    continue;
                }
                _ => {}
            }
            if !resp.status().is_success() {
                return Err(Self::error_from(resp, &abs).await);
            }
            let sent_md5 = hasher
                .lock()
                .map(|h| hex::encode(h.clone().finalize()))
                .unwrap_or_default();
            break (resp, sent_md5);
        };
        let (resp, sent_md5) = resp;
        let doc: SingleDoc = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(format!("Unexpected upload response: {e}")))?;
        let server_md5 = doc
            .data
            .attributes
            .md5sum
            .as_deref()
            .and_then(md5_base64_to_hex);
        if let Some(server_md5) = server_md5 {
            if !sent_md5.is_empty() && server_md5 != sent_md5 {
                return Err(ProviderError::TransferFailed(format!(
                    "Checksum mismatch after upload of '{abs}': sent md5 {sent_md5}, Twake stored {server_md5}"
                )));
            }
        }
        if let Some(tx) = &progress_tx {
            let _ = tx.send((total, total));
        }
        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let abs = self.resolve_path(path);
        if abs == "/" {
            return Err(ProviderError::PermissionDenied(
                "The Twake root cannot be deleted".into(),
            ));
        }
        let doc = self.lookup(&abs).await?;
        let url = self.url(&format!("/files/{}", urlencoding::encode(&doc.id)));
        let resp = self.send(|c| c.delete(&url)).await?;
        if !resp.status().is_success() {
            return Err(Self::error_from(resp, &abs).await);
        }
        Ok(())
    }

    async fn delete_permanent(&mut self, path: &str) -> Result<bool, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let abs = self.resolve_path(path);
        if abs == "/" {
            return Err(ProviderError::PermissionDenied(
                "The Twake root cannot be deleted".into(),
            ));
        }
        let doc = self.lookup(&abs).await?;
        let id = urlencoding::encode(&doc.id).into_owned();
        let url = self.url(&format!("/files/{id}"));
        let resp = self.send(|c| c.delete(&url)).await?;
        if !resp.status().is_success() {
            return Err(Self::error_from(resp, &abs).await);
        }
        let url = self.url(&format!("/files/trash/{id}"));
        let resp = self.send(|c| c.delete(&url)).await?;
        if !resp.status().is_success() {
            return Err(Self::error_from(resp, &abs).await);
        }
        Ok(true)
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let from_abs = self.resolve_path(from);
        let to_abs = self.resolve_path(to);
        if from_abs == "/" {
            return Err(ProviderError::PermissionDenied(
                "The Twake root cannot be renamed".into(),
            ));
        }
        let doc = self.lookup(&from_abs).await?;
        let (to_parent, to_name) = Self::split_path(&to_abs);
        let (from_parent, _) = Self::split_path(&from_abs);
        let mut attributes = serde_json::json!({ "name": to_name });
        // A doc stored with updated_at < created_at (uploaded by an older build or
        // another client) cannot be patched at all; sending the later of the two
        // repairs it in the same request.
        if let Some(ts) = latest_timestamp(
            doc.attributes.updated_at.as_deref(),
            doc.attributes.created_at.as_deref(),
        ) {
            attributes["updated_at"] = serde_json::Value::String(ts.to_string());
        }
        if to_parent != from_parent {
            attributes["dir_id"] = serde_json::Value::String(self.dir_id(&to_parent).await?);
        }
        let body = serde_json::json!({
            "data": { "type": "io.cozy.files", "id": doc.id, "attributes": attributes }
        })
        .to_string();
        let url = self.url(&format!("/files/{}", urlencoding::encode(&doc.id)));
        let resp = self
            .send(|c| {
                c.patch(&url)
                    .header(CONTENT_TYPE, "application/vnd.api+json")
                    .body(body.clone())
            })
            .await?;
        if !resp.status().is_success() {
            return Err(Self::error_from(resp, &to_abs).await);
        }
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let abs = self.resolve_path(path);
        if abs == "/" {
            return Err(ProviderError::AlreadyExists("/".into()));
        }
        let (parent, name) = Self::split_path(&abs);
        let parent_id = self.dir_id(&parent).await?;
        let url = self.url(&format!(
            "/files/{}?Type=directory&Name={}",
            urlencoding::encode(&parent_id),
            urlencoding::encode(&name)
        ));
        let resp = self.send(|c| c.post(&url)).await?;
        if !resp.status().is_success() {
            return Err(Self::error_from(resp, &abs).await);
        }
        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let abs = self.resolve_path(path);
        let dir_id = self.dir_id(&abs).await?;
        if !self.list_dir(&dir_id).await?.is_empty() {
            return Err(ProviderError::DirectoryNotEmpty(abs));
        }
        self.delete(&abs).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        let abs = self.resolve_path(path);
        let doc = self.lookup(&abs).await?;
        if !doc.is_dir() {
            return Err(ProviderError::InvalidPath(format!(
                "'{abs}' is not a directory"
            )));
        }
        // cozy-stack trashes a directory with everything under it.
        self.delete(&abs).await
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let abs = self.resolve_path(path);
        if abs == "/" {
            return Ok(RemoteEntry::directory("/".into(), "/".into()));
        }
        let doc = self.lookup(&abs).await?;
        Ok(Self::doc_to_entry(&doc, abs))
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        Ok(self.stat(path).await?.size)
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        match self.stat(path).await {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let url = self.url("/files/metadata?Path=%2F");
        let resp = self.send(|c| c.get(&url)).await?;
        if !resp.status().is_success() {
            return Err(Self::error_from(resp, "/").await);
        }
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        if self.server_version.is_none() {
            // Public build info, best effort.
            if let Ok(resp) = self.client.get(self.url("/version")).send().await {
                if let Ok(v) = resp.json::<serde_json::Value>().await {
                    self.server_version = v["version"].as_str().map(|s| s.to_string());
                }
            }
        }
        Ok(match &self.server_version {
            Some(v) => format!("Twake Drive at {} (cozy-stack {})", self.instance(), v),
            None => format!("Twake Drive at {}", self.instance()),
        })
    }

    fn supports_server_copy(&self) -> bool {
        true
    }

    fn transfer_executor_kind(&self) -> super::ProviderTransferExecutorKind {
        super::ProviderTransferExecutorKind::HttpClonePool
    }

    fn transfer_executor_max_sessions(&self) -> u16 {
        TWAKE_TRANSFER_MAX_SESSIONS
    }

    fn clone_for_transfer(&self) -> Result<Box<dyn StorageProvider>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        Ok(Box::new(self.clone_worker()))
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let from_abs = self.resolve_path(from);
        let to_abs = self.resolve_path(to);
        let doc = self.lookup(&from_abs).await?;
        if doc.is_dir() {
            return Err(ProviderError::NotSupported(
                "Twake copies files server-side, not directories".into(),
            ));
        }
        let (to_parent, to_name) = Self::split_path(&to_abs);
        let parent_id = self.dir_id(&to_parent).await?;
        let url = self.url(&format!(
            "/files/{}/copy?Name={}&DirID={}",
            urlencoding::encode(&doc.id),
            urlencoding::encode(&to_name),
            urlencoding::encode(&parent_id)
        ));
        let resp = self.send(|c| c.post(&url)).await?;
        if !resp.status().is_success() {
            return Err(Self::error_from(resp, &to_abs).await);
        }
        Ok(())
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let url = self.url("/settings/disk-usage");
        let resp = self.send(|c| c.get(&url)).await?;
        if !resp.status().is_success() {
            return Err(Self::error_from(resp, "disk usage").await);
        }
        let usage: DiskUsage = resp.json().await.map_err(|e| {
            ProviderError::ParseError(format!("Unexpected disk-usage response: {e}"))
        })?;
        let used = usage.data.attributes.used.unwrap_or(0);
        // No quota attribute (or 0) means an unlimited self-hosted instance.
        let total = usage.data.attributes.quota.unwrap_or(0);
        Ok(StorageInfo {
            used,
            total,
            free: total.saturating_sub(used),
            versioning_bytes: usage.data.attributes.versions.filter(|v| *v > 0),
        })
    }

    fn supports_checksum(&self) -> bool {
        true
    }

    async fn checksum(&mut self, path: &str) -> Result<HashMap<String, String>, ProviderError> {
        let entry = self.stat(path).await?;
        let mut out = HashMap::new();
        if let Some(md5) = entry.metadata.get("md5") {
            out.insert("md5".to_string(), md5.clone());
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_url_accepts_bare_host_full_url_and_app_url() {
        assert_eq!(
            normalize_instance_url("axp.twake.app").unwrap(),
            "https://axp.twake.app"
        );
        assert_eq!(
            normalize_instance_url(" https://axp.twake.app/ ").unwrap(),
            "https://axp.twake.app"
        );
        assert_eq!(
            normalize_instance_url("https://axp-drive.twake.app/#/folder/io.cozy.files.root-dir")
                .unwrap(),
            "https://axp.twake.app"
        );
        assert_eq!(
            normalize_instance_url("AXP-Settings.Twake.App").unwrap(),
            "https://axp.twake.app"
        );
        assert_eq!(
            normalize_instance_url("https://bob-photos.mycozy.cloud").unwrap(),
            "https://bob.mycozy.cloud"
        );
    }

    #[test]
    fn instance_url_leaves_self_hosted_hosts_alone() {
        assert_eq!(
            normalize_instance_url("https://cozy-prod.example.org").unwrap(),
            "https://cozy-prod.example.org"
        );
        assert_eq!(
            normalize_instance_url("https://files.example.org:8443/path").unwrap(),
            "https://files.example.org:8443"
        );
        // A deeper subdomain of a hosted domain is not an app host.
        assert_eq!(
            normalize_instance_url("https://a-b.c.twake.app").unwrap(),
            "https://a-b.c.twake.app"
        );
    }

    #[test]
    fn instance_url_refuses_plain_http_except_loopback() {
        assert!(normalize_instance_url("http://axp.twake.app").is_err());
        assert!(normalize_instance_url("ftp://axp.twake.app").is_err());
        assert!(normalize_instance_url("").is_err());
        assert_eq!(
            normalize_instance_url("http://localhost:8080").unwrap(),
            "http://localhost:8080"
        );
    }

    #[test]
    fn credentials_round_trip_and_debug_redacts_secrets() {
        let creds = TwakeCredentials {
            instance: "https://axp.twake.app".into(),
            client_id: "cid".into(),
            client_secret: "super-secret".into(),
            registration_access_token: "reg-token".into(),
            refresh_token: "refresh-token".into(),
        };
        let back = TwakeCredentials::from_stored(&creds.to_stored()).unwrap();
        assert_eq!(back.client_id, "cid");
        assert_eq!(back.refresh_token, "refresh-token");
        let dbg = format!("{:?}", creds);
        assert!(!dbg.contains("super-secret"));
        assert!(!dbg.contains("reg-token"));
        assert!(!dbg.contains("refresh-token"));
    }

    #[test]
    fn stored_credentials_are_never_a_json_object() {
        // The CLI / MCP profile loaders unwrap a JSON-object password as the
        // legacy {username, password} shape, which turned the sign-in blob into
        // an empty password on those paths (found live 2026-09-24).
        let creds = TwakeCredentials {
            instance: "https://axp.twake.app".into(),
            client_id: "cid".into(),
            client_secret: "s".into(),
            registration_access_token: "r".into(),
            refresh_token: "t".into(),
        };
        let stored = creds.to_stored();
        assert!(stored.starts_with("twake1:"));
        let as_json = serde_json::from_str::<serde_json::Value>(&stored);
        assert!(!matches!(as_json, Ok(serde_json::Value::Object(_))));
    }

    #[test]
    fn credentials_reject_a_plain_password_or_an_incomplete_blob() {
        assert!(matches!(
            TwakeCredentials::from_stored("hunter2"),
            Err(ProviderError::AuthenticationFailed(_))
        ));
        let incomplete = r#"{"instance":"https://a.twake.app","client_id":"x","client_secret":"","registration_access_token":"r","refresh_token":"t"}"#;
        // A bare JSON object is not the stored form (see TwakeCredentials).
        assert!(TwakeCredentials::from_stored(incomplete).is_err());
        let encoded = format!(
            "twake1:{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(incomplete)
        );
        assert!(TwakeCredentials::from_stored(&encoded).is_err());
    }

    fn config_with(host: &str, password: Option<String>) -> ProviderConfig {
        ProviderConfig {
            name: "t".into(),
            provider_type: ProviderType::Twake,
            host: host.into(),
            port: None,
            username: None,
            password,
            initial_path: None,
            extra: HashMap::new(),
        }
    }

    #[test]
    fn config_refuses_credentials_issued_by_another_instance() {
        let creds = TwakeCredentials {
            instance: "https://axp.twake.app".into(),
            client_id: "cid".into(),
            client_secret: "s".into(),
            registration_access_token: "r".into(),
            refresh_token: "t".into(),
        };
        let ok = TwakeConfig::from_provider_config(&config_with(
            "https://axp-drive.twake.app",
            Some(creds.to_stored()),
        ));
        assert!(ok.is_ok());
        let other = TwakeConfig::from_provider_config(&config_with(
            "bob.twake.app",
            Some(creds.to_stored()),
        ));
        assert!(matches!(other, Err(ProviderError::AuthenticationFailed(_))));
        let missing = TwakeConfig::from_provider_config(&config_with("axp.twake.app", None));
        assert!(matches!(
            missing,
            Err(ProviderError::AuthenticationFailed(_))
        ));
    }

    #[test]
    fn pkce_challenge_is_unpadded_base64url_sha256() {
        // Expected value computed independently with Python's hashlib + base64
        // (urlsafe_b64encode(sha256(v)).rstrip("=")), the same S256 the Twake
        // spike used successfully against a live instance.
        let challenge = pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r7wW1gFWFOEjXk");
        assert_eq!(challenge, "bwWFMyPfdG9qreDhH2lmftFx_dFeLDalzcT1gb_j68g");
        assert!(!challenge.contains(['=', '+', '/']));
        let verifier = random_urlsafe(64);
        assert!(
            verifier.len() >= 43 && verifier.len() <= 128,
            "RFC 7636 length"
        );
    }

    #[test]
    fn md5_base64_converts_to_lowercase_hex() {
        // md5("Hello world!") = 86fb269d190d2c85f6e0468ceca42a20
        assert_eq!(
            md5_base64_to_hex("hvsmnRkNLIX24EaM7KQqIA==").as_deref(),
            Some("86fb269d190d2c85f6e0468ceca42a20")
        );
        assert_eq!(md5_base64_to_hex("not base64 !!"), None);
        assert_eq!(md5_base64_to_hex("AAAA"), None);
    }

    #[test]
    fn paths_normalize_split_and_join() {
        assert_eq!(TwakeProvider::normalize_path(""), "/");
        assert_eq!(TwakeProvider::normalize_path("a//b/./c/"), "/a/b/c");
        assert_eq!(TwakeProvider::normalize_path("/a/b/../c"), "/a/c");
        assert_eq!(TwakeProvider::normalize_path("/../.."), "/");
        assert_eq!(
            TwakeProvider::split_path("/a/b/f.txt"),
            ("/a/b".to_string(), "f.txt".to_string())
        );
        assert_eq!(
            TwakeProvider::split_path("/f.txt"),
            ("/".to_string(), "f.txt".to_string())
        );
        assert_eq!(TwakeProvider::join_path("/", "x"), "/x");
        assert_eq!(TwakeProvider::join_path("/a", "x"), "/a/x");
    }

    #[test]
    fn listing_json_parses_sizes_as_number_or_string_and_skips_trash() {
        let body = r#"{
          "links": {"next": "/files/root?page[cursor]=abc"},
          "data": {"type":"io.cozy.files","id":"io.cozy.files.root-dir","attributes":{"type":"directory","name":""}},
          "included": [
            {"type":"io.cozy.files","id":"f1","attributes":{"type":"file","name":"a.txt","size":"12","md5sum":"hvsmnRkNLIX24EaM7KQqIA==","updated_at":"2026-09-24T17:03:14Z","mime":"text/plain"}},
            {"type":"io.cozy.files","id":"f2","attributes":{"type":"file","name":"b.bin","size":7,"trashed":false}},
            {"type":"io.cozy.files","id":"d1","attributes":{"type":"directory","name":"Photos"}}
          ]
        }"#;
        let page: DirListing = serde_json::from_str(body).unwrap();
        assert_eq!(page.included.len(), 3);
        assert_eq!(page.included[0].attributes.size, Some(12));
        assert_eq!(page.included[1].attributes.size, Some(7));
        let e = TwakeProvider::doc_to_entry(&page.included[0], "/a.txt".into());
        assert_eq!(e.size, 12);
        assert_eq!(
            e.metadata.get("md5").map(String::as_str),
            Some("86fb269d190d2c85f6e0468ceca42a20")
        );
        let d = TwakeProvider::doc_to_entry(&page.included[2], "/Photos".into());
        assert!(d.is_dir);
        assert_eq!(d.size, 0);
        assert_eq!(
            ensure_page_limit(page.links.next.as_deref().unwrap()),
            "/files/root?page[cursor]=abc&page%5Blimit%5D=100"
        );
    }

    #[test]
    fn clone_for_transfer_requires_a_connection_and_shares_the_token() {
        let creds = TwakeCredentials {
            instance: "https://axp.twake.app".into(),
            client_id: "cid".into(),
            client_secret: "s".into(),
            registration_access_token: "r".into(),
            refresh_token: "t".into(),
        };
        let mut p = TwakeProvider::new(TwakeConfig {
            credentials: creds,
            initial_path: None,
        });
        assert!(matches!(
            p.clone_for_transfer(),
            Err(ProviderError::NotConnected)
        ));
        p.connected = true;
        p.access_token = SecretString::from("live-token".to_string());
        let worker = p.clone_worker();
        assert!(worker.connected);
        assert_eq!(worker.access_token.expose_secret(), "live-token");
        assert_eq!(
            p.transfer_executor_kind(),
            super::super::ProviderTransferExecutorKind::HttpClonePool
        );
    }

    #[test]
    fn latest_timestamp_picks_the_later_parsable_one() {
        let created = "2026-09-24T18:06:26.123Z";
        let updated = "2025-01-02T03:04:05Z";
        assert_eq!(
            latest_timestamp(Some(updated), Some(created)),
            Some(created)
        );
        assert_eq!(
            latest_timestamp(Some(created), Some(updated)),
            Some(created)
        );
        assert_eq!(
            latest_timestamp(Some("garbage"), Some(updated)),
            Some(updated)
        );
        assert_eq!(latest_timestamp(None, Some(updated)), Some(updated));
        assert_eq!(latest_timestamp(None, Some("garbage")), None);
    }

    #[test]
    fn errors_map_to_provider_variants() {
        assert!(matches!(
            TwakeProvider::classify_error(404, "", "/x"),
            ProviderError::NotFound(_)
        ));
        assert!(matches!(
            TwakeProvider::classify_error(409, "", "/x"),
            ProviderError::AlreadyExists(_)
        ));
        assert!(matches!(
            TwakeProvider::classify_error(401, "", "/x"),
            ProviderError::AuthenticationFailed(_)
        ));
        let e = TwakeProvider::classify_error(
            413,
            r#"{"errors":[{"status":"413","title":"Request Entity Too Large","detail":"The file is too big"}]}"#,
            "/x",
        );
        assert!(
            matches!(e, ProviderError::TransferFailed(ref m) if m.contains("The file is too big"))
        );
    }
}
