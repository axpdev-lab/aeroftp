//! GitHub authentication helpers
//!
//! Supports:
//! - Fine-grained PAT (manual paste)
//! - GitHub App Device Flow (browser authorization)
//! - GitHub App Installation Token (bot mode with .pem)

use serde::Deserialize;
use std::time::Duration;
use log::{info, debug};

/// GitHub App configuration for Device Flow (AeroFTP's official app)
pub const GITHUB_APP_CLIENT_ID: &str = "Iv23liBpUihk573Igvos";

// ── Device Flow ──────────────────────────────────────────────────

/// Device Flow step 1 response
#[derive(Debug, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// Device Flow token response
#[derive(Debug, Deserialize)]
pub struct DeviceTokenResponse {
    pub access_token: Option<String>,
    pub token_type: Option<String>,
    pub scope: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
    pub interval: Option<u64>,
}

/// Request a device code from GitHub
pub async fn request_device_code() -> Result<DeviceCodeResponse, String> {
    let client = reqwest::Client::new();
    let resp = client
        .post("https://github.com/login/device/code")
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!("client_id={}", GITHUB_APP_CLIENT_ID))
        .send()
        .await
        .map_err(|e| format!("Failed to request device code: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("GitHub device code request failed ({}): {}", status, body));
    }

    resp.json::<DeviceCodeResponse>()
        .await
        .map_err(|e| format!("Failed to parse device code response: {}", e))
}

/// Poll for the access token after user has authorized
pub async fn poll_for_token(device_code: &str, interval: u64) -> Result<String, String> {
    let client = reqwest::Client::new();
    let mut poll_interval = Duration::from_secs(interval.max(5));
    let max_attempts = 120;

    for attempt in 0..max_attempts {
        debug!("GitHub Device Flow: polling attempt {} (interval: {}s)", attempt + 1, poll_interval.as_secs());

        tokio::time::sleep(poll_interval).await;

        let form_body = format!(
            "client_id={}&device_code={}&grant_type=urn:ietf:params:oauth:grant-type:device_code",
            GITHUB_APP_CLIENT_ID, device_code
        );
        let resp = client
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(form_body)
            .send()
            .await
            .map_err(|e| format!("Token poll failed: {}", e))?;

        let token_resp: DeviceTokenResponse = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse token response: {}", e))?;

        if let Some(access_token) = token_resp.access_token {
            info!("GitHub Device Flow: authorization successful");
            return Ok(access_token);
        }

        match token_resp.error.as_deref() {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                if let Some(new_interval) = token_resp.interval {
                    poll_interval = Duration::from_secs(new_interval);
                } else {
                    poll_interval += Duration::from_secs(5);
                }
                continue;
            }
            Some("expired_token") => return Err("Authorization expired. Please try again.".to_string()),
            Some("access_denied") => return Err("Authorization denied by user.".to_string()),
            Some(error) => {
                let desc = token_resp.error_description.unwrap_or_default();
                return Err(format!("Authorization failed: {} — {}", error, desc));
            }
            None => return Err("Unexpected response from GitHub (no token and no error)".to_string()),
        }
    }

    Err("Authorization timed out after 10 minutes.".to_string())
}

// ── Installation Token (Bot Mode with .pem) ──────────────────────

/// Response from GitHub's installation token endpoint
#[derive(Debug, Deserialize)]
pub struct InstallationTokenResponse {
    pub token: String,
    pub expires_at: String,
}

/// Generate a JWT from a GitHub App private key (.pem file)
/// The JWT is used to authenticate as the GitHub App and request installation tokens
pub fn generate_app_jwt(pem_contents: &str, app_id: &str) -> Result<String, String> {
    use jsonwebtoken::{encode, EncodingKey, Header, Algorithm};
    use serde::Serialize;

    #[derive(Serialize)]
    struct Claims {
        iat: u64,   // Issued at
        exp: u64,   // Expires at (max 10 minutes)
        iss: String, // App ID
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("System time error: {}", e))?
        .as_secs();

    let claims = Claims {
        iat: now.saturating_sub(60), // 1 minute in the past for clock skew
        exp: now + 600,              // 10 minutes from now (GitHub max)
        iss: app_id.to_string(),
    };

    let key = EncodingKey::from_rsa_pem(pem_contents.as_bytes())
        .map_err(|e| format!("Invalid PEM key: {}", e))?;

    let header = Header::new(Algorithm::RS256);
    encode(&header, &claims, &key)
        .map_err(|e| format!("JWT encoding failed: {}", e))
}

/// Exchange a JWT for an installation access token
/// This token can be used for API calls and commits will show the app's identity/logo
pub async fn get_installation_token(
    pem_contents: &str,
    app_id: &str,
    installation_id: &str,
) -> Result<InstallationTokenResponse, String> {
    let jwt = generate_app_jwt(pem_contents, app_id)?;

    let client = reqwest::Client::new();
    let url = format!(
        "https://api.github.com/app/installations/{}/access_tokens",
        installation_id
    );

    let resp = client
        .post(&url)
        .header("Accept", "application/vnd.github+json")
        .header("Authorization", format!("Bearer {}", jwt))
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", "AeroFTP")
        .send()
        .await
        .map_err(|e| format!("Installation token request failed: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("GitHub installation token failed ({}): {}", status, body));
    }

    let token_resp: InstallationTokenResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse installation token: {}", e))?;

    info!("GitHub App installation token obtained (expires: {})", token_resp.expires_at);
    Ok(token_resp)
}

/// Validate a .pem file by attempting to generate a JWT
pub fn validate_pem(pem_contents: &str, app_id: &str) -> Result<(), String> {
    generate_app_jwt(pem_contents, app_id)?;
    Ok(())
}
