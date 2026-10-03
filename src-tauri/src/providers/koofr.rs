//! Koofr Cloud Storage Provider: Native REST API v2.1
//!
//! European privacy-focused cloud storage with 10 GB free tier.
//! Mount-centric, path-based API: every file operation uses (mountId, path).
//!
//! Auth: App Password (HTTP Basic) or OAuth2 Bearer token.
//! API: <https://app.koofr.net/api/v2.1>
//! Content: <https://app.koofr.net/content/api/v2.1>

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use reqwest::header::{HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE, RANGE};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

use super::{
    response_bytes_with_limit, sanitize_api_error, send_with_retry, FileVersion, HttpRetryConfig,
    ProviderConfig, ProviderError, ProviderType, RemoteEntry, ShareLinkCapabilities, ShareLinkInfo,
    ShareLinkOptions, ShareLinkResult, StorageInfo, StorageProvider, TransferOptimizationHints,
    MAX_DOWNLOAD_TO_BYTES,
};

const API_BASE: &str = "https://app.koofr.net/api/v2.1";
const CONTENT_BASE: &str = "https://app.koofr.net/content/api/v2.1";
/// Version header recommended by Koofr for forward compatibility (lowercase for HeaderName::from_static)
const KOOFR_VERSION_HEADER: &str = "x-koofr-version";

#[cfg(debug_assertions)]
fn koofr_log(msg: &str) {
    eprintln!("[koofr] {}", msg);
}

#[cfg(not(debug_assertions))]
fn koofr_log(_msg: &str) {}

fn mask_credential(value: &str) -> String {
    if value.is_empty() {
        return value.to_string();
    }
    if let Some(at) = value.find('@') {
        let local = &value[..at];
        let domain = &value[at..];
        let visible = local.floor_char_boundary(3);
        format!("{}***{}", &local[..visible], domain)
    } else if value.len() <= 3 {
        "***".to_string()
    } else {
        format!("{}***", &value[..value.floor_char_boundary(3)])
    }
}

// ─── Configuration ───

pub struct KoofrConfig {
    /// Email address for authentication
    pub email: String,
    /// App password (generated at Koofr preferences)
    pub password: SecretString,
    /// Optional initial path to navigate to after connect
    pub initial_path: Option<String>,
}

impl KoofrConfig {
    pub fn from_provider_config(config: &ProviderConfig) -> Result<Self, ProviderError> {
        let email = config
            .username
            .clone()
            .ok_or_else(|| ProviderError::InvalidConfig("Email is required".into()))?;
        if email.is_empty() {
            return Err(ProviderError::InvalidConfig("Email cannot be empty".into()));
        }
        let password = config
            .password
            .clone()
            .ok_or_else(|| ProviderError::InvalidConfig("App password is required".into()))?;
        if password.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "App password cannot be empty".into(),
            ));
        }
        Ok(Self {
            email,
            password: password.into(),
            initial_path: config.initial_path.clone(),
        })
    }
}

// ─── API Response Structures ───

