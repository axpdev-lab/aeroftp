//! Yandex Disk Storage Provider
//!
//! Implements StorageProvider for Yandex Disk using the REST API v1.
//! Authentication: OAuth 2.0 token (long-lived, 1 year).
//! API: https://cloud-api.yandex.net/v1/disk
//!
//! Key characteristics:
//! - JSON responses (not XML)
//! - Two-step upload/download (get URL -> transfer)
//! - Path-based API with `disk:/` prefix
//! - Auth header: `Authorization: OAuth {token}` (not Bearer)
//! - 5 GB free storage

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use reqwest::header::{HeaderValue, AUTHORIZATION};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use std::collections::HashMap;

use super::{
    response_bytes_with_limit, sanitize_api_error, ProviderError, ProviderType, RemoteEntry,
    ShareLinkCapabilities, ShareLinkInfo, ShareLinkOptions, ShareLinkResult, StorageInfo,
    StorageProvider, MAX_DOWNLOAD_TO_BYTES,
};

const API_BASE: &str = "https://cloud-api.yandex.net/v1/disk";

#[cfg(debug_assertions)]
fn yd_log(msg: &str) {
    eprintln!("[yandex-disk] {}", msg);
}

#[cfg(not(debug_assertions))]
fn yd_log(_msg: &str) {}

// ─── API Response Structures ─────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct YdDiskInfo {
    #[serde(default)]
    total_space: u64,
    #[serde(default)]
    used_space: u64,
    #[serde(default)]
    trash_size: u64,
}

#[derive(Debug, Deserialize)]
struct YdResource {
    #[serde(default)]
    name: String,
    #[serde(default)]
    path: String,
    #[serde(default, rename = "type")]
    resource_type: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    modified: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    created: Option<String>,
    #[serde(default)]
    md5: Option<String>,
    #[serde(default)]
    mime_type: Option<String>,
    #[serde(default)]
    public_url: Option<String>,
    #[serde(default)]
    origin_path: Option<String>,
    #[serde(default, rename = "_embedded")]
    embedded: Option<YdResourceList>,
}

#[derive(Debug, Deserialize)]
struct YdResourceList {
    #[serde(default)]
    items: Vec<YdResource>,
    #[serde(default)]
    total: Option<u64>,
    #[serde(default)]
    #[allow(dead_code)]
    offset: u64,
    #[serde(default)]
    #[allow(dead_code)]
    limit: u64,
}

#[derive(Debug, Deserialize)]
struct YdLink {
    href: String,
    #[allow(dead_code)]
    method: Option<String>,
}

