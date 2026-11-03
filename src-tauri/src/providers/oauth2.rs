//! OAuth2 Authentication Module
//!
//! Provides OAuth2 authentication flow for cloud providers like Google Drive,
//! Dropbox, and OneDrive. Uses system browser for authorization and keyring
//! for secure token storage.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use oauth2::{
    basic::BasicClient, AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken,
    EndpointNotSet, EndpointSet, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, RefreshToken,
    Scope, TokenResponse, TokenUrl,
};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// In-memory token cache for when vault is locked (master mode).
/// Tokens survive the session but are NOT persisted to disk.
static MEMORY_TOKEN_CACHE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// Configured OAuth2 client with auth and token endpoints set (v5 typestates)
type ConfiguredClient =
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

/// Simple error wrapper for the oauth2 HTTP client adapter.
#[derive(Debug)]
struct OAuth2TransportError(String);

impl std::fmt::Display for OAuth2TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for OAuth2TransportError {}

/// reqwest's `Display` for a timeout is the generic "error sending request",
/// so a token request that ran out of time says so.
fn token_transport_error(
    e: reqwest::Error,
    limit: std::time::Duration,
) -> oauth2::HttpClientError<OAuth2TransportError> {
    if e.is_timeout() {
        oauth2::HttpClientError::Other(format!(
            "the token server did not answer within {} s",
            limit.as_secs()
        ))
    } else {
        oauth2::HttpClientError::Other(e.to_string())
    }
}

/// Async HTTP client adapter for oauth2 v5.
/// Bridges the project's reqwest (0.13) with oauth2's `AsyncHttpClient` trait.
/// Required because oauth2 v5's built-in reqwest support targets reqwest 0.12.
struct OAuth2HttpClient;

impl<'c> oauth2::AsyncHttpClient<'c> for OAuth2HttpClient {
    type Error = oauth2::HttpClientError<OAuth2TransportError>;
    type Future = std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<oauth2::HttpResponse, Self::Error>>
                + Send
                + Sync
                + 'c,
        >,
    >;

    /// Send one token request, connect and whole reply bounded by the
    /// sign-in limit.
    fn call(&'c self, request: oauth2::HttpRequest) -> Self::Future {
        Box::pin(async move {
            let limit = super::http_limits::limit(super::http_limits::SIGN_IN_TIMEOUT);
            let client = reqwest::Client::builder()
                .user_agent(crate::providers::AEROFTP_USER_AGENT)
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(limit)
                .timeout(limit)
                .build()
                .map_err(|e| oauth2::HttpClientError::Other(e.to_string()))?;

            let method = reqwest::Method::from_bytes(request.method().as_str().as_bytes())
                .unwrap_or(reqwest::Method::POST);
            let url = request.uri().to_string();

            let mut builder = client.request(method, &url);
            for (name, value) in request.headers() {
                builder = builder.header(name.as_str(), value.as_bytes());
            }
            builder = builder.body(request.into_body());

            let response = builder
                .send()
                .await
                .map_err(|e| token_transport_error(e, limit))?;

            let status_code = response.status().as_u16();
            let headers = response.headers().clone();
            let body = response
                .bytes()
                .await
                .map_err(|e| token_transport_error(e, limit))?;

            let mut http_response = http::Response::builder().status(
                http::StatusCode::from_u16(status_code)
                    .unwrap_or(http::StatusCode::INTERNAL_SERVER_ERROR),
            );
            for (name, value) in headers.iter() {
                http_response = http_response.header(name.as_str(), value.as_bytes());
            }
            http_response
                .body(body.to_vec())
                .map_err(|e| oauth2::HttpClientError::Other(e.to_string()))
        })
    }
}
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

use super::ProviderError;

/// OAuth2 provider types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OAuthProvider {
    Google,
    GooglePhotos,
    Dropbox,
    OneDrive,
    Box,
    PCloud,
    ZohoWorkdrive,
    YandexDisk,
}

impl std::fmt::Display for OAuthProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OAuthProvider::Google => write!(f, "Google Drive"),
            OAuthProvider::GooglePhotos => write!(f, "Google Photos"),
            OAuthProvider::Dropbox => write!(f, "Dropbox"),
            OAuthProvider::OneDrive => write!(f, "OneDrive"),
            OAuthProvider::YandexDisk => write!(f, "Yandex Disk"),
            OAuthProvider::Box => write!(f, "Box"),
            OAuthProvider::PCloud => write!(f, "pCloud"),
            OAuthProvider::ZohoWorkdrive => write!(f, "Zoho WorkDrive"),
        }
    }
}

/// OAuth2 configuration for a provider
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthConfig {
    pub provider: OAuthProvider,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub auth_url: String,
    pub token_url: String,
    pub scopes: Vec<String>,
    pub redirect_uri: String,
    /// Extra query parameters for the authorization URL (e.g., token_access_type=offline for Dropbox)
    pub extra_auth_params: Vec<(String, String)>,
    /// Server profile identifier that owns these tokens. When non-empty the
    /// vault stores tokens under `oauth_<provider>_<profile_id>`, enabling two
    /// distinct profiles for the same provider (work + personal Google Drive)
    /// to coexist on the same device. When empty the vault falls back to the
    /// legacy singleton key `oauth_<provider>`, which preserves the historic
    /// behaviour for callers that have not yet been wired through. A read with
    /// a non-empty profile_id that misses the per-profile key migrates the
    /// legacy entry under the new key on the first hit. Issue #214.
    profile_id: String,
}

impl Default for OAuthConfig {
    fn default() -> Self {
        Self {
            provider: OAuthProvider::Google,
            client_id: String::new(),
            client_secret: None,
            auth_url: String::new(),
            token_url: String::new(),
            scopes: Vec::new(),
            redirect_uri: String::new(),
            extra_auth_params: Vec::new(),
            profile_id: String::new(),
        }
    }
}

impl OAuthConfig {
    /// Bind this configuration to a server profile so the vault layer stores
    /// the resulting tokens under `oauth_<provider>_<profile_id>` instead of
    /// the legacy singleton `oauth_<provider>` key. Issue #214.
    pub fn with_profile_id(mut self, profile_id: &str) -> Self {
        self.profile_id = profile_id.to_string();
        self
    }

    /// Server profile identifier bound to this configuration. Empty when the
    /// caller has not bound a profile yet, in which case the vault falls back
    /// to the legacy per-provider key.
    pub fn profile_id(&self) -> &str {
        &self.profile_id
    }

    /// Create Google Drive OAuth config with dynamic callback port
    pub fn google_with_port(client_id: &str, client_secret: &str, port: u16) -> Self {
        Self {
            provider: OAuthProvider::Google,
            client_id: client_id.to_string(),
            client_secret: Some(client_secret.to_string()),
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".to_string(),
            token_url: "https://oauth2.googleapis.com/token".to_string(),
            scopes: vec![
                "https://www.googleapis.com/auth/drive".to_string(),
                "https://www.googleapis.com/auth/drive.file".to_string(),
            ],
            redirect_uri: format!("http://127.0.0.1:{}/callback", port),
            // prompt=consent forces Google to re-issue a refresh token on
            // every authorization. Without it Google returns a refresh
            // token only on the first consent: a re-auth after the token
            // expires then yields an access-token-only response, and the
            // profile breaks again as soon as that access token lapses.
            // See issue #196.
            extra_auth_params: vec![
                ("access_type".to_string(), "offline".to_string()),
                ("prompt".to_string(), "consent".to_string()),
            ],
            profile_id: String::new(),
        }
    }

    /// Create Google Drive OAuth config (default port for token refresh only)
    pub fn google(client_id: &str, client_secret: &str) -> Self {
        Self::google_with_port(client_id, client_secret, 0)
    }

    /// Create Google Photos OAuth config with dynamic callback port
    pub fn google_photos_with_port(client_id: &str, client_secret: &str, port: u16) -> Self {
        Self {
            provider: OAuthProvider::GooglePhotos,
            client_id: client_id.to_string(),
            client_secret: Some(client_secret.to_string()),
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".to_string(),
            token_url: "https://oauth2.googleapis.com/token".to_string(),
            scopes: vec![
                "https://www.googleapis.com/auth/photoslibrary.readonly".to_string(),
                "https://www.googleapis.com/auth/photoslibrary.appendonly".to_string(),
            ],
            redirect_uri: format!("http://127.0.0.1:{}/callback", port),
            // Same Google quirk as Drive: prompt=consent guarantees a
            // refresh token on every re-authorization. See issue #196.
            extra_auth_params: vec![
                ("access_type".to_string(), "offline".to_string()),
                ("prompt".to_string(), "consent".to_string()),
            ],
            profile_id: String::new(),
        }
    }

    /// Create Google Photos OAuth config (default port for token refresh only)
    pub fn google_photos(client_id: &str, client_secret: &str) -> Self {
        Self::google_photos_with_port(client_id, client_secret, 0)
    }

    /// Create Dropbox OAuth config with dynamic callback port
    pub fn dropbox_with_port(client_id: &str, client_secret: &str, port: u16) -> Self {
        Self {
            provider: OAuthProvider::Dropbox,
            client_id: client_id.to_string(),
            client_secret: Some(client_secret.to_string()),
            auth_url: "https://www.dropbox.com/oauth2/authorize".to_string(),
            token_url: "https://api.dropboxapi.com/oauth2/token".to_string(),
            scopes: vec![
                "account_info.read".to_string(),
                "files.metadata.read".to_string(),
                "files.metadata.write".to_string(),
                "files.content.read".to_string(),
                "files.content.write".to_string(),
                "files.permanent_delete".to_string(),
                "sharing.read".to_string(),
                "sharing.write".to_string(),
            ],
            redirect_uri: format!("http://127.0.0.1:{}/callback", port),
            extra_auth_params: vec![("token_access_type".to_string(), "offline".to_string())],
            profile_id: String::new(),
        }
    }

