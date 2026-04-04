//! Koofr Cloud Storage Provider — Native REST API v2.1
//!
//! European privacy-focused cloud storage with 10 GB free tier.
//! Mount-centric, path-based API — every file operation uses (mountId, path).
//!
//! Auth: App Password (HTTP Basic) or OAuth2 Bearer token.
//! API: <https://app.koofr.net/api/v2.1>
//! Content: <https://app.koofr.net/content/api/v2.1>

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use reqwest::header::{HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE, RANGE};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

use super::{
    sanitize_api_error, response_bytes_with_limit, MAX_DOWNLOAD_TO_BYTES,
    FileVersion, ProviderConfig, ProviderError, ProviderType, RemoteEntry,
    StorageInfo, StorageProvider, TransferOptimizationHints,
    HttpRetryConfig, send_with_retry,
    ShareLinkOptions, ShareLinkResult,
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
        let visible = local.len().min(3);
        format!("{}***{}", &local[..visible], domain)
    } else if value.len() <= 3 {
        "***".to_string()
    } else {
        format!("{}***", &value[..3])
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
            return Err(ProviderError::InvalidConfig("App password cannot be empty".into()));
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
}

impl KoofrProvider {
    pub fn new(config: KoofrConfig) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(crate::providers::AEROFTP_USER_AGENT)
            .timeout(Duration::from_secs(300))
            .connect_timeout(Duration::from_secs(30))
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
        }
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
            ProviderError::AuthenticationFailed(format!(
                "Invalid characters in credentials: {}",
                e
            ))
        })
    }

    fn api_url(&self, path: &str) -> String {
        format!("{}{}", API_BASE, path)
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
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();

        if let Ok(err) = serde_json::from_str::<KoofrError>(&body) {
            if let Some(inner) = err.error {
                let code = inner.code.as_deref().unwrap_or("Unknown");
                let message = inner.message.as_deref().unwrap_or("Unknown error");
                return match code {
                    "NotFound" => ProviderError::NotFound(message.to_string()),
                    "Forbidden" => ProviderError::PermissionDenied(message.to_string()),
                    "Unauthorized" => {
                        ProviderError::AuthenticationFailed(message.to_string())
                    }
                    _ => ProviderError::ServerError(format!(
                        "Koofr API error ({}): {} — {}",
                        status, code, message
                    )),
                };
            }
        }

        ProviderError::ServerError(format!(
            "Koofr API error ({}): {}",
            status,
            sanitize_api_error(&body)
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
        chrono::DateTime::from_timestamp(secs, 0).map(|dt| {
            dt.format("%Y-%m-%d %H:%M:%SZ").to_string()
        })
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
        let user_body = resp
            .text()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Failed to read user body: {}", e)))?;

        koofr_log(&format!("User response: {}", &user_body[..user_body.len().min(300)]));

        let user: KoofrUser = serde_json::from_str(&user_body)
            .map_err(|e| ProviderError::ConnectionFailed(format!(
                "Failed to parse user: {}. Body: {}",
                e,
                &user_body[..user_body.len().min(200)]
            )))?;

        self.account_email = user.email.clone();
        koofr_log(&format!(
            "Authenticated as {} {}",
            mask_credential(user.first_name.as_deref().unwrap_or("")),
            mask_credential(user.last_name.as_deref().unwrap_or(""))
        ));

        // 2. Get mounts and find primary
        let resp = self.get(&self.api_url("/mounts")).await?;
        let resp = Self::check_response(resp).await?;
        let body = resp
            .text()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Failed to read mounts body: {}", e)))?;

        koofr_log(&format!("Mounts response ({} bytes): {}", body.len(), &body[..body.len().min(500)]));

        // Try parsing as bare array first, then as wrapped { "mounts": [...] }
        let mounts: Vec<KoofrMount> = serde_json::from_str(&body)
            .or_else(|_| {
                serde_json::from_str::<KoofrMountsResponse>(&body).map(|r| r.mounts)
            })
            .map_err(|e| ProviderError::ConnectionFailed(format!(
                "Failed to parse mounts: {}. Body preview: {}",
                e,
                &body[..body.len().min(200)]
            )))?;

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
        // NOTE: Koofr API returns spaceTotal/spaceUsed in MiB — multiply by 1024*1024 for bytes
        const MIB: i64 = 1024 * 1024;
        let mount_detail_url = format!("{}/mounts/{}", API_BASE, self.mount_id);
        match self.get(&mount_detail_url).await {
            Ok(resp) => {
                let detail_body = resp.text().await.unwrap_or_default();
                koofr_log(&format!("Mount detail response: {}", &detail_body[..detail_body.len().min(500)]));

                // Try bare KoofrMount first, then raw Value extraction
                if let Ok(detail) = serde_json::from_str::<KoofrMount>(&detail_body) {
                    self.space_total = detail.space_total.unwrap_or(0) * MIB;
                    self.space_used = detail.space_used.unwrap_or(0) * MIB;
                } else if let Ok(val) = serde_json::from_str::<serde_json::Value>(&detail_body) {
                    self.space_total = val.get("spaceTotal").and_then(|v| v.as_i64()).unwrap_or(0) * MIB;
                    self.space_used = val.get("spaceUsed").and_then(|v| v.as_i64()).unwrap_or(0) * MIB;
                } else {
                    koofr_log("Mount detail parse failed, using list values");
                    self.space_total = primary.space_total.unwrap_or(0) * MIB;
                    self.space_used = primary.space_used.unwrap_or(0) * MIB;
                }
                koofr_log(&format!("Quota after MiB→bytes: total={}, used={}", self.space_total, self.space_used));
            }
            Err(e) => {
                koofr_log(&format!("Mount detail fetch failed: {}, using list values", e));
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
                    API_BASE,
                    self.mount_id,
                    urlencoding::encode(&normalized)
                );
                match self.get(&url).await {
                    Ok(resp) if resp.status().is_success() => {
                        self.current_path = normalized;
                    }
                    _ => {
                        koofr_log(&format!(
                            "Initial path '{}' not found, using root",
                            initial
                        ));
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
            API_BASE,
            self.mount_id,
            urlencoding::encode(&resolved)
        );

        let resp = self.get(&url).await?;
        let resp = Self::check_response(resp).await?;
        let file_list: KoofrFileList = resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Failed to parse file list: {}", e))
        })?;

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
            API_BASE,
            self.mount_id,
            urlencoding::encode(&target)
        );
        let resp = self.get(&url).await?;
        let resp = Self::check_response(resp).await?;
        let info: KoofrFileInfo = resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Failed to parse file info: {}", e))
        })?;

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
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let resolved = self.resolve_path(remote_path);
        let url = format!(
            "{}/mounts/{}/files/get?path={}",
            CONTENT_BASE,
            self.mount_id,
            urlencoding::encode(&resolved)
        );

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
        let mut stream = resp.bytes_stream();
        let mut atomic = super::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Create file failed: {}", e)))?;
        let mut downloaded: u64 = 0;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk
                .map_err(|e| ProviderError::TransferFailed(format!("Stream error: {}", e)))?;
            atomic.write_all(&chunk)
                .await
                .map_err(|e| ProviderError::TransferFailed(format!("Write error: {}", e)))?;
            downloaded += chunk.len() as u64;
            if let Some(ref cb) = on_progress {
                cb(downloaded, total_size);
            }
        }
        atomic.commit()
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Failed to finalize download: {}", e)))?;

        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let resolved = self.resolve_path(remote_path);
        let url = format!(
            "{}/mounts/{}/files/get?path={}",
            CONTENT_BASE,
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

        if let Some(ref cb) = on_progress {
            cb(0, file_size);
        }

        // Preserve modification time
        let modified_ms = std::fs::metadata(local_path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        let url = format!(
            "{}/mounts/{}/files/put?path={}&filename={}&autorename=true&overwrite=true&info=true{}",
            CONTENT_BASE,
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
        let stream = tokio_util::io::ReaderStream::new(file);
        let body = reqwest::Body::wrap_stream(stream);

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

        if let Some(ref cb) = on_progress {
            cb(file_size, file_size);
        }

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
            API_BASE,
            self.mount_id,
            urlencoding::encode(parent)
        );

        #[derive(Serialize)]
        struct CreateFolder {
            name: String,
        }

        let resp = self
            .post_json(&url, &CreateFolder { name: folder_name.to_string() })
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
            API_BASE,
            self.mount_id,
            urlencoding::encode(&resolved)
        );

        let resp = self.delete_req(&url).await?;
        Self::check_response(resp).await?;
        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
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

        let (from_parent, _) = Self::split_path(&from_resolved);
        let (to_parent, to_name) = Self::split_path(&to_resolved);

        // If same parent directory → rename, otherwise → move
        if from_parent == to_parent {
            let url = format!(
                "{}/mounts/{}/files/rename?path={}",
                API_BASE,
                self.mount_id,
                urlencoding::encode(&from_resolved)
            );

            #[derive(Serialize)]
            struct RenameRequest {
                name: String,
            }

            let resp = self
                .put_json(&url, &RenameRequest { name: to_name.to_string() })
                .await?;
            Self::check_response(resp).await?;
        } else {
            // Move to different directory
            let url = format!(
                "{}/mounts/{}/files/move?path={}",
                API_BASE,
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

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let resolved = self.resolve_path(path);
        let url = format!(
            "{}/mounts/{}/files/info?path={}",
            API_BASE,
            self.mount_id,
            urlencoding::encode(&resolved)
        );

        let resp = self.get(&url).await?;
        let resp = Self::check_response(resp).await?;
        let info: KoofrFileInfo = resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Failed to parse file info: {}", e))
        })?;

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
            "Koofr Cloud Storage — Mount: {} — {:.1} GB / {:.1} GB",
            self.mount_id,
            self.space_used as f64 / 1_073_741_824.0,
            self.space_total as f64 / 1_073_741_824.0,
        ))
    }

    // ─── Advanced Capabilities ───

    fn supports_server_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let from_resolved = self.resolve_path(from);
        let to_resolved = self.resolve_path(to);

        let url = format!(
            "{}/mounts/{}/files/copy?path={}",
            API_BASE,
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

    async fn create_share_link(
        &mut self,
        path: &str,
        options: ShareLinkOptions,
    ) -> Result<ShareLinkResult, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let resolved = self.resolve_path(path);
        let url = format!(
            "{}/mounts/{}/links",
            API_BASE, self.mount_id
        );

        #[derive(Serialize)]
        struct CreateLink {
            path: String,
        }

        let resp = self
            .post_json(&url, &CreateLink { path: resolved })
            .await?;

        let status = resp.status();
        if !status.is_success() && status.as_u16() != 201 {
            return Err(Self::parse_error(resp).await);
        }

        let link: KoofrLink = resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Failed to parse link: {}", e))
        })?;

        let _ = &options; // acknowledge options
        Ok(ShareLinkResult {
            url: link.short_url.unwrap_or(link.url),
            password: None,
            expires_at: None,
        })
    }

    async fn remove_share_link(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        // First, find the link for this path
        let url = format!(
            "{}/mounts/{}/links",
            API_BASE, self.mount_id
        );
        let resp = self.get(&url).await?;
        let resp = Self::check_response(resp).await?;
        let links: KoofrLinksResponse = resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Failed to parse links: {}", e))
        })?;

        let resolved = self.resolve_path(path);
        // No link found is not an error
        for link in &links.links {
            if link.url.contains(&resolved) || link.id == resolved {
                let delete_url = format!(
                    "{}/mounts/{}/links/{}",
                    API_BASE, self.mount_id, link.id
                );
                let resp = self.delete_req(&delete_url).await?;
                Self::check_response(resp).await?;
                return Ok(());
            }
        }

        Ok(())
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        // Refresh mount info — Koofr returns spaceTotal/spaceUsed in MiB
        const MIB: i64 = 1024 * 1024;
        let url = format!("{}/mounts/{}", API_BASE, self.mount_id);
        let resp = self.get(&url).await?;
        let resp = Self::check_response(resp).await?;
        let body = resp.text().await.map_err(|e| {
            ProviderError::ServerError(format!("Failed to read mount response: {}", e))
        })?;
        koofr_log(&format!("storage_info mount response: {}", &body[..body.len().min(500)]));

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
                &body[..body.len().min(200)]
            )));
        }

        let total = self.space_total.max(0) as u64;
        let used = self.space_used.max(0) as u64;

        Ok(StorageInfo {
            total,
            used,
            free: total.saturating_sub(used),
        })
    }

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(
        &mut self,
        path: &str,
        pattern: &str,
    ) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let resolved = self.resolve_path(path);
        let url = format!(
            "{}/search?query={}&mountId={}&path={}&limit=256",
            API_BASE,
            urlencoding::encode(pattern),
            urlencoding::encode(&self.mount_id),
            urlencoding::encode(&resolved)
        );

        let resp = self.get(&url).await?;
        let resp = Self::check_response(resp).await?;
        let search: KoofrSearchResponse = resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Failed to parse search: {}", e))
        })?;

        let entries: Vec<RemoteEntry> = search
            .hits
            .iter()
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
            CONTENT_BASE,
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
        let mut stream = resp.bytes_stream();

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(local_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Open file failed: {}", e)))?;

        let mut downloaded = offset;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk
                .map_err(|e| ProviderError::TransferFailed(format!("Stream error: {}", e)))?;
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
            API_BASE,
            self.mount_id,
            urlencoding::encode(&resolved)
        );

        let resp = self.get(&url).await?;
        let resp = Self::check_response(resp).await?;
        let versions: KoofrVersionsResponse = resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Failed to parse versions: {}", e))
        })?;

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
            CONTENT_BASE,
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
            .map_err(|e| ProviderError::TransferFailed(format!("Download version failed: {}", e)))?;

        if !resp.status().is_success() {
            return Err(Self::parse_error(resp).await);
        }

        use futures_util::StreamExt;

        let mut stream = resp.bytes_stream();
        let mut atomic = super::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Create file failed: {}", e)))?;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk
                .map_err(|e| ProviderError::TransferFailed(format!("Stream error: {}", e)))?;
            atomic.write_all(&chunk)
                .await
                .map_err(|e| ProviderError::TransferFailed(format!("Write error: {}", e)))?;
        }
        atomic.commit()
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Failed to finalize download: {}", e)))?;

        Ok(())
    }

    async fn restore_version(
        &mut self,
        path: &str,
        version_id: &str,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let resolved = self.resolve_path(path);
        let url = format!(
            "{}/mounts/{}/files/versions/change?path={}&version={}",
            API_BASE,
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
            CONTENT_BASE,
            self.mount_id,
            urlencoding::encode(&resolved)
        );

        let range_value = HeaderValue::from_str(&format!(
            "bytes={}-{}",
            offset,
            offset + len - 1
        ))
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

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ProviderError::TransferFailed(format!("Read bytes failed: {}", e)))?;
        Ok(bytes.to_vec())
    }

    fn transfer_optimization_hints(&self) -> TransferOptimizationHints {
        TransferOptimizationHints {
            supports_resume_download: true,
            supports_server_checksum: true,
            preferred_checksum_algo: Some("koofr".to_string()),
            supports_delta_sync: true,
            ..Default::default()
        }
    }
}