#[derive(Debug, Deserialize)]
struct KoofrUser {
    #[allow(dead_code)]
    #[serde(default)]
    id: String,
    #[serde(rename = "firstName", default)]
    first_name: Option<String>,
    #[serde(rename = "lastName", default)]
    last_name: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct KoofrMount {
    id: String,
    name: String,
    #[serde(rename = "type", default)]
    mount_type: Option<String>,
    #[serde(rename = "isPrimary", default)]
    is_primary: Option<bool>,
    #[serde(rename = "isShared", default)]
    is_shared: Option<bool>,
    #[serde(rename = "spaceTotal", default)]
    space_total: Option<i64>,
    #[serde(rename = "spaceUsed", default)]
    space_used: Option<i64>,
    #[serde(default)]
    online: bool,
    #[serde(rename = "canUpload", default)]
    can_upload: Option<bool>,
    #[serde(rename = "canWrite", default)]
    can_write: Option<bool>,
    #[serde(rename = "overQuota", default)]
    over_quota: Option<bool>,
    #[serde(rename = "origin", default)]
    origin: Option<String>,
}

/// Wrapper in case the API returns `{ "mounts": [...] }` instead of a bare array
#[derive(Debug, Deserialize)]
struct KoofrMountsResponse {
    mounts: Vec<KoofrMount>,
}

#[derive(Debug, Deserialize)]
struct KoofrFile {
    name: String,
    #[serde(rename = "type")]
    file_type: String,
    #[serde(default)]
    modified: i64,
    #[serde(default)]
    size: i64,
    #[serde(rename = "contentType")]
    content_type: Option<String>,
    hash: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    tags: Option<HashMap<String, Vec<String>>>,
}

#[derive(Debug, Deserialize)]
struct KoofrFileList {
    files: Vec<KoofrFile>,
}

#[derive(Debug, Deserialize)]
struct KoofrFileInfo {
    #[serde(flatten)]
    file: KoofrFile,
}

#[derive(Debug, Deserialize)]
struct KoofrLink {
    id: String,
    #[serde(default)]
    url: String,
    #[serde(rename = "shortUrl")]
    short_url: Option<String>,
    #[serde(rename = "hasPassword")]
    #[allow(dead_code)]
    has_password: Option<bool>,
    #[serde(rename = "validFrom", default)]
    valid_from: Option<String>,
    #[serde(rename = "validTo", default)]
    valid_to: Option<String>,
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct KoofrLinksResponse {
    links: Vec<KoofrLink>,
}

#[derive(Debug, Deserialize)]
struct KoofrVersionsResponse {
    versions: Vec<KoofrVersion>,
}

#[derive(Debug, Deserialize)]
struct KoofrVersion {
    id: String,
    #[serde(rename = "type")]
    #[allow(dead_code)]
    version_type: Option<String>,
    #[serde(default)]
    modified: i64,
    #[serde(default)]
    size: i64,
    #[serde(rename = "contentType")]
    #[allow(dead_code)]
    content_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct KoofrTrashResponse {
    files: Vec<KoofrTrashFile>,
    #[serde(rename = "pageInfo")]
    #[allow(dead_code)]
    page_info: Option<KoofrPageInfo>,
}

#[derive(Debug, Deserialize)]
pub struct KoofrTrashFile {
    name: String,
    path: String,
    #[serde(rename = "mountId")]
    #[allow(dead_code)]
    mount_id: Option<String>,
    #[serde(default)]
    size: i64,
    #[serde(default)]
    deleted: i64,
    #[serde(rename = "contentType")]
    content_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct KoofrPageInfo {
    #[allow(dead_code)]
    cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct KoofrSearchResponse {
    hits: Vec<KoofrSearchHit>,
}

#[derive(Debug, Deserialize)]
struct KoofrSearchHit {
    name: String,
    path: String,
    #[serde(rename = "type")]
    hit_type: String,
    #[serde(default)]
    modified: i64,
    #[serde(default)]
    size: i64,
    #[serde(rename = "contentType")]
    content_type: Option<String>,
    #[serde(rename = "mountId")]
    #[allow(dead_code)]
    mount_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct KoofrError {
    error: Option<KoofrErrorInner>,
}

#[derive(Debug, Deserialize)]
struct KoofrErrorInner {
    code: Option<String>,
    message: Option<String>,
}

// ─── Provider ───

pub struct KoofrProvider {
    config: KoofrConfig,
    client: reqwest::Client,
    connected: bool,
    mount_id: String,
    current_path: String,
    space_total: i64,
    space_used: i64,
    account_email: Option<String>,
    /// Multi-thread concurrent-Range download (rclone `--multi-thread-streams`).
    /// `1` = disabled (default). Set via `set_multi_thread_download`.
    multi_thread_streams: usize,
    /// Files at or above this size use the concurrent-Range path when
    /// `multi_thread_streams >= 2`.
    multi_thread_cutoff: u64,
    /// Test-only content API base (`http://127.0.0.1:port`) for local HTTP
    /// fixtures. Production paths never set it.
    #[cfg(test)]
    content_base_override: Option<String>,
    /// Test-only API base (`http://127.0.0.1:port`) for local HTTP fixtures.
    /// Production paths never set it.
    #[cfg(test)]
    api_base_override: Option<String>,
}

/// Provider-specific hard cap on concurrent Range streams (mirrors S3's 16).
const KOOFR_MULTI_THREAD_MAX_STREAMS: usize = 16;

impl KoofrProvider {
    pub fn new(config: KoofrConfig) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(crate::providers::AEROFTP_USER_AGENT)
            .connect_timeout(Duration::from_secs(30))
            // 1800s (30 min): see webdav.rs rationale. The Koofr 1 GiB
            // benchmark on a residential connection consistently exceeded
            // 5 minutes wall-clock per run.
            .read_timeout(Duration::from_secs(1800))
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            config,
            client,
            connected: false,
            mount_id: String::new(),
            current_path: "/".into(),
            space_total: 0,
            space_used: 0,
            account_email: None,
            multi_thread_streams: 1,
            multi_thread_cutoff: 8 * 1024 * 1024,
            #[cfg(test)]
            content_base_override: None,
            #[cfg(test)]
            api_base_override: None,
        }
    }

    /// Base of the REST API.
    fn api_base(&self) -> &str {
        #[cfg(test)]
        if let Some(base) = &self.api_base_override {
            return base.as_str();
        }
        API_BASE
    }

    /// Base of the content API (uploads and downloads).
    fn content_base(&self) -> &str {
        #[cfg(test)]
        if let Some(base) = &self.content_base_override {
            return base.as_str();
        }
        CONTENT_BASE
    }

    /// Build Basic Auth header: base64(email:password)
    fn auth_header(&self) -> Result<HeaderValue, ProviderError> {
        use base64::{engine::general_purpose::STANDARD, Engine};
        let credentials = format!(
            "{}:{}",
            self.config.email,
            self.config.password.expose_secret()
        );
        let encoded = STANDARD.encode(credentials.as_bytes());
        HeaderValue::from_str(&format!("Basic {}", encoded)).map_err(|e| {
            ProviderError::AuthenticationFailed(format!("Invalid characters in credentials: {}", e))
        })
    }

    fn api_url(&self, path: &str) -> String {
        format!("{}{}", self.api_base(), path)
    }

    fn version_header() -> (HeaderName, HeaderValue) {
        (
            HeaderName::from_static(KOOFR_VERSION_HEADER),
            HeaderValue::from_static("2.1"),
        )
    }

    /// GET with retry and auth
    async fn get(&self, url: &str) -> Result<reqwest::Response, ProviderError> {
        let (vk, vv) = Self::version_header();
        let request = self
            .client
            .get(url)
            .header(AUTHORIZATION, self.auth_header()?)
            .header(vk, vv)
            .build()
            .map_err(|e| ProviderError::ConnectionFailed(format!("Build request failed: {}", e)))?;
        send_with_retry(&self.client, request, &HttpRetryConfig::default())
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Request failed: {}", e)))
    }

    /// POST JSON with retry and auth
    async fn post_json(
        &self,
        url: &str,
        body: &impl Serialize,
    ) -> Result<reqwest::Response, ProviderError> {
        let json = serde_json::to_vec(body)
            .map_err(|e| ProviderError::InvalidConfig(format!("JSON serialize failed: {}", e)))?;
        let (vk, vv) = Self::version_header();
        let request = self
            .client
            .post(url)
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json; charset=utf-8")
            .header(vk, vv)
            .body(json)
            .build()
            .map_err(|e| ProviderError::ConnectionFailed(format!("Build request failed: {}", e)))?;
        send_with_retry(&self.client, request, &HttpRetryConfig::default())
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Request failed: {}", e)))
    }

    /// PUT JSON with retry and auth
    async fn put_json(
        &self,
        url: &str,
        body: &impl Serialize,
    ) -> Result<reqwest::Response, ProviderError> {
        let json = serde_json::to_vec(body)
            .map_err(|e| ProviderError::InvalidConfig(format!("JSON serialize failed: {}", e)))?;
        let (vk, vv) = Self::version_header();
        let request = self
            .client
            .put(url)
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json; charset=utf-8")
            .header(vk, vv)
            .body(json)
            .build()
            .map_err(|e| ProviderError::ConnectionFailed(format!("Build request failed: {}", e)))?;
        send_with_retry(&self.client, request, &HttpRetryConfig::default())
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Request failed: {}", e)))
    }

    /// DELETE with retry and auth
    async fn delete_req(&self, url: &str) -> Result<reqwest::Response, ProviderError> {
        let (vk, vv) = Self::version_header();
        let request = self
            .client
            .delete(url)
            .header(AUTHORIZATION, self.auth_header()?)
            .header(vk, vv)
            .build()
            .map_err(|e| ProviderError::ConnectionFailed(format!("Build request failed: {}", e)))?;
        send_with_retry(&self.client, request, &HttpRetryConfig::default())
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Request failed: {}", e)))
    }

    /// Parse error from response body
    async fn parse_error(resp: reqwest::Response) -> ProviderError {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        Self::classify_koofr_error(status, &body)
    }

    /// Pure status+body -> ProviderError classifier (extracted from `parse_error`
    /// so it can be unit-tested without a live `reqwest::Response`). The Koofr API
    /// returns a JSON `{ "error": { "code", "message" } }`; the body-level code
    /// drives the mapping, falling back to the HTTP status for the message shape.
    fn classify_koofr_error(status: u16, body: &str) -> ProviderError {
        // Preserve the original message shape: `reqwest::StatusCode` Displays as
        // "<code> <reason>" (e.g. "404 Not Found"), so reconstruct it from the u16.
        let status_text = reqwest::StatusCode::from_u16(status)
            .map(|s| s.to_string())
            .unwrap_or_else(|_| status.to_string());

        if let Ok(err) = serde_json::from_str::<KoofrError>(body) {
            if let Some(inner) = err.error {
                let code = inner.code.as_deref().unwrap_or("Unknown");
                let message = inner.message.as_deref().unwrap_or("Unknown error");
                return match code {
                    "NotFound" => ProviderError::NotFound(message.to_string()),
                    // A rename or move onto a taken name (409).
                    "AlreadyExists" => ProviderError::AlreadyExists(message.to_string()),
                    "Forbidden" => ProviderError::PermissionDenied(message.to_string()),
                    "Unauthorized" => ProviderError::AuthenticationFailed(message.to_string()),
                    _ => ProviderError::ServerError(format!(
                        "Koofr API error ({}): {}: {}",
                        status_text, code, message
                    )),
                };
            }
        }

        ProviderError::ServerError(format!(
            "Koofr API error ({}): {}",
            status_text,
            sanitize_api_error(body)
        ))
    }

    /// Check response status, returning Ok for success codes
    async fn check_response(resp: reqwest::Response) -> Result<reqwest::Response, ProviderError> {
        let status = resp.status();
        if status.is_success() || status.as_u16() == 303 {
            Ok(resp)
        } else if status.as_u16() == 401 {
            Err(ProviderError::AuthenticationFailed(
                "Invalid credentials. Generate an App Password at Koofr > Preferences > Password."
                    .into(),
            ))
        } else if status.as_u16() == 404 {
            Err(Self::parse_error(resp).await)
        } else if status.as_u16() == 429 {
            Err(ProviderError::ServerError(
                "Rate limit exceeded. Please retry later.".into(),
            ))
        } else {
            Err(Self::parse_error(resp).await)
        }
    }

    fn normalize_path(path: &str) -> String {
        let trimmed = path.trim().replace('\\', "/");
        if trimmed.is_empty() || trimmed == "/" {
            return "/".into();
        }
        let p = if trimmed.starts_with('/') {
            trimmed
        } else {
            format!("/{}", trimmed)
        };
        p.trim_end_matches('/').to_string()
    }

    fn resolve_path(&self, path: &str) -> String {
        let trimmed = path.trim();
        if trimmed.is_empty() || trimmed == "." {
            return self.current_path.clone();
        }
        let normalized = Self::normalize_path(trimmed);
        if normalized.starts_with('/') {
            normalized
        } else {
            let base = self.current_path.trim_end_matches('/');
            format!("{}/{}", base, normalized)
        }
    }

    /// Split "/a/b/file.txt" → ("/a/b", "file.txt")
    fn split_path(path: &str) -> (&str, &str) {
        match path.rfind('/') {
            Some(0) => ("/", &path[1..]),
            Some(pos) => (&path[..pos], &path[pos + 1..]),
            None => ("/", path),
        }
    }

    /// Convert millisecond timestamp to human-readable date
    fn format_timestamp(ms: i64) -> Option<String> {
        if ms <= 0 {
            return None;
        }
        let secs = ms / 1000;
        chrono::DateTime::from_timestamp(secs, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%SZ").to_string())
    }

    fn file_to_entry(&self, file: &KoofrFile, parent_path: &str) -> RemoteEntry {
        let is_dir = file.file_type == "dir";
        let path = if parent_path == "/" {
            format!("/{}", file.name)
        } else {
            format!("{}/{}", parent_path, file.name)
        };

        let mut metadata = HashMap::new();
        if let Some(ref hash) = file.hash {
            metadata.insert("hash".to_string(), hash.clone());
        }
        if let Some(ref ct) = file.content_type {
            metadata.insert("content_type".to_string(), ct.clone());
        }

        RemoteEntry {
            name: file.name.clone(),
            path,
            is_dir,
            size: file.size.max(0) as u64,
            modified: Self::format_timestamp(file.modified),
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            metadata,
            mime_type: file.content_type.clone(),
        }
    }
}

#[async_trait]
impl StorageProvider for KoofrProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Koofr
    }

    fn display_name(&self) -> String {
        format!("Koofr ({})", self.config.email)
    }

