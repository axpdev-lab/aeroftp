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
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

/// In-memory token cache for when vault is locked (master mode).
/// Tokens survive the session but are NOT persisted to disk.
static MEMORY_TOKEN_CACHE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
static MEMORY_TOKEN_REVISIONS: [std::sync::atomic::AtomicU64; 8] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 8];

fn legacy_revision(provider: OAuthProvider) -> &'static std::sync::atomic::AtomicU64 {
    let index = match provider {
        OAuthProvider::Google => 0,
        OAuthProvider::GooglePhotos => 1,
        OAuthProvider::Dropbox => 2,
        OAuthProvider::OneDrive => 3,
        OAuthProvider::Box => 4,
        OAuthProvider::PCloud => 5,
        OAuthProvider::ZohoWorkdrive => 6,
        OAuthProvider::YandexDisk => 7,
    };
    &MEMORY_TOKEN_REVISIONS[index]
}

fn invalidate_memory_tokens(
    cache: &mut Option<HashMap<String, String>>,
    revision: &std::sync::atomic::AtomicU64,
    account: &str,
) {
    revision.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    if let Some(map) = cache.as_mut() {
        map.remove(account);
    }
}

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

/// Provenance stays inside the encrypted record. A new authorization mints a
/// family; refresh preserves it. Old clients dropping it fail closed in a bound
/// connection instead of silently substituting a different login.
#[derive(Serialize, Deserialize)]
struct TokenRecord {
    #[serde(flatten)]
    tokens: StoredTokens,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner_family: Option<String>,
    #[serde(skip)]
    source: TokenSource,
    #[serde(skip)]
    credential_owner: Option<i64>,
    #[serde(skip)]
    raw: Zeroizing<String>,
}

#[derive(Default)]
enum TokenSource {
    #[default]
    Unavailable,
    Partition(i64),
    Vault,
    Memory(String),
}

impl TokenRecord {
    fn family(&self) -> Result<String, ProviderError> {
        if let Some(family) = &self.owner_family {
            return Ok(family.clone());
        }
        // Compatibility for a record written before families were present.
        // The identifier is keyed even inside the encrypted record. A memory
        // token has a process key and can only ever receive an epoch journal.
        static MEMORY_KEY: std::sync::OnceLock<Zeroizing<[u8; 32]>> = std::sync::OnceLock::new();
        let key = match crate::credential_store::CredentialStore::from_cache() {
            Some(store) => Zeroizing::new(store.derive_user_partition_wrapping_key()),
            None => MEMORY_KEY
                .get_or_init(|| {
                    let bytes = crate::crypto::random_bytes(32);
                    let mut key = [0; 32];
                    key.copy_from_slice(&bytes);
                    Zeroizing::new(key)
                })
                .clone(),
        };
        let key = crate::user_crypto::secret_key_from_bytes(&key);
        let material = Zeroizing::new(
            serde_json::to_string(&(&self.tokens.access_token, &self.tokens.refresh_token))
                .map_err(|_| token_owner_changed())?,
        );
        crate::user_crypto::metadata_tag(&key, b"oauth-legacy-owner-family-v2", &material)
            .map(|tag| format!("legacy-v2:{tag}"))
            .map_err(|_| token_owner_changed())
    }
}

struct TokenOwner {
    user: Option<i64>,
    family: String,
}

fn token_owner_changed() -> ProviderError {
    ProviderError::TargetChanged
}

/// Own one legacy pathname before reading it. Failure restores without
/// overwriting a newly created original. A conflicting recovery copy remains
/// owner-only rather than deleting an authorization that was not migrated.
struct ClaimedTokenFile {
    original: std::path::PathBuf,
    path: std::path::PathBuf,
}

impl ClaimedTokenFile {
    // Called only while LegacyTokenGuard excludes every cooperating migrator
    // and sign-out. A crash releases that lock but does not run Drop.
    fn recovery_paths(
        original: &std::path::Path,
    ) -> Result<Vec<std::path::PathBuf>, ProviderError> {
        let parent = original.parent().ok_or_else(token_owner_changed)?;
        let stem = original
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(token_owner_changed)?;
        let prefix = format!("{stem}.migration-");
        let mut claims = Vec::new();
        for entry in std::fs::read_dir(parent).map_err(|_| token_owner_changed())? {
            let entry = entry.map_err(|_| token_owner_changed())?;
            let name = entry.file_name();
            let Some(suffix) = name.to_str().and_then(|s| s.strip_prefix(&prefix)) else {
                continue;
            };
            if uuid::Uuid::parse_str(suffix).is_err() {
                continue;
            }
            if !entry
                .file_type()
                .map_err(|_| token_owner_changed())?
                .is_file()
            {
                return Err(token_owner_changed());
            }
            claims.push(entry.path());
        }
        Ok(claims)
    }

    fn recover(original: &std::path::Path) -> Result<(), ProviderError> {
        let claims = Self::recovery_paths(original)?;
        if !original.exists() {
            // Never select arbitrarily between different stranded logins.
            if claims.len() > 1 {
                return Err(token_owner_changed());
            }
            if let Some(path) = claims.first() {
                std::fs::hard_link(path, original).map_err(|_| token_owner_changed())?;
            }
        }
        // A recreated original is the newer authorization; old claims must
        // never resurrect after a later migration or sign-out. Unlink only:
        // a crash during recovery can leave two names for the same inode.
        for path in claims {
            std::fs::remove_file(path).map_err(|_| token_owner_changed())?;
        }
        Ok(())
    }

    fn claim(original: &std::path::Path) -> Result<Self, ProviderError> {
        let path = original.with_extension(format!("migration-{}", uuid::Uuid::new_v4()));
        std::fs::rename(original, &path)
            .map_err(|_| ProviderError::AuthenticationFailed("No stored authorization".into()))?;
        let claimed = Self {
            original: original.to_owned(),
            path,
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&claimed.path, std::fs::Permissions::from_mode(0o600))
                .map_err(|_| {
                    ProviderError::Other("Could not protect legacy authorization".into())
                })?;
        }
        Ok(claimed)
    }

    fn discard(self) -> Result<(), ProviderError> {
        std::fs::remove_file(&self.path)
            .map_err(|_| ProviderError::Other("Could not remove migrated authorization".into()))
    }
}

impl Drop for ClaimedTokenFile {
    fn drop(&mut self) {
        if !self.path.exists() {
            return;
        }
        // hard_link creates only an absent destination, unlike rename on Unix.
        // Keep the protected recovery file when an original was recreated.
        if std::fs::hard_link(&self.path, &self.original).is_ok() {
            let _ = std::fs::remove_file(&self.path);
        } else {
            warn!("Legacy authorization retained in a protected migration recovery file");
        }
    }
}

/// Permanent lock inode shared by legacy migration and sign-out in every
/// desktop/CLI/MCP process. Never unlink it: a replacement inode would allow
/// two independent owners. The OS releases the advisory lock after a crash.
struct LegacyTokenGuard(Option<std::fs::File>);

#[cfg(all(test, unix))]
thread_local! {
    /// Test seam: the sign-out lock on this thread answers ENOLCK, so a test
    /// can drive the whole `delete_tokens` path, vault included, into it.
    static FORCE_SIGNOUT_ENOLCK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Make the sign-out lock on this thread answer ENOLCK (tests only).
#[cfg(all(test, unix))]
pub(crate) fn force_signout_enolck_for_test(on: bool) {
    FORCE_SIGNOUT_ENOLCK.with(|force| force.set(on));
}

/// Why the caller wants the legacy lock. Only the lock-error policy differs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LegacyLockUse {
    Migration,
    Signout,
}

/// What a failed lock attempt proves about other lock holders.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LegacyLockFailure {
    /// The filesystem has no file locks, so no process can hold this one.
    NoLocks,
    /// ENOLCK: the kernel may only be out of lock records while another
    /// process still holds the lock.
    Unproven,
    Other,
}