// ─── Koofr-specific operations (exposed via Tauri commands) ───

impl KoofrProvider {
    /// List trash items
    pub async fn list_trash(&self) -> Result<Vec<KoofrTrashFile>, ProviderError> {
        let url = format!("{}/trash?pageSize=1000", API_BASE);
        let resp = self.get(&url).await?;
        let resp = Self::check_response(resp).await?;
        let trash: KoofrTrashResponse = resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Failed to parse trash: {}", e))
        })?;
        Ok(trash.files)
    }

    /// Restore files from trash
    pub async fn restore_from_trash(
        &self,
        files: Vec<(String, String)>, // (mount_id, path) pairs
    ) -> Result<(), ProviderError> {
        let url = format!("{}/trash/undelete", API_BASE);

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
        let url = format!("{}/trash", API_BASE);
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
    let provider = guard
        .as_mut()
        .ok_or("Not connected")?;
    let koofr = provider
        .as_any_mut()
        .downcast_mut::<KoofrProvider>()
        .ok_or("Not a Koofr connection")?;

    let default_mount = koofr.mount_id.clone();
    let files = koofr.list_trash().await.map_err(|e| e.to_string())?;
    Ok(files
        .into_iter()
        .map(|f| KoofrTrashEntry {
            name: f.name,
            path: f.path,
            size: f.size.max(0) as u64,
            deleted: KoofrProvider::format_timestamp(f.deleted)
                .unwrap_or_else(|| "Unknown".into()),
            content_type: f.content_type,
            mount_id: f.mount_id.unwrap_or_else(|| default_mount.clone()),
        })
        .collect())
}

#[tauri::command]
pub async fn koofr_restore_trash(
    state: tauri::State<'_, crate::provider_commands::ProviderState>,
    files: Vec<(String, String)>,
) -> Result<(), String> {
    let mut guard = state.provider.lock().await;
    let provider = guard
        .as_mut()
        .ok_or("Not connected")?;
    let koofr = provider
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
    let provider = guard
        .as_mut()
        .ok_or("Not connected")?;
    let koofr = provider
        .as_any_mut()
        .downcast_mut::<KoofrProvider>()
        .ok_or("Not a Koofr connection")?;

    koofr.empty_trash().await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