    fn account_email(&self) -> Option<String> {
        self.account_email.clone()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        koofr_log(&format!("Connecting as {}", self.config.email));

        // 1. Verify credentials by fetching user info
        let resp = self.get(&self.api_url("/user")).await?;
        let resp = Self::check_response(resp).await?;
        let user_body = resp.text().await.map_err(|e| {
            ProviderError::ConnectionFailed(format!("Failed to read user body: {}", e))
        })?;

        koofr_log(&format!(
            "User response: {}",
            &user_body[..user_body.floor_char_boundary(300)]
        ));

        let user: KoofrUser = serde_json::from_str(&user_body).map_err(|e| {
            ProviderError::ConnectionFailed(format!(
                "Failed to parse user: {}. Body: {}",
                e,
                &user_body[..user_body.floor_char_boundary(200)]
            ))
        })?;

        self.account_email = user.email.clone();
        koofr_log(&format!(
            "Authenticated as {} {}",
            mask_credential(user.first_name.as_deref().unwrap_or("")),
            mask_credential(user.last_name.as_deref().unwrap_or(""))
        ));

        // 2. Get mounts and find primary
        let resp = self.get(&self.api_url("/mounts")).await?;
        let resp = Self::check_response(resp).await?;
        let body = resp.text().await.map_err(|e| {
            ProviderError::ConnectionFailed(format!("Failed to read mounts body: {}", e))
        })?;

        koofr_log(&format!(
            "Mounts response ({} bytes): {}",
            body.len(),
            &body[..body.floor_char_boundary(500)]
        ));

        // Try parsing as bare array first, then as wrapped { "mounts": [...] }
        let mounts: Vec<KoofrMount> = serde_json::from_str(&body)
            .or_else(|_| serde_json::from_str::<KoofrMountsResponse>(&body).map(|r| r.mounts))
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!(
                    "Failed to parse mounts: {}. Body preview: {}",
                    e,
                    &body[..body.floor_char_boundary(200)]
                ))
            })?;

        let primary = mounts
            .iter()
            .find(|m| m.is_primary == Some(true) && m.online)
            .or_else(|| mounts.iter().find(|m| m.online))
            .ok_or_else(|| {
                ProviderError::ConnectionFailed(
                    "No online mount found. Your Koofr storage may be offline.".into(),
                )
            })?;

        self.mount_id = primary.id.clone();

        // Fetch detailed mount info for accurate quota (list may omit spaceTotal/spaceUsed)
        // NOTE: Koofr API returns spaceTotal/spaceUsed in MiB: multiply by 1024*1024 for bytes
        const MIB: i64 = 1024 * 1024;
        let mount_detail_url = format!("{}/mounts/{}", self.api_base(), self.mount_id);
        match self.get(&mount_detail_url).await {
            Ok(resp) => {
                let detail_body = resp.text().await.unwrap_or_default();
                koofr_log(&format!(
                    "Mount detail response: {}",
                    &detail_body[..detail_body.floor_char_boundary(500)]
                ));

                // Try bare KoofrMount first, then raw Value extraction
                if let Ok(detail) = serde_json::from_str::<KoofrMount>(&detail_body) {
                    self.space_total = detail.space_total.unwrap_or(0) * MIB;
                    self.space_used = detail.space_used.unwrap_or(0) * MIB;
                } else if let Ok(val) = serde_json::from_str::<serde_json::Value>(&detail_body) {
                    self.space_total =
                        val.get("spaceTotal").and_then(|v| v.as_i64()).unwrap_or(0) * MIB;
                    self.space_used =
                        val.get("spaceUsed").and_then(|v| v.as_i64()).unwrap_or(0) * MIB;
                } else {
                    koofr_log("Mount detail parse failed, using list values");
                    self.space_total = primary.space_total.unwrap_or(0) * MIB;
                    self.space_used = primary.space_used.unwrap_or(0) * MIB;
                }
                koofr_log(&format!(
                    "Quota after MiB→bytes: total={}, used={}",
                    self.space_total, self.space_used
                ));
            }
            Err(e) => {
                koofr_log(&format!(
                    "Mount detail fetch failed: {}, using list values",
                    e
                ));
                self.space_total = primary.space_total.unwrap_or(0) * MIB;
                self.space_used = primary.space_used.unwrap_or(0) * MIB;
            }
        }

        koofr_log(&format!(
            "Using mount '{}' (id={}, {:.1} GB / {:.1} GB)",
            primary.name,
            self.mount_id,
            self.space_used as f64 / 1_073_741_824.0,
            self.space_total as f64 / 1_073_741_824.0
        ));

        // 3. Navigate to initial path if specified
        self.current_path = "/".into();
        if let Some(ref initial) = self.config.initial_path {
            let normalized = Self::normalize_path(initial);
            if !normalized.is_empty() && normalized != "/" {
                // Verify path exists
                let url = format!(
                    "{}/mounts/{}/files/info?path={}",
                    self.api_base(),
                    self.mount_id,
                    urlencoding::encode(&normalized)
                );
                match self.get(&url).await {
                    Ok(resp) if resp.status().is_success() => {
                        self.current_path = normalized;
                    }
                    _ => {
                        koofr_log(&format!("Initial path '{}' not found, using root", initial));
                    }
                }
            }
        }

        self.connected = true;
        koofr_log("Connected successfully");
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        self.mount_id.clear();
        self.current_path = "/".into();
        koofr_log("Disconnected");
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let resolved = self.resolve_path(path);
        let url = format!(
            "{}/mounts/{}/files/list?path={}",
            self.api_base(),
            self.mount_id,
            urlencoding::encode(&resolved)
        );

        let resp = self.get(&url).await?;
        let resp = Self::check_response(resp).await?;
        let file_list: KoofrFileList = resp
            .json()
            .await
            .map_err(|e| ProviderError::ServerError(format!("Failed to parse file list: {}", e)))?;

        let entries: Vec<RemoteEntry> = file_list
            .files
            .iter()
            .map(|f| self.file_to_entry(f, &resolved))
            .collect();

        // Update current_path on successful listing
        self.current_path = resolved;
        Ok(entries)
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let target = self.resolve_path(path);

        // Verify directory exists
        let url = format!(
            "{}/mounts/{}/files/info?path={}",
            self.api_base(),
            self.mount_id,
            urlencoding::encode(&target)
        );
        let resp = self.get(&url).await?;
        let resp = Self::check_response(resp).await?;
        let info: KoofrFileInfo = resp
            .json()
            .await
            .map_err(|e| ProviderError::ServerError(format!("Failed to parse file info: {}", e)))?;

        if info.file.file_type != "dir" {
            return Err(ProviderError::NotFound(format!(
                "'{}' is not a directory",
                target
            )));
        }

        self.current_path = target;
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        if self.current_path == "/" {
            return Ok(());
        }
        let current = self.current_path.clone();
        let (parent, _) = Self::split_path(&current);
        self.current_path = if parent.is_empty() {
            "/".into()
        } else {
            parent.to_string()
        };
        Ok(())
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        self.download_with_size_hint(remote_path, local_path, None, on_progress)
            .await
    }