fn legacy_lock_failure(error: &std::io::Error) -> LegacyLockFailure {
    if error.kind() == std::io::ErrorKind::Unsupported {
        LegacyLockFailure::NoLocks
    } else if crate::user_partitions::file_locks_unavailable(error) {
        LegacyLockFailure::Unproven
    } else {
        LegacyLockFailure::Other
    }
}

impl LegacyTokenGuard {
    fn acquire(legacy_path: &std::path::Path) -> Result<Self, ProviderError> {
        let guard = Self::lock(legacy_path, LegacyLockUse::Migration)?;
        if guard.0.is_some() {
            ClaimedTokenFile::recover(legacy_path)?;
        }
        Ok(guard)
    }

    fn acquire_for_signout(legacy_path: &std::path::Path) -> Result<Self, ProviderError> {
        Self::lock(legacy_path, LegacyLockUse::Signout)
    }

    fn lock(legacy_path: &std::path::Path, use_: LegacyLockUse) -> Result<Self, ProviderError> {
        #[cfg(all(test, unix))]
        if use_ == LegacyLockUse::Signout && FORCE_SIGNOUT_ENOLCK.with(std::cell::Cell::get) {
            return Self::lock_with(legacy_path, use_, |_| {
                Err(std::fs::TryLockError::Error(
                    std::io::Error::from_raw_os_error(libc::ENOLCK),
                ))
            });
        }
        Self::lock_with(legacy_path, use_, |file| file.try_lock())
    }

    /// `try_lock` is a parameter so a test can play a lock error that the
    /// operating system cannot be made to return on demand.
    fn lock_with(
        legacy_path: &std::path::Path,
        use_: LegacyLockUse,
        mut try_lock: impl FnMut(&std::fs::File) -> Result<(), std::fs::TryLockError>,
    ) -> Result<Self, ProviderError> {
        let path = legacy_path.with_extension("migration.lock");
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(false).write(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = options.open(path).map_err(|e| {
            ProviderError::Other(format!(
                "Legacy authorization lock unavailable: {}",
                e.kind()
            ))
        })?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match try_lock(&file) {
                Ok(()) => {
                    return Ok(Self(Some(file)));
                }
                Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(25))
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(ProviderError::Other(
                        "Legacy authorization is busy; try again".into(),
                    ))
                }
                Err(std::fs::TryLockError::Error(error)) => {
                    let unlocked = match (legacy_lock_failure(&error), use_) {
                        // A filesystem without locks: no process can hold this
                        // lock, and `with_legacy_claim_guard` refuses to migrate
                        // with an unlocked guard, so no migrator can exist for a
                        // sign-out to race with.
                        (LegacyLockFailure::NoLocks, _) => true,
                        // An unlocked guard only disables migration.
                        (LegacyLockFailure::Unproven, LegacyLockUse::Migration) => true,
                        // ENOLCK says this process got no lock record, not that
                        // nobody holds the lock. What is on disk proves nothing
                        // either: a migrator's recovery moves its claim back to
                        // the legacy name between any two looks. Sign-out waits
                        // for a lock it can prove.
                        (LegacyLockFailure::Unproven, LegacyLockUse::Signout) => {
                            return Err(ProviderError::Other(
                                "Legacy authorization lock unavailable (no lock record left); nothing was signed out, try again".into(),
                            ))
                        }
                        (LegacyLockFailure::Other, _) => false,
                    };
                    if unlocked {
                        return Ok(Self(None));
                    }
                    return Err(ProviderError::Other(format!(
                        "Legacy authorization lock unavailable: {}",
                        error.kind()
                    )));
                }
            }
        }
    }
}

impl Drop for LegacyTokenGuard {
    fn drop(&mut self) {
        if let Some(file) = &self.0 {
            let _ = file.unlock();
        }
    }
}

// All callers use file -> cache -> vault order. The callback owns the claim,
// so its returns/errors finalize it before either guard is released.
fn with_legacy_token_claim<T>(
    cache: &Mutex<Option<HashMap<String, String>>>,
    path: &std::path::Path,
    migrate: impl FnOnce(
        &mut Option<HashMap<String, String>>,
        ClaimedTokenFile,
    ) -> Result<T, ProviderError>,
) -> Result<T, ProviderError> {
    let process = LegacyTokenGuard::acquire(path)?;
    with_legacy_claim_guard(cache, path, process, migrate)
}

fn with_legacy_claim_guard<T>(
    cache: &Mutex<Option<HashMap<String, String>>>,
    path: &std::path::Path,
    process: LegacyTokenGuard,
    migrate: impl FnOnce(
        &mut Option<HashMap<String, String>>,
        ClaimedTokenFile,
    ) -> Result<T, ProviderError>,
) -> Result<T, ProviderError> {
    if process.0.is_none() {
        return Err(ProviderError::AuthenticationFailed(
            "Legacy authorization cannot be migrated on a filesystem without locks; sign in again"
                .into(),
        ));
    }
    let mut cache = cache.lock().map_err(|_| token_owner_changed())?;
    let claimed = ClaimedTokenFile::claim(path)?;
    migrate(&mut cache, claimed)
}

// A token miss with no legacy file and no claim is answered before any lock:
// with nothing to migrate there is nothing to coordinate.
fn with_stored_legacy_token_claim<T>(
    cache: &Mutex<Option<HashMap<String, String>>>,
    path: &std::path::Path,
    migrate: impl FnOnce(
        &mut Option<HashMap<String, String>>,
        ClaimedTokenFile,
    ) -> Result<T, ProviderError>,
) -> Result<T, ProviderError> {
    if !path.exists() && ClaimedTokenFile::recovery_paths(path)?.is_empty() {
        return Err(ProviderError::AuthenticationFailed(
            "No stored authorization".into(),
        ));
    }
    with_legacy_token_claim(cache, path, migrate)
}

// Sign-out purges every valid stranded claim without selecting a login to
// restore. Keep the same file -> cache -> vault order as migration.
#[cfg(test)]
fn with_legacy_signout<T>(
    cache: &Mutex<Option<HashMap<String, String>>>,
    path: &std::path::Path,
    clear: impl FnOnce(&mut Option<HashMap<String, String>>) -> Result<T, ProviderError>,
) -> Result<T, ProviderError> {
    let process = LegacyTokenGuard::acquire_for_signout(path)?;
    with_legacy_signout_guard(cache, path, process, clear)
}

// Only the unix tests can play ENOLCK, so this shares their cfg.
#[cfg(all(test, unix))]
fn with_legacy_signout_with<T>(
    cache: &Mutex<Option<HashMap<String, String>>>,
    path: &std::path::Path,
    try_lock: impl FnMut(&std::fs::File) -> Result<(), std::fs::TryLockError>,
    clear: impl FnOnce(&mut Option<HashMap<String, String>>) -> Result<T, ProviderError>,
) -> Result<T, ProviderError> {
    let process = LegacyTokenGuard::lock_with(path, LegacyLockUse::Signout, try_lock)?;
    with_legacy_signout_guard(cache, path, process, clear)
}