/// Validate that a Yandex API-returned URL is safe to follow (SSRF prevention).
fn validate_yd_url(url: &str) -> Result<(), ProviderError> {
    if !url.starts_with("https://") {
        return Err(ProviderError::ServerError(format!(
            "Unsafe URL scheme (expected https): {}",
            &url[..url.len().min(40)]
        )));
    }
    if let Some(host) = url
        .strip_prefix("https://")
        .and_then(|s| s.split('/').next())
    {
        let host = host.split(':').next().unwrap_or(host);
        if !host.ends_with(".yandex.net")
            && !host.ends_with(".yandex.ru")
            && !host.ends_with(".yandex.com")
            && !host.ends_with(".yandexcloud.net")
        {
            return Err(ProviderError::ServerError(format!(
                "Unexpected host in Yandex URL: {}",
                host
            )));
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct YdFilesResourceList {
    #[serde(default)]
    items: Vec<YdResource>,
}

#[derive(Debug, Deserialize)]
struct YdError {
    #[serde(default)]
    error: String,
    #[serde(default)]
    description: String,
}

// ─── Path Helpers ────────────────────────────────────────────────────

/// Validate a path for traversal attacks and null bytes.
fn validate_yd_path(path: &str) -> Result<(), ProviderError> {
    if path.contains('\0') {
        return Err(ProviderError::InvalidPath(
            "Path contains null byte".to_string(),
        ));
    }
    for component in path.split('/') {
        if component == ".." {
            return Err(ProviderError::InvalidPath(
                "Path traversal (..) not allowed".to_string(),
            ));
        }
    }
    Ok(())
}

/// Encode a Yandex Disk path for use in query parameters.
/// Paths are prefixed with `disk:/` and each segment is URL-encoded individually.
fn encode_yd_path(path: &str) -> String {
    let clean = path.trim_start_matches("disk:");
    let clean = clean.trim_start_matches('/');
    if clean.is_empty() {
        return "disk:/".to_string();
    }
    let encoded_segments: Vec<String> = clean
        .split('/')
        .filter(|seg| !seg.is_empty())
        .map(|seg| urlencoding::encode(seg).into_owned())
        .collect();
    format!("disk:/{}", encoded_segments.join("/"))
}

/// Normalize a path from the API response (strip `disk:/` prefix) to internal format.
fn normalize_path(api_path: &str) -> String {
    let stripped = api_path.strip_prefix("disk:").unwrap_or(api_path);
    if stripped.is_empty() || stripped == "/" {
        "/".to_string()
    } else if stripped.starts_with('/') {
        stripped.to_string()
    } else {
        format!("/{}", stripped)
    }
}

/// Convert a YdResource to a RemoteEntry.
fn resource_to_entry(res: &YdResource) -> RemoteEntry {
    let norm_path = normalize_path(&res.path);
    let mut metadata = HashMap::new();
    if let Some(ref md5) = res.md5 {
        metadata.insert("md5".to_string(), md5.clone());
    }
    if let Some(ref url) = res.public_url {
        metadata.insert("public_url".to_string(), url.clone());
    }
    if let Some(ref origin) = res.origin_path {
        metadata.insert("origin_path".to_string(), origin.clone());
    }
    RemoteEntry {
        name: res.name.clone(),
        path: norm_path,
        is_dir: res.resource_type == "dir",
        size: res.size,
        modified: res.modified.clone(),
        permissions: None,
        owner: None,
        group: None,
        is_symlink: false,
        link_target: None,
        mime_type: res.mime_type.clone(),
        metadata,
    }
}

fn yandex_auth_message(description: &str) -> String {
    let clean = if description.trim().is_empty() {
        "Unauthorized"
    } else {
        description.trim()
    };
    if is_yandex_terminal_auth_message(clean) {
        format!(
            "Yandex token revoked or expired: {}. Regenerate the OAuth token at oauth.yandex.com and re-add the server.",
            clean
        )
    } else {
        clean.to_string()
    }
}

fn is_yandex_terminal_auth_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("revoked")
        || lower.contains("expired")
        || lower.contains("invalid_token")
        || lower.contains("invalid oauth")
        || lower.contains("token is invalid")
        || lower.contains("token not valid")
}

fn is_yandex_retryable_auth_error(err: &ProviderError) -> bool {
    match err {
        ProviderError::AuthenticationFailed(message) => !is_yandex_terminal_auth_message(message),
        _ => false,
    }
}

// ─── Provider ────────────────────────────────────────────────────────

pub struct YandexDiskProvider {
    client: reqwest::Client,
    access_token: SecretString,
    connected: bool,
    current_path: String,
}

impl YandexDiskProvider {
    pub fn new(access_token: String, initial_path: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(crate::providers::AEROFTP_USER_AGENT)
            .connect_timeout(std::time::Duration::from_secs(30))
            .read_timeout(std::time::Duration::from_secs(300))
            .build()
            .unwrap_or_default();
        Self {
            client,
            access_token: SecretString::from(access_token),
            connected: false,
            current_path: initial_path.unwrap_or_else(|| "/".to_string()),
        }
    }

    fn auth_header(&self) -> HeaderValue {
        HeaderValue::from_str(&format!("OAuth {}", self.access_token.expose_secret()))
            .unwrap_or_else(|_| HeaderValue::from_static("OAuth invalid"))
    }

    /// Yandex OAuth tokens are user-provisioned static tokens; there is no
    /// refresh-token exchange available to this provider. The reauth hook is
    /// therefore a bounded transient-401 retry with a short backoff.
    async fn reauth(&mut self) -> Result<(), ProviderError> {
        yd_log("401 from Yandex API; backing off before one retry");
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        Ok(())
    }

    async fn with_reauth<T, F>(&mut self, mut op: F) -> Result<T, ProviderError>
    where
        F: for<'a> FnMut(&'a mut Self) -> BoxFuture<'a, Result<T, ProviderError>>,
    {
        match op(self).await {
            Err(err) if is_yandex_retryable_auth_error(&err) => {
                self.reauth().await?;
                op(self).await
            }
            other => other,
        }
    }

    async fn send_auth_checked(
        &self,
        rb: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ProviderError> {
        let resp = rb
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;
        if resp.status().as_u16() == 401 {
            return Err(self.parse_error(resp).await);
        }
        Ok(resp)
    }

    async fn send_with_reauth<F>(
        &mut self,
        mut build: F,
    ) -> Result<reqwest::Response, ProviderError>
    where
        F: FnMut(&Self) -> reqwest::RequestBuilder,
    {
        self.with_reauth(|this| {
            let rb = build(this);
            Box::pin(async move { this.send_auth_checked(rb).await })
        })
        .await
    }

    /// Resolve a relative path against current_path with traversal validation.
    fn resolve_path_safe(&self, path: &str) -> Result<String, ProviderError> {
        validate_yd_path(path)?;
        let resolved = if path.is_empty() || path == "." || path == "/" {
            self.current_path.clone()
        } else if path.starts_with('/') || path.starts_with("disk:") {
            path.to_string()
        } else {
            let base = self.current_path.trim_end_matches('/');
            format!("{}/{}", base, path)
        };
        Ok(resolved)
    }

    /// Resolve a relative path (infallible: for backward compat in cd/pwd).
    fn resolve_path(&self, path: &str) -> String {
        self.resolve_path_safe(path)
            .unwrap_or_else(|_| "/".to_string())
    }

    /// Parse an API error response into a ProviderError.
    async fn parse_error(&self, resp: reqwest::Response) -> ProviderError {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if let Ok(err) = serde_json::from_str::<YdError>(&body) {
            match err.error.as_str() {
                "UnauthorizedError" => {
                    return ProviderError::AuthenticationFailed(yandex_auth_message(
                        &err.description,
                    ));
                }
                "DiskNotFoundError" | "DiskPathDoesntExistsError" => {
                    return ProviderError::NotFound(err.description);
                }
                "DiskResourceAlreadyExistsError" | "PlatformResourceAlreadyExists" => {
                    return ProviderError::AlreadyExists(err.description);
                }
                "DiskPathPointsToRootError" => {
                    return ProviderError::InvalidPath(err.description);
                }
                "DiskStorageQuotaExhaustedError" => {
                    return ProviderError::TransferFailed("Storage quota exhausted".into());
                }
                _ => {}
            }
        }
        match status.as_u16() {
            401 => {
                ProviderError::AuthenticationFailed(yandex_auth_message(&sanitize_api_error(&body)))
            }
            403 => ProviderError::PermissionDenied("Forbidden".into()),
            404 => ProviderError::NotFound(sanitize_api_error(&body)),
            409 => ProviderError::AlreadyExists(sanitize_api_error(&body)),
            429 => ProviderError::ServerError("Rate limit exceeded".into()),
            507 => ProviderError::TransferFailed("Insufficient storage".into()),
            _ => ProviderError::ServerError(format!(
                "HTTP {}: {}",
                status,
                sanitize_api_error(&body)
            )),
        }
    }

    /// List directory contents with pagination.
    async fn list_path(&mut self, path: &str) -> Result<Vec<YdResource>, ProviderError> {
        let encoded = encode_yd_path(path);
        let mut all_items = Vec::new();
        let mut offset: u64 = 0;
        let limit: u64 = 1000;

        loop {
            let url = format!(
                "{}/resources?path={}&limit={}&offset={}",
                API_BASE, encoded, limit, offset
            );
            yd_log(&format!("LIST {}", url));

            let resp = self
                .send_with_reauth(|this| {
                    this.client
                        .get(&url)
                        .header(AUTHORIZATION, this.auth_header())
                })
                .await?;

            if !resp.status().is_success() {
                return Err(self.parse_error(resp).await);
            }

            let resource: YdResource = resp
                .json()
                .await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            if let Some(embedded) = resource.embedded {
                let count = embedded.items.len() as u64;
                all_items.extend(embedded.items);
                let total = embedded.total.unwrap_or(0);
                if total == 0 || offset + count >= total || all_items.len() > 100_000 {
                    break;
                }
                offset += count;
            } else {
                break;
            }
        }

        Ok(all_items)
    }

    /// Get metadata for a single resource.
    async fn get_resource(&mut self, path: &str) -> Result<YdResource, ProviderError> {
        let encoded = encode_yd_path(path);
        let url = format!("{}/resources?path={}", API_BASE, encoded);
        yd_log(&format!("STAT {}", url));

        let resp = self
            .send_with_reauth(|this| {
                this.client
                    .get(&url)
                    .header(AUTHORIZATION, this.auth_header())
            })
            .await?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        resp.json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))
    }

    // ─── Public trash methods (not in StorageProvider trait) ─────────

    /// List trash contents.
    pub async fn list_trash(&mut self) -> Result<Vec<RemoteEntry>, ProviderError> {
        let mut all_items = Vec::new();
        let mut offset: u64 = 0;
        let limit: u64 = 1000;

        loop {
            let url = format!(
                "{}/trash/resources?path=/&limit={}&offset={}",
                API_BASE, limit, offset
            );
            let resp = self
                .send_with_reauth(|this| {
                    this.client
                        .get(&url)
                        .header(AUTHORIZATION, this.auth_header())
                })
                .await?;

            if !resp.status().is_success() {
                return Err(self.parse_error(resp).await);
            }

            let resource: YdResource = resp
                .json()
                .await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            if let Some(embedded) = resource.embedded {
                let count = embedded.items.len() as u64;
                let entries: Vec<RemoteEntry> =
                    embedded.items.iter().map(resource_to_entry).collect();
                all_items.extend(entries);
                let total = embedded.total.unwrap_or(0);
                if total == 0 || offset + count >= total || all_items.len() > 100_000 {
                    break;
                }
                offset += count;
            } else {
                break;
            }
        }

        Ok(all_items)
    }

    /// Restore a resource from trash.
    pub async fn restore_from_trash(&mut self, trash_path: &str) -> Result<(), ProviderError> {
        let encoded = urlencoding::encode(trash_path);
        let url = format!("{}/trash/resources/restore?path={}", API_BASE, encoded);
        let resp = self
            .send_with_reauth(|this| {
                this.client
                    .put(&url)
                    .header(AUTHORIZATION, this.auth_header())
            })
            .await?;

        let status = resp.status();
        if status.is_success() || status.as_u16() == 201 || status.as_u16() == 202 {
            Ok(())
        } else {
            Err(self.parse_error(resp).await)
        }
    }

    /// Empty the entire trash.
    pub async fn empty_trash(&mut self) -> Result<(), ProviderError> {
        let url = format!("{}/trash/resources", API_BASE);
        let resp = self
            .send_with_reauth(|this| {
                this.client
                    .delete(&url)
                    .header(AUTHORIZATION, this.auth_header())
            })
            .await?;

        let status = resp.status();
        if status.is_success() || status.as_u16() == 204 || status.as_u16() == 202 {
            Ok(())
        } else {
            Err(self.parse_error(resp).await)
        }
    }

    /// Permanently delete a specific item from trash.
    pub async fn permanent_delete_from_trash(
        &mut self,
        trash_path: &str,
    ) -> Result<(), ProviderError> {
        let encoded = urlencoding::encode(trash_path);
        let url = format!("{}/trash/resources?path={}", API_BASE, encoded);
        let resp = self
            .send_with_reauth(|this| {
                this.client
                    .delete(&url)
                    .header(AUTHORIZATION, this.auth_header())
            })
            .await?;

        let status = resp.status();
        if status.is_success() || status.as_u16() == 204 || status.as_u16() == 202 {
            Ok(())
        } else {
            Err(self.parse_error(resp).await)
        }
    }
}