    async fn download_with_size_hint(
        &mut self,
        remote_path: &str,
        local_path: &str,
        size_hint: Option<u64>,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let mut on_progress = on_progress;

        let resolved = self.resolve_path(remote_path);
        let url = format!(
            "{}/mounts/{}/files/get?path={}",
            self.content_base(),
            self.mount_id,
            urlencoding::encode(&resolved)
        );

        // PD-HTTP-2: concurrent-Range download behind a real strict 206 probe.
        // Koofr content auth is a static Basic header (no per-request nonce),
        // so it is safe to replay concurrently through the live client.
        if self.multi_thread_streams >= 2
            && !super::multi_thread::size_hint_rules_out_ranges(size_hint, self.multi_thread_cutoff)
        {
            let req = super::multi_thread::HttpRangeRequest {
                client: self.client.clone(),
                url: url.clone(),
                headers: vec![(AUTHORIZATION, self.auth_header()?)],
                local_path: local_path.to_string(),
                provider_type: ProviderType::Koofr,
                streams: self.multi_thread_streams,
                max_streams: KOOFR_MULTI_THREAD_MAX_STREAMS,
                cutoff: self.multi_thread_cutoff,
                known_size: size_hint,
            };
            match super::multi_thread::try_http_concurrent_range_download(req, on_progress).await {
                super::multi_thread::HttpRangeAttempt::Completed => return Ok(()),
                super::multi_thread::HttpRangeAttempt::Failed(e) => return Err(e),
                super::multi_thread::HttpRangeAttempt::Fallback(p) => on_progress = p,
            }
        }

        let request = self
            .client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .build()
            .map_err(|e| ProviderError::TransferFailed(format!("Build request failed: {}", e)))?;

        let resp = send_with_retry(&self.client, request, &HttpRetryConfig::default())
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Download failed: {}", e)))?;

        if !resp.status().is_success() {
            return Err(Self::parse_error(resp).await);
        }

        // Streaming download
        use futures_util::StreamExt;

        let total_size = resp.content_length().unwrap_or(0);
        let mut stream = Box::pin(crate::transfer_dag::throttle::throttle_stream(
            resp.bytes_stream(),
            crate::transfer_dag::governor::TransferDirection::Download,
        ));
        let mut atomic = super::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Create file failed: {}", e)))?;
        let mut downloaded: u64 = 0;

        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|e| ProviderError::TransferFailed(format!("Stream error: {}", e)))?;
            atomic
                .write_all(&chunk)
                .await
                .map_err(|e| ProviderError::TransferFailed(format!("Write error: {}", e)))?;
            downloaded += chunk.len() as u64;
            if let Some(ref cb) = on_progress {
                cb(downloaded, total_size);
            }
        }
        atomic.commit().await.map_err(|e| {
            ProviderError::TransferFailed(format!("Failed to finalize download: {}", e))
        })?;

        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let resolved = self.resolve_path(remote_path);
        let url = format!(
            "{}/mounts/{}/files/get?path={}",
            self.content_base(),
            self.mount_id,
            urlencoding::encode(&resolved)
        );

        let resp = self.get(&url).await?;
        if !resp.status().is_success() {
            return Err(Self::parse_error(resp).await);
        }

        response_bytes_with_limit(resp, MAX_DOWNLOAD_TO_BYTES).await
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let resolved = self.resolve_path(remote_path);
        let (parent, filename) = Self::split_path(&resolved);

        // Get file metadata
        let file_meta = tokio::fs::metadata(local_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Cannot read file: {}", e)))?;
        let file_size = file_meta.len();

        // The body reports the bytes as they go out; 100 percent waits for
        // Koofr's answer (see `UploadProgress`).
        let progress = super::upload_progress::UploadProgress::new(on_progress, file_size);
        progress.start();

        // Preserve modification time
        let modified_ms = std::fs::metadata(local_path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        let url = format!(
            "{}/mounts/{}/files/put?path={}&filename={}&autorename=true&overwrite=true&info=true{}",
            self.content_base(),
            self.mount_id,
            urlencoding::encode(parent),
            urlencoding::encode(filename),
            if modified_ms > 0 {
                format!("&modified={}", modified_ms)
            } else {
                String::new()
            }
        );

        // Streaming upload via ReaderStream
        let file = tokio::fs::File::open(local_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Open file failed: {}", e)))?;
        let body = progress.file_body(file);

        let resp = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(body)
            .send()
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Upload failed: {}", e)))?;

        if !resp.status().is_success() {
            return Err(Self::parse_error(resp).await);
        }

        progress.complete();

        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let resolved = self.resolve_path(path);
        let (parent, folder_name) = Self::split_path(&resolved);

        let url = format!(
            "{}/mounts/{}/files/folder?path={}",
            self.api_base(),
            self.mount_id,
            urlencoding::encode(parent)
        );

        #[derive(Serialize)]
        struct CreateFolder {
            name: String,
        }

        let resp = self
            .post_json(
                &url,
                &CreateFolder {
                    name: folder_name.to_string(),
                },
            )
            .await?;
        Self::check_response(resp).await?;
        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let resolved = self.resolve_path(path);
        let url = format!(
            "{}/mounts/{}/files/remove?path={}",
            self.api_base(),
            self.mount_id,
            urlencoding::encode(&resolved)
        );

        let resp = self.delete_req(&url).await?;
        Self::check_response(resp).await?;
        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        // Round 2 of the 4.2.1 review: the API's delete takes a folder's
        // content along, so a folder that lists anything is refused here and
        // only one that listed empty reaches it.
        // `list` moves this session into the listed folder; the check must
        // not, or the delete and every later relative path would resolve
        // from inside the folder being removed (CodeRabbit on #979).
        let saved_current_path = self.current_path.clone();
        let checked = self.refuse_non_empty_dir(path).await;
        self.current_path = saved_current_path;
        checked?;
        // Koofr uses the same remove endpoint for files and directories
        self.delete(path).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        // Koofr's remove endpoint handles recursive deletion
        self.delete(path).await
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let from_resolved = self.resolve_path(from);
        let to_resolved = self.resolve_path(to);
        if from_resolved == to_resolved {
            return Ok(());
        }

        let (from_parent, _) = Self::split_path(&from_resolved);
        let (to_parent, to_name) = Self::split_path(&to_resolved);

        // If same parent directory → rename, otherwise → move
        if from_parent == to_parent {
            let url = format!(
                "{}/mounts/{}/files/rename?path={}",
                self.api_base(),
                self.mount_id,
                urlencoding::encode(&from_resolved)
            );

            #[derive(Serialize)]
            struct RenameRequest {
                name: String,
            }

            let resp = self
                .put_json(
                    &url,
                    &RenameRequest {
                        name: to_name.to_string(),
                    },
                )
                .await?;
            Self::check_response(resp).await?;
        } else {
            // Move to different directory
            let url = format!(
                "{}/mounts/{}/files/move?path={}",
                self.api_base(),
                self.mount_id,
                urlencoding::encode(&from_resolved)
            );

            #[derive(Serialize)]
            struct MoveRequest {
                #[serde(rename = "toMountId")]
                to_mount_id: String,
                #[serde(rename = "toPath")]
                to_path: String,
            }

            let resp = self
                .put_json(
                    &url,
                    &MoveRequest {
                        to_mount_id: self.mount_id.clone(),
                        to_path: to_resolved,
                    },
                )
                .await?;
            Self::check_response(resp).await?;
        }

        Ok(())
    }

    /// Koofr's rename and move refuse a taken name and offer no overwrite, so a
    /// replace sets the item at `to` aside, renames `from` in, and then deletes
    /// the one set aside, which Koofr keeps in its trash: see
    /// [`super::replace_by_setting_aside`].
    async fn replace(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let (from, to) = (self.resolve_path(from), self.resolve_path(to));
        super::replace_by_setting_aside(self, &from, &to).await
    }

    /// No: a replace sets the old item aside, so the name is empty for a
    /// moment. The callers that need atomicity (CLI `edit`, MCP
    /// `remote_edit`, the crypt marker paths) refuse before they write
    /// anything.
    async fn supports_atomic_replace(&mut self) -> Result<bool, ProviderError> {
        Ok(false)
    }

    /// Yes: the replace above renames the item at the destination aside,
    /// moves the new one in, and only then deletes the old one, which is
    /// what an edit's non-atomic opt-in needs.
    fn replace_sets_aside(&self) -> bool {
        true
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let resolved = self.resolve_path(path);
        let url = format!(
            "{}/mounts/{}/files/info?path={}",
            self.api_base(),
            self.mount_id,
            urlencoding::encode(&resolved)
        );

        let resp = self.get(&url).await?;
        let resp = Self::check_response(resp).await?;
        let info: KoofrFileInfo = resp
            .json()
            .await
            .map_err(|e| ProviderError::ServerError(format!("Failed to parse file info: {}", e)))?;

        let (parent, _) = Self::split_path(&resolved);
        Ok(self.file_to_entry(&info.file, parent))
    }

    async fn size(&mut self, path: &str) -> Result<u64, ProviderError> {
        let entry = self.stat(path).await?;
        Ok(entry.size)
    }

    async fn exists(&mut self, path: &str) -> Result<bool, ProviderError> {
        match self.stat(path).await {
            Ok(_) => Ok(true),
            Err(ProviderError::NotFound(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn keep_alive(&mut self) -> Result<(), ProviderError> {
        // Verify auth is still valid
        let resp = self.get(&self.api_url("/user/authenticated")).await?;
        if resp.status().as_u16() == 204 {
            Ok(())
        } else {
            Err(ProviderError::AuthenticationFailed(
                "Session expired".into(),
            ))
        }
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok(format!(
            "Koofr Cloud Storage: Mount: {}: {:.1} GB / {:.1} GB",
            self.mount_id,
            self.space_used as f64 / 1_073_741_824.0,
            self.space_total as f64 / 1_073_741_824.0,
        ))
    }

    // ─── Advanced Capabilities ───

    fn supports_server_copy(&self) -> bool {
        true
    }

    fn supports_server_side_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        // Legacy alias kept so CLI / MCP / provider_commands callers keep
        // working. The real `/mounts/<id>/files/copy` implementation lives
        // on `server_side_copy` (S3-T10 migration, v4.0.0).
        StorageProvider::server_side_copy(self, from, to).await
    }

    async fn server_side_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let from_resolved = self.resolve_path(from);
        let to_resolved = self.resolve_path(to);

        let url = format!(
            "{}/mounts/{}/files/copy?path={}",
            self.api_base(),
            self.mount_id,
            urlencoding::encode(&from_resolved)
        );

        #[derive(Serialize)]
        struct CopyRequest {
            #[serde(rename = "toMountId")]
            to_mount_id: String,
            #[serde(rename = "toPath")]
            to_path: String,
        }

        let resp = self
            .put_json(
                &url,
                &CopyRequest {
                    to_mount_id: self.mount_id.clone(),
                    to_path: to_resolved,
                },
            )
            .await?;
        Self::check_response(resp).await?;
        Ok(())
    }

    fn supports_share_links(&self) -> bool {
        true
    }

    fn share_link_capabilities(&self) -> ShareLinkCapabilities {
        ShareLinkCapabilities {
            supports_expiration: false,
            supports_password: false,
            supports_permissions: false,
            available_permissions: vec![],
            supports_list_links: true,
            supports_revoke: true,
        }
    }

    async fn create_share_link(
        &mut self,
        path: &str,
        options: ShareLinkOptions,
    ) -> Result<ShareLinkResult, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let resolved = self.resolve_path(path);
        let url = format!("{}/mounts/{}/links", self.api_base(), self.mount_id);

        #[derive(Serialize)]
        struct CreateLink {
            path: String,
        }

        let resp = self.post_json(&url, &CreateLink { path: resolved }).await?;

        let status = resp.status();
        if !status.is_success() && status.as_u16() != 201 {
            return Err(Self::parse_error(resp).await);
        }

        let link: KoofrLink = resp
            .json()
            .await
            .map_err(|e| ProviderError::ServerError(format!("Failed to parse link: {}", e)))?;

        let _ = &options; // acknowledge options
        Ok(ShareLinkResult {
            url: link.short_url.unwrap_or(link.url),
            password: None,
            expires_at: None,
        })
    }

    async fn list_share_links(&mut self, path: &str) -> Result<Vec<ShareLinkInfo>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let url = format!("{}/mounts/{}/links", self.api_base(), self.mount_id);
        let resp = self.get(&url).await?;
        let resp = Self::check_response(resp).await?;
        let links_resp: KoofrLinksResponse = resp
            .json()
            .await
            .map_err(|e| ProviderError::ServerError(format!("Failed to parse links: {}", e)))?;

        let resolved = self.resolve_path(path);
        let links = links_resp
            .links
            .into_iter()
            .filter(|link| {
                if let Some(ref p) = link.path {
                    p == &resolved
                } else {
                    link.url.contains(&resolved)
                }
            })
            .map(|link| ShareLinkInfo {
                id: link.id,
                url: link.short_url.unwrap_or(link.url),
                created_at: link.valid_from,
                expires_at: link.valid_to,
                password_protected: link.has_password.unwrap_or(false),
                permissions: None,
            })
            .collect();

        Ok(links)
    }

    async fn remove_share_link(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        // First, find the link for this path
        let url = format!("{}/mounts/{}/links", self.api_base(), self.mount_id);
        let resp = self.get(&url).await?;
        let resp = Self::check_response(resp).await?;
        let links: KoofrLinksResponse = resp
            .json()
            .await
            .map_err(|e| ProviderError::ServerError(format!("Failed to parse links: {}", e)))?;

        let resolved = self.resolve_path(path);
        // No link found is not an error
        for link in &links.links {
            if link.url.contains(&resolved) || link.id == resolved {
                let delete_url = format!(
                    "{}/mounts/{}/links/{}",
                    self.api_base(),
                    self.mount_id,
                    link.id
                );
                let resp = self.delete_req(&delete_url).await?;
                Self::check_response(resp).await?;
                return Ok(());
            }
        }

        Ok(())
    }

    async fn remove_share_link_by_id(
        &mut self,
        _path: &str,
        link_id: &str,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        // The id becomes a URL path segment.
        if !super::is_link_id(link_id) {
            return Err(ProviderError::InvalidPath(format!(
                "Not a Koofr link id: {link_id}"
            )));
        }
        let delete_url = format!(
            "{}/mounts/{}/links/{}",
            self.api_base(),
            self.mount_id,
            link_id
        );
        let resp = self.delete_req(&delete_url).await?;
        Self::check_response(resp).await?;
        Ok(())
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        // Refresh mount info: Koofr returns spaceTotal/spaceUsed in MiB
        const MIB: i64 = 1024 * 1024;
        let url = format!("{}/mounts/{}", self.api_base(), self.mount_id);
        let resp = self.get(&url).await?;
        let resp = Self::check_response(resp).await?;
        let body = resp.text().await.map_err(|e| {
            ProviderError::ServerError(format!("Failed to read mount response: {}", e))
        })?;
        koofr_log(&format!(
            "storage_info mount response: {}",
            &body[..body.floor_char_boundary(500)]
        ));

        // Try typed parse first, then raw Value extraction
        if let Ok(mount) = serde_json::from_str::<KoofrMount>(&body) {
            self.space_total = mount.space_total.unwrap_or(0) * MIB;
            self.space_used = mount.space_used.unwrap_or(0) * MIB;
        } else if let Ok(val) = serde_json::from_str::<serde_json::Value>(&body) {
            self.space_total = val.get("spaceTotal").and_then(|v| v.as_i64()).unwrap_or(0) * MIB;
            self.space_used = val.get("spaceUsed").and_then(|v| v.as_i64()).unwrap_or(0) * MIB;
        } else {
            return Err(ProviderError::ServerError(format!(
                "Failed to parse mount info. Body preview: {}",
                &body[..body.floor_char_boundary(200)]
            )));
        }

        let total = self.space_total.max(0) as u64;
        let used = self.space_used.max(0) as u64;

        Ok(StorageInfo {
            total,
            used,
            free: total.saturating_sub(used),
            versioning_bytes: None,
        })
    }

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(&mut self, path: &str, pattern: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let resolved = self.resolve_path(path);

        // Koofr /search?query= is substring on filename and ignores globs.
        // Strip glob chars for the broad server prefilter, then apply the
        // precise filter client-side via matches_find_pattern.
        let literal: String = pattern
            .chars()
            .filter(|c| !matches!(c, '*' | '?' | '[' | ']'))
            .collect();
        let server_query = if literal.is_empty() {
            ".".to_string()
        } else {
            literal
        };

        let url = format!(
            "{}/search?query={}&mountId={}&path={}&limit=256",
            self.api_base(),
            urlencoding::encode(&server_query),
            urlencoding::encode(&self.mount_id),
            urlencoding::encode(&resolved)
        );

        let resp = self.get(&url).await?;
        let resp = Self::check_response(resp).await?;
        let search: KoofrSearchResponse = resp
            .json()
            .await
            .map_err(|e| ProviderError::ServerError(format!("Failed to parse search: {}", e)))?;

        let entries: Vec<RemoteEntry> = search
            .hits
            .iter()
            .filter(|hit| super::matches_find_pattern(&hit.name, pattern))
            .map(|hit| RemoteEntry {
                name: hit.name.clone(),
                path: hit.path.clone(),
                is_dir: hit.hit_type == "dir",
                size: hit.size.max(0) as u64,
                modified: Self::format_timestamp(hit.modified),
                permissions: None,
                owner: None,
                group: None,
                is_symlink: false,
                link_target: None,
                metadata: HashMap::new(),
                mime_type: hit.content_type.clone(),
            })
            .collect();

        Ok(entries)
    }

    fn supports_resume(&self) -> bool {
        true
    }

    async fn resume_download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        offset: u64,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let resolved = self.resolve_path(remote_path);
        let url = format!(
            "{}/mounts/{}/files/get?path={}",
            self.content_base(),
            self.mount_id,
            urlencoding::encode(&resolved)
        );

        let range_value = HeaderValue::from_str(&format!("bytes={}-", offset))
            .map_err(|e| ProviderError::TransferFailed(format!("Invalid range: {}", e)))?;

        let request = self
            .client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .header(RANGE, range_value)
            .build()
            .map_err(|e| ProviderError::TransferFailed(format!("Build request failed: {}", e)))?;

        let resp = send_with_retry(&self.client, request, &HttpRetryConfig::default())
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Resume download failed: {}", e)))?;

        if !resp.status().is_success() {
            return Err(Self::parse_error(resp).await);
        }

        use futures_util::StreamExt;
        use tokio::io::AsyncWriteExt;

        let total_size = resp.content_length().unwrap_or(0) + offset;
        let mut stream = Box::pin(crate::transfer_dag::throttle::throttle_stream(
            resp.bytes_stream(),
            crate::transfer_dag::governor::TransferDirection::Download,
        ));

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(local_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Open file failed: {}", e)))?;

        let mut downloaded = offset;
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|e| ProviderError::TransferFailed(format!("Stream error: {}", e)))?;
            file.write_all(&chunk)
                .await
                .map_err(|e| ProviderError::TransferFailed(format!("Write error: {}", e)))?;
            downloaded += chunk.len() as u64;
            if let Some(ref cb) = on_progress {
                cb(downloaded, total_size);
            }
        }
        file.flush()
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Flush error: {}", e)))?;

        Ok(())
    }

    fn supports_checksum(&self) -> bool {
        true
    }

    async fn checksum(&mut self, path: &str) -> Result<HashMap<String, String>, ProviderError> {
        let entry = self.stat(path).await?;
        let mut result = HashMap::new();
        if let Some(hash) = entry.metadata.get("hash") {
            result.insert("koofr".to_string(), hash.clone());
        }
        Ok(result)
    }

    fn supports_versions(&self) -> bool {
        true
    }

    async fn list_versions(&mut self, path: &str) -> Result<Vec<FileVersion>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let resolved = self.resolve_path(path);
        let url = format!(
            "{}/mounts/{}/files/versions?path={}",
            self.api_base(),
            self.mount_id,
            urlencoding::encode(&resolved)
        );

        let resp = self.get(&url).await?;
        let resp = Self::check_response(resp).await?;
        let versions: KoofrVersionsResponse = resp
            .json()
            .await
            .map_err(|e| ProviderError::ServerError(format!("Failed to parse versions: {}", e)))?;

        let result: Vec<FileVersion> = versions
            .versions
            .iter()
            .map(|v| FileVersion {
                id: v.id.clone(),
                modified: Self::format_timestamp(v.modified),
                size: v.size.max(0) as u64,
                modified_by: None,
            })
            .collect();

        Ok(result)
    }

    async fn download_version(
        &mut self,
        path: &str,
        version_id: &str,
        local_path: &str,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        // The Koofr content endpoint supports version downloads
        // by appending the version query parameter
        let resolved = self.resolve_path(path);
        let url = format!(
            "{}/mounts/{}/files/get?path={}&version={}",
            self.content_base(),
            self.mount_id,
            urlencoding::encode(&resolved),
            urlencoding::encode(version_id)
        );

        let request = self
            .client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .build()
            .map_err(|e| ProviderError::TransferFailed(format!("Build request failed: {}", e)))?;

        let resp = send_with_retry(&self.client, request, &HttpRetryConfig::default())
            .await
            .map_err(|e| {
                ProviderError::TransferFailed(format!("Download version failed: {}", e))
            })?;

        if !resp.status().is_success() {
            return Err(Self::parse_error(resp).await);
        }

        use futures_util::StreamExt;

        let mut stream = Box::pin(crate::transfer_dag::throttle::throttle_stream(
            resp.bytes_stream(),
            crate::transfer_dag::governor::TransferDirection::Download,
        ));
        let mut atomic = super::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Create file failed: {}", e)))?;

        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|e| ProviderError::TransferFailed(format!("Stream error: {}", e)))?;
            atomic
                .write_all(&chunk)
                .await
                .map_err(|e| ProviderError::TransferFailed(format!("Write error: {}", e)))?;
        }
        atomic.commit().await.map_err(|e| {
            ProviderError::TransferFailed(format!("Failed to finalize download: {}", e))
        })?;

        Ok(())
    }

    async fn restore_version(&mut self, path: &str, version_id: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let resolved = self.resolve_path(path);
        let url = format!(
            "{}/mounts/{}/files/versions/change?path={}&version={}",
            self.api_base(),
            self.mount_id,
            urlencoding::encode(&resolved),
            urlencoding::encode(version_id)
        );

        let resp = self.post_json(&url, &serde_json::json!({})).await?;
        Self::check_response(resp).await?;
        Ok(())
    }

    fn supports_delta_sync(&self) -> bool {
        true
    }

    async fn read_range(
        &mut self,
        path: &str,
        offset: u64,
        len: u64,
    ) -> Result<Vec<u8>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let resolved = self.resolve_path(path);
        let url = format!(
            "{}/mounts/{}/files/get?path={}",
            self.content_base(),
            self.mount_id,
            urlencoding::encode(&resolved)
        );

        // len == 0 has no range, and offset + len - 1 must not wrap: a wrapped
        // end makes an invalid Range header that servers ignore, turning a
        // bounded probe into a full-body download.
        if len == 0 {
            return Ok(Vec::new());
        }
        let end = offset.checked_add(len - 1).ok_or_else(|| {
            ProviderError::TransferFailed("read_range end overflows u64".to_string())
        })?;
        let range_value = HeaderValue::from_str(&format!("bytes={}-{}", offset, end))
            .map_err(|e| ProviderError::TransferFailed(format!("Invalid range: {}", e)))?;

        let request = self
            .client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .header(RANGE, range_value)
            .build()
            .map_err(|e| ProviderError::TransferFailed(format!("Build request failed: {}", e)))?;

        let resp = send_with_retry(&self.client, request, &HttpRetryConfig::default())
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Range read failed: {}", e)))?;

        if !resp.status().is_success() {
            return Err(Self::parse_error(resp).await);
        }

        // The caller writes what comes back at the offset it asked for, so a
        // whole file answered to a ranged request would put the head of the
        // object there and still add up to the right length, and a 206 that
        // does not name its window would do the same with a success code.
        let status = resp.status();
        let answered = resp
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_string());
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Read bytes failed: {}", e)))?;
        match super::multi_thread::ranged_answer(
            status,
            answered.as_deref(),
            bytes.len() as u64,
            offset,
            end,
        ) {
            Ok(super::multi_thread::RangedAnswer::Window) => Ok(bytes.to_vec()),
            Ok(super::multi_thread::RangedAnswer::WholeObject) => {
                Ok(super::multi_thread::slice_whole_object(&bytes, offset, len))
            }
            Err(why) => Err(ProviderError::ParallelRefused(
                super::multi_thread::parallel_refused("Koofr range read", path, &why),
            )),
        }
    }

    fn transfer_optimization_hints(&self) -> TransferOptimizationHints {
        // Shaped-graph multipart trait (S3-T09): intentionally NotSupported
        // by design on this Koofr native-API path.
        //
        // The Koofr REST API exposes upload via
        // `PUT /content/api/v2/mounts/<mount>/files/put?path=…` as a
        // single-shot streaming PUT against the content endpoint. There
        // is no documented chunked append/commit endpoint, no resumable
        // session URL, and no per-chunk offset write primitive. Real
        // file-level parallelism for Koofr customers comes from the
        // WebDAV gateway, which `WebDavProvider` already covers with
        // its Nextcloud-style chunked upload trait wiring (when the
        // host is detected as a Nextcloud-compatible endpoint - see
        // `is_nextcloud_for_dav()` in webdav.rs). For Koofr's own native
        // gateway (`app.koofr.net`) the gateway returns single-PUT
        // semantics, so no per-part fan-out is feasible.
        //
        // The legacy upload() path already streams the body via
        // `tokio_util::io::ReaderStream` so large files do not pin RAM.
        // We leave `supports_multipart=false` and let the runner pick
        // the legacy single-stream path.
        TransferOptimizationHints {
            supports_resume_download: true,
            // The Koofr content endpoint honours HTTP Range (used by
            // `read_range` and, after a live strict 206 probe, by the
            // PD-HTTP-2 concurrent-Range download path).
            supports_range_download: true,
            supports_server_checksum: true,
            preferred_checksum_algo: Some("koofr".to_string()),
            supports_delta_sync: true,
            ..Default::default()
        }
    }

    fn set_multi_thread_download(&mut self, streams: usize, cutoff_bytes: u64) {
        self.multi_thread_streams = streams.clamp(1, KOOFR_MULTI_THREAD_MAX_STREAMS);
        self.multi_thread_cutoff = cutoff_bytes;
    }

    fn planned_download_segments(&self, file_size: u64) -> usize {
        // No provider floor: the explicit cutoff applies as-is. The live
        // single-file gate runs inside the shared HTTP helper with the same
        // planner inputs.
        crate::provider_transfer_executor::plan_segment_count(
            file_size,
            self.multi_thread_streams,
            KOOFR_MULTI_THREAD_MAX_STREAMS,
            crate::provider_transfer_executor::SegmentCutoff::Explicit(self.multi_thread_cutoff),
            0,
        )
    }
}