fn with_legacy_signout_guard<T>(
    cache: &Mutex<Option<HashMap<String, String>>>,
    path: &std::path::Path,
    _process: LegacyTokenGuard,
    clear: impl FnOnce(&mut Option<HashMap<String, String>>) -> Result<T, ProviderError>,
) -> Result<T, ProviderError> {
    let mut cache = cache.lock().map_err(|_| token_owner_changed())?;
    for claim in ClaimedTokenFile::recovery_paths(path)? {
        // A claim holds the plaintext legacy authorization, so overwrite it
        // before the unlink. A crash during recovery can leave the claim and
        // the legacy file as two names of one inode: the overwrite then
        // reaches that file too, which this sign-out is removing anyway.
        crate::credential_store::secure_delete(&claim).map_err(|_| {
            ProviderError::Other("Could not remove legacy authorization recovery file".into())
        })?;
    }
    clear(&mut cache)
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
    #[cfg(any(test, feature = "proton-native"))]
    pub(crate) async fn acquire(slug: &str) -> Option<Self> {
        Self::acquire_checked(slug).await.ok().flatten()
    }

    async fn acquire_checked(slug: &str) -> Result<Option<Self>, ProviderError> {
        let dir = OAuth2Manager::token_dir().map_err(|_| {
            ProviderError::Other("Authorization refresh lease directory is unavailable".into())
        })?;
        Self::acquire_in(&dir, slug).await
    }

    async fn acquire_in(dir: &std::path::Path, slug: &str) -> Result<Option<Self>, ProviderError> {
        // A profile id can carry a path separator (an imported remote name).
        // Only the lock file name is made safe; token keys are unchanged. Two
        // ids that share a safe name only serialize their refreshes.
        let safe_slug: String = slug
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let path = dir.join(format!("refresh-{}.lock", safe_slug));
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
                    return Ok(Some(Self { path }));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Steal a stale lease (previous holder crashed).
                    if let Ok(modified) = std::fs::metadata(&path).and_then(|m| m.modified()) {
                        if modified
                            .elapsed()
                            .map(|d| d.as_secs() >= Self::STALE_SECS)
                            .unwrap_or(true)
                        {
                            match std::fs::remove_file(&path) {
                                Ok(()) => continue,
                                // Another waiter stole it first.
                                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                                // A stale lease that cannot be removed would
                                // otherwise be retried here without a bound.
                                Err(e) => {
                                    return Err(ProviderError::Other(format!(
                                        "Authorization refresh lease storage unavailable: {}",
                                        e.kind()
                                    )))
                                }
                            }
                        }
                    }
                    if started.elapsed().as_millis() as u64 >= Self::MAX_WAIT_MS {
                        return Ok(None);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(Self::POLL_MS)).await;
                }
                Err(error) => {
                    return Err(ProviderError::Other(format!(
                        "Authorization refresh lease storage unavailable: {}",
                        error.kind()
                    )))
                }
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
    /// Sealed on the first token used by connect, shared with transfer clones.
    /// Key by profile, not user: changing active user must not create a new seal.
    token_owners: Arc<Mutex<HashMap<String, TokenOwner>>>,
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
            token_owners: Arc::clone(&self.token_owners),
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
            token_owners: Arc::new(Mutex::new(HashMap::new())),
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
        let account = Self::token_account_key(config.provider, &config.profile_id);
        let observed = self.load_token_record(config.provider, &config.profile_id)?;
        self.verify_token_owner(&account, &observed)?;
        use subtle::ConstantTimeEq;
        if !observed
            .tokens
            .refresh_token
            .as_ref()
            .is_some_and(|stored| bool::from(stored.as_bytes().ct_eq(refresh_token.as_bytes())))
        {
            return Err(token_owner_changed());
        }
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

        // Update keyring under the profile-bound key, off the async worker:
        // another process may be writing the vault at the same moment.
        crate::credential_store::off_async_worker(|| {
            self.persist_refreshed_record(&account, &observed, &tokens)
        })?;

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
        let account = Self::token_account_key(config.provider, &config.profile_id);
        let mut record = self.load_token_record(config.provider, &config.profile_id)?;
        self.verify_token_owner(&account, &record)?;
        let mut tokens = record.tokens.clone();

        if tokens.is_expired() {
            // Per-profile lease: two profiles pointing at distinct accounts of
            // the same provider must not block each other's refresh.
            let slug = Self::token_account_key(config.provider, &config.profile_id);
            // Bounded, self-healing cross-process lease. `None` => another
            // process is the refresh owner: fall through to
            // the re-load below and use whatever it persisted.
            let _lease = RefreshLease::acquire_checked(&slug).await?;
            // Double-check: the lease winner (possibly the other process)
            // may have already rotated+persisted a fresh token.
            record = self.load_token_record(config.provider, &config.profile_id)?;
            self.verify_token_owner(&account, &record)?;
            tokens = record.tokens.clone();
            if tokens.is_expired() {
                if _lease.is_none() {
                    return Err(ProviderError::Other(
                        "Authorization refresh is busy; try again".into(),
                    ));
                }
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
        let record = TokenRecord {
            tokens: tokens.clone(),
            owner_family: Some(format!("v1:{}", uuid::Uuid::new_v4())),
            source: TokenSource::Unavailable,
            credential_owner: self.credential_user,
            raw: Zeroizing::new(String::new()),
        };
        let json = Zeroizing::new(
            serde_json::to_string_pretty(&record)
                .map_err(|_| ProviderError::Other("Failed to serialize tokens".into()))?,
        );

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
            map.insert(account, json.to_string());
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
        self.load_token_record(provider, profile_id)
            .map(|record| record.tokens)
    }

    fn decode_record(
        json: &str,
        source: TokenSource,
        credential_owner: Option<i64>,
    ) -> Result<TokenRecord, ProviderError> {
        let mut record: TokenRecord = serde_json::from_str(json)
            .map_err(|_| ProviderError::Other("Failed to parse tokens".into()))?;
        record.raw = Zeroizing::new(json.to_owned());
        record.source = source;
        record.credential_owner = credential_owner;
        Ok(record)
    }

    pub(crate) fn sync_owner_identity(
        &self,
        config: &OAuthConfig,
    ) -> Result<Zeroizing<String>, ProviderError> {
        let account = Self::token_account_key(config.provider, &config.profile_id);
        if !self
            .token_owners
            .lock()
            .map_err(|_| token_owner_changed())?
            .contains_key(&account)
        {
            return Err(token_owner_changed());
        }
        let record = self.load_token_record(config.provider, &config.profile_id)?;
        self.verify_token_owner(&account, &record)?;
        serde_json::to_string(&(record.credential_owner, record.family()?, config))
            .map(Zeroizing::new)
            .map_err(|_| token_owner_changed())
    }

    fn verify_token_owner(&self, account: &str, record: &TokenRecord) -> Result<(), ProviderError> {
        use subtle::ConstantTimeEq;
        let family = record.family()?;
        let mut owners = self
            .token_owners
            .lock()
            .map_err(|_| token_owner_changed())?;
        if let Some(owner) = owners.get(account) {
            if owner.user != record.credential_owner
                || !bool::from(owner.family.as_bytes().ct_eq(family.as_bytes()))
            {
                return Err(token_owner_changed());
            }
        } else {
            owners.insert(
                account.to_owned(),
                TokenOwner {
                    user: record.credential_owner,
                    family,
                },
            );
        }
        Ok(())
    }

    /// Save only to the exact source loaded before the network exchange.
    /// No mirror: the partition is authoritative, and mirroring a stale refresh
    /// could overwrite a fresh login made concurrently in the other store.
    fn persist_refreshed_record(
        &self,
        account: &str,
        observed: &TokenRecord,
        tokens: &StoredTokens,
    ) -> Result<(), ProviderError> {
        let current_owner =
            crate::user_partitions::credential_owner_and_fallback(self.credential_user)
                .map_err(|_| token_owner_changed())?
                .0;
        if current_owner != observed.credential_owner {
            return Err(token_owner_changed());
        }
        if let Some(user) = current_owner {
            crate::user_partitions::ensure_credential_user_unlocked(user)
                .map_err(|_| token_owner_changed())?;
        }
        let replacement = TokenRecord {
            tokens: tokens.clone(),
            owner_family: Some(observed.family()?),
            source: TokenSource::Unavailable,
            credential_owner: current_owner,
            raw: Zeroizing::new(String::new()),
        };
        let json = Zeroizing::new(
            serde_json::to_string_pretty(&replacement)
                .map_err(|_| ProviderError::Other("Failed to serialize tokens".into()))?,
        );
        let unchanged = match &observed.source {
            TokenSource::Partition(user) => {
                let store = crate::credential_store::CredentialStore::from_cache()
                    .ok_or_else(token_owner_changed)?;
                let conn = crate::user_partitions::open_or_init_cli()
                    .map_err(|_| token_owner_changed())?;
                let key = Zeroizing::new(store.derive_user_partition_wrapping_key());
                crate::user_partitions::replace_user_credential_if_unchanged(
                    &conn,
                    &key,
                    *user,
                    account,
                    "oauth",
                    (&observed.raw, &json),
                    self.credential_user.is_none(),
                )
                .map_err(|_| token_owner_changed())?
            }
            TokenSource::Vault | TokenSource::Memory(_) => {
                let update = || -> Result<bool, String> {
                    match &observed.source {
                        TokenSource::Vault => {
                            let store = crate::credential_store::CredentialStore::from_cache()
                                .ok_or_else(|| "Authorization store closed".to_owned())?;
                            store
                                .replace_secret_if_unchanged(account, &observed.raw, &json)
                                .map_err(|_| "Authorization changed".to_owned())
                        }
                        TokenSource::Memory(key) => {
                            let mut cache = MEMORY_TOKEN_CACHE
                                .lock()
                                .map_err(|_| "Authorization changed".to_owned())?;
                            let Some(map) = cache.as_mut() else {
                                return Ok(false);
                            };
                            if !map.get(key).is_some_and(|raw| raw == observed.raw.as_str()) {
                                return Ok(false);
                            }
                            map.insert(key.clone(), json.to_string());
                            Ok(true)
                        }
                        _ => Ok(false),
                    }
                };
                if self.credential_user.is_some() {
                    update().map_err(|_| token_owner_changed())?
                } else {
                    let store = crate::credential_store::CredentialStore::from_cache();
                    crate::user_partitions::with_unchanged_active_credential_user(
                        store.as_ref(),
                        observed.credential_owner,
                        update,
                    )
                    .map_err(|_| token_owner_changed())?
                }
            }
            TokenSource::Unavailable => false,
        };
        if unchanged {
            Ok(())
        } else {
            Err(token_owner_changed())
        }
    }

    fn load_token_record(
        &self,
        provider: OAuthProvider,
        profile_id: &str,
    ) -> Result<TokenRecord, ProviderError> {
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
            crate::user_partitions::init_or_migrate_cli(&store)
                .map_err(|_| ProviderError::Other("Failed to initialize token owner".into()))?;
            let conn = crate::user_partitions::open_or_init_cli()
                .map_err(|_| ProviderError::Other("Failed to open token owner".into()))?;
            let key = Zeroizing::new(store.derive_user_partition_wrapping_key());
            let resolved =
                crate::user_partitions::get_user_credential_for(&conn, &key, user, &account)
                    .map_err(|e| {
                        ProviderError::Other(format!("Failed to read user tokens: {e}"))
                    })?;
            if let Some(json) = resolved {
                return Self::decode_record(&json, TokenSource::Partition(user), credential_user);
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
                return Self::decode_record(&json, TokenSource::Vault, credential_user);
            }

            // Lazy migration: per-profile key missing, legacy hit ➜ rebind + remove.
            if !profile_id.is_empty() {
                if let Ok(json) = store.get(&legacy) {
                    Self::decode_record(&json, TokenSource::Unavailable, credential_user)?;
                    let moved = store
                        .move_secret_if_unchanged(&legacy, &account, &json)
                        .map_err(|_| token_owner_changed())?;
                    // A concurrent authorization wins. Re-read the actual
                    // destination, not the stale singleton we inspected.
                    if moved || store.get_secret(&account).is_ok() {
                        let current = store
                            .get_secret(&account)
                            .map_err(|_| token_owner_changed())?;
                        return Self::decode_record(&current, TokenSource::Vault, credential_user);
                    }
                    return Err(token_owner_changed());
                }
            }
        }

        // Fallback: try in-memory cache (mirror of the vault layout above).
        if let Ok(cache) = MEMORY_TOKEN_CACHE.lock() {
            if let Some(map) = cache.as_ref() {
                if let Some(json) = map.get(&account) {
                    return Self::decode_record(
                        json,
                        TokenSource::Memory(account.clone()),
                        credential_user,
                    );
                }
                if !profile_id.is_empty() {
                    if let Some(json) = map.get(&legacy) {
                        return Self::decode_record(
                            json,
                            TokenSource::Memory(legacy.clone()),
                            credential_user,
                        );
                    }
                }
            }
        }

        let legacy_path =
            Self::token_dir()?.join(format!("oauth2_{:?}.json", provider).to_lowercase());
        with_stored_legacy_token_claim(&MEMORY_TOKEN_CACHE, &legacy_path, |cache, claimed| {
            let observed_legacy_revision =
                legacy_revision(provider).load(std::sync::atomic::Ordering::SeqCst);
            let json = Zeroizing::new(std::fs::read_to_string(&claimed.path).map_err(|_| {
                ProviderError::AuthenticationFailed("Could not read legacy authorization".into())
            })?);
            Self::decode_record(&json, TokenSource::Unavailable, credential_user)?;
            if crate::credential_store::CredentialStore::from_cache().is_none() {
                let _ = crate::credential_store::CredentialStore::init();
            }
            let Some(store) = crate::credential_store::CredentialStore::from_cache() else {
                // Establish an exact CAS-writable source before any refresh can
                // consume a rotating token. A concurrent fresh memory login wins.
                // Drop still restores the old file without clobbering a new one.
                if legacy_revision(provider).load(std::sync::atomic::Ordering::SeqCst)
                    != observed_legacy_revision
                {
                    claimed.discard()?;
                    return Err(token_owner_changed());
                }
                let newer_login = cache.as_ref().is_some_and(|map| map.contains_key(&account));
                let result = Self::remember_legacy_in_memory(
                    cache,
                    legacy_revision(provider),
                    observed_legacy_revision,
                    account,
                    &json,
                    credential_user,
                )?;
                if newer_login {
                    claimed.discard()?;
                }
                return Ok(result);
            };
            Self::migrate_claimed_to_vault(&store, &account, &json, credential_user, claimed)
        })
    }

    fn migrate_claimed_to_vault(
        store: &crate::credential_store::CredentialStore,
        account: &str,
        json: &str,
        credential_user: Option<i64>,
        claimed: ClaimedTokenFile,
    ) -> Result<TokenRecord, ProviderError> {
        store
            .store_secret_if_absent(account, json)
            .map_err(|_| token_owner_changed())?;
        let current = store
            .get_secret(account)
            .map_err(|_| token_owner_changed())?;
        let result = Self::decode_record(&current, TokenSource::Vault, credential_user)?;
        // A newer login also consumes this obsolete legacy candidate. Restoring
        // it would let a second profile silently import the old cloud account.
        claimed.discard()?;
        Ok(result)
    }

    fn remember_legacy_in_memory(
        cache: &mut Option<HashMap<String, String>>,
        revision: &std::sync::atomic::AtomicU64,
        observed: u64,
        account: String,
        json: &str,
        user: Option<i64>,
    ) -> Result<TokenRecord, ProviderError> {
        if revision.load(std::sync::atomic::Ordering::SeqCst) != observed {
            return Err(token_owner_changed());
        }
        let current = cache
            .get_or_insert_with(HashMap::new)
            .entry(account.clone())
            .or_insert_with(|| json.to_owned());
        Self::decode_record(current, TokenSource::Memory(account), user)
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

        // MUV-4: drop the per-profile token from the user's partition too,
        // but only once the sign-out holds its lock (below): a sign-out refused
        // for the lock must leave every copy in place. The account is resolved
        // now, before the wait: a switch while the lock is awaited must not
        // move the delete to another account's row with the same profile id.
        let partition_user = if profile_id.is_empty() {
            None
        } else {
            match self.credential_user {
                Some(user) => Some(user),
                None => crate::user_partitions::active_user_for_signout()
                    .map_err(|e| ProviderError::Other(format!("Failed to delete tokens: {e}")))?,
            }
        };
        let unmirror = || -> Result<(), ProviderError> {
            let Some(user) = partition_user else {
                return Ok(());
            };
            // Fallible: a partition copy that could not be removed fails the
            // sign-out before the vault, the cache or the legacy files go.
            crate::user_partitions::try_unmirror_active_credential(user, &account)
                .map_err(|e| ProviderError::Other(format!("Failed to delete tokens: {e}")))
        };

        let json_path =
            Self::token_dir()?.join(format!("oauth2_{:?}.json", provider).to_lowercase());
        Self::delete_shared_tokens(
            provider,
            &account,
            &json_path,
            LegacyTokenGuard::acquire_for_signout,
            unmirror,
        )
    }

    /// The part of [`Self::delete_tokens`] the legacy owner's copies share:
    /// take the sign-out lock first, then remove the partition mirror, the
    /// vault entry, the memory cache and the legacy files, all under it.
    fn delete_shared_tokens(
        provider: OAuthProvider,
        account: &str,
        json_path: &std::path::Path,
        lock: impl FnOnce(&std::path::Path) -> Result<LegacyTokenGuard, ProviderError>,
        unmirror: impl FnOnce() -> Result<(), ProviderError>,
    ) -> Result<(), ProviderError> {
        let process = lock(json_path)?;
        with_legacy_signout_guard(&MEMORY_TOKEN_CACHE, json_path, process, |cache| {
            unmirror()?;
            // Delete from vault
            if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
                store.delete(account).map_err(|_| token_owner_changed())?;
            }
            invalidate_memory_tokens(cache, legacy_revision(provider), account);
            // The legacy files are the ones next to the `json_path` this
            // sign-out holds the lock for, never a path found again here: a
            // second lookup would leave the lock's folder (a test's fixture)
            // for the app's real token folder.
            if json_path.exists() {
                let _ = crate::credential_store::secure_delete(json_path);
            }
            let enc_path = json_path.with_extension("enc");
            if enc_path.exists() {
                let _ = crate::credential_store::secure_delete(&enc_path);
            }
            Ok(())
        })?;

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
    #[test]
    fn legacy_cache_signout_revision_rejects_a_paused_read() {
        use std::sync::{Arc, Barrier};
        let revision = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let cache = Arc::new(Mutex::new(None));
        let observed = revision.load(std::sync::atomic::Ordering::SeqCst);
        let barrier = Arc::new(Barrier::new(2));
        let deletion = {
            let revision = revision.clone();
            let cache = cache.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                invalidate_memory_tokens(&mut cache.lock().unwrap(), &revision, "account");
                barrier.wait();
            })
        };
        barrier.wait();
        let json = serde_json::to_string(&StoredTokens {
            access_token: "old".into(),
            refresh_token: None,
            expires_at: None,
            token_type: "Bearer".into(),
            scopes: vec![],
        })
        .unwrap();
        let mut cache = cache.lock().unwrap();
        assert!(matches!(
            OAuth2Manager::remember_legacy_in_memory(
                &mut cache,
                &revision,
                observed,
                "account".into(),
                &json,
                None
            ),
            Err(ProviderError::TargetChanged)
        ));
        assert!(cache.is_none());
        assert!(OAuth2Manager::remember_legacy_in_memory(
            &mut cache,
            &revision,
            revision.load(std::sync::atomic::Ordering::SeqCst),
            "account".into(),
            &json,
            None
        )
        .is_ok());
        drop(cache);
        deletion.join().unwrap();
    }

    #[test]
    fn legacy_claim_and_signout_coordinate_between_processes() {
        const MODE: &str = "AEROFTP_LEGACY_CLAIM_PROCESS_TEST";
        const TEST: &str =
            "providers::oauth2::tests::legacy_claim_and_signout_coordinate_between_processes";
        fn wait_for(path: &std::path::Path) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
            while !path.exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "barrier timed out: {}",
                    path.display()
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        if let Ok(mode) = std::env::var(MODE) {
            let dir = std::path::PathBuf::from(
                std::env::var_os("AEROFTP_LEGACY_CLAIM_TEST_DIR").unwrap(),
            );
            let path = dir.join("legacy.json");
            if mode == "signout" {
                std::fs::write(dir.join("deleting"), b"").unwrap();
                let _guard = LegacyTokenGuard::acquire(&path).unwrap();
                // The production sign-out holds this same guard through vault,
                // memory and file cleanup. A waiting claimant cannot reappear.
                let _ = std::fs::remove_file(dir.join("destination"));
                let _ = std::fs::remove_file(&path);
                std::fs::write(dir.join("deleted"), b"").unwrap();
            } else {
                with_legacy_token_claim(&Mutex::new(None), &path, |_, claimed| {
                    std::fs::write(dir.join("claimed"), b"").unwrap();
                    wait_for(&dir.join("release"));
                    std::fs::write(dir.join("destination"), b"old authorization").unwrap();
                    if mode == "vault" {
                        claimed.discard()?;
                    } else {
                        drop(claimed);
                    }
                    Ok(())
                })
                .unwrap();
            }
            return;
        }
        for mode in ["memory", "vault"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("legacy.json");
            std::fs::write(&path, b"old authorization").unwrap();
            let spawn = |mode: &str| {
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
                    .env(MODE, mode)
                    .env("AEROFTP_LEGACY_CLAIM_TEST_DIR", dir.path())
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap()
            };
            let mut migration = spawn(mode);
            wait_for(&dir.path().join("claimed"));
            let mut deletion = spawn("signout");
            wait_for(&dir.path().join("deleting"));
            let lock = std::fs::OpenOptions::new()
                .write(true)
                .open(path.with_extension("migration.lock"))
                .unwrap();
            assert!(matches!(
                lock.try_lock(),
                Err(std::fs::TryLockError::WouldBlock)
            ));
            assert!(!dir.path().join("deleted").exists());
            std::fs::write(dir.path().join("release"), b"").unwrap();
            assert!(migration.wait().unwrap().success());
            assert!(deletion.wait().unwrap().success());
            assert!(!path.exists());
            assert!(!dir.path().join("destination").exists());
            assert_eq!(
                std::fs::read_dir(dir.path())
                    .unwrap()
                    .filter_map(Result::ok)
                    .filter(|entry| entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("legacy.migration-"))
                    .count(),
                0
            );
            // Keep the permanent inode, and prove the crashed/exited owners
            // leave no stale lock that could block another migration/sign-out.
            assert!(lock.try_lock().is_ok());
            lock.unlock().unwrap();
        }
    }

    #[test]
    fn legacy_migration_finalizes_before_signout_in_memory_and_vault_paths() {
        use std::sync::{mpsc, Arc, Barrier};
        for vault_available in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let original = dir.path().join("legacy.json");
            std::fs::write(&original, "old authorization").unwrap();
            let cache = Arc::new(Mutex::new(None));
            let vault = Arc::new(Mutex::new(HashMap::<String, String>::new()));
            let claimed_barrier = Arc::new(Barrier::new(2));
            let release = Arc::new(Barrier::new(2));
            let migration = {
                let (cache, vault, original, claimed_barrier, release) = (
                    cache.clone(),
                    vault.clone(),
                    original.clone(),
                    claimed_barrier.clone(),
                    release.clone(),
                );
                std::thread::spawn(move || {
                    with_legacy_token_claim(&cache, &original, |cache, claimed| {
                        claimed_barrier.wait();
                        release.wait();
                        if vault_available {
                            vault
                                .lock()
                                .unwrap()
                                .insert("account".into(), "old authorization".into());
                            claimed.discard()?;
                        } else {
                            cache
                                .get_or_insert_with(HashMap::new)
                                .insert("account".into(), "old authorization".into());
                            // The memory path restores the legacy file for durability.
                            drop(claimed);
                        }
                        Ok(())
                    })
                })
            };
            claimed_barrier.wait();
            // A sign-out cannot complete while a claim is hidden, regardless
            // of which destination the migration will use.
            assert!(cache.try_lock().is_err());
            let (done_tx, done_rx) = mpsc::channel();
            let deletion = {
                let (cache, vault, original) = (cache.clone(), vault.clone(), original.clone());
                std::thread::spawn(move || {
                    let mut cache = cache.lock().unwrap();
                    vault.lock().unwrap().remove("account");
                    *cache = None;
                    let _ = std::fs::remove_file(&original);
                    done_tx.send(()).unwrap();
                })
            };
            assert!(done_rx.try_recv().is_err());
            release.wait();
            migration.join().unwrap().unwrap();
            deletion.join().unwrap();
            done_rx.recv().unwrap();
            assert!(!original.exists());
            assert!(cache.lock().unwrap().is_none());
            assert!(vault.lock().unwrap().is_empty());
            let remaining: Vec<_> = std::fs::read_dir(dir.path())
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            assert_eq!(
                remaining,
                vec![std::ffi::OsString::from("legacy.migration.lock")]
            );
        }
    }

    #[test]
    fn unsupported_file_locks_disable_legacy_migration_without_claiming_a_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.json");
        std::fs::write(&path, "authorization").unwrap();
        let migrated = std::cell::Cell::new(false);
        let result =
            with_legacy_claim_guard(&Mutex::new(None), &path, LegacyTokenGuard(None), |_, _| {
                migrated.set(true);
                Ok(())
            });
        assert!(matches!(
            result,
            Err(ProviderError::AuthenticationFailed(_))
        ));
        assert!(!migrated.get());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "authorization");
        assert!(crate::user_partitions::file_locks_unavailable(
            &std::io::Error::from(std::io::ErrorKind::Unsupported)
        ));
    }

    #[tokio::test]
    async fn refresh_lease_storage_failure_is_not_reported_as_contention() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let error = match RefreshLease::acquire_in(file.path(), "test").await {
            Err(error) => error,
            Ok(_) => panic!("a non-directory cannot hold a lease"),
        };
        let message = error.to_string();
        assert!(message.contains("lease storage unavailable"));
        assert!(!message.contains("busy"));
    }

    #[test]
    fn signout_purges_multiple_stranded_claims_without_restoring_a_login() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("legacy.json");
        for value in ["A", "B"] {
            std::fs::write(
                original.with_extension(format!("migration-{}", uuid::Uuid::new_v4())),
                value,
            )
            .unwrap();
        }
        assert!(LegacyTokenGuard::acquire(&original).is_err());
        let cache = Mutex::new(Some(HashMap::from([("login".into(), "B".into())])));
        with_legacy_signout(&cache, &original, |tokens| {
            *tokens = None;
            Ok(())
        })
        .unwrap();
        assert!(cache.lock().unwrap().is_none());
        assert!(ClaimedTokenFile::recovery_paths(&original)
            .unwrap()
            .is_empty());
        let _migration = LegacyTokenGuard::acquire(&original).unwrap();
        assert!(!original.exists(), "sign-out must not restore either login");
    }

    // Only the unix tests can play ENOLCK, so the helper shares their cfg.
    #[cfg(unix)]
    fn enolck() -> std::fs::TryLockError {
        std::fs::TryLockError::Error(std::io::Error::from_raw_os_error(libc::ENOLCK))
    }

    fn unsupported_lock() -> std::fs::TryLockError {
        std::fs::TryLockError::Error(std::io::Error::from(std::io::ErrorKind::Unsupported))
    }

    #[cfg(unix)]
    #[test]
    fn signout_that_sees_enolck_leaves_an_active_migration_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("legacy.json");
        std::fs::write(&original, "authorization").unwrap();
        // The migrator holds the real lock and its claim, as another process
        // would while the kernel has no lock record left for this sign-out.
        let _migrator = LegacyTokenGuard::acquire(&original).unwrap();
        let claimed = ClaimedTokenFile::claim(&original).unwrap();
        let cache = Mutex::new(Some(HashMap::from([("login".into(), "A".into())])));
        let cleared = std::cell::Cell::new(false);

        let refused =
            LegacyTokenGuard::lock_with(&original, LegacyLockUse::Signout, |_| Err(enolck()))
                .and_then(|guard| {
                    with_legacy_signout_guard(&cache, &original, guard, |tokens| {
                        cleared.set(true);
                        *tokens = None;
                        Ok(())
                    })
                });

        assert!(matches!(refused, Err(ProviderError::Other(_))));
        assert!(!cleared.get(), "nothing may be cleared without the lock");
        assert_eq!(
            std::fs::read_to_string(&claimed.path).unwrap(),
            "authorization"
        );
        assert_eq!(
            cache
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .get("login")
                .unwrap(),
            "A"
        );
        // The legacy file alone, before a migrator claims it, is refused too.
        drop(claimed);
        assert!(original.exists());
        assert!(
            LegacyTokenGuard::lock_with(&original, LegacyLockUse::Signout, |_| Err(enolck()))
                .is_err()
        );
        assert_eq!(std::fs::read_to_string(&original).unwrap(), "authorization");
    }

    /// With nothing on disk at all, ENOLCK still refuses: an empty disk is
    /// one of the states a migrator's recovery passes through.
    #[cfg(unix)]
    #[test]
    fn signout_that_sees_enolck_is_refused_even_with_nothing_left_to_migrate() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("legacy.json");
        let cache = Mutex::new(Some(HashMap::from([("login".into(), "A".into())])));
        let refused = with_legacy_signout_with(
            &cache,
            &original,
            |_| Err(enolck()),
            |tokens| {
                *tokens = None;
                Ok(())
            },
        );
        let message = match refused {
            Err(ProviderError::Other(message)) => message,
            other => panic!("sign-out must fail closed on ENOLCK: {other:?}"),
        };
        assert!(message.contains("try again"), "{message}");
        assert_eq!(cache.lock().unwrap().as_ref().unwrap()["login"], "A");
    }

    /// The interleaving the review lists, driven through the injectable
    /// try-lock: while sign-out asks for the lock, a migrator holding it
    /// recovers its claim to the legacy name. Sign-out used to look at the
    /// disk after that and could see neither a legacy file nor a claim at the
    /// moment between them; now it refuses on ENOLCK whatever the disk shows,
    /// so the restored authorization and the cache are left to the migrator.
    #[cfg(unix)]
    #[test]
    fn signout_on_enolck_cannot_overtake_a_claim_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("legacy.json");
        let claim = original.with_extension(format!("migration-{}", uuid::Uuid::new_v4()));
        std::fs::write(&claim, "authorization").unwrap();
        let _migrator = LegacyTokenGuard::lock(&original, LegacyLockUse::Migration).unwrap();
        let cache = Mutex::new(Some(HashMap::from([("login".into(), "A".into())])));
        let cleared = std::cell::Cell::new(false);
        // Each state of the recovery as sign-out may see it: the claim alone,
        // the legacy file alone, and neither (the gap a two-look check missed).
        for step in ["claim", "restored", "empty"] {
            let refused = with_legacy_signout_with(
                &cache,
                &original,
                |_| {
                    match step {
                        "restored" if claim.exists() => std::fs::rename(&claim, &original).unwrap(),
                        "empty" if original.exists() => {
                            std::fs::rename(&original, dir.path().join("held")).unwrap()
                        }
                        _ => {}
                    }
                    Err(enolck())
                },
                |tokens| {
                    cleared.set(true);
                    *tokens = None;
                    Ok(())
                },
            );
            assert!(
                matches!(refused, Err(ProviderError::Other(_))),
                "{step}: {refused:?}"
            );
            assert!(
                !cleared.get(),
                "{step}: nothing may be cleared without the lock"
            );
            assert_eq!(
                cache.lock().unwrap().as_ref().unwrap()["login"],
                "A",
                "{step}"
            );
        }
        // The migrator's authorization survived every refused sign-out.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("held")).unwrap(),
            "authorization"
        );
    }

    /// The whole sign-out of `delete_tokens` refused on ENOLCK takes nothing
    /// away: the partition mirror is removed only after the lock is held, as
    /// are the vault entry and the memory cache, so "nothing was signed out"
    /// is true. With the lock the same call removes them.
    #[cfg(unix)]
    #[test]
    fn a_signout_refused_on_enolck_removes_no_copy_of_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let json_path = dir.path().join("oauth2_box.json");
        // The legacy files of THIS fixture: the sign-out removes these, next
        // to the lock it holds, and nothing in the app's real token folder.
        std::fs::write(&json_path, "legacy").unwrap();
        std::fs::write(json_path.with_extension("enc"), "legacy").unwrap();
        let account = format!("oauth_box_enolck-{}", uuid::Uuid::new_v4());
        MEMORY_TOKEN_CACHE
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(account.clone(), "{}".into());
        let unmirrored = std::cell::Cell::new(false);
        let refused = OAuth2Manager::delete_shared_tokens(
            OAuthProvider::Box,
            &account,
            &json_path,
            |path| LegacyTokenGuard::lock_with(path, LegacyLockUse::Signout, |_| Err(enolck())),
            || {
                unmirrored.set(true);
                Ok(())
            },
        );
        assert!(
            matches!(&refused, Err(ProviderError::Other(message)) if message.contains("nothing was signed out")),
            "{refused:?}"
        );
        assert!(!unmirrored.get(), "the partition mirror stays");
        let cached = |account: &str| {
            MEMORY_TOKEN_CACHE
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|map| map.contains_key(account))
        };
        assert!(cached(&account), "the memory cache stays");
        assert!(
            json_path.exists() && json_path.with_extension("enc").exists(),
            "the legacy files stay"
        );

        OAuth2Manager::delete_shared_tokens(
            OAuthProvider::Box,
            &account,
            &json_path,
            LegacyTokenGuard::acquire_for_signout,
            || {
                unmirrored.set(true);
                Ok(())
            },
        )
        .unwrap();
        assert!(unmirrored.get());
        assert!(!cached(&account));
        assert!(
            !json_path.exists() && !json_path.with_extension("enc").exists(),
            "the fixture's legacy files are the ones removed"
        );
    }

    /// The partition copy goes first, under the lock, and a failure to remove
    /// it (here a refused SQLite delete) fails the sign-out before the vault,
    /// the memory cache or the legacy files are touched.
    #[test]
    fn a_partition_delete_that_fails_stops_the_signout_before_any_other_copy() {
        let dir = tempfile::tempdir().unwrap();
        let json_path = dir.path().join("oauth2_box.json");
        std::fs::write(&json_path, "legacy").unwrap();
        let account = format!("oauth_box_partition-{}", uuid::Uuid::new_v4());
        MEMORY_TOKEN_CACHE
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(account.clone(), "{}".into());
        let failed = OAuth2Manager::delete_shared_tokens(
            OAuthProvider::Box,
            &account,
            &json_path,
            LegacyTokenGuard::acquire_for_signout,
            || {
                Err(ProviderError::Other(
                    "Failed to delete tokens: Delete user credential: database is locked".into(),
                ))
            },
        );
        assert!(failed.is_err(), "{failed:?}");
        assert!(MEMORY_TOKEN_CACHE
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|map| map.contains_key(&account)));
        assert!(json_path.exists(), "the legacy file stays");
        MEMORY_TOKEN_CACHE
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .remove(&account);
    }

    /// A token miss with no legacy file and no claim answers "no stored
    /// authorization" and takes no lock: the lock file is never created.
    #[test]
    fn a_token_miss_with_nothing_on_disk_takes_no_legacy_lock() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("legacy.json");
        let result: Result<(), _> =
            with_stored_legacy_token_claim(&Mutex::new(None), &original, |_, _| {
                panic!("nothing to migrate")
            });
        assert!(
            matches!(&result, Err(ProviderError::AuthenticationFailed(message)) if message == "No stored authorization"),
            "{result:?}"
        );
        assert!(
            !original.with_extension("migration.lock").exists(),
            "no lock was taken"
        );
    }

    #[cfg(unix)]
    #[test]
    fn enolck_disables_migration_like_a_filesystem_without_locks() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("legacy.json");
        std::fs::write(&original, "authorization").unwrap();
        for failure in [enolck as fn() -> _, unsupported_lock] {
            let guard =
                LegacyTokenGuard::lock_with(
                    &original,
                    LegacyLockUse::Migration,
                    |_| Err(failure()),
                )
                .unwrap();
            assert!(guard.0.is_none());
            let result: Result<(), _> =
                with_legacy_claim_guard(&Mutex::new(None), &original, guard, |_, _| {
                    panic!("an unlocked guard must not migrate")
                });
            assert!(matches!(
                result,
                Err(ProviderError::AuthenticationFailed(_))
            ));
            assert_eq!(std::fs::read_to_string(&original).unwrap(), "authorization");
        }
    }

    #[test]
    fn signout_on_a_filesystem_without_locks_still_purges_claims() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("legacy.json");
        std::fs::write(
            original.with_extension(format!("migration-{}", uuid::Uuid::new_v4())),
            "A",
        )
        .unwrap();
        let guard = LegacyTokenGuard::lock_with(&original, LegacyLockUse::Signout, |_| {
            Err(unsupported_lock())
        })
        .unwrap();
        assert!(guard.0.is_none());
        let cache = Mutex::new(Some(HashMap::new()));
        with_legacy_signout_guard(&cache, &original, guard, |tokens| {
            *tokens = None;
            Ok(())
        })
        .unwrap();
        assert!(cache.lock().unwrap().is_none());
        assert!(ClaimedTokenFile::recovery_paths(&original)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn lock_errors_are_classified_by_what_they_prove() {
        assert_eq!(
            legacy_lock_failure(&std::io::Error::from(std::io::ErrorKind::Unsupported)),
            LegacyLockFailure::NoLocks
        );
        #[cfg(unix)]
        {
            for code in [libc::ENOSYS, libc::EOPNOTSUPP] {
                assert_eq!(
                    legacy_lock_failure(&std::io::Error::from_raw_os_error(code)),
                    LegacyLockFailure::NoLocks
                );
            }
            assert_eq!(
                legacy_lock_failure(&std::io::Error::from_raw_os_error(libc::ENOLCK)),
                LegacyLockFailure::Unproven
            );
        }
        assert_eq!(
            legacy_lock_failure(&std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
            LegacyLockFailure::Other
        );
    }

    #[cfg(unix)]
    #[test]
    fn signout_overwrites_a_stranded_claim_before_unlinking_it() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("legacy.json");
        let claim = original.with_extension(format!("migration-{}", uuid::Uuid::new_v4()));
        std::fs::write(&claim, "refresh-token-secret").unwrap();
        // A second name keeps the claim's blocks readable after the unlink.
        let witness = dir.path().join("witness");
        std::fs::hard_link(&claim, &witness).unwrap();
        with_legacy_signout(&Mutex::new(None), &original, |_| Ok(())).unwrap();
        assert!(!claim.exists());
        let left = std::fs::read(&witness).unwrap();
        assert!(
            !String::from_utf8_lossy(&left).contains("refresh-token-secret"),
            "the plaintext must be overwritten, not only unlinked"
        );
    }

    #[tokio::test]
    async fn refresh_lease_file_name_is_safe_for_a_profile_id_with_a_separator() {
        let dir = tempfile::tempdir().unwrap();
        let slug = "oauth_box_rclone-team/drive..\\x-1";
        let lease = RefreshLease::acquire_in(dir.path(), slug)
            .await
            .unwrap()
            .expect("a free lease is granted");
        assert_eq!(lease.path.parent().unwrap(), dir.path());
        assert!(lease.path.is_file());
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["refresh-oauth_box_rclone-team_drive___x-1.lock"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_stale_lease_that_cannot_be_removed_is_an_error_not_a_spin() {
        let dir = tempfile::tempdir().unwrap();
        // A directory at the lease path: create_new sees it, unlink cannot
        // remove it.
        let lock_path = dir.path().join("refresh-stuck.lock");
        std::fs::create_dir(&lock_path).unwrap();
        let stale = std::time::SystemTime::now()
            - std::time::Duration::from_secs(RefreshLease::STALE_SECS + 60);
        filetime::set_file_mtime(&lock_path, filetime::FileTime::from_system_time(stale)).unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            RefreshLease::acquire_in(dir.path(), "stuck"),
        )
        .await
        .expect("acquisition must return instead of retrying forever");
        let message = match result {
            Err(error) => error.to_string(),
            Ok(_) => panic!("an unremovable stale lease cannot be granted"),
        };
        assert!(message.contains("lease storage unavailable"));
    }

    #[test]
    fn crashed_legacy_claim_is_recovered_without_resurrecting_a_newer_login() {
        const MODE: &str = "AEROFTP_LEGACY_CRASH_CLAIM";
        const TEST: &str = "providers::oauth2::tests::crashed_legacy_claim_is_recovered_without_resurrecting_a_newer_login";
        if let Some(path) = std::env::var_os(MODE) {
            let path = std::path::PathBuf::from(path);
            let _guard = LegacyTokenGuard::acquire(&path).unwrap();
            let _claimed = ClaimedTokenFile::claim(&path).unwrap();
            std::process::exit(0); // No destructors, like a killed migrator.
        }
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("legacy.json");
        let crash = || {
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--test-threads=1"])
                .env(MODE, &original)
                .status()
                .unwrap()
                .success());
            assert!(!original.exists());
        };
        std::fs::write(&original, "A").unwrap();
        crash();
        let recovered = with_legacy_token_claim(&Mutex::new(None), &original, |_, claimed| {
            let value = std::fs::read_to_string(&claimed.path).unwrap();
            claimed.discard()?;
            Ok(value)
        })
        .unwrap();
        assert_eq!(recovered, "A");
        std::fs::write(&original, "A").unwrap();
        crash();
        std::fs::write(&original, "B").unwrap();
        {
            let _guard = LegacyTokenGuard::acquire(&original).unwrap();
            assert_eq!(std::fs::read_to_string(&original).unwrap(), "B");
            std::fs::remove_file(&original).unwrap(); // Real sign-out lock scope.
        }
        let _guard = LegacyTokenGuard::acquire(&original).unwrap();
        assert!(!original.exists(), "old A must never return after sign-out");
    }

    #[test]
    fn claimed_plaintext_cleanup_never_deletes_a_new_authorization() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("legacy.json");
        std::fs::write(&original, "A").unwrap();
        let claimed = super::ClaimedTokenFile::claim(&original).unwrap();
        std::fs::write(&original, "B").unwrap();
        assert_eq!(std::fs::read_to_string(&claimed.path).unwrap(), "A");
        claimed.discard().unwrap();
        assert_eq!(std::fs::read_to_string(&original).unwrap(), "B");
    }

    #[test]
    fn claimed_plaintext_restores_on_failure_without_clobber() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("legacy.json");
        std::fs::write(&original, "A").unwrap();
        drop(super::ClaimedTokenFile::claim(&original).unwrap());
        assert_eq!(std::fs::read_to_string(&original).unwrap(), "A");
        let claimed = super::ClaimedTokenFile::claim(&original).unwrap();
        let recovery = claimed.path.clone();
        std::fs::write(&original, "B").unwrap();
        drop(claimed);
        assert_eq!(std::fs::read_to_string(&original).unwrap(), "B");
        assert_eq!(std::fs::read_to_string(&recovery).unwrap(), "A");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&recovery).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

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

        {
            let manager = OAuth2Manager::new();
            let provider = OAuthProvider::GooglePhotos;
            let dir = OAuth2Manager::token_dir().unwrap();
            let path = dir.join("oauth2_googlephotos.json");
            let old = serde_json::to_string(&StoredTokens {
                access_token: "old-legacy".into(),
                refresh_token: None,
                expires_at: None,
                token_type: "Bearer".into(),
                scopes: vec![],
            })
            .unwrap();
            let fresh = old.replace("old-legacy", "fresh-login");
            std::fs::write(&path, &old).unwrap();
            let claimed = ClaimedTokenFile::claim(&path).unwrap();
            let store = crate::credential_store::CredentialStore::from_cache().unwrap();
            let account = OAuth2Manager::token_account_key(provider, "migration-winner");
            // Login B wins after legacy A was claimed but before insertion.
            store.store(&account, &fresh).unwrap();
            let winner =
                OAuth2Manager::migrate_claimed_to_vault(&store, &account, &old, None, claimed)
                    .unwrap();
            assert_eq!(winner.tokens.access_token, "fresh-login");
            assert!(!path.exists());
            assert!(manager
                .load_token_record(provider, "second-profile")
                .is_err());
            assert_eq!(
                manager
                    .load_tokens(provider, "migration-winner")
                    .unwrap()
                    .access_token,
                "fresh-login"
            );
            store.delete(&account).unwrap();
        }

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

        // The backend tag survives a new manager/process with the same real
        // authorization, without retaining any JavaScript credentials/map.
        let owner = crate::sync_owner::GuiOwnerContext::capture().unwrap();
        let proof = primary.sync_owner_identity(&config).unwrap();
        let original_binding = owner.persistent_tag(&proof).unwrap().unwrap();
        let after_restart = OAuth2Manager::new();
        after_restart.get_valid_token(&config).await.unwrap();
        let restarted_proof = after_restart.sync_owner_identity(&config).unwrap();
        assert!(crate::sync_owner::bindings_match(
            &original_binding,
            &owner.persistent_tag(&restarted_proof).unwrap().unwrap()
        ));

        // A legitimate rotation preserves the encrypted provenance and remains
        // visible to all clones, without resealing them to another login.
        let account = OAuth2Manager::token_account_key(config.provider, &profile);
        let observed = primary
            .load_token_record(config.provider, &profile)
            .unwrap();
        let mut rotated = tokens.clone();
        rotated.access_token = "rotated-A".into();
        rotated.refresh_token = Some("rotated-refresh-A".into());
        primary
            .persist_refreshed_record(&account, &observed, &rotated)
            .unwrap();
        let cloned = primary.clone();
        assert_eq!(
            cloned
                .get_valid_token(&config)
                .await
                .unwrap()
                .expose_secret(),
            "rotated-A"
        );

        // A new login during an in-flight refresh wins storage; neither the
        // primary nor a clone is allowed to use that login as connection A.
        let before_login = primary
            .load_token_record(config.provider, &profile)
            .unwrap();
        primary
            .store_tokens(config.provider, &profile, &other_tokens)
            .unwrap();
        assert!(primary
            .persist_refreshed_record(&account, &before_login, &rotated)
            .is_err());
        assert_eq!(
            primary
                .load_tokens(config.provider, &profile)
                .unwrap()
                .access_token,
            "other-access"
        );
        assert!(primary
            .get_valid_token(&config)
            .await
            .unwrap_err()
            .to_string()
            .contains("CONNECTION_CHANGED"));
        assert!(cloned
            .get_valid_token(&config)
            .await
            .unwrap_err()
            .to_string()
            .contains("CONNECTION_CHANGED"));
        // Only a new connection may adopt the newly authorized account.
        let reconnected = OAuth2Manager::new();
        assert_eq!(
            reconnected
                .get_valid_token(&config)
                .await
                .unwrap()
                .expose_secret(),
            "other-access"
        );

        let changed_proof = reconnected.sync_owner_identity(&config).unwrap();
        let changed_binding = owner.persistent_tag(&changed_proof).unwrap().unwrap();
        assert!(!crate::sync_owner::bindings_match(
            &original_binding,
            &changed_binding
        ));
        let _ = primary.delete_tokens(OAuthProvider::Dropbox, &profile);
        let _ = primary.delete_tokens(OAuthProvider::Dropbox, &other_profile);
    }
}