    /// Create Dropbox OAuth config (default port for token refresh only)
    pub fn dropbox(client_id: &str, client_secret: &str) -> Self {
        Self::dropbox_with_port(client_id, client_secret, 0)
    }

    /// Create OneDrive OAuth config with dynamic callback port
    /// Microsoft Entra ID requires http://localhost (not 127.0.0.1) for redirect URIs
    pub fn onedrive_with_port(client_id: &str, client_secret: &str, port: u16) -> Self {
        Self {
            provider: OAuthProvider::OneDrive,
            client_id: client_id.to_string(),
            client_secret: Some(client_secret.to_string()),
            auth_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize".to_string(),
            token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token".to_string(),
            scopes: vec![
                "Files.ReadWrite".to_string(),
                "Files.ReadWrite.All".to_string(),
                "offline_access".to_string(),
            ],
            redirect_uri: format!("http://localhost:{}/callback", port),
            extra_auth_params: vec![],
            profile_id: String::new(),
        }
    }

    /// Create OneDrive OAuth config (default port for token refresh only)
    pub fn onedrive(client_id: &str, client_secret: &str) -> Self {
        Self::onedrive_with_port(client_id, client_secret, 0)
    }

    /// Create Box OAuth config with dynamic callback port
    pub fn box_cloud_with_port(client_id: &str, client_secret: &str, port: u16) -> Self {
        Self {
            provider: OAuthProvider::Box,
            client_id: client_id.to_string(),
            client_secret: Some(client_secret.to_string()),
            auth_url: "https://account.box.com/api/oauth2/authorize".to_string(),
            token_url: "https://api.box.com/oauth2/token".to_string(),
            scopes: vec![],
            redirect_uri: format!("http://127.0.0.1:{}/callback", port),
            extra_auth_params: vec![],
            profile_id: String::new(),
        }
    }

    /// Create Box OAuth config (default port for token refresh only)
    pub fn box_cloud(client_id: &str, client_secret: &str) -> Self {
        Self::box_cloud_with_port(client_id, client_secret, 0)
    }

    /// Create pCloud OAuth config with dynamic callback port and region
    pub fn pcloud_with_port(client_id: &str, client_secret: &str, port: u16, region: &str) -> Self {
        // GAP-A06: pCloud EU uses eapi.pcloud.com for API/token endpoint
        let token_url = if region == "eu" {
            "https://eapi.pcloud.com/oauth2_token"
        } else {
            "https://api.pcloud.com/oauth2_token"
        };
        Self {
            provider: OAuthProvider::PCloud,
            client_id: client_id.to_string(),
            client_secret: Some(client_secret.to_string()),
            auth_url: "https://my.pcloud.com/oauth2/authorize".to_string(),
            token_url: token_url.to_string(),
            scopes: vec![],
            redirect_uri: format!("http://localhost:{}/callback", port),
            extra_auth_params: vec![],
            profile_id: String::new(),
        }
    }

    /// Create pCloud OAuth config (default port for token refresh only)
    pub fn pcloud(client_id: &str, client_secret: &str, region: &str) -> Self {
        Self::pcloud_with_port(client_id, client_secret, 0, region)
    }

    /// Get Zoho OAuth domain for a given region
    fn zoho_domain(region: &str) -> &'static str {
        match region {
            "eu" => "accounts.zoho.eu",
            "in" => "accounts.zoho.in",
            "au" => "accounts.zoho.com.au",
            "jp" => "accounts.zoho.jp",
            "uk" => "accounts.zoho.uk",
            "ca" => "accounts.zohocloud.ca",
            "sa" => "accounts.zoho.sa",
            "cn" => "accounts.zoho.com.cn",
            "ae" => "accounts.zoho.ae",
            _ => "accounts.zoho.com", // US default
        }
    }

    /// Create Zoho WorkDrive OAuth config with dynamic callback port and region
    pub fn zoho_with_port(client_id: &str, client_secret: &str, port: u16, region: &str) -> Self {
        let domain = Self::zoho_domain(region);
        Self {
            provider: OAuthProvider::ZohoWorkdrive,
            client_id: client_id.to_string(),
            client_secret: Some(client_secret.to_string()),
            auth_url: format!("https://{}/oauth/v2/auth", domain),
            token_url: format!("https://{}/oauth/v2/token", domain),
            scopes: vec![
                "WorkDrive.files.ALL".to_string(),
                "WorkDrive.team.ALL".to_string(),
                "WorkDrive.workspace.ALL".to_string(),
                "WorkDrive.teamfolders.ALL".to_string(),
                "WorkDrive.links.ALL".to_string(),
                "WorkDrive.labels.ALL".to_string(),
                "ZohoFiles.files.ALL".to_string(),
            ],
            redirect_uri: format!("http://127.0.0.1:{}/callback", port),
            extra_auth_params: vec![
                ("access_type".to_string(), "offline".to_string()),
                ("prompt".to_string(), "consent".to_string()),
            ],
            profile_id: String::new(),
        }
    }

    /// Create Zoho WorkDrive OAuth config (default port for token refresh only)
    pub fn zoho(client_id: &str, client_secret: &str, region: &str) -> Self {
        Self::zoho_with_port(client_id, client_secret, 0, region)
    }

    /// Create Yandex Disk OAuth config with dynamic callback port
    pub fn yandex_disk_with_port(client_id: &str, client_secret: &str, port: u16) -> Self {
        Self {
            provider: OAuthProvider::YandexDisk,
            client_id: client_id.to_string(),
            client_secret: Some(client_secret.to_string()),
            auth_url: "https://oauth.yandex.com/authorize".to_string(),
            token_url: "https://oauth.yandex.com/token".to_string(),
            scopes: vec![
                "cloud_api:disk.read".to_string(),
                "cloud_api:disk.write".to_string(),
                "cloud_api:disk.info".to_string(),
                "cloud_api:disk.app_folder".to_string(),
            ],
            redirect_uri: format!("http://localhost:{}/callback", port),
            extra_auth_params: vec![],
            profile_id: String::new(),
        }
    }

    /// Create Yandex Disk OAuth config (default port for token refresh only)
    pub fn yandex_disk(client_id: &str, client_secret: &str) -> Self {
        Self::yandex_disk_with_port(client_id, client_secret, 0)
    }
}

/// Stored OAuth2 tokens: zeroized on drop (VER-007)
#[derive(Debug, Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct StoredTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<i64>, // Unix timestamp
    pub token_type: String,
    pub scopes: Vec<String>,
}

impl StoredTokens {
    /// Check if token is expired (with 5 min buffer)
    pub fn is_expired(&self) -> bool {
        if let Some(expires_at) = self.expires_at {
            let now = chrono::Utc::now().timestamp();
            expires_at <= now + 300 // 5 minutes buffer
        } else {
            false // No expiry = assume valid
        }
    }
}

/// Cross-process advisory lease guarding the OAuth refresh + persist
/// critical section.
///
/// The in-process `refresh_guard` mutex only serializes refreshes inside ONE
/// process. The desktop app and the long-running MCP server are SEPARATE
/// processes sharing the same vault. Without cross-process coordination both
/// can refresh the same provider concurrently and, for providers that rotate
/// the refresh token on every refresh (Box, Dropbox, Google), invalidate
/// each other's token: the exact F5 failure mode (MCP serving a stale /
/// dead token after a GUI re-auth). This lease makes the refresh
/// single-owner across processes; the loser waits a bounded time then
/// re-reads the token the winner just persisted (hot-reload) instead of
/// racing the rotation.
///
/// Implemented with an atomic `create_new` lock file: portable (O_EXCL on
/// Unix, CREATE_NEW on Windows), no extra crate. A stale lease (holder
/// crashed without cleanup) is stolen after `STALE_SECS`, and acquisition
/// is bounded by `MAX_WAIT_MS`, so OAuth can never wedge permanently.
///
/// The native Proton Drive client takes the same lease for its rotating
/// session (`proton_native::vault`), keyed on its own vault key.
pub(crate) struct RefreshLease {
    path: std::path::PathBuf,
}

impl RefreshLease {
    const STALE_SECS: u64 = 30;
    const MAX_WAIT_MS: u64 = 10_000;
    const POLL_MS: u64 = 150;

    /// Try to become the single cross-process owner of the refresh for
    /// `slug`. `Some(lease)` => WE own it, perform the refresh. `None` =>
    /// gave up waiting or no lock dir; caller must re-load tokens (the other
    /// owner is refreshing) rather than refresh in parallel.
    pub(crate) async fn acquire(slug: &str) -> Option<Self> {
        let dir = OAuth2Manager::token_dir().ok()?;
        let path = dir.join(format!("refresh-{}.lock", slug));
        let started = std::time::Instant::now();
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = writeln!(
                        f,
                        "{} {}",
                        std::process::id(),
                        chrono::Utc::now().timestamp()
                    );
                    return Some(Self { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Steal a stale lease (previous holder crashed).
                    if let Ok(modified) = std::fs::metadata(&path).and_then(|m| m.modified()) {
                        if modified
                            .elapsed()
                            .map(|d| d.as_secs() >= Self::STALE_SECS)
                            .unwrap_or(true)
                        {
                            let _ = std::fs::remove_file(&path);
                            continue;
                        }
                    }
                    if started.elapsed().as_millis() as u64 >= Self::MAX_WAIT_MS {
                        return None;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(Self::POLL_MS)).await;
                }
                Err(_) => return None, // unexpected fs error: degrade gracefully
            }
        }
    }
}