// ─── Koofr-specific operations (exposed via Tauri commands) ───

impl KoofrProvider {
    /// List trash items
    pub async fn list_trash(&self) -> Result<Vec<KoofrTrashFile>, ProviderError> {
        let url = format!("{}/trash?pageSize=1000", self.api_base());
        let resp = self.get(&url).await?;
        let resp = Self::check_response(resp).await?;
        let trash: KoofrTrashResponse = resp
            .json()
            .await
            .map_err(|e| ProviderError::ServerError(format!("Failed to parse trash: {}", e)))?;
        Ok(trash.files)
    }

    /// Restore files from trash
    pub async fn restore_from_trash(
        &self,
        files: Vec<(String, String)>, // (mount_id, path) pairs
    ) -> Result<(), ProviderError> {
        let url = format!("{}/trash/undelete", self.api_base());

        #[derive(Serialize)]
        struct UndeleteRequest {
            files: Vec<UndeleteFile>,
        }
        #[derive(Serialize)]
        struct UndeleteFile {
            #[serde(rename = "mountId")]
            mount_id: String,
            path: String,
        }

        let body = UndeleteRequest {
            files: files
                .into_iter()
                .map(|(m, p)| UndeleteFile {
                    mount_id: m,
                    path: p,
                })
                .collect(),
        };

        let resp = self.post_json(&url, &body).await?;
        Self::check_response(resp).await?;
        Ok(())
    }

