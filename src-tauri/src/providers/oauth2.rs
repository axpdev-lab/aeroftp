//! OAuth2 Authentication Module
//!
//! Provides OAuth2 authentication flow for cloud providers like Google Drive,
//! Dropbox, and OneDrive. Uses system browser for authorization and keyring
//! for secure token storage.

use oauth2::{
    basic::BasicClient, AuthUrl, AuthorizationCode, ClientId, ClientSecret,
    CsrfToken, EndpointNotSet, EndpointSet, PkceCodeChallenge, PkceCodeVerifier,
    RedirectUrl, Scope, TokenResponse, TokenUrl, RefreshToken,
};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Configured OAuth2 client with auth and token endpoints set (v5 typestates)
type ConfiguredClient = BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

/// Simple error wrapper for the oauth2 HTTP client adapter.
#[derive(Debug)]
struct OAuth2TransportError(String);

impl std::fmt::Display for OAuth2TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for OAuth2TransportError {}

/// Async HTTP client adapter for oauth2 v5.
/// Bridges the project's reqwest (0.13) with oauth2's `AsyncHttpClient` trait.
/// Required because oauth2 v5's built-in reqwest support targets reqwest 0.12.
struct OAuth2HttpClient;

impl<'c> oauth2::AsyncHttpClient<'c> for OAuth2HttpClient {
    type Error = oauth2::HttpClientError<OAuth2TransportError>;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<oauth2::HttpResponse, Self::Error>> + Send + Sync + 'c>,
    >;

    fn call(&'c self, request: oauth2::HttpRequest) -> Self::Future {
        Box::pin(async move {
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
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

            let response = builder.send().await
                .map_err(|e| oauth2::HttpClientError::Other(e.to_string()))?;

            let status_code = response.status().as_u16();
            let headers = response.headers().clone();
            let body = response.bytes().await
                .map_err(|e| oauth2::HttpClientError::Other(e.to_string()))?;

            let mut http_response = http::Response::builder()
                .status(http::StatusCode::from_u16(status_code).unwrap_or(http::StatusCode::INTERNAL_SERVER_ERROR));
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
use tracing::info;

use super::ProviderError;

/// OAuth2 provider types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OAuthProvider {
    Google,
    Dropbox,
    OneDrive,
    Box,
    PCloud,
}

impl std::fmt::Display for OAuthProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OAuthProvider::Google => write!(f, "Google Drive"),
            OAuthProvider::Dropbox => write!(f, "Dropbox"),
            OAuthProvider::OneDrive => write!(f, "OneDrive"),
            OAuthProvider::Box => write!(f, "Box"),
            OAuthProvider::PCloud => write!(f, "pCloud"),
        }
    }
}

/// OAuth2 configuration for a provider
#[derive(Debug, Clone)]
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
}

impl OAuthConfig {
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
            extra_auth_params: vec![
                ("access_type".to_string(), "offline".to_string()),
            ],
        }
    }

    /// Create Google Drive OAuth config (default port for token refresh only)
    pub fn google(client_id: &str, client_secret: &str) -> Self {
        Self::google_with_port(client_id, client_secret, 0)
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
                "sharing.read".to_string(),
                "sharing.write".to_string(),
            ],
            redirect_uri: format!("http://127.0.0.1:{}/callback", port),
            extra_auth_params: vec![
                ("token_access_type".to_string(), "offline".to_string()),
            ],
        }
    }

    /// Create Dropbox OAuth config (default port for token refresh only)
    pub fn dropbox(client_id: &str, client_secret: &str) -> Self {
        Self::dropbox_with_port(client_id, client_secret, 0)
    }

    /// Create OneDrive OAuth config with dynamic callback port
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
            redirect_uri: format!("http://127.0.0.1:{}/callback", port),
            extra_auth_params: vec![],
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
        }
    }

    /// Create Box OAuth config (default port for token refresh only)
    pub fn box_cloud(client_id: &str, client_secret: &str) -> Self {
        Self::box_cloud_with_port(client_id, client_secret, 0)
    }

    /// Create pCloud OAuth config with dynamic callback port
    pub fn pcloud_with_port(client_id: &str, client_secret: &str, port: u16) -> Self {
        Self {
            provider: OAuthProvider::PCloud,
            client_id: client_id.to_string(),
            client_secret: Some(client_secret.to_string()),
            auth_url: "https://my.pcloud.com/oauth2/authorize".to_string(),
            token_url: "https://api.pcloud.com/oauth2_token".to_string(),
            scopes: vec![],
            redirect_uri: format!("http://127.0.0.1:{}/callback", port),
            extra_auth_params: vec![],
        }
    }

    /// Create pCloud OAuth config (default port for token refresh only)
    pub fn pcloud(client_id: &str, client_secret: &str) -> Self {
        Self::pcloud_with_port(client_id, client_secret, 0)
    }
}

/// Stored OAuth2 tokens
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// OAuth2 Manager for handling authentication flows
pub struct OAuth2Manager {
    /// Pending PKCE verifiers for ongoing auth flows
    pending_verifiers: Arc<RwLock<HashMap<String, PkceCodeVerifier>>>,
    /// Callback server port (used in redirect URL generation)
    #[allow(dead_code)]
    callback_port: u16,
}