impl Drop for RefreshLease {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// OAuth2 Manager for handling authentication flows.
///
/// `Clone` is deliberate and cheap: it shares `pending_verifiers` and the
/// in-process `refresh_guard` so transfer workers for the same provider
/// instance cannot rotate refresh tokens concurrently (H-04 / DAG-P1-05B).
/// Cross-process serialization still goes through [`RefreshLease`].
pub struct OAuth2Manager {
    /// Pending PKCE verifiers for ongoing auth flows
    pending_verifiers: Arc<RwLock<HashMap<String, PkceCodeVerifier>>>,
    /// Callback server port (used in redirect URL generation)
    #[allow(dead_code)]
    callback_port: u16,
    credential_user: Option<i64>,
    /// Guard to prevent concurrent token refresh (H-04: avoids invalid_grant race).
    /// Shared across clones of the same manager so Dropbox/Box part workers
    /// serialize refresh against the primary.
    refresh_guard: Arc<tokio::sync::Mutex<()>>,
}

/// Secret PKCE material for an encrypted, profile-bound headless attempt.
/// Callers must never serialize this into an IPC or CLI response.
#[derive(Serialize, Deserialize)]
pub struct HeadlessOAuthFlow {
    pub config: OAuthConfig,
    pub state: String,
    verifier: String,
    pub expires_at: i64,
}

impl Clone for OAuth2Manager {
    fn clone(&self) -> Self {
        Self {
            pending_verifiers: Arc::clone(&self.pending_verifiers),
            callback_port: self.callback_port,
            credential_user: self.credential_user,
            refresh_guard: Arc::clone(&self.refresh_guard),
        }
    }
}

impl OAuth2Manager {
    /// Bind refresh reads and writes to the account that owns the profile.
    pub fn with_credential_user(mut self, user_id: Option<i64>) -> Self {
        self.credential_user = user_id;
        self
    }
    /// The user whose tokens, bound to a profile or not, must stay in their own
    /// partition: anyone but the legacy owner. In the shared vault they would
    /// be readable through the legacy owner's fallback, for example when an
    /// imported profile keeps its id.
    fn private_token_user(&self) -> Result<Option<i64>, ProviderError> {
        crate::user_partitions::private_token_user(self.credential_user, "oauth")
            .map_err(|e| ProviderError::Other(format!("Failed to resolve token owner: {}", e)))
    }

    fn mirror_tokens(
        &self,
        store: &crate::credential_store::CredentialStore,
        account: &str,
        json: &str,
    ) -> Result<(), ProviderError> {
        let mirrored = if let Some(user) = self.credential_user {
            crate::user_partitions::mirror_credential_for_user_typed(
                store, user, account, "oauth", json,
            )
        } else {
            crate::user_partitions::mirror_active_credential(store, account, "oauth", json)
        };
        mirrored.map_err(|e| ProviderError::Other(format!("Failed to store tokens: {e}")))
    }
    pub async fn begin_headless(
        &self,
        config: OAuthConfig,
    ) -> Result<(String, HeadlessOAuthFlow), ProviderError> {
        let (url, state) = self.start_auth_flow(&config).await?;
        let verifier = self
            .pending_verifiers
            .write()
            .await
            .remove(&state)
            .ok_or_else(|| ProviderError::Other("Missing PKCE verifier".into()))?;
        Ok((
            url,
            HeadlessOAuthFlow {
                config,
                state,
                verifier: verifier.secret().clone(),
                expires_at: chrono::Utc::now().timestamp() + 600,
            },
        ))
    }

    /// Exchange without persisting: the caller owns the explicit user/profile scope.
    pub async fn finish_headless(
        &self,
        pending: &HeadlessOAuthFlow,
        code: &str,
        state: &str,
    ) -> Result<StoredTokens, ProviderError> {
        if state != pending.state || pending.expires_at <= chrono::Utc::now().timestamp() {
            return Err(ProviderError::AuthenticationFailed(
                "Authorization state mismatched or expired; begin again".into(),
            ));
        }
        self.exchange_auth_code(
            &pending.config,
            code,
            PkceCodeVerifier::new(pending.verifier.clone()),
        )
        .await
    }

    pub fn new() -> Self {
        Self {
            pending_verifiers: Arc::new(RwLock::new(HashMap::new())),
            callback_port: 0, // Will be assigned dynamically by OS
            credential_user: None,
            refresh_guard: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Test-only: expose the shared refresh guard Arc for pointer-equality
    /// checks across transfer-worker clones (DAG-P1-05B).
    #[cfg(test)]
    pub(crate) fn refresh_guard_for_test(&self) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(&self.refresh_guard)
    }

    /// Start OAuth2 authorization flow - returns URL to open in browser
    pub async fn start_auth_flow(
        &self,
        config: &OAuthConfig,
    ) -> Result<(String, String), ProviderError> {
        let client = self.create_client(config)?;

        // Generate PKCE challenge
        let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

        // Generate state token for CSRF protection
        let (auth_url, csrf_token) = {
            let mut auth_builder = client
                .authorize_url(CsrfToken::new_random)
                .set_pkce_challenge(pkce_challenge);

            // Add scopes
            for scope in &config.scopes {
                auth_builder = auth_builder.add_scope(Scope::new(scope.clone()));
            }

            // Add extra auth parameters (e.g., token_access_type=offline for Dropbox)
            for (key, value) in &config.extra_auth_params {
                auth_builder = auth_builder.add_extra_param(key, value);
            }

            auth_builder.url()
        };

        let state = csrf_token.secret().clone();

        // Store verifier for later
        {
            let mut verifiers = self.pending_verifiers.write().await;
            verifiers.insert(state.clone(), pkce_verifier);
        }

        info!("OAuth2 auth URL generated for {:?}", config.provider);

        Ok((auth_url.to_string(), state))
    }

    /// Complete OAuth2 flow with authorization code
    pub async fn complete_auth_flow(
        &self,
        config: &OAuthConfig,
        code: &str,
        state: &str,
    ) -> Result<StoredTokens, ProviderError> {
        // Get and remove pending verifier
        let verifier = {
            let mut verifiers = self.pending_verifiers.write().await;
            verifiers.remove(state).ok_or_else(|| {
                ProviderError::AuthenticationFailed(
                    "Invalid state token - authorization flow expired or invalid".to_string(),
                )
            })?
        };

        let tokens = self.exchange_auth_code(config, code, verifier).await?;
        self.store_tokens(config.provider, &config.profile_id, &tokens)?;
        Ok(tokens)
    }

    async fn exchange_auth_code(
        &self,
        config: &OAuthConfig,
        code: &str,
        verifier: PkceCodeVerifier,
    ) -> Result<StoredTokens, ProviderError> {
        let client = self.create_client(config)?;
        let token_result = client
            .exchange_code(AuthorizationCode::new(code.to_string()))
            .set_pkce_verifier(verifier)
            .request_async(&OAuth2HttpClient)
            .await
            .map_err(|e| {
                ProviderError::AuthenticationFailed(format!("Token exchange failed: {}", e))
            })?;

        let expires_at = token_result
            .expires_in()
            .map(|d| chrono::Utc::now().timestamp() + d.as_secs() as i64);

        let tokens = StoredTokens {
            access_token: token_result.access_token().secret().clone(),
            refresh_token: token_result.refresh_token().map(|t| t.secret().clone()),
            expires_at,
            token_type: "Bearer".to_string(),
            scopes: config.scopes.clone(),
        };

        info!("OAuth2 tokens obtained for {:?}", config.provider);

        Ok(tokens)
    }

    /// Refresh access token using refresh token
    pub async fn refresh_tokens(
        &self,
        config: &OAuthConfig,
        refresh_token: &str,
    ) -> Result<StoredTokens, ProviderError> {
        let client = self.create_client(config)?;

        let token_result = client
            .exchange_refresh_token(&RefreshToken::new(refresh_token.to_string()))
            .request_async(&OAuth2HttpClient)
            .await
            .map_err(|e| {
                ProviderError::AuthenticationFailed(format!("Token refresh failed: {}", e))
            })?;

        let expires_at = token_result
            .expires_in()
            .map(|d| chrono::Utc::now().timestamp() + d.as_secs() as i64);

        let tokens = StoredTokens {
            access_token: token_result.access_token().secret().clone(),
            refresh_token: token_result
                .refresh_token()
                .map(|t| t.secret().clone())
                .or_else(|| Some(refresh_token.to_string())), // Keep old refresh token if not returned
            expires_at,
            token_type: "Bearer".to_string(),
            scopes: config.scopes.clone(),
        };

        // Update keyring under the profile-bound key.
        self.store_tokens(config.provider, &config.profile_id, &tokens)?;

        info!("OAuth2 tokens refreshed for {:?}", config.provider);

        Ok(tokens)
    }

    /// Get valid access token (refreshing if needed).
    ///
    /// Two layers of concurrency control:
    /// - `refresh_guard`: serializes refresh within THIS process (H-04).
    /// - `RefreshLease`: serializes refresh ACROSS processes (desktop app vs
    ///   long-running MCP server), so refresh-token rotation can't make them
    ///   invalidate each other (F5 #1). Double-checked: after winning the
    ///   lease we re-load, so if the other process already refreshed we use
    ///   its result instead of refreshing again.
    pub async fn get_valid_token(
        &self,
        config: &OAuthConfig,
    ) -> Result<SecretString, ProviderError> {
        let _guard = self.refresh_guard.lock().await;
        let mut tokens = self.load_tokens(config.provider, &config.profile_id)?;

        if tokens.is_expired() {
            // Per-profile lease: two profiles pointing at distinct accounts of
            // the same provider must not block each other's refresh.
            let slug = Self::token_account_key(config.provider, &config.profile_id);
            // Bounded, self-healing cross-process lease. `None` => another
            // process is the refresh owner (or no lock dir): fall through to
            // the re-load below and use whatever it persisted.
            let _lease = RefreshLease::acquire(&slug).await;
            // Double-check: the lease winner (possibly the other process)
            // may have already rotated+persisted a fresh token.
            tokens = self.load_tokens(config.provider, &config.profile_id)?;
            if tokens.is_expired() {
                if let Some(ref refresh_token) = tokens.refresh_token {
                    tokens = self.refresh_tokens(config, refresh_token).await?;
                } else {
                    return Err(ProviderError::AuthenticationFailed(
                        "Token expired and no refresh token available".to_string(),
                    ));
                }
            }
            // _lease drops here, releasing the cross-process lock.
        }

        Ok(SecretString::from(tokens.access_token.clone()))
    }

    /// Get the token storage directory
    fn token_dir() -> Result<std::path::PathBuf, ProviderError> {
        let token_dir = crate::portable::aeroftp_data_root()
            .ok_or_else(|| ProviderError::Other("Could not find AeroFTP data root".to_string()))?
            .join("oauth_tokens");
        if !token_dir.exists() {
            std::fs::create_dir_all(&token_dir).map_err(|e| {
                ProviderError::Other(format!("Failed to create token directory: {}", e))
            })?;
        }
        Ok(token_dir)
    }

    /// Compose the per-profile vault key when `profile_id` is non-empty,
    /// falling back to the legacy singleton key otherwise. Issue #214.
    fn token_account_key(provider: OAuthProvider, profile_id: &str) -> String {
        if profile_id.is_empty() {
            format!("oauth_{:?}", provider).to_lowercase()
        } else {
            format!("oauth_{:?}_{}", provider, profile_id).to_lowercase()
        }
    }

    /// Legacy singleton vault key, used as fallback by `load_tokens` and
    /// migrated lazily on first hit when the caller supplies a non-empty
    /// `profile_id`. Issue #214.
    fn legacy_token_account_key(provider: OAuthProvider) -> String {
        format!("oauth_{:?}", provider).to_lowercase()
    }

    /// Store tokens for the given provider/profile pair. With an empty
    /// `profile_id` this writes the legacy singleton key, preserving the
    /// historic behaviour for callers that have not yet been wired through.
    /// Issue #214.
    pub fn store_tokens(
        &self,
        provider: OAuthProvider,
        profile_id: &str,
        tokens: &StoredTokens,
    ) -> Result<(), ProviderError> {
        let json = serde_json::to_string_pretty(tokens)
            .map_err(|e| ProviderError::Other(format!("Failed to serialize tokens: {}", e)))?;

        let account = Self::token_account_key(provider, profile_id);

        if let Some(user) = self.private_token_user()? {
            if crate::credential_store::CredentialStore::from_cache().is_none() {
                let _ = crate::credential_store::CredentialStore::init();
            }
            let store =
                crate::credential_store::CredentialStore::from_cache().ok_or_else(|| {
                    ProviderError::Other(
                        "Credential vault is not open; tokens not stored".to_string(),
                    )
                })?;
            crate::user_partitions::store_credential_for_user(
                &store, user, &account, "oauth", &json,
            )
            .map_err(|e| ProviderError::Other(format!("Failed to store tokens: {}", e)))?;
            info!("Tokens stored in user partition for {:?}", provider);
            return Ok(());
        }

        // Store in universal vault
        if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
            store
                .store(&account, &json)
                .map_err(|e| ProviderError::Other(format!("Failed to store tokens: {}", e)))?;
            // MUV-4: mirror the refreshed token into the active user's partition
            // (per-profile keys only; the legacy singleton stays vault-only).
            if !profile_id.is_empty() {
                self.mirror_tokens(&store, &account, &json)?;
            }
            info!("Tokens stored in credential vault for {:?}", provider);
            return Ok(());
        }

        // Vault not open: try auto-initializing vault first
        if crate::credential_store::CredentialStore::init().is_ok() {
            if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
                store
                    .store(&account, &json)
                    .map_err(|e| ProviderError::Other(format!("Failed to store tokens: {}", e)))?;
                if !profile_id.is_empty() {
                    self.mirror_tokens(&store, &account, &json)?;
                }
                info!("Tokens stored in auto-initialized vault for {:?}", provider);
                return Ok(());
            }
        }