    /// Empty trash permanently
    pub async fn empty_trash(&self) -> Result<(), ProviderError> {
        let url = format!("{}/trash", self.api_base());
        let resp = self.delete_req(&url).await?;
        Self::check_response(resp).await?;
        Ok(())
    }
}

// ─── Tauri Commands ───

#[derive(Serialize)]
pub struct KoofrTrashEntry {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub deleted: String,
    pub content_type: Option<String>,
    pub mount_id: String,
}

#[tauri::command]
pub async fn koofr_list_trash(
    state: tauri::State<'_, crate::provider_commands::ProviderState>,
) -> Result<Vec<KoofrTrashEntry>, String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    let koofr = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<KoofrProvider>()
        .ok_or("Not a Koofr connection")?;

    let default_mount = koofr.mount_id.clone();
    let files = koofr.list_trash().await.map_err(|e| e.to_string())?;
    let mut entries: Vec<KoofrTrashEntry> = files
        .into_iter()
        .map(|f| KoofrTrashEntry {
            name: f.name,
            path: f.path,
            size: f.size.max(0) as u64,
            deleted: KoofrProvider::format_timestamp(f.deleted).unwrap_or_else(|| "Unknown".into()),
            content_type: f.content_type,
            mount_id: f.mount_id.unwrap_or_else(|| default_mount.clone()),
        })
        .collect();
    // Decode only the display name when a crypt overlay is live; path/mount_id are
    // kept raw so restore round-trips the exact tokens. No-op when Crypt is off.
    for entry in &mut entries {
        // The Koofr trash API does not report entry type, so decode as a file leaf
        // (is_dir = false): correct for standard/AeroCrypt names (type-agnostic) and
        // for rclone off-mode file leaves; an off-mode directory simply fails the
        // suffix decode and is left verbatim (decode-or-passthrough), never garbled.
        if let Some(plain) = crate::crypt_overlay_provider::decode_overlay_trash_name(
            &mut **provider,
            &entry.name,
            false,
        ) {
            entry.name = plain;
        }
    }
    Ok(entries)
}