// ─── StorageProvider Trait Implementation ─────────────────────────────

#[async_trait]
impl StorageProvider for YandexDiskProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::YandexDisk
    }

    fn display_name(&self) -> String {
        "Yandex Disk".to_string()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        yd_log("Connecting: verifying token via GET /v1/disk/");

        let resp = self
            .client
            .get(format!("{}/", API_BASE))
            .header(AUTHORIZATION, self.auth_header())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        // Verify we can parse the disk info
        let _info: YdDiskInfo = resp.json().await.map_err(|e| {
            ProviderError::ConnectionFailed(format!("Failed to parse disk info: {}", e))
        })?;

        self.connected = true;
        yd_log("Connected successfully");
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        yd_log("Disconnected");
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
        let items = self.list_path(&resolved).await?;
        Ok(items.iter().map(resource_to_entry).collect())
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        // Verify the path exists and is a directory
        let resource = self.get_resource(&resolved).await?;
        if resource.resource_type != "dir" {
            return Err(ProviderError::InvalidPath(format!(
                "'{}' is not a directory",
                resolved
            )));
        }
        self.current_path = normalize_path(&resource.path);
        yd_log(&format!("cd -> {}", self.current_path));
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        if self.current_path == "/" {
            return Ok(());
        }
        let parent = match self.current_path.rfind('/') {
            Some(0) | None => "/".to_string(),
            Some(idx) => self.current_path[..idx].to_string(),
        };
        self.current_path = parent;
        yd_log(&format!("cd_up -> {}", self.current_path));
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
        let encoded = encode_yd_path(&resolved);
        yd_log(&format!("download: {} -> {}", resolved, local_path));

        // Step 1: Get download URL
        let url = format!("{}/resources/download?path={}", API_BASE, encoded);
        let resp = self
            .send_with_reauth(|this| {
                this.client
                    .get(&url)
                    .header(AUTHORIZATION, this.auth_header())
            })
            .await?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        let link: YdLink = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;
        validate_yd_url(&link.href)?;

        // Step 2: Download from the URL (no auth needed, streaming)
        let resp = self
            .client
            .get(&link.href)
            .send()
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::TransferFailed(format!(
                "Download failed: HTTP {}",
                resp.status()
            )));
        }

        let total = resp.content_length().unwrap_or(0);
        let mut atomic = super::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(ProviderError::IoError)?;

        let mut stream = resp.bytes_stream();
        let mut downloaded: u64 = 0;
        use futures_util::StreamExt;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            atomic
                .write_all(&chunk)
                .await
                .map_err(ProviderError::IoError)?;
            downloaded += chunk.len() as u64;
            if let Some(ref cb) = on_progress {
                cb(downloaded, total);
            }
        }

        atomic.commit().await.map_err(ProviderError::IoError)?;
        yd_log(&format!("download complete: {} bytes", downloaded));
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
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(remote_path);
        let encoded = encode_yd_path(&resolved);

        // Step 1: Get download URL (same as download())
        let url = format!("{}/resources/download?path={}", API_BASE, encoded);
        let resp = self
            .send_with_reauth(|this| {
                this.client
                    .get(&url)
                    .header(AUTHORIZATION, this.auth_header())
            })
            .await?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        let link: YdLink = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;
        validate_yd_url(&link.href)?;

        // Step 2: Resumable download (Yandex CDN URLs don't need auth)
        super::http_resumable_download(
            local_path,
            |range_header| {
                let mut req = self.client.get(&link.href);
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
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(remote_path);
        let encoded = encode_yd_path(&resolved);

        // Step 1: Get download URL
        let url = format!("{}/resources/download?path={}", API_BASE, encoded);
        let resp = self
            .send_with_reauth(|this| {
                this.client
                    .get(&url)
                    .header(AUTHORIZATION, this.auth_header())
            })
            .await?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        let link: YdLink = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;
        validate_yd_url(&link.href)?;

        // Step 2: Download with size limit
        let resp = self
            .client
            .get(&link.href)
            .send()
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ProviderError::TransferFailed(format!(
                "Download failed: HTTP {}",
                resp.status()
            )));
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
        let encoded = encode_yd_path(&resolved);
        yd_log(&format!("upload: {} -> {}", local_path, resolved));

        // Read file into chunks for streaming upload
        let file_meta = tokio::fs::metadata(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let total = file_meta.len();

        // Step 1: Get upload URL
        let url = format!(
            "{}/resources/upload?path={}&overwrite=true",
            API_BASE, encoded
        );
        let resp = self
            .send_with_reauth(|this| {
                this.client
                    .get(&url)
                    .header(AUTHORIZATION, this.auth_header())
            })
            .await?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        let link: YdLink = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;
        validate_yd_url(&link.href)?;

        // Step 2: PUT file data to the upload URL (no auth needed)
        // Stream from file to avoid loading entire file into memory
        let file = tokio::fs::File::open(local_path)
            .await
            .map_err(ProviderError::IoError)?;

        use futures_util::StreamExt;
        use tokio_util::io::ReaderStream;

        let progress_cb = on_progress;
        let mut uploaded: u64 = 0;
        let stream = ReaderStream::with_capacity(file, 65536).map(move |chunk| {
            if let Ok(bytes) = &chunk {
                uploaded += bytes.len() as u64;
                if let Some(ref cb) = progress_cb {
                    cb(uploaded, total);
                }
            }
            chunk
        });

        let body = reqwest::Body::wrap_stream(stream);
        let resp = self
            .client
            .put(&link.href)
            .header("Content-Type", "application/octet-stream")
            .body(body)
            .send()
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        let status = resp.status();
        if status.is_success() || status.as_u16() == 201 || status.as_u16() == 202 {
            yd_log(&format!("upload complete: {} bytes", total));
            Ok(())
        } else {
            Err(ProviderError::TransferFailed(format!(
                "Upload failed: HTTP {}",
                status
            )))
        }
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        let encoded = encode_yd_path(&resolved);
        let url = format!("{}/resources?path={}", API_BASE, encoded);
        yd_log(&format!("mkdir: {}", resolved));

        let resp = self
            .send_with_reauth(|this| {
                this.client
                    .put(&url)
                    .header(AUTHORIZATION, this.auth_header())
            })
            .await?;

        if resp.status().is_success() || resp.status().as_u16() == 201 {
            Ok(())
        } else {
            Err(self.parse_error(resp).await)
        }
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        let encoded = encode_yd_path(&resolved);
        let url = format!("{}/resources?path={}&permanently=true", API_BASE, encoded);
        yd_log(&format!("delete: {}", resolved));

        let resp = self
            .send_with_reauth(|this| {
                this.client
                    .delete(&url)
                    .header(AUTHORIZATION, this.auth_header())
            })
            .await?;

        let status = resp.status();
        if status.is_success() || status.as_u16() == 204 || status.as_u16() == 202 {
            Ok(())
        } else {
            Err(self.parse_error(resp).await)
        }
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.delete(path).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        self.delete(path).await
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let from_resolved = self.resolve_path(from);
        let to_resolved = self.resolve_path(to);
        let from_encoded = encode_yd_path(&from_resolved);
        let to_encoded = encode_yd_path(&to_resolved);
        let url = format!(
            "{}/resources/move?from={}&path={}",
            API_BASE, from_encoded, to_encoded
        );
        yd_log(&format!("rename: {} -> {}", from_resolved, to_resolved));

        let resp = self
            .send_with_reauth(|this| {
                this.client
                    .post(&url)
                    .header(AUTHORIZATION, this.auth_header())
            })
            .await?;

        let status = resp.status();
        if status.is_success() || status.as_u16() == 201 || status.as_u16() == 202 {
            Ok(())
        } else {
            Err(self.parse_error(resp).await)
        }
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        let resource = self.get_resource(&resolved).await?;
        Ok(resource_to_entry(&resource))
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
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resp = self
            .send_with_reauth(|this| {
                this.client
                    .get(format!("{}/", API_BASE))
                    .header(AUTHORIZATION, this.auth_header())
            })
            .await?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        let info: YdDiskInfo = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        Ok(format!(
            "Yandex Disk | Total: {:.1} GB | Used: {:.1} GB | Trash: {:.1} MB",
            info.total_space as f64 / 1_073_741_824.0,
            info.used_space as f64 / 1_073_741_824.0,
            info.trash_size as f64 / 1_048_576.0,
        ))
    }

    // ─── Optional capabilities ───────────────────────────────────────

    fn supports_share_links(&self) -> bool {
        true
    }

    fn share_link_capabilities(&self) -> ShareLinkCapabilities {
        ShareLinkCapabilities {
            supports_expiration: false,
            supports_password: false,
            supports_permissions: false,
            available_permissions: vec![],
            supports_list_links: false,
            supports_revoke: true,
        }
    }

    async fn list_share_links(&mut self, path: &str) -> Result<Vec<ShareLinkInfo>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        let resource = self.get_resource(&resolved).await?;

        if let Some(ref url) = resource.public_url {
            Ok(vec![ShareLinkInfo {
                id: resolved,
                url: url.clone(),
                created_at: None,
                expires_at: None,
                password_protected: false,
                permissions: Some("public".to_string()),
            }])
        } else {
            Ok(vec![])
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
        let encoded = encode_yd_path(&resolved);

        // Publish the resource
        let url = format!("{}/resources/publish?path={}", API_BASE, encoded);
        let resp = self
            .send_with_reauth(|this| {
                this.client
                    .put(&url)
                    .header(AUTHORIZATION, this.auth_header())
            })
            .await?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        // Fetch updated metadata to get public_url
        let resource = self.get_resource(&resolved).await?;
        let share_url = resource.public_url.ok_or_else(|| {
            ProviderError::ServerError("No public URL returned after publish".into())
        })?;

        let _ = &options; // acknowledge options
        Ok(ShareLinkResult {
            url: share_url,
            password: None,
            expires_at: None,
        })
    }

    async fn remove_share_link(&mut self, path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(path);
        let encoded = encode_yd_path(&resolved);
        let url = format!("{}/resources/unpublish?path={}", API_BASE, encoded);

        let resp = self
            .send_with_reauth(|this| {
                this.client
                    .put(&url)
                    .header(AUTHORIZATION, this.auth_header())
            })
            .await?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(self.parse_error(resp).await)
        }
    }

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(
        &mut self,
        _path: &str,
        pattern: &str,
    ) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        // Use flat file list and filter by pattern
        let mut results = Vec::new();
        let mut offset: u64 = 0;
        let limit: u64 = 1000;

        loop {
            let url = format!(
                "{}/resources/files?limit={}&offset={}",
                API_BASE, limit, offset
            );
            let resp = self
                .send_with_reauth(|this| {
                    this.client
                        .get(&url)
                        .header(AUTHORIZATION, this.auth_header())
                })
                .await?;

            if !resp.status().is_success() {
                return Err(self.parse_error(resp).await);
            }

            let list: YdFilesResourceList = resp
                .json()
                .await
                .map_err(|e| ProviderError::ParseError(e.to_string()))?;

            let count = list.items.len() as u64;
            for item in &list.items {
                if super::matches_find_pattern(&item.name, pattern) {
                    results.push(resource_to_entry(item));
                }
            }

            if count < limit {
                break;
            }
            offset += count;
            // Safety cap
            if offset > 50_000 {
                break;
            }
        }

        Ok(results)
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resp = self
            .send_with_reauth(|this| {
                this.client
                    .get(format!("{}/", API_BASE))
                    .header(AUTHORIZATION, this.auth_header())
            })
            .await?;

        if !resp.status().is_success() {
            return Err(self.parse_error(resp).await);
        }

        let info: YdDiskInfo = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(e.to_string()))?;

        Ok(StorageInfo {
            used: info.used_space,
            total: info.total_space,
            free: info.total_space.saturating_sub(info.used_space),
        })
    }

    fn supports_checksum(&self) -> bool {
        true
    }

    async fn checksum(&mut self, path: &str) -> Result<HashMap<String, String>, ProviderError> {
        let entry = self.stat(path).await?;
        let mut checksums = HashMap::new();
        if let Some(md5) = entry.metadata.get("md5") {
            checksums.insert("md5".to_string(), md5.clone());
        }
        Ok(checksums)
    }

    fn supports_server_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let from_resolved = self.resolve_path(from);
        let to_resolved = self.resolve_path(to);
        let from_encoded = encode_yd_path(&from_resolved);
        let to_encoded = encode_yd_path(&to_resolved);
        let url = format!(
            "{}/resources/copy?from={}&path={}",
            API_BASE, from_encoded, to_encoded
        );
        yd_log(&format!("copy: {} -> {}", from_resolved, to_resolved));

        let resp = self
            .send_with_reauth(|this| {
                this.client
                    .post(&url)
                    .header(AUTHORIZATION, this.auth_header())
            })
            .await?;

        let status = resp.status();
        if status.is_success() || status.as_u16() == 201 || status.as_u16() == 202 {
            Ok(())
        } else {
            Err(self.parse_error(resp).await)
        }
    }

    fn supports_remote_upload(&self) -> bool {
        true
    }

    async fn remote_upload(&mut self, url: &str, dest_path: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let resolved = self.resolve_path(dest_path);
        let encoded = encode_yd_path(&resolved);
        let url_encoded = urlencoding::encode(url);
        let api_url = format!(
            "{}/resources/upload?url={}&path={}",
            API_BASE, url_encoded, encoded
        );
        yd_log(&format!("remote_upload: {} -> {}", url, resolved));

        let resp = self
            .send_with_reauth(|this| {
                this.client
                    .post(&api_url)
                    .header(AUTHORIZATION, this.auth_header())
            })
            .await?;

        let status = resp.status();
        if status.is_success() || status.as_u16() == 202 {
            Ok(())
        } else {
            Err(self.parse_error(resp).await)
        }
    }

    fn transfer_optimization_hints(&self) -> super::TransferOptimizationHints {
        super::TransferOptimizationHints {
            supports_resume_download: true,
            ..Default::default()
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_yd_path_root() {
        assert_eq!(encode_yd_path("/"), "disk:/");
        assert_eq!(encode_yd_path(""), "disk:/");
        assert_eq!(encode_yd_path("disk:/"), "disk:/");
    }

    #[test]
    fn test_encode_yd_path_segments() {
        assert_eq!(
            encode_yd_path("/Documents/test.txt"),
            "disk:/Documents/test.txt"
        );
        assert_eq!(
            encode_yd_path("/My Files/photo 1.jpg"),
            "disk:/My%20Files/photo%201.jpg"
        );
    }

    #[test]
    fn test_encode_yd_path_with_disk_prefix() {
        assert_eq!(
            encode_yd_path("disk:/Documents/test.txt"),
            "disk:/Documents/test.txt"
        );
    }

    #[test]
    fn test_normalize_path() {
        assert_eq!(normalize_path("disk:/"), "/");
        assert_eq!(normalize_path("disk:/Documents"), "/Documents");
        assert_eq!(normalize_path("disk:/foo/bar.txt"), "/foo/bar.txt");
        assert_eq!(normalize_path("/already/normalized"), "/already/normalized");
    }

    #[test]
    fn test_resource_to_entry_file() {
        let res = YdResource {
            name: "test.txt".to_string(),
            path: "disk:/Documents/test.txt".to_string(),
            resource_type: "file".to_string(),
            size: 1024,
            modified: Some("2024-01-15T10:30:00+00:00".to_string()),
            created: None,
            md5: Some("abc123".to_string()),
            mime_type: Some("text/plain".to_string()),
            public_url: None,
            origin_path: None,
            embedded: None,
        };
        let entry = resource_to_entry(&res);
        assert_eq!(entry.name, "test.txt");
        assert_eq!(entry.path, "/Documents/test.txt");
        assert!(!entry.is_dir);
        assert_eq!(entry.size, 1024);
        assert_eq!(entry.metadata.get("md5"), Some(&"abc123".to_string()));
    }

    #[test]
    fn test_resource_to_entry_dir() {
        let res = YdResource {
            name: "Photos".to_string(),
            path: "disk:/Photos".to_string(),
            resource_type: "dir".to_string(),
            size: 0,
            modified: None,
            created: None,
            md5: None,
            mime_type: None,
            public_url: None,
            origin_path: None,
            embedded: None,
        };
        let entry = resource_to_entry(&res);
        assert!(entry.is_dir);
        assert_eq!(entry.path, "/Photos");
    }

    #[test]
    fn test_resolve_path() {
        let provider = YandexDiskProvider::new("test".into(), Some("/Documents".into()));
        assert_eq!(provider.resolve_path("file.txt"), "/Documents/file.txt");
        assert_eq!(provider.resolve_path("/absolute/path"), "/absolute/path");
        assert_eq!(provider.resolve_path("disk:/something"), "disk:/something");
    }

    #[test]
    fn retry_filter_accepts_generic_unauthorized_once() {
        let err = ProviderError::AuthenticationFailed("Unauthorized".into());
        assert!(is_yandex_retryable_auth_error(&err));
    }

    #[test]
    fn retry_filter_rejects_terminal_token_messages() {
        let err = ProviderError::AuthenticationFailed(yandex_auth_message("invalid_token"));
        assert!(!is_yandex_retryable_auth_error(&err));
        let msg = err.to_string();
        assert!(msg.contains("Regenerate the OAuth token"));
    }
}