        // Vault requires master password: store in memory only (never on disk unencrypted)
        if let Ok(mut cache) = MEMORY_TOKEN_CACHE.lock() {
            let map = cache.get_or_insert_with(HashMap::new);
            map.insert(account, json);
        }

        info!("Tokens stored in memory for {:?} (vault locked)", provider);
        Ok(())
    }

    /// Load tokens for the given provider/profile pair. When `profile_id` is
    /// non-empty and the proven legacy owner's per-profile key misses the vault,
    /// the legacy singleton key is consulted as a one-shot fallback: on hit the
    /// value is rebound under the per-profile key and the legacy entry is removed.
    /// Other users never inherit global, memory-cached or plaintext legacy tokens.
    pub fn load_tokens(
        &self,
        provider: OAuthProvider,
        profile_id: &str,
    ) -> Result<StoredTokens, ProviderError> {
        let account = Self::token_account_key(provider, profile_id);
        let legacy = Self::legacy_token_account_key(provider);
        let (credential_user, legacy_fallback_allowed) =
            crate::user_partitions::credential_owner_and_fallback(self.credential_user).map_err(
                |e| ProviderError::Other(format!("Failed to resolve token owner: {}", e)),
            )?;

        // MUV-4: read the captured user's own partition first, including the
        // legacy singleton key. A locked owner fails here before any global,
        // memory or plaintext fallback. Only the proven legacy owner may
        // continue, and an account switch cannot change the owner mid-read.
        if let (Some(store), Some(user)) = (
            crate::credential_store::CredentialStore::from_cache(),
            credential_user,
        ) {
            let resolved = crate::user_partitions::resolve_credential_for_user_owner_aware(
                &store, user, &account,
            )
            .map_err(|e| ProviderError::Other(format!("Failed to read user tokens: {}", e)))?;
            if let Some(json) = resolved {
                return serde_json::from_str(&json)
                    .map_err(|e| ProviderError::Other(format!("Failed to parse tokens: {}", e)));
            }
        }

        // The partition read above refuses a locked user only while the vault is
        // open; the lock alone must also stop the memory and plaintext fallbacks.
        if let Some(user) = credential_user {
            crate::user_partitions::ensure_credential_user_unlocked(user)
                .map_err(|e| ProviderError::Other(format!("Failed to read user tokens: {}", e)))?;
        }

        if !legacy_fallback_allowed {
            return Err(ProviderError::AuthenticationFailed(
                "No stored tokens for this user".to_string(),
            ));
        }

        // Try vault first under the per-profile (or legacy when profile_id is empty) key.
        if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
            if let Ok(json) = store.get(&account) {
                return serde_json::from_str(&json)
                    .map_err(|e| ProviderError::Other(format!("Failed to parse tokens: {}", e)));
            }

            // Lazy migration: per-profile key missing, legacy hit ➜ rebind + remove.
            if !profile_id.is_empty() {
                if let Ok(json) = store.get(&legacy) {
                    let tokens: StoredTokens = serde_json::from_str(&json).map_err(|e| {
                        ProviderError::Other(format!("Failed to parse legacy tokens: {}", e))
                    })?;
                    if store.store(&account, &json).is_ok() {
                        let _ = store.delete(&legacy);
                        info!(
                            "Migrated legacy {:?} token to per-profile vault key",
                            provider
                        );
                    } else {
                        warn!(
                            "Per-profile vault write failed for {:?}; legacy token left in place",
                            provider
                        );
                    }
                    return Ok(tokens);
                }
            }
        }

        // Fallback: try in-memory cache (mirror of the vault layout above).
        if let Ok(cache) = MEMORY_TOKEN_CACHE.lock() {
            if let Some(map) = cache.as_ref() {
                if let Some(json) = map.get(&account) {
                    return serde_json::from_str(json).map_err(|e| {
                        ProviderError::Other(format!("Failed to parse tokens: {}", e))
                    });
                }
                if !profile_id.is_empty() {
                    if let Some(json) = map.get(&legacy) {
                        return serde_json::from_str(json).map_err(|e| {
                            ProviderError::Other(format!("Failed to parse tokens: {}", e))
                        });
                    }
                }
            }
        }

        // Legacy: try plaintext file: migrate to vault immediately, then delete the file
        let legacy_path =
            Self::token_dir()?.join(format!("oauth2_{:?}.json", provider).to_lowercase());
        let json = std::fs::read_to_string(&legacy_path)
            .map_err(|e| ProviderError::AuthenticationFailed(format!("No stored tokens: {}", e)))?;

        warn!(
            "Found legacy plaintext OAuth token file for {:?}: migrating to vault",
            provider
        );

        let tokens: StoredTokens = serde_json::from_str(&json)
            .map_err(|e| ProviderError::Other(format!("Failed to parse tokens: {}", e)))?;