#[tauri::command]
pub async fn koofr_restore_trash(
    state: tauri::State<'_, crate::provider_commands::ProviderState>,
    files: Vec<(String, String)>,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    let koofr = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<KoofrProvider>()
        .ok_or("Not a Koofr connection")?;

    koofr
        .restore_from_trash(files)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn koofr_empty_trash(
    state: tauri::State<'_, crate::provider_commands::ProviderState>,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard.as_mut().ok_or("Not connected")?;
    let koofr = crate::crypt_overlay_provider::concrete_provider_mut(&mut **provider)
        .as_any_mut()
        .downcast_mut::<KoofrProvider>()
        .ok_or("Not a Koofr connection")?;

    koofr.empty_trash().await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A byte cut inside a multibyte character panics: masking must cut on a
    /// character boundary (an email or a name is not always ASCII).
    #[test]
    fn mask_credential_never_splits_a_character() {
        for value in [
            "aaé@example.com",
            "ééé@x.it",
            "abécdef",
            "日本語テスト",
            "a😀b@x",
        ] {
            let masked = mask_credential(value);
            assert!(masked.contains("***"), "{value} -> {masked}");
        }
    }

    /// Upload a 300 KB file through `files/put` to a local fixture answering
    /// `status`; returns the outcome and the progress updates.
    async fn upload_against_fixture(status: u16) -> (Result<(), ProviderError>, Vec<(u64, u64)>) {
        use crate::providers::upload_progress::fixture::{recorder, serve, temp_file, Route};
        let (base, server) = serve(vec![Route::post(
            "/mounts/m1/files/put",
            status,
            r#"{"name":"f.bin","size":307200}"#,
        )])
        .await;
        let mut provider = KoofrProvider::new(KoofrConfig {
            email: "user@example.com".to_string(),
            password: secrecy::SecretString::from("pass".to_string()),
            initial_path: None,
        });
        provider.content_base_override = Some(base);
        provider.mount_id = "m1".to_string();
        provider.connected = true;
        let file = temp_file(300 * 1024);
        let (callback, updates) = recorder();
        let outcome = provider
            .upload(file.path().to_str().unwrap(), "/f.bin", Some(callback))
            .await;
        server.abort();
        let updates = updates.lock().unwrap().clone();
        (outcome, updates)
    }

    /// `files/put` streams the file: the bar follows the bytes going out and
    /// reaches 100 only on Koofr's success answer. It used to report 0, then
    /// the total after the response.
    #[tokio::test]
    async fn upload_reports_real_progress() {
        use crate::providers::upload_progress::fixture::assert_real_progress;
        let (outcome, updates) = upload_against_fixture(200).await;
        assert!(outcome.is_ok(), "{outcome:?}");
        assert_eq!(
            updates.first(),
            Some(&(0, 300 * 1024)),
            "the bar opens at 0"
        );
        assert_real_progress(&updates, 300 * 1024, true);

        let (outcome, updates) = upload_against_fixture(500).await;
        assert!(outcome.is_err());
        assert_real_progress(&updates, 300 * 1024, false);
    }

    #[test]
    fn multi_thread_cutoff_has_no_provider_floor() {
        // R21: Koofr honors the caller's cutoff as-is (no 1 MiB floor), so
        // `--multi-thread-cutoff 500K` works again like before #905.
        let mut provider = KoofrProvider::new(KoofrConfig {
            email: "user@example.com".to_string(),
            password: secrecy::SecretString::from("pass".to_string()),
            initial_path: None,
        });
        assert_eq!(provider.multi_thread_cutoff_floor(), 0);
        provider.set_multi_thread_download(4, 500 * 1024);
        assert_eq!(provider.multi_thread_cutoff, 500 * 1024);
    }

    #[test]
    fn test_normalize_path() {
        assert_eq!(KoofrProvider::normalize_path(""), "/");
        assert_eq!(KoofrProvider::normalize_path("/"), "/");
        assert_eq!(KoofrProvider::normalize_path("foo"), "/foo");
        assert_eq!(KoofrProvider::normalize_path("/foo/"), "/foo");
        assert_eq!(KoofrProvider::normalize_path("/a/b/c"), "/a/b/c");
        assert_eq!(KoofrProvider::normalize_path("a\\b\\c"), "/a/b/c");
    }

    #[test]
    fn test_split_path() {
        assert_eq!(KoofrProvider::split_path("/file.txt"), ("/", "file.txt"));
        assert_eq!(
            KoofrProvider::split_path("/a/b/file.txt"),
            ("/a/b", "file.txt")
        );
        assert_eq!(KoofrProvider::split_path("/a/b"), ("/a", "b"));
        assert_eq!(KoofrProvider::split_path("file.txt"), ("/", "file.txt"));
    }

    /// Koofr refuses a rename or move onto a taken name with 409 and the
    /// body code `AlreadyExists`, which became a ServerError (CLI exit 99,
    /// live on 2026-09-26) instead of AlreadyExists (exit 9).
    #[test]
    fn a_taken_name_is_already_exists() {
        let taken = r#"{"error":{"code":"AlreadyExists","message":"File already exists"}}"#;
        assert!(matches!(
            KoofrProvider::classify_koofr_error(409, taken),
            ProviderError::AlreadyExists(_)
        ));
    }

    /// A provider on a Koofr double that answers every request 409
    /// `AlreadyExists` and records its path.
    async fn provider_on_refusing_koofr(
    ) -> (KoofrProvider, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::sync::{Arc, Mutex};
        let calls: Arc<Mutex<Vec<String>>> = Arc::default();
        let seen = Arc::clone(&calls);
        let app = axum::Router::new().fallback(axum::routing::any(move |uri: axum::http::Uri| {
            seen.lock().unwrap().push(uri.path().to_string());
            async {
                (
                    axum::http::StatusCode::CONFLICT,
                    r#"{"error":{"code":"AlreadyExists","message":"File already exists"}}"#,
                )
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        let mut provider = KoofrProvider::new(KoofrConfig {
            email: "u@example.com".to_string(),
            password: secrecy::SecretString::from("p".to_string()),
            initial_path: None,
        });
        provider.connected = true;
        provider.mount_id = "M".to_string();
        provider.api_base_override = Some(format!("http://{addr}"));
        (provider, calls)
    }

    /// A rename onto its own path is a no-op everywhere else; Koofr refused
    /// it (live on 2026-09-26). Nothing is sent.
    #[tokio::test]
    async fn a_rename_onto_its_own_path_sends_nothing() {
        let (mut provider, calls) = provider_on_refusing_koofr().await;
        provider.rename("/a.txt", "/a.txt").await.expect("no-op");
        assert!(
            calls.lock().unwrap().is_empty(),
            "{:?}",
            calls.lock().unwrap()
        );
    }

    /// A Koofr double that keeps `items` (path, content, is a folder) in
    /// memory: `info` finds them, `rename` and `move` move one (409
    /// `AlreadyExists` onto a taken path), `remove` deletes one. Returns a
    /// provider on it, the items, and every change as `rename FROM TO` or
    /// `remove PATH`.
    #[allow(clippy::type_complexity)]
    async fn provider_on_koofr_items(
        items: &[(&str, &str, bool)],
    ) -> (
        KoofrProvider,
        std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, (String, bool)>>>,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        use axum::response::IntoResponse;
        use std::sync::{Arc, Mutex};
        let store: Arc<Mutex<std::collections::BTreeMap<String, (String, bool)>>> =
            Arc::new(Mutex::new(
                items
                    .iter()
                    .map(|(path, content, dir)| (path.to_string(), (content.to_string(), *dir)))
                    .collect(),
            ));
        let changes: Arc<Mutex<Vec<String>>> = Arc::default();
        let (items, seen) = (Arc::clone(&store), Arc::clone(&changes));
        let app = axum::Router::new().fallback(axum::routing::any(
            move |uri: axum::http::Uri, body: String| {
                let (items, seen) = (Arc::clone(&items), Arc::clone(&seen));
                async move {
                    let url = reqwest::Url::parse(&format!("http://h{uri}")).unwrap();
                    let path = url
                        .query_pairs()
                        .find(|(k, _)| k == "path")
                        .map(|(_, v)| v.to_string())
                        .unwrap_or_default();
                    let args: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                    let refused = |status: u16, code: &str| {
                        (
                            axum::http::StatusCode::from_u16(status).unwrap(),
                            format!(r#"{{"error":{{"code":"{code}","message":"{code}"}}}}"#),
                        )
                            .into_response()
                    };
                    let mut items = items.lock().unwrap();
                    let parent = |p: &str| p.rsplit_once('/').map(|(a, _)| a.to_string()).unwrap();
                    let target = match uri.path() {
                        "/mounts/M/files/info" => {
                            return match items.get(&path) {
                                Some((_, dir)) => axum::Json(serde_json::json!({
                                    "name": path.rsplit('/').next().unwrap(),
                                    "type": if *dir { "dir" } else { "file" },
                                }))
                                .into_response(),
                                None => refused(404, "NotFound"),
                            };
                        }
                        "/mounts/M/files/remove" => {
                            items.remove(&path);
                            seen.lock().unwrap().push(format!("remove {path}"));
                            return axum::Json(serde_json::json!({})).into_response();
                        }
                        "/mounts/M/files/rename" => {
                            format!("{}/{}", parent(&path), args["name"].as_str().unwrap_or(""))
                        }
                        "/mounts/M/files/move" => args["toPath"].as_str().unwrap_or("").to_string(),
                        other => return refused(400, other),
                    };
                    if items.contains_key(&target) {
                        return refused(409, "AlreadyExists");
                    }
                    let Some(item) = items.remove(&path) else {
                        return refused(404, "NotFound");
                    };
                    items.insert(target.clone(), item);
                    seen.lock().unwrap().push(format!("rename {path} {target}"));
                    axum::Json(serde_json::json!({})).into_response()
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        let mut provider = KoofrProvider::new(KoofrConfig {
            email: "u@example.com".to_string(),
            password: secrecy::SecretString::from("p".to_string()),
            initial_path: None,
        });
        provider.connected = true;
        provider.mount_id = "M".to_string();
        provider.api_base_override = Some(format!("http://{addr}"));
        (provider, store, changes)
    }

    /// Koofr's rename and move never overwrite, so the replace behind CLI
    /// `edit` and the served WebDAV MOVE with `Overwrite: T` was the rename,
    /// which Koofr refuses onto the file it is meant to replace (409, live on
    /// 2026-09-26). It now sets the old file aside, renames the new one in
    /// and removes the one set aside.
    #[tokio::test]
    async fn a_replace_sets_the_old_file_aside_renames_the_new_one_in_then_removes_it() {
        let (mut provider, store, changes) =
            provider_on_koofr_items(&[("/d/a.txt", "A", false), ("/d/b.txt", "B", false)]).await;
        provider
            .replace("/d/a.txt", "/d/b.txt")
            .await
            .expect("replace");
        let changes = changes.lock().unwrap().clone();
        assert_eq!(changes.len(), 3, "{changes:?}");
        assert!(
            changes[0].starts_with("rename /d/b.txt /d/.b.txt.aeroftp-replaced-"),
            "{changes:?}"
        );
        assert_eq!(changes[1], "rename /d/a.txt /d/b.txt");
        assert!(
            changes[2].starts_with("remove /d/.b.txt.aeroftp-replaced-"),
            "{changes:?}"
        );
        let store = store.lock().unwrap().clone();
        assert_eq!(store.len(), 1, "{store:?}");
        assert_eq!(store["/d/b.txt"].0, "A");
    }

    /// The replace sets the old item aside, so the name is empty for a
    /// moment: no atomic replace is claimed, and the callers that need one
    /// refuse before they write.
    #[tokio::test]
    async fn koofr_does_not_claim_an_atomic_replace() {
        let (mut provider, _) = provider_on_refusing_koofr().await;
        assert!(!provider.supports_atomic_replace().await.unwrap());
        assert!(provider.replace_sets_aside());
    }

    // Row 4 (#347): the body-level Koofr error code drives the variant; a code we
    // do not special-case folds into ServerError with the HTTP status in the message.
    #[test]
    fn classify_koofr_error_maps_body_codes_to_variants() {
        let nf = r#"{"error":{"code":"NotFound","message":"file not found"}}"#;
        assert!(matches!(
            KoofrProvider::classify_koofr_error(404, nf),
            ProviderError::NotFound(ref m) if m == "file not found"
        ));

        let forbidden = r#"{"error":{"code":"Forbidden","message":"no access"}}"#;
        assert!(matches!(
            KoofrProvider::classify_koofr_error(403, forbidden),
            ProviderError::PermissionDenied(ref m) if m == "no access"
        ));

        let unauth = r#"{"error":{"code":"Unauthorized","message":"bad app password"}}"#;
        assert!(matches!(
            KoofrProvider::classify_koofr_error(401, unauth),
            ProviderError::AuthenticationFailed(ref m) if m == "bad app password"
        ));

        // Unknown body code -> ServerError carrying the status (Display form) + code + message.
        let other = r#"{"error":{"code":"Teapot","message":"short and stout"}}"#;
        match KoofrProvider::classify_koofr_error(409, other) {
            ProviderError::ServerError(msg) => {
                assert!(msg.contains("409 Conflict"), "got: {msg}");
                assert!(msg.contains("Teapot"), "got: {msg}");
                assert!(msg.contains("short and stout"), "got: {msg}");
            }
            e => panic!("expected ServerError, got {e:?}"),
        }
    }

    // Missing/empty code or message fall back to the "Unknown"/"Unknown error" defaults.
    #[test]
    fn classify_koofr_error_defaults_missing_code_and_message() {
        let only_error = r#"{"error":{}}"#;
        match KoofrProvider::classify_koofr_error(500, only_error) {
            ProviderError::ServerError(msg) => {
                assert!(msg.contains("500 Internal Server Error"), "got: {msg}");
                assert!(msg.contains("Unknown"), "got: {msg}");
                assert!(msg.contains("Unknown error"), "got: {msg}");
            }
            e => panic!("expected ServerError, got {e:?}"),
        }
    }

    // A body that is not the Koofr error envelope (or has no `error` field) falls
    // through to the sanitized-body ServerError branch, still tagged with the status.
    #[test]
    fn classify_koofr_error_falls_back_to_sanitized_body() {
        // Valid JSON but no `error` key.
        match KoofrProvider::classify_koofr_error(502, r#"{"unexpected":"shape"}"#) {
            ProviderError::ServerError(msg) => {
                assert!(msg.contains("502 Bad Gateway"), "got: {msg}");
            }
            e => panic!("expected ServerError, got {e:?}"),
        }
        // Non-JSON garbage body: still a ServerError, never a panic.
        match KoofrProvider::classify_koofr_error(503, "<html>maintenance</html>") {
            ProviderError::ServerError(msg) => {
                assert!(msg.contains("503 Service Unavailable"), "got: {msg}");
            }
            e => panic!("expected ServerError, got {e:?}"),
        }
    }

    #[test]
    fn test_format_timestamp() {
        assert_eq!(KoofrProvider::format_timestamp(0), None);
        assert_eq!(KoofrProvider::format_timestamp(-1), None);
        // 2024-03-01 00:00:00 UTC = 1709251200000 ms
        let ts = KoofrProvider::format_timestamp(1709251200000);
        assert!(ts.is_some());
        assert!(ts.unwrap().starts_with("2024-03-01"));
    }

    #[test]
    fn test_config_validation() {
        let config = ProviderConfig {
            name: "test".into(),
            provider_type: ProviderType::Koofr,
            host: "app.koofr.net".into(),
            port: Some(443),
            username: Some("test@example.com".into()),
            password: Some("app-password".into()),
            initial_path: None,
            extra: HashMap::new(),
        };
        let result = KoofrConfig::from_provider_config(&config);
        assert!(result.is_ok());

        // Missing email
        let config_no_email = ProviderConfig {
            username: None,
            ..config.clone()
        };
        assert!(KoofrConfig::from_provider_config(&config_no_email).is_err());

        // Empty password
        let config_no_pass = ProviderConfig {
            password: Some(String::new()),
            ..config.clone()
        };
        assert!(KoofrConfig::from_provider_config(&config_no_pass).is_err());
    }

    /// Revoking one link out of the Manage list deletes that link by its
    /// id. Through the path alone the backend looked for the path inside the
    /// link URL, which a short link does not contain.
    #[tokio::test]
    async fn revoking_a_listed_link_deletes_that_link_id() {
        use std::sync::{Arc, Mutex};
        let calls: Arc<Mutex<Vec<String>>> = Arc::default();
        let seen = Arc::clone(&calls);
        let app = axum::Router::new().fallback(axum::routing::any(
            move |method: axum::http::Method, uri: axum::http::Uri| {
                seen.lock().unwrap().push(format!(
                    "{method} {}{}",
                    uri.path(),
                    uri.query().map(|q| format!("?{q}")).unwrap_or_default()
                ));
                async { (axum::http::StatusCode::OK, r#"{}"#) }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        let mut provider = KoofrProvider::new(KoofrConfig {
            email: "u@example.com".to_string(),
            password: secrecy::SecretString::from("p".to_string()),
            initial_path: None,
        });
        provider.connected = true;
        provider.mount_id = "M".to_string();
        provider.api_base_override = Some(format!("http://{addr}"));

        provider
            .remove_share_link_by_id("/a.txt", "L-1_a")
            .await
            .expect("deleted");
        let refused = provider.remove_share_link_by_id("/a.txt", "../files").await;
        assert!(
            matches!(refused, Err(ProviderError::InvalidPath(_))),
            "{refused:?}"
        );
        assert_eq!(*calls.lock().unwrap(), ["DELETE /mounts/M/links/L-1_a"]);
    }
}