impl OAuth2Manager {
    pub fn new() -> Self {
        Self {
            pending_verifiers: Arc::new(RwLock::new(HashMap::new())),
            callback_port: 0, // Will be assigned dynamically by OS
        }
    }

    /// Start OAuth2 authorization flow - returns URL to open in browser
    pub async fn start_auth_flow(&self, config: &OAuthConfig) -> Result<(String, String), ProviderError> {
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
            verifiers.remove(state)
                .ok_or_else(|| ProviderError::AuthenticationFailed(
                    "Invalid state token - authorization flow expired or invalid".to_string()
                ))?
        };
        
        let client = self.create_client(config)?;
        
        // Exchange code for tokens
        let token_result = client
            .exchange_code(AuthorizationCode::new(code.to_string()))
            .set_pkce_verifier(verifier)
            .request_async(&OAuth2HttpClient)
            .await
            .map_err(|e| ProviderError::AuthenticationFailed(format!("Token exchange failed: {}", e)))?;
        
        let expires_at = token_result.expires_in().map(|d| {
            chrono::Utc::now().timestamp() + d.as_secs() as i64
        });
        
        let tokens = StoredTokens {
            access_token: token_result.access_token().secret().clone(),
            refresh_token: token_result.refresh_token().map(|t| t.secret().clone()),
            expires_at,
            token_type: "Bearer".to_string(),
            scopes: config.scopes.clone(),
        };
        
        // Store in keyring
        self.store_tokens(config.provider, &tokens)?;
        
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
            .map_err(|e| ProviderError::AuthenticationFailed(format!("Token refresh failed: {}", e)))?;
        
        let expires_at = token_result.expires_in().map(|d| {
            chrono::Utc::now().timestamp() + d.as_secs() as i64
        });
        
        let tokens = StoredTokens {
            access_token: token_result.access_token().secret().clone(),
            refresh_token: token_result.refresh_token()
                .map(|t| t.secret().clone())
                .or_else(|| Some(refresh_token.to_string())), // Keep old refresh token if not returned
            expires_at,
            token_type: "Bearer".to_string(),
            scopes: config.scopes.clone(),
        };
        
        // Update keyring
        self.store_tokens(config.provider, &tokens)?;
        
        info!("OAuth2 tokens refreshed for {:?}", config.provider);
        