        // Migrate to vault under the per-profile key (or legacy when profile_id is empty).
        let mut migrated = false;
        if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
            if store.store(&account, &json).is_ok() {
                migrated = true;
                info!(
                    "Legacy tokens for {:?} migrated to credential vault",
                    provider
                );
            }
        }
        if !migrated {
            // Try auto-init vault
            if crate::credential_store::CredentialStore::init().is_ok() {
                if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
                    if store.store(&account, &json).is_ok() {
                        migrated = true;
                        info!(
                            "Legacy tokens for {:?} migrated to auto-initialized vault",
                            provider
                        );
                    }
                }
            }
        }

        // Delete legacy plaintext file after successful migration
        if migrated {
            if let Err(e) = crate::credential_store::secure_delete(&legacy_path) {
                warn!(
                    "Failed to secure-delete legacy token file for {:?}: {}",
                    provider, e
                );
                // Fallback: try normal delete
                let _ = std::fs::remove_file(&legacy_path);
            } else {
                info!("Legacy plaintext token file deleted for {:?}", provider);
            }
        } else {
            warn!(
                "Could not migrate legacy tokens to vault for {:?}: vault unavailable. \
                 Plaintext file remains until vault is unlocked.",
                provider
            );
        }

        Ok(tokens)
    }

    /// Delete tokens from credential vault, memory cache, and legacy files
    /// for the given provider/profile pair. With an empty `profile_id` this
    /// targets the legacy singleton key. Plaintext file remnants and
    /// per-provider keyring entries are cleared too. Issue #214.
    pub fn delete_tokens(
        &self,
        provider: OAuthProvider,
        profile_id: &str,
    ) -> Result<(), ProviderError> {
        let account = Self::token_account_key(provider, profile_id);

        // A non-owner's tokens live only in its partition: the global key,
        // memory cache and plaintext files belong to the legacy owner.
        if let Some(user) = self.private_token_user()? {
            let conn = crate::user_partitions::open_or_init_cli()
                .map_err(|e| ProviderError::Other(format!("Failed to delete tokens: {}", e)))?;
            crate::user_partitions::delete_user_credential_for(&conn, user, &account)
                .map_err(|e| ProviderError::Other(format!("Failed to delete tokens: {}", e)))?;
            info!("Tokens deleted from user partition for {:?}", provider);
            return Ok(());
        }

        // Delete from vault
        if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
            let _ = store.delete(&account);
        }
        // MUV-4: drop the per-profile token from the active user's partition too.
        if !profile_id.is_empty() {
            if let Some(user) = self.credential_user {
                crate::user_partitions::unmirror_credential_for_user_best_effort_any(
                    user, &account,
                );
            } else {
                crate::user_partitions::unmirror_active_credential(&account);
            }
        }

        // Delete from in-memory cache
        if let Ok(mut cache) = MEMORY_TOKEN_CACHE.lock() {
            if let Some(map) = cache.as_mut() {
                map.remove(&account);
            }
        }

        // Delete legacy .json file if exists
        let json_path =
            Self::token_dir()?.join(format!("oauth2_{:?}.json", provider).to_lowercase());
        if json_path.exists() {
            let _ = crate::credential_store::secure_delete(&json_path);
        }

        // Delete legacy .enc file if exists
        let enc_path = Self::token_dir()?.join(format!("oauth2_{:?}.enc", provider).to_lowercase());
        if enc_path.exists() {
            let _ = crate::credential_store::secure_delete(&enc_path);
        }

        info!("Tokens deleted for {:?}", provider);
        Ok(())
    }

    /// Check if tokens exist for the given provider/profile pair. Honours the
    /// same lazy-migration rules as `load_tokens`. Issue #214.
    pub fn has_tokens(&self, provider: OAuthProvider, profile_id: &str) -> bool {
        self.load_tokens(provider, profile_id).is_ok()
    }

    /// Create OAuth2 client from config (v5 builder API)
    fn create_client(&self, config: &OAuthConfig) -> Result<ConfiguredClient, ProviderError> {
        let client_id = ClientId::new(config.client_id.clone());

        let auth_url = AuthUrl::new(config.auth_url.clone())
            .map_err(|e| ProviderError::Other(format!("Invalid auth URL: {}", e)))?;

        let token_url = TokenUrl::new(config.token_url.clone())
            .map_err(|e| ProviderError::Other(format!("Invalid token URL: {}", e)))?;

        let redirect_url = RedirectUrl::new(config.redirect_uri.clone())
            .map_err(|e| ProviderError::Other(format!("Invalid redirect URL: {}", e)))?;

        let mut client = BasicClient::new(client_id)
            .set_auth_uri(auth_url)
            .set_token_uri(token_url)
            .set_redirect_uri(redirect_url);

        if let Some(ref secret) = config.client_secret {
            client = client.set_client_secret(ClientSecret::new(secret.clone()));
        }

        Ok(client)
    }
}

impl Default for OAuth2Manager {
    fn default() -> Self {
        Self::new()
    }
}

/// Bind the OAuth2 callback listener on a specific port (0 = ephemeral).
/// Returns the listener and the actual port assigned by the OS.
pub async fn bind_callback_listener_on_port(
    port: u16,
) -> Result<(tokio::net::TcpListener, u16), ProviderError> {
    use tokio::net::TcpListener;

    let listener = TcpListener::bind(format!("127.0.0.1:{}", port))
        .await
        .map_err(|e| {
            ProviderError::Other(format!(
                "Failed to bind callback server on port {}: {}",
                port, e
            ))
        })?;

    let actual_port = listener
        .local_addr()
        .map(|a| a.port())
        .map_err(|e| ProviderError::Other(format!("Failed to get local port: {}", e)))?;

    info!("OAuth callback listener bound on port {}", actual_port);
    Ok((listener, actual_port))
}

/// Bind the OAuth2 callback listener on an ephemeral port.
/// Returns the listener and the actual port assigned by the OS.
pub async fn bind_callback_listener() -> Result<(tokio::net::TcpListener, u16), ProviderError> {
    bind_callback_listener_on_port(0).await
}

/// Read only the `state` parameter out of a callback request, without failing on
/// anything else in it.
///
/// The full parser refuses a request carrying an `error` parameter, which is the
/// right answer for the callback we are waiting for and the wrong one for a
/// stray request from some other local process: it would abort the flow. So the
/// state is read first and used to decide whether the request is ours at all.
fn callback_state(request: &str) -> Option<String> {
    let line = request.lines().next()?;
    let target = line.split_whitespace().nth(1)?;
    let query = target.split_once('?')?.1;
    for pair in query.split('&') {
        if let Some(("state", value)) = pair.split_once('=') {
            return Some(urlencoding::decode(value).ok()?.to_string());
        }
    }
    None
}

/// Wait for the OAuth2 callback that belongs to THIS flow on an already-bound
/// listener. Returns (code, state) extracted from the callback request.
///
/// Several providers require a fixed redirect port, so the listener sits on a
/// well-known local port during the flow. It used to accept exactly one
/// connection and hand whatever arrived to the caller, which meant any local
/// process that reached the port first consumed the single accept and the real
/// browser redirect then found nothing listening. State and PKCE stop that from
/// becoming account injection; they do not stop it from being a denial of
/// service. So requests that are not ours are answered and ignored, and the
/// listener keeps waiting until the expected state arrives or the deadline
/// passes.
pub async fn wait_for_callback(
    listener: tokio::net::TcpListener,
    expected_state: &str,
) -> Result<(String, String), ProviderError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::time::{timeout, Duration, Instant};

    // A3-01: bounded overall, so an endless stream of stray requests cannot keep
    // the flow alive forever. The budget is for the whole wait, not per accept.
    //
    // Five minutes, matching the timeout the callers already wrap this in. The
    // previous two minutes made theirs unreachable, so a sign-in with a slow
    // second factor failed at two minutes against a promise of five.
    let deadline = Instant::now() + Duration::from_secs(300);
    // A browser sends its request immediately after connecting, so a socket that
    // has said nothing in this long is not the callback. It has to be SHORT: the
    // loop is sequential, so every stalled connection delays the real redirect
    // by exactly this much, and a handful of them at thirty seconds each used to
    // eat the whole budget while the genuine request waited in the backlog.
    const READ_TIMEOUT: Duration = Duration::from_secs(5);
    let (mut socket, code, state) = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(ProviderError::Timeout);
        }
        let (mut socket, _): (tokio::net::TcpStream, _) = timeout(remaining, listener.accept())
            .await
            .map_err(|_| ProviderError::Timeout)?
            .map_err(|e| ProviderError::Other(format!("Failed to accept connection: {}", e)))?;

        let mut buffer = vec![0u8; 4096];
        // A3-01: Timeout on read to prevent slow-loris style attacks on the
        // callback socket. Capped by what is left of the overall budget too, so
        // the real ceiling is the deadline rather than the deadline plus one
        // more read.
        let read_budget = READ_TIMEOUT.min(deadline.saturating_duration_since(Instant::now()));
        let n: usize = match timeout(read_budget, socket.read(&mut buffer)).await {
            Ok(Ok(n)) => n,
            // A connection that stalls or dies is not the callback: drop it and
            // keep listening rather than ending the flow on someone else's socket.
            _ => continue,
        };
        let request = String::from_utf8_lossy(&buffer[..n]).into_owned();

        if callback_state(&request).as_deref() != Some(expected_state) {
            let _ = socket
                .write_all(
                    b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\n\r\nThis is not the AeroFTP authorization AeroFTP is waiting for.\n",
                )
                .await;
            continue;
        }

        // The state matches, so this request IS ours: from here a failure is a
        // real failure of this flow (the user denied access, for instance) and
        // must be surfaced rather than waited out.
        let (code, state) = parse_callback_request(&request)?;
        break (socket, code, state);
    };

    // Send success response with proper UTF-8 charset - Professional branded page
    let response = r#"HTTP/1.1 200 OK
Content-Type: text/html; charset=utf-8
Connection: close

<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>AeroFTP - Authorization Complete</title>
    <style>
        * { margin: 0; padding: 0; box-sizing: border-box; }
        
        body {
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Oxygen, Ubuntu, sans-serif;
            min-height: 100vh;
            display: flex;
            justify-content: center;
            align-items: center;
            background: linear-gradient(135deg, #0f0f1a 0%, #1a1a2e 50%, #16213e 100%);
            color: #fff;
            overflow: hidden;
        }
        
        /* Animated background particles */
        .bg-particles {
            position: fixed;
            top: 0;
            left: 0;
            width: 100%;
            height: 100%;
            pointer-events: none;
            overflow: hidden;
            z-index: 0;
        }
        
        .particle {
            position: absolute;
            width: 4px;
            height: 4px;
            background: rgba(0, 212, 255, 0.3);
            border-radius: 50%;
            animation: float 15s infinite;
        }
        
        .particle:nth-child(1) { left: 10%; animation-delay: 0s; }
        .particle:nth-child(2) { left: 20%; animation-delay: 2s; }
        .particle:nth-child(3) { left: 30%; animation-delay: 4s; }
        .particle:nth-child(4) { left: 40%; animation-delay: 6s; }
        .particle:nth-child(5) { left: 50%; animation-delay: 8s; }
        .particle:nth-child(6) { left: 60%; animation-delay: 10s; }
        .particle:nth-child(7) { left: 70%; animation-delay: 12s; }
        .particle:nth-child(8) { left: 80%; animation-delay: 14s; }
        .particle:nth-child(9) { left: 90%; animation-delay: 1s; }
        .particle:nth-child(10) { left: 95%; animation-delay: 3s; }
        
        @keyframes float {
            0%, 100% { transform: translateY(100vh) scale(0); opacity: 0; }
            10% { opacity: 1; }
            90% { opacity: 1; }
            100% { transform: translateY(-100vh) scale(1); opacity: 0; }
        }
        
        .container {
            position: relative;
            z-index: 1;
            text-align: center;
            padding: 60px 30px;
            background: rgba(22, 33, 62, 0.8);
            backdrop-filter: blur(20px);
            border-radius: 24px;
            box-shadow: 0 25px 80px rgba(0, 0, 0, 0.5), 0 0 0 1px rgba(255, 255, 255, 0.1);
            max-width: 440px;
            animation: slideUp 0.6s ease-out;
        }
        
        @keyframes slideUp {
            from { opacity: 0; transform: translateY(30px); }
            to { opacity: 1; transform: translateY(0); }
        }
        
        /* Logo */
        .logo {
            margin-bottom: 30px;
        }
        
        .logo img {
            height: 80px;
            filter: drop-shadow(0 4px 20px rgba(0, 212, 255, 0.4));
        }
        
        .app-name {
            font-size: 28px;
            font-weight: 700;
            background: linear-gradient(135deg, #00d4ff, #0099ff);
            -webkit-background-clip: text;
            -webkit-text-fill-color: transparent;
            background-clip: text;
            margin-top: 12px;
            letter-spacing: -0.5px;
        }
        
        /* Success icon */
        .success-icon {
            width: 90px;
            height: 90px;
            margin: 20px auto 30px;
            background: linear-gradient(135deg, #00d4ff, #00ff88);
            border-radius: 50%;
            display: flex;
            justify-content: center;
            align-items: center;
            animation: pulse 2s infinite;
            box-shadow: 0 10px 40px rgba(0, 212, 255, 0.3);
        }
        
        @keyframes pulse {
            0%, 100% { box-shadow: 0 10px 40px rgba(0, 212, 255, 0.3); }
            50% { box-shadow: 0 10px 60px rgba(0, 212, 255, 0.5); }
        }
        
        .success-icon svg {
            width: 45px;
            height: 45px;
            stroke: #fff;
            stroke-width: 3;
            fill: none;
            animation: checkmark 0.8s ease-out 0.3s both;
        }
        
        @keyframes checkmark {
            from { stroke-dashoffset: 50; }
            to { stroke-dashoffset: 0; }
        }
        
        .success-icon svg path {
            stroke-dasharray: 50;
            stroke-dashoffset: 0;
        }
        
        h1 {
            font-size: 26px;
            font-weight: 600;
            color: #fff;
            margin-bottom: 12px;
        }
        
        .subtitle {
            font-size: 16px;
            color: rgba(255, 255, 255, 0.7);
            line-height: 1.6;
            margin-bottom: 30px;
        }
        
        .provider-badge {
            display: inline-flex;
            align-items: center;
            gap: 8px;
            padding: 10px 20px;
            background: rgba(255, 255, 255, 0.1);
            border-radius: 30px;
            font-size: 14px;
            color: rgba(255, 255, 255, 0.9);
            margin-bottom: 30px;
        }
        
        .provider-badge svg {
            width: 20px;
            height: 20px;
        }
        
        .close-hint {
            font-size: 13px;
            color: rgba(255, 255, 255, 0.5);
            padding-top: 20px;
            border-top: 1px solid rgba(255, 255, 255, 0.1);
        }
        
        .close-hint kbd {
            display: inline-block;
            padding: 2px 8px;
            background: rgba(255, 255, 255, 0.1);
            border-radius: 4px;
            font-family: monospace;
            font-size: 12px;
            margin: 0 2px;
        }
    </style>
</head>
<body>
    <div class="bg-particles">
        <div class="particle"></div>
        <div class="particle"></div>
        <div class="particle"></div>
        <div class="particle"></div>
        <div class="particle"></div>
        <div class="particle"></div>
        <div class="particle"></div>
        <div class="particle"></div>
        <div class="particle"></div>
        <div class="particle"></div>
    </div>
    
    <div class="container">
        <div class="logo">
            <div class="app-name">AeroFTP</div>
        </div>
        
        <div class="success-icon">
            <svg viewBox="0 0 24 24">
                <path d="M5 13l4 4L19 7" stroke-linecap="round" stroke-linejoin="round"/>
            </svg>
        </div>
        
        <h1>Authorization Successful</h1>
        <p class="subtitle">Your cloud account has been connected securely.<br>You're all set to access your files!</p>
        
        <div class="provider-badge">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                <path d="M12 2L2 7l10 5 10-5-10-5zM2 17l10 5 10-5M2 12l10 5 10-5"/>
            </svg>
            Cloud Storage Connected
        </div>
        
        <p class="close-hint">You can close this tab and return to AeroFTP<br>or press <kbd>Ctrl</kbd> + <kbd>W</kbd></p>
    </div>
    
    <script>
        // Auto-close after 5 seconds (optional)
        // setTimeout(() => window.close(), 5000);
    </script>
</body>
</html>"#;

    let _: () = socket
        .write_all(response.as_bytes())
        .await
        .map_err(|e| ProviderError::Other(format!("Failed to send response: {}", e)))?;

    Ok((code, state))
}

/// Parse OAuth callback request to extract code and state
fn parse_callback_request(request: &str) -> Result<(String, String), ProviderError> {
    // Find the GET line
    let first_line = request
        .lines()
        .next()
        .ok_or_else(|| ProviderError::AuthenticationFailed("Empty request".to_string()))?;

    // Extract path: GET /callback?code=xxx&state=yyy HTTP/1.1
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() < 2 {
        return Err(ProviderError::AuthenticationFailed(
            "Invalid request format".to_string(),
        ));
    }

    let path = parts[1];
    let query_start = path
        .find('?')
        .ok_or_else(|| ProviderError::AuthenticationFailed("No query parameters".to_string()))?;

    let query = &path[query_start + 1..];

    let mut code = None;
    let mut state = None;

    for param in query.split('&') {
        let mut kv = param.splitn(2, '=');
        let key = kv.next().unwrap_or("");
        let value = kv.next().unwrap_or("");

        match key {
            "code" => {
                code = Some(
                    urlencoding::decode(value)
                        .map_err(|e| {
                            ProviderError::AuthenticationFailed(format!(
                                "Invalid URL encoding in code: {}",
                                e
                            ))
                        })?
                        .to_string(),
                )
            }
            "state" => {
                state = Some(
                    urlencoding::decode(value)
                        .map_err(|e| {
                            ProviderError::AuthenticationFailed(format!(
                                "Invalid URL encoding in state: {}",
                                e
                            ))
                        })?
                        .to_string(),
                )
            }
            "error" => {
                return Err(ProviderError::AuthenticationFailed(format!(
                    "OAuth error: {}",
                    value
                )))
            }
            _ => {}
        }
    }

    let code =
        code.ok_or_else(|| ProviderError::AuthenticationFailed("Missing code".to_string()))?;
    let state =
        state.ok_or_else(|| ProviderError::AuthenticationFailed("Missing state".to_string()))?;

    Ok((code, state))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A token request (exchange or refresh) to a server that accepts the
    /// connection and never answers. The clock is paused, so a limit elapses
    /// at once and the outer bound only fires when the request has none.
    #[tokio::test(start_paused = true)]
    async fn a_token_request_the_server_never_answers_ends() {
        use oauth2::AsyncHttpClient;
        let request = http::Request::builder()
            .method("POST")
            .uri(crate::providers::mute_http_endpoint())
            .body(b"grant_type=refresh_token".to_vec())
            .expect("request");
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(3600),
            OAuth2HttpClient.call(request),
        )
        .await
        .expect("the token request is still waiting for the server after an hour");
        let error = outcome.expect_err("no answer").to_string();
        assert!(error.contains("did not answer within 30 s"), "{error}");
    }

    #[test]
    fn test_parse_callback_request() {
        let request = "GET /callback?code=abc123&state=xyz789 HTTP/1.1\r\nHost: localhost\r\n";
        let (code, state) = parse_callback_request(request).unwrap();
        assert_eq!(code, "abc123");
        assert_eq!(state, "xyz789");
    }

    #[test]
    fn test_oauth_config_google() {
        let config = OAuthConfig::google("client_id", "client_secret");
        assert_eq!(config.provider, OAuthProvider::Google);
        assert!(!config.scopes.is_empty());
    }

    #[test]
    #[ignore = "process-global vault/session/env; run alone with --ignored --test-threads=1"]
    fn legacy_singleton_fails_closed_for_locked_legacy_owner() {
        use crate::credential_store::CredentialStore;
        use crate::user_partitions as partitions;

        let _env = crate::test_env::lock();
        let _partition = partitions::test_lock();
        let previous_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let previous_passphrase = std::env::var_os("AEROFTP_USER_PASSPHRASE");
        let temporary = tempfile::TempDir::new().expect("temporary config");
        std::env::set_var("XDG_CONFIG_HOME", temporary.path());
        std::env::remove_var("AEROFTP_USER_PASSPHRASE");
        CredentialStore::clear_cache();
        CredentialStore::bootstrap_master_password("oauth-owner-test-pass")
            .expect("bootstrap vault");
        CredentialStore::unlock_with_master("oauth-owner-test-pass", None).expect("unlock vault");
        let store = CredentialStore::from_cache().expect("cached vault");
        partitions::init_or_migrate_cli(&store).expect("init partitions");
        let mut conn = partitions::open_or_init_cli().expect("partition DB");
        let root_key = store.derive_user_partition_wrapping_key();
        let owner = partitions::get_active_user(&conn)
            .expect("active user")
            .expect("legacy owner");
        partitions::change_user_passphrase(
            &conn,
            &root_key,
            owner.id,
            None,
            Some("legacy-owner-pass"),
        )
        .expect("protect legacy owner");

        let manager = OAuth2Manager::new();
        let vault_json = serde_json::to_string(&StoredTokens {
            access_token: "locked-vault-decoy".into(),
            refresh_token: None,
            expires_at: None,
            token_type: "Bearer".into(),
            scopes: vec![],
        })
        .expect("vault decoy");
        store
            .store("oauth_dropbox", &vault_json)
            .expect("seed vault decoy");
        *MEMORY_TOKEN_CACHE.lock().expect("memory cache") = None;
        partitions::cli_lock_session();

        let error = manager
            .load_tokens(OAuthProvider::Dropbox, "")
            .expect_err("locked owner must not read the global vault");
        assert!(format!("{error:?}").contains("USER_LOCKED"), "{error:?}");
        assert!(!format!("{error:?}").contains("locked-vault-decoy"));

        store.delete("oauth_dropbox").expect("remove vault decoy");
        let memory_json = serde_json::to_string(&StoredTokens {
            access_token: "locked-memory-decoy".into(),
            refresh_token: None,
            expires_at: None,
            token_type: "Bearer".into(),
            scopes: vec![],
        })
        .expect("memory decoy");
        *MEMORY_TOKEN_CACHE.lock().expect("memory cache") =
            Some(HashMap::from([("oauth_dropbox".to_string(), memory_json)]));
        let error = manager
            .load_tokens(OAuthProvider::Dropbox, "")
            .expect_err("locked owner must not read the memory cache");
        assert!(format!("{error:?}").contains("USER_LOCKED"), "{error:?}");
        assert!(!format!("{error:?}").contains("locked-memory-decoy"));
        assert!(MEMORY_TOKEN_CACHE
            .lock()
            .expect("memory cache")
            .as_ref()
            .is_some_and(|map| map.contains_key("oauth_dropbox")));

        *MEMORY_TOKEN_CACHE.lock().expect("memory cache") = None;
        let legacy_path = OAuth2Manager::token_dir()
            .expect("token directory")
            .join("oauth2_dropbox.json");
        std::fs::write(&legacy_path, &vault_json).expect("seed plaintext decoy");
        let error = manager
            .load_tokens(OAuthProvider::Dropbox, "")
            .expect_err("locked owner must not read the plaintext fallback");
        assert!(format!("{error:?}").contains("USER_LOCKED"), "{error:?}");
        assert!(!format!("{error:?}").contains("locked-vault-decoy"));
        assert!(
            legacy_path.exists(),
            "locked read must not migrate or delete"
        );

        // The lock alone refuses even while the vault is closed, before the
        // memory or plaintext fallbacks run.
        CredentialStore::clear_cache();
        let error = manager
            .load_tokens(OAuthProvider::Dropbox, "")
            .expect_err("locked owner must not read plaintext with the vault closed");
        assert!(format!("{error:?}").contains("USER_LOCKED"), "{error:?}");
        assert!(legacy_path.exists(), "closed-vault read must not migrate");
        CredentialStore::unlock_with_master("oauth-owner-test-pass", None).expect("reopen vault");

        partitions::unlock_user(&conn, &root_key, owner.id, Some("legacy-owner-pass"))
            .expect("unlock legacy owner");
        store
            .store("oauth_dropbox", &vault_json)
            .expect("restore positive control");
        let unlocked = manager
            .load_tokens(OAuthProvider::Dropbox, "")
            .expect("unlocked legacy owner retains migration path");
        assert_eq!(unlocked.access_token, "locked-vault-decoy");

        let nonowner = partitions::create_user(&mut conn, &root_key, "Nonowner", None, None, None)
            .expect("create nonowner");
        partitions::set_active_user(&conn, nonowner.id).expect("activate nonowner");
        let error = manager
            .load_tokens(OAuthProvider::Dropbox, "")
            .expect_err("nonowner must not inherit the legacy singleton");
        assert!(!format!("{error:?}").contains("locked-vault-decoy"));

        // A non-owner singleton login lives in its own partition only: the
        // legacy owner's global token and plaintext file survive store/delete.
        let global_access = || {
            serde_json::from_str::<StoredTokens>(&store.get("oauth_dropbox").expect("owner token"))
                .expect("owner token json")
                .access_token
                .clone()
        };
        let nonowner_tokens = StoredTokens {
            access_token: "nonowner-token".into(),
            refresh_token: None,
            expires_at: None,
            token_type: "Bearer".into(),
            scopes: vec![],
        };
        manager
            .store_tokens(OAuthProvider::Dropbox, "", &nonowner_tokens)
            .expect("nonowner stores its own singleton");
        assert_eq!(global_access(), "locked-vault-decoy");
        assert_eq!(
            manager
                .load_tokens(OAuthProvider::Dropbox, "")
                .expect("nonowner reads its own singleton")
                .access_token,
            "nonowner-token"
        );
        manager
            .delete_tokens(OAuthProvider::Dropbox, "")
            .expect("nonowner logout");
        assert!(manager.load_tokens(OAuthProvider::Dropbox, "").is_err());
        assert_eq!(global_access(), "locked-vault-decoy");
        assert!(legacy_path.exists(), "nonowner logout keeps owner files");

        // Profile-bound tokens of a non-owner, written at runtime or by an import,
        // stay in its partition too, so the legacy owner cannot read them back
        // through its vault fallback when both carry the same profile id.
        manager
            .store_tokens(OAuthProvider::Dropbox, "shared-profile", &nonowner_tokens)
            .expect("nonowner stores a profile-bound token");
        let imported = serde_json::to_string(&nonowner_tokens).expect("import json");
        partitions::store_credential_for_user_typed_dual(
            &store,
            nonowner.id,
            "oauth_dropbox_imported-profile",
            "oauth",
            &imported,
        )
        .expect("import writes the nonowner token");
        partitions::store_active_credential_typed_dual(
            &store,
            "jottacloud_refresh_imported-profile",
            "jottacloud_refresh",
            &imported,
        )
        .expect("import writes the nonowner Jottacloud chain");
        for key in [
            "oauth_dropbox_shared-profile",
            "oauth_dropbox_imported-profile",
            "jottacloud_refresh_imported-profile",
        ] {
            assert!(
                store.get(key).is_err(),
                "{key} must not reach the shared vault"
            );
        }
        assert_eq!(
            manager
                .load_tokens(OAuthProvider::Dropbox, "imported-profile")
                .expect("nonowner reads its imported token")
                .access_token,
            "nonowner-token"
        );

        partitions::set_active_user(&conn, owner.id).expect("reactivate owner");
        partitions::unlock_user(&conn, &root_key, owner.id, Some("legacy-owner-pass"))
            .expect("unlock owner after switch");
        assert_eq!(
            manager
                .load_tokens(OAuthProvider::Dropbox, "")
                .expect("owner keeps its singleton")
                .access_token,
            "locked-vault-decoy"
        );
        // The owner may still lazily rebind its own legacy singleton; it must
        // never receive the other user's token for the shared profile id.
        let owner_view = manager.load_tokens(OAuthProvider::Dropbox, "shared-profile");
        assert!(
            !format!("{owner_view:?}").contains("nonowner-token"),
            "{owner_view:?}"
        );

        *MEMORY_TOKEN_CACHE.lock().expect("memory cache") = None;
        CredentialStore::clear_cache();
        match previous_xdg {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        match previous_passphrase {
            Some(value) => std::env::set_var("AEROFTP_USER_PASSPHRASE", value),
            None => std::env::remove_var("AEROFTP_USER_PASSPHRASE"),
        }
    }

    /// Issue #214: an empty `profile_id` keeps the legacy singleton vault key,
    /// preserving historic behaviour for callers that have not yet been wired
    /// through the per-profile flow.
    #[test]
    fn test_token_account_key_legacy_when_profile_id_empty() {
        let key = OAuth2Manager::token_account_key(OAuthProvider::Google, "");
        assert_eq!(key, "oauth_google");
        assert_eq!(
            key,
            OAuth2Manager::legacy_token_account_key(OAuthProvider::Google)
        );
    }

    /// Issue #214: a non-empty `profile_id` switches to the per-profile vault
    /// key so two saved profiles for the same provider (work + personal
    /// Google Drive) can coexist on the same device.
    #[test]
    fn test_token_account_key_per_profile_when_profile_id_present() {
        let work = OAuth2Manager::token_account_key(OAuthProvider::Google, "abc-123");
        let personal = OAuth2Manager::token_account_key(OAuthProvider::Google, "def-456");
        assert_eq!(work, "oauth_google_abc-123");
        assert_eq!(personal, "oauth_google_def-456");
        assert_ne!(work, personal);
    }

    /// Issue #214: `with_profile_id` is the public surface for binding a
    /// configuration to a profile id; the value flows through to the vault
    /// key composer.
    #[test]
    fn test_with_profile_id_threaded_into_account_key() {
        let config = OAuthConfig::google("cid", "csec").with_profile_id("server-7f3a");
        assert_eq!(config.profile_id(), "server-7f3a");
        let key = OAuth2Manager::token_account_key(config.provider, config.profile_id());
        assert_eq!(key, "oauth_google_server-7f3a");
    }

    #[tokio::test]
    async fn refresh_lease_raii_and_stale_steal() {
        // F5 #1: the cross-process lease must be exclusive while held,
        // self-clean on drop, and steal a stale (crashed-holder) lock.
        let slug = format!("test_lease_{}", std::process::id());
        let dir = OAuth2Manager::token_dir().expect("token dir");
        let lock_path = dir.join(format!("refresh-{}.lock", slug));
        let _ = std::fs::remove_file(&lock_path);

        // Fresh acquire wins and creates the lock file.
        let lease = RefreshLease::acquire(&slug).await.expect("first acquire");
        assert!(lock_path.exists(), "lock file must exist while held");

        // Drop releases and removes the file.
        drop(lease);
        assert!(
            !lock_path.exists(),
            "lock file must be removed on lease drop"
        );

        // A stale lock (older than STALE_SECS) gets stolen, not deadlocked.
        std::fs::write(&lock_path, b"99999 0").unwrap();
        let stale = std::time::SystemTime::now()
            - std::time::Duration::from_secs(RefreshLease::STALE_SECS + 5);
        filetime::set_file_mtime(&lock_path, filetime::FileTime::from_system_time(stale)).unwrap();
        let stolen = RefreshLease::acquire(&slug)
            .await
            .expect("stale lock must be stolen");
        assert!(lock_path.exists());
        drop(stolen);
        assert!(!lock_path.exists());
    }

    // ---- DAG-P1-05B: shared refresh ownership across transfer clones ----

    #[test]
    fn oauth_manager_clone_shares_refresh_guard_and_pending_verifiers() {
        let primary = OAuth2Manager::new();
        let worker = primary.clone();
        assert!(
            Arc::ptr_eq(&primary.refresh_guard, &worker.refresh_guard),
            "clone must share the in-process refresh mutex"
        );
        assert!(
            Arc::ptr_eq(&primary.pending_verifiers, &worker.pending_verifiers),
            "clone must share pending PKCE verifiers"
        );
        // Distinct manager instances stay independent.
        let other = OAuth2Manager::new();
        assert!(!Arc::ptr_eq(&primary.refresh_guard, &other.refresh_guard));
    }

    #[tokio::test]
    async fn concurrent_clones_enter_at_most_one_refresh_critical_section() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc as StdArc;
        use std::time::Duration;

        let primary = OAuth2Manager::new();
        let clones: Vec<OAuth2Manager> = (0..8).map(|_| primary.clone()).collect();
        let in_flight = StdArc::new(AtomicUsize::new(0));
        let peak = StdArc::new(AtomicUsize::new(0));
        let mut set = tokio::task::JoinSet::new();
        for m in clones {
            let in_flight = StdArc::clone(&in_flight);
            let peak = StdArc::clone(&peak);
            set.spawn(async move {
                let _guard = m.refresh_guard.lock().await;
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                // Hold long enough that concurrent tasks would overlap if the
                // mutex were not shared / exclusive.
                tokio::time::sleep(Duration::from_millis(40)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
            });
        }
        while set.join_next().await.is_some() {}
        assert_eq!(
            peak.load(Ordering::SeqCst),
            1,
            "shared refresh_guard must admit only one critical section at a time"
        );
    }

    /// Runs in a re-executed copy of the test binary with its own config, vault
    /// and partition DB, as `native_relocation_tests` does, so the developer's
    /// real vault and active user are never read or written.
    #[test]
    fn clones_re_read_shared_profile_token_source() {
        const MODE: &str = "AEROFTP_OAUTH_CLONES_TEST";
        const TEST: &str = "providers::oauth2::tests::clones_re_read_shared_profile_token_source";
        if std::env::var(MODE).as_deref() == Ok("child") {
            crate::credential_store::CredentialStore::init().expect("temporary vault");
            let store =
                crate::credential_store::CredentialStore::from_cache().expect("cached vault");
            crate::user_partitions::init_or_migrate_cli(&store).expect("partitions");
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(clones_re_read_shared_profile_token_source_body());
            return;
        }
        let executable = std::env::current_exe().expect("test binary");
        let root = tempfile::Builder::new()
            .prefix("oauth-clones-")
            .tempdir_in(executable.parent().expect("binary directory"))
            .expect("temporary root");
        let child = root
            .path()
            .join(format!("oauth-clones-test{}", std::env::consts::EXE_SUFFIX));
        std::fs::hard_link(&executable, &child)
            .or_else(|_| std::fs::copy(&executable, &child).map(|_| ()))
            .expect("child binary");
        std::fs::write(root.path().join("portable.marker"), b"").expect("portable marker");
        let empty_path = root.path().join("no-external-tools");
        std::fs::create_dir(&empty_path).expect("empty PATH");
        let output = std::process::Command::new(child)
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(MODE, "child")
            .env("PATH", empty_path)
            .env(
                "AEROFTP_MASTER_PASSWORD",
                "synthetic oauth clones fixture master",
            )
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("XDG_DATA_HOME", root.path().join("data"))
            .env("XDG_CACHE_HOME", root.path().join("cache"))
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent-oauth-clones",
            )
            .output()
            .expect("run isolated child");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    async fn clones_re_read_shared_profile_token_source_body() {
        use secrecy::ExposeSecret;

        let primary = OAuth2Manager::new();
        let profile = format!("p1-05b-oauth-{}", std::process::id());
        // Clean any leftover from a previous run of this process.
        let _ = primary.delete_tokens(OAuthProvider::Dropbox, &profile);

        let tokens = StoredTokens {
            access_token: "winner-access-token".to_string(),
            refresh_token: Some("winner-refresh-token".to_string()),
            // Far future so get_valid_token does not attempt a network refresh.
            expires_at: Some(chrono::Utc::now().timestamp() + 3600),
            token_type: "Bearer".to_string(),
            scopes: vec![],
        };
        primary
            .store_tokens(OAuthProvider::Dropbox, &profile, &tokens)
            .expect("store");

        let config = OAuthConfig::dropbox("app-key", "app-secret").with_profile_id(&profile);
        let workers: Vec<OAuth2Manager> = (0..4).map(|_| primary.clone()).collect();
        let mut set = tokio::task::JoinSet::new();
        for w in workers {
            let cfg = config.clone();
            set.spawn(async move {
                w.get_valid_token(&cfg)
                    .await
                    .map(|s| s.expose_secret().to_string())
                    .map_err(|e| e.to_string())
            });
        }
        let mut got = Vec::new();
        while let Some(res) = set.join_next().await {
            got.push(res.expect("join").expect("token"));
        }
        assert_eq!(got.len(), 4);
        assert!(
            got.iter().all(|t| t == "winner-access-token"),
            "all clones must re-read the same profile-scoped winning token"
        );

        // Distinct profile IDs stay independent (serialization keys differ).
        let other_profile = format!("{profile}-other");
        let other_tokens = StoredTokens {
            access_token: "other-access".to_string(),
            refresh_token: None,
            expires_at: Some(chrono::Utc::now().timestamp() + 3600),
            token_type: "Bearer".to_string(),
            scopes: vec![],
        };
        primary
            .store_tokens(OAuthProvider::Dropbox, &other_profile, &other_tokens)
            .expect("store other");
        let other_cfg =
            OAuthConfig::dropbox("app-key", "app-secret").with_profile_id(&other_profile);
        let other = primary
            .get_valid_token(&other_cfg)
            .await
            .expect("other token");
        assert_eq!(other.expose_secret(), "other-access");
        let original = primary.get_valid_token(&config).await.expect("original");
        assert_eq!(original.expose_secret(), "winner-access-token");

        let _ = primary.delete_tokens(OAuthProvider::Dropbox, &profile);
        let _ = primary.delete_tokens(OAuthProvider::Dropbox, &other_profile);
    }
}