        Ok(tokens)
    }

    /// Get valid access token (refreshing if needed)
    pub async fn get_valid_token(
        &self,
        config: &OAuthConfig,
    ) -> Result<SecretString, ProviderError> {
        let mut tokens = self.load_tokens(config.provider)?;

        if tokens.is_expired() {
            if let Some(ref refresh_token) = tokens.refresh_token {
                tokens = self.refresh_tokens(config, refresh_token).await?;
            } else {
                return Err(ProviderError::AuthenticationFailed(
                    "Token expired and no refresh token available".to_string()
                ));
            }
        }

        Ok(SecretString::from(tokens.access_token))
    }

    /// Get the token storage directory
    fn token_dir() -> Result<std::path::PathBuf, ProviderError> {
        let base = dirs::config_dir()
            .ok_or_else(|| ProviderError::Other("Could not find config directory".to_string()))?;
        let token_dir = base.join("aeroftp").join("oauth_tokens");
        if !token_dir.exists() {
            std::fs::create_dir_all(&token_dir)
                .map_err(|e| ProviderError::Other(format!("Failed to create token directory: {}", e)))?;
        }
        Ok(token_dir)
    }

    /// Store tokens in secure credential store (OS keyring or encrypted vault)
    pub fn store_tokens(&self, provider: OAuthProvider, tokens: &StoredTokens) -> Result<(), ProviderError> {
        let json = serde_json::to_string_pretty(tokens)
            .map_err(|e| ProviderError::Other(format!("Failed to serialize tokens: {}", e)))?;

        let account = format!("oauth_{:?}", provider).to_lowercase();

        // Store in universal vault
        if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
            store.store(&account, &json)
                .map_err(|e| ProviderError::Other(format!("Failed to store tokens: {}", e)))?;
            info!("Tokens stored in credential vault for {:?}", provider);
            return Ok(());
        }

        // Vault not open — fallback to file with secure permissions
        let token_path = Self::token_dir()?.join(format!("oauth2_{:?}.json", provider).to_lowercase());
        std::fs::write(&token_path, &json)
            .map_err(|e| ProviderError::Other(format!("Failed to store tokens: {}", e)))?;
        let _ = crate::credential_store::ensure_secure_permissions(&token_path);

        info!("Tokens stored in file for {:?} (vault not open)", provider);
        Ok(())
    }

    /// Load tokens from credential vault or legacy file
    pub fn load_tokens(&self, provider: OAuthProvider) -> Result<StoredTokens, ProviderError> {
        let account = format!("oauth_{:?}", provider).to_lowercase();

        // Try vault first
        if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
            if let Ok(json) = store.get(&account) {
                return serde_json::from_str(&json)
                    .map_err(|e| ProviderError::Other(format!("Failed to parse tokens: {}", e)));
            }
        }

        // Fallback: try legacy file
        let token_path = Self::token_dir()?.join(format!("oauth2_{:?}.json", provider).to_lowercase());
        let json = std::fs::read_to_string(&token_path)
            .map_err(|e| ProviderError::AuthenticationFailed(format!("No stored tokens: {}", e)))?;

        serde_json::from_str(&json)
            .map_err(|e| ProviderError::Other(format!("Failed to parse tokens: {}", e)))
    }

    /// Delete tokens from credential vault and legacy file
    pub fn delete_tokens(&self, provider: OAuthProvider) -> Result<(), ProviderError> {
        let account = format!("oauth_{:?}", provider).to_lowercase();

        // Delete from vault
        if let Some(store) = crate::credential_store::CredentialStore::from_cache() {
            let _ = store.delete(&account);
        }

        // Also delete legacy file if exists
        let token_path = Self::token_dir()?.join(format!("oauth2_{:?}.json", provider).to_lowercase());
        if token_path.exists() {
            let _ = crate::credential_store::secure_delete(&token_path);
        }

        info!("Tokens deleted for {:?}", provider);
        Ok(())
    }

    /// Alias for delete_tokens
    pub fn clear_tokens(&self, provider: OAuthProvider) -> Result<(), ProviderError> {
        self.delete_tokens(provider)
    }

    /// Check if tokens exist for provider
    pub fn has_tokens(&self, provider: OAuthProvider) -> bool {
        self.load_tokens(provider).is_ok()
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
pub async fn bind_callback_listener_on_port(port: u16) -> Result<(tokio::net::TcpListener, u16), ProviderError> {
    use tokio::net::TcpListener;

    let listener = TcpListener::bind(format!("127.0.0.1:{}", port))
        .await
        .map_err(|e| ProviderError::Other(format!("Failed to bind callback server on port {}: {}", port, e)))?;

    let actual_port = listener.local_addr()
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

/// Wait for an OAuth2 callback on an already-bound listener.
/// Returns (code, state) extracted from the callback request.
pub async fn wait_for_callback(listener: tokio::net::TcpListener) -> Result<(String, String), ProviderError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    
    let (mut socket, _): (tokio::net::TcpStream, _) = listener.accept()
        .await
        .map_err(|e| ProviderError::Other(format!("Failed to accept connection: {}", e)))?;
    
    let mut buffer = vec![0u8; 4096];
    let n: usize = socket.read(&mut buffer)
        .await
        .map_err(|e| ProviderError::Other(format!("Failed to read request: {}", e)))?;
    
    let request = String::from_utf8_lossy(&buffer[..n]);
    
    // Parse the request to extract code and state
    let (code, state) = parse_callback_request(&request)?;
    
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
            padding: 60px 50px;
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
        
        <p class="close-hint">You can close this window and return to AeroFTP<br>or press <kbd>Alt</kbd> + <kbd>F4</kbd></p>
    </div>
    
    <script>
        // Auto-close after 5 seconds (optional)
        // setTimeout(() => window.close(), 5000);
    </script>
</body>
</html>"#;
    
    let _: () = socket.write_all(response.as_bytes())
        .await
        .map_err(|e| ProviderError::Other(format!("Failed to send response: {}", e)))?;
    
    Ok((code, state))
}

/// Parse OAuth callback request to extract code and state
fn parse_callback_request(request: &str) -> Result<(String, String), ProviderError> {
    // Find the GET line
    let first_line = request.lines().next()
        .ok_or_else(|| ProviderError::AuthenticationFailed("Empty request".to_string()))?;
    
    // Extract path: GET /callback?code=xxx&state=yyy HTTP/1.1
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() < 2 {
        return Err(ProviderError::AuthenticationFailed("Invalid request format".to_string()));
    }
    
    let path = parts[1];
    let query_start = path.find('?')
        .ok_or_else(|| ProviderError::AuthenticationFailed("No query parameters".to_string()))?;
    
    let query = &path[query_start + 1..];
    
    let mut code = None;
    let mut state = None;
    
    for param in query.split('&') {
        let mut kv = param.splitn(2, '=');
        let key = kv.next().unwrap_or("");
        let value = kv.next().unwrap_or("");
        
        match key {
            "code" => code = Some(urlencoding::decode(value).unwrap_or_default().to_string()),
            "state" => state = Some(urlencoding::decode(value).unwrap_or_default().to_string()),
            "error" => return Err(ProviderError::AuthenticationFailed(format!("OAuth error: {}", value))),
            _ => {}
        }
    }
    
    let code = code.ok_or_else(|| ProviderError::AuthenticationFailed("Missing code".to_string()))?;
    let state = state.ok_or_else(|| ProviderError::AuthenticationFailed("Missing state".to_string()))?;
    
    Ok((code, state))
}

#[cfg(test)]
mod tests {
    use super::*;
    
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
        assert!(config.scopes.len() > 0);
    }
}
