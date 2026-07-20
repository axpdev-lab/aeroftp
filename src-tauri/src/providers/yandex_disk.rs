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
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::{
    response_bytes_with_limit, sanitize_api_error, MultipartHandle, ProviderError, ProviderType,
    RemoteEntry, ShareLinkCapabilities, ShareLinkInfo, ShareLinkOptions, ShareLinkResult,
    StorageInfo, StorageProvider, UploadedPart, MAX_DOWNLOAD_TO_BYTES,
};

const API_BASE: &str = "https://cloud-api.yandex.net/v1/disk";

/// Maximum number of attempts (initial + retries) for an upload PUT against
/// a Yandex upload-target URL. Each attempt re-acquires the upload-target
/// because the URL has a short TTL and may already be 410 Gone if reused.
const YANDEX_UPLOAD_MAX_ATTEMPTS: u32 = 3;

/// Base delay for exponential backoff between upload retries (500 ms, 1 s, 2 s).
const YANDEX_UPLOAD_BACKOFF_BASE_MS: u64 = 500;

/// Y3 fix: switch to chunked Content-Range upload for files larger than this
/// threshold. Yandex Disk caps upload throughput around 1 Mbps server-side and
/// closes the TCP connection mid-stream on long single-shot PUTs, so a 100 MiB
/// payload (~13 min) and a 1 GiB payload (~2 h) reliably fail under retries
/// that all rebuild the same single connection. Splitting the body lets each
/// PUT bound itself to a few seconds and survive Yandex's idle disconnects.
/// Below this threshold the original single-PUT path is preserved unchanged.
const YANDEX_UPLOAD_CHUNKED_THRESHOLD_BYTES: u64 = 32 * 1024 * 1024; // 32 MiB

/// Per-chunk size for the resumable PUT loop. 8 MiB sits comfortably within
/// the empirical 1 Mbps Yandex upload cap: each chunk completes in ~60 s
/// well below any plausible idle-timeout the server enforces.
const YANDEX_UPLOAD_CHUNK_SIZE_BYTES: u64 = 8 * 1024 * 1024;

/// Maximum retries per chunk on transient 5xx/network errors before we
/// either re-acquire a fresh upload-target or surface the failure.
const YANDEX_UPLOAD_CHUNK_MAX_RETRIES: u32 = 4;

/// Maximum number of full upload-session restarts when the upload-target URL
/// returns 410 Gone (TTL expired) mid-transfer. A fresh session restarts the
/// payload from byte 0; we cap retries to bound worst-case wall time.
const YANDEX_UPLOAD_MAX_SESSION_RESTARTS: u32 = 2;

#[cfg(debug_assertions)]
fn yd_log(msg: &str) {
    eprintln!("[yandex-disk] {}", msg);
}

#[cfg(not(debug_assertions))]
fn yd_log(_msg: &str) {}

/// HTTP statuses that indicate a transient failure during upload PUT and
/// can be retried after re-acquiring a fresh upload-target.
fn is_yandex_upload_retryable_status(status: reqwest::StatusCode) -> bool {
    let code = status.as_u16();
    status.is_server_error() || code == 408 || code == 425 || code == 429
}

/// ProviderError variants raised during the GET upload-target step that
/// indicate a transient failure worth retrying with backoff.
fn is_yandex_upload_retryable_error(err: &ProviderError) -> bool {
    matches!(
        err,
        ProviderError::NetworkError(_) | ProviderError::ServerError(_),
    )
}

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

/// Side-band metadata embedded in `MultipartHandle.upload_id` for the
/// Yandex Disk chunked-upload path (S3-T05). `href` is the ephemeral
/// upload-target signed URL returned by `/v1/disk/resources/upload`;
/// `encoded_path` is the URL-encoded disk path the runner can re-use
/// to re-acquire a fresh target if the original expires (410 Gone).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct YandexMultipartMeta {
    href: String,
    encoded_path: String,
    total: u64,
    part: u64,
}

impl YandexMultipartMeta {
    fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    fn decode(raw: &str) -> Result<Self, ProviderError> {
        serde_json::from_str(raw).map_err(|e| {
            ProviderError::Other(format!("Yandex Disk multipart handle decode failed: {}", e))
        })
    }
}

/// Compute the per-chunk size the runner should slice `total` bytes into.
///
/// Matches the legacy `YANDEX_UPLOAD_CHUNK_SIZE_BYTES` (8 MiB) so the
/// trait wiring and the legacy single-shot path agree on chunk geometry.
fn yandex_runner_part_size(total: u64) -> u64 {
    YANDEX_UPLOAD_CHUNK_SIZE_BYTES.min(total.max(1))
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
            .read_timeout(std::time::Duration::from_secs(1800))
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
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        Self::classify_yandex_error(status, &body)
    }

    /// Pure status+body -> ProviderError classifier (extracted from `parse_error`
    /// so it can be unit-tested without a live `reqwest::Response`). Yandex Disk
    /// returns a JSON `{ "error", "description" }`; the body-level `error` string is
    /// tried first (richer, status-independent mapping), then the HTTP status.
    fn classify_yandex_error(status: u16, body: &str) -> ProviderError {
        if let Ok(err) = serde_json::from_str::<YdError>(body) {
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
        match status {
            401 => {
                ProviderError::AuthenticationFailed(yandex_auth_message(&sanitize_api_error(body)))
            }
            403 => ProviderError::PermissionDenied("Forbidden".into()),
            404 => ProviderError::NotFound(sanitize_api_error(body)),
            409 => ProviderError::AlreadyExists(sanitize_api_error(body)),
            429 => ProviderError::ServerError("Rate limit exceeded".into()),
            507 => ProviderError::TransferFailed("Insufficient storage".into()),
            // Preserve the original message shape: `reqwest::StatusCode` Displays as
            // "<code> <reason>", so reconstruct it from the u16.
            _ => ProviderError::ServerError(format!(
                "HTTP {}: {}",
                reqwest::StatusCode::from_u16(status)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|_| status.to_string()),
                sanitize_api_error(body)
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

    /// Request a fresh upload-target URL from the Yandex Disk API.
    ///
    /// Yandex returns a short-lived signed URL (`https://uploader<NNN><tag>.disk.yandex.net/upload-target/...`)
    /// that the caller must PUT the body to. The URL has an implicit TTL and
    /// can return 410 Gone if reused after expiration: callers retrying an
    /// upload after a transient failure must re-acquire a new one rather than
    /// re-PUT to the same href.
    async fn acquire_upload_target(&mut self, encoded: &str) -> Result<YdLink, ProviderError> {
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
        Ok(link)
    }

    /// Y3 fix: chunked Content-Range upload for files larger than
    /// `YANDEX_UPLOAD_CHUNKED_THRESHOLD_BYTES`. Mirrors the Google Drive
    /// resumable-upload pattern, which Yandex Disk's uploader endpoint
    /// implements via:
    /// - 201 Created: full payload uploaded
    /// - 202 Accepted: partial accept, optional `Range: bytes=0-N` hint for
    ///   the next start offset (ignored: we use our own contiguous offset
    ///   since each chunk is sent in order)
    /// - 4xx (especially 410 Gone): upload-target expired, restart whole
    ///   payload with a fresh upload session
    /// - 5xx / network: per-chunk retry with exponential backoff
    async fn upload_chunked(
        &mut self,
        local_path: &str,
        encoded: &str,
        total: u64,
        progress_tx: Option<tokio::sync::mpsc::UnboundedSender<(u64, u64)>>,
    ) -> Result<(), ProviderError> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};

        if total == 0 {
            // Edge case: zero-byte uploads still need an upload-target PUT
            // with an empty body. Fall back to single-PUT semantics.
            let link = self.acquire_upload_target(encoded).await?;
            let resp = self
                .client
                .put(&link.href)
                .header("Content-Type", "application/octet-stream")
                .body(Vec::<u8>::new())
                .send()
                .await
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;
            return if resp.status().is_success() {
                Ok(())
            } else {
                Err(ProviderError::TransferFailed(format!(
                    "Upload failed: HTTP {}",
                    resp.status()
                )))
            };
        }

        for session in 0..=YANDEX_UPLOAD_MAX_SESSION_RESTARTS {
            let link = self.acquire_upload_target(encoded).await?;
            yd_log(&format!(
                "chunked upload session {}/{}: target acquired ({} bytes total, chunk {})",
                session + 1,
                YANDEX_UPLOAD_MAX_SESSION_RESTARTS + 1,
                total,
                YANDEX_UPLOAD_CHUNK_SIZE_BYTES
            ));

            let mut file = tokio::fs::File::open(local_path)
                .await
                .map_err(ProviderError::IoError)?;

            let mut uploaded: u64 = 0;
            let mut session_failed_with_gone = false;
            let mut session_error: Option<ProviderError> = None;

            'chunks: while uploaded < total {
                let remaining = total - uploaded;
                let chunk_size = remaining.min(YANDEX_UPLOAD_CHUNK_SIZE_BYTES);
                let chunk_end = uploaded + chunk_size - 1;

                file.seek(SeekFrom::Start(uploaded))
                    .await
                    .map_err(ProviderError::IoError)?;
                let mut buf = vec![0u8; chunk_size as usize];
                file.read_exact(&mut buf)
                    .await
                    .map_err(ProviderError::IoError)?;

                let content_range = format!("bytes {}-{}/{}", uploaded, chunk_end, total);

                for chunk_attempt in 1..=YANDEX_UPLOAD_CHUNK_MAX_RETRIES {
                    let req = self
                        .client
                        .put(&link.href)
                        .header("Content-Type", "application/octet-stream")
                        .header("Content-Range", &content_range)
                        .body(buf.clone());

                    match req.send().await {
                        Ok(resp) => {
                            let status = resp.status();
                            let code = status.as_u16();
                            if code == 201 {
                                yd_log(&format!(
                                    "chunked upload complete: {} bytes ({} chunks)",
                                    total,
                                    total.div_ceil(YANDEX_UPLOAD_CHUNK_SIZE_BYTES)
                                ));
                                if let Some(ref tx) = progress_tx {
                                    let _ = tx.send((total, total));
                                }
                                return Ok(());
                            } else if code == 202 {
                                uploaded = chunk_end + 1;
                                if let Some(ref tx) = progress_tx {
                                    let _ = tx.send((uploaded, total));
                                }
                                continue 'chunks;
                            } else if code == 410 || code == 404 {
                                yd_log(&format!(
                                    "chunked upload upload-target gone (HTTP {}): restart session",
                                    code
                                ));
                                session_failed_with_gone = true;
                                session_error = Some(ProviderError::TransferFailed(format!(
                                    "Upload target expired: HTTP {}",
                                    code
                                )));
                                break 'chunks;
                            } else if is_yandex_upload_retryable_status(status)
                                && chunk_attempt < YANDEX_UPLOAD_CHUNK_MAX_RETRIES
                            {
                                let backoff_ms =
                                    YANDEX_UPLOAD_BACKOFF_BASE_MS * (1u64 << (chunk_attempt - 1));
                                yd_log(&format!(
                                    "chunk {}-{} HTTP {} (attempt {}/{}), retry in {} ms",
                                    uploaded,
                                    chunk_end,
                                    status,
                                    chunk_attempt,
                                    YANDEX_UPLOAD_CHUNK_MAX_RETRIES,
                                    backoff_ms
                                ));
                                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms))
                                    .await;
                                continue;
                            } else {
                                return Err(ProviderError::TransferFailed(format!(
                                    "Upload failed at byte {}: HTTP {}",
                                    uploaded, status
                                )));
                            }
                        }
                        Err(e) => {
                            if chunk_attempt < YANDEX_UPLOAD_CHUNK_MAX_RETRIES {
                                let backoff_ms =
                                    YANDEX_UPLOAD_BACKOFF_BASE_MS * (1u64 << (chunk_attempt - 1));
                                yd_log(&format!(
                                    "chunk {}-{} network error (attempt {}/{}): {}. Retry in {} ms",
                                    uploaded,
                                    chunk_end,
                                    chunk_attempt,
                                    YANDEX_UPLOAD_CHUNK_MAX_RETRIES,
                                    e,
                                    backoff_ms
                                ));
                                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms))
                                    .await;
                                continue;
                            }
                            return Err(ProviderError::TransferFailed(e.to_string()));
                        }
                    }
                }
            }

            if !session_failed_with_gone {
                // Either we exited the chunks loop without sending the last
                // chunk (shouldn't happen with the logic above) or every
                // chunk got 202 and the server never returned 201. Surface
                // the last error if we have one, otherwise treat as success.
                if uploaded >= total {
                    return Ok(());
                }
                if let Some(e) = session_error {
                    return Err(e);
                }
                return Err(ProviderError::TransferFailed(format!(
                    "Upload incomplete at byte {}/{}",
                    uploaded, total
                )));
            }
            // Session expired (410): loop and acquire a fresh upload-target.
        }

        Err(ProviderError::TransferFailed(
            "Upload exhausted session restarts (upload-target kept expiring)".to_string(),
        ))
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

        let file_meta = tokio::fs::metadata(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let total = file_meta.len();

        // Forward progress updates from each per-attempt stream to the
        // caller through a serialized channel. On retry the per-attempt
        // counter restarts at 0, which the caller sees as a normal reset
        // before climbing back to `total` on the successful PUT.
        let progress_tx = on_progress.map(|cb| {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(u64, u64)>();
            tokio::spawn(async move {
                while let Some((sent, t)) = rx.recv().await {
                    cb(sent, t);
                }
            });
            tx
        });

        // Y3 fix: large payloads must use chunked Content-Range to survive
        // Yandex's mid-stream TCP disconnects on slow upload paths.
        if total > YANDEX_UPLOAD_CHUNKED_THRESHOLD_BYTES {
            return self
                .upload_chunked(local_path, &encoded, total, progress_tx)
                .await;
        }

        let mut last_error: Option<ProviderError> = None;
        for attempt in 1..=YANDEX_UPLOAD_MAX_ATTEMPTS {
            // Step 1: acquire a fresh upload-target on every attempt. Yandex
            // upload-target URLs are short-lived (signed with TTL); reusing
            // one from a failed attempt risks 410 Gone. The GET itself goes
            // through send_with_reauth which already handles 401-once.
            let link = match self.acquire_upload_target(&encoded).await {
                Ok(link) => link,
                Err(e)
                    if is_yandex_upload_retryable_error(&e)
                        && attempt < YANDEX_UPLOAD_MAX_ATTEMPTS =>
                {
                    let backoff_ms = YANDEX_UPLOAD_BACKOFF_BASE_MS * (1u64 << (attempt - 1));
                    yd_log(&format!(
                        "acquire upload-target failed on attempt {}/{}: {}. Retry in {} ms",
                        attempt, YANDEX_UPLOAD_MAX_ATTEMPTS, e, backoff_ms,
                    ));
                    last_error = Some(e);
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                    continue;
                }
                Err(e) => return Err(e),
            };

            // Step 2: PUT body. ReaderStream takes ownership of the file
            // handle, so we re-open it for every attempt.
            let file = tokio::fs::File::open(local_path)
                .await
                .map_err(ProviderError::IoError)?;

            use futures_util::StreamExt;
            use tokio_util::io::ReaderStream;

            let progress_for_attempt = progress_tx.clone();
            let mut uploaded: u64 = 0;
            let stream = ReaderStream::with_capacity(file, 65536).map(move |chunk| {
                if let Ok(bytes) = &chunk {
                    uploaded += bytes.len() as u64;
                    if let Some(ref tx) = progress_for_attempt {
                        let _ = tx.send((uploaded, total));
                    }
                }
                chunk
            });

            let body = reqwest::Body::wrap_stream(stream);
            let put_result = self
                .client
                .put(&link.href)
                .header("Content-Type", "application/octet-stream")
                .body(body)
                .send()
                .await;

            match put_result {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() || status.as_u16() == 201 || status.as_u16() == 202 {
                        yd_log(&format!(
                            "upload complete: {} bytes (attempt {}/{})",
                            total, attempt, YANDEX_UPLOAD_MAX_ATTEMPTS,
                        ));
                        return Ok(());
                    }
                    let err =
                        ProviderError::TransferFailed(format!("Upload failed: HTTP {}", status,));
                    if is_yandex_upload_retryable_status(status)
                        && attempt < YANDEX_UPLOAD_MAX_ATTEMPTS
                    {
                        let backoff_ms = YANDEX_UPLOAD_BACKOFF_BASE_MS * (1u64 << (attempt - 1));
                        yd_log(&format!(
                            "upload HTTP {} on attempt {}/{}, retry in {} ms with fresh upload-target",
                            status, attempt, YANDEX_UPLOAD_MAX_ATTEMPTS, backoff_ms,
                        ));
                        last_error = Some(err);
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                        continue;
                    }
                    return Err(err);
                }
                Err(e) => {
                    let err = ProviderError::TransferFailed(e.to_string());
                    if attempt < YANDEX_UPLOAD_MAX_ATTEMPTS {
                        let backoff_ms = YANDEX_UPLOAD_BACKOFF_BASE_MS * (1u64 << (attempt - 1));
                        yd_log(&format!(
                            "upload network error on attempt {}/{}: {}. Retry in {} ms with fresh upload-target",
                            attempt, YANDEX_UPLOAD_MAX_ATTEMPTS, e, backoff_ms,
                        ));
                        last_error = Some(err);
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                        continue;
                    }
                    return Err(err);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            ProviderError::TransferFailed("Upload exhausted retries".to_string())
        }))
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

    // delete_permanent: not overridden. Yandex Disk's `delete()` already
    // appends `permanently=true` to the API call (hard delete that bypasses
    // trash), so the default Ok(false) no-op is correct: there is nothing
    // left to purge afterwards. The inherent `permanent_delete_from_trash`
    // exists for the separate case of items already in trash from another
    // client.

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
            versioning_bytes: None,
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

    fn supports_server_side_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        // Legacy alias kept so CLI / MCP / provider_commands callers keep
        // working. The real `/resources/copy` implementation lives on
        // `server_side_copy` (S3-T10 migration, v4.0.0).
        StorageProvider::server_side_copy(self, from, to).await
    }

    async fn server_side_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
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
            supports_multipart: true,
            multipart_threshold: YANDEX_UPLOAD_CHUNKED_THRESHOLD_BYTES,
            multipart_part_size: YANDEX_UPLOAD_CHUNK_SIZE_BYTES,
            // Yandex upload-target accepts `Content-Range` only in
            // monotonically increasing order: parallel chunks at random
            // offsets fail with 400. Strict sequential dispatch.
            multipart_max_parallel: 1,
            supports_resume_download: true,
            supports_resume_upload: true,
            ..Default::default()
        }
    }

    // Shaped-graph multipart trait wiring (S3-T05).
    //
    // Yandex Disk's chunked-upload protocol maps onto the multipart trait as:
    //   1. `begin_multipart_upload` → GET `/resources/upload?path=X` to
    //      acquire a short-lived signed upload-target URL. The trait
    //      handle embeds the href alongside the URL-encoded disk path
    //      so callers can re-acquire on 410 Gone without resolving the
    //      destination tree again.
    //   2. `upload_part` → PUT `<href>` with `Content-Range: bytes A-B/T`.
    //      Yandex returns 201 on the final chunk, 202 on intermediate
    //      ones; both are success. 410/404 means the upload-target
    //      expired - we surface a typed error so the runner can decide
    //      whether to rebuild the whole upload from scratch.
    //   3. `complete_multipart_upload` → no-op: Yandex finalises when
    //      the 201 lands on the closing chunk. We still validate part
    //      count so a runner bug surfaces as a typed error.
    //   4. `abort_multipart_upload` → no-op. Yandex GCs unused
    //      upload-targets after their TTL expires.
    async fn begin_multipart_upload(
        &mut self,
        remote_path: &str,
        total_size: u64,
        _content_type: Option<&str>,
        _local_source_path: Option<&str>,
    ) -> Result<MultipartHandle, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        if total_size == 0 {
            return Err(ProviderError::Other(
                "Yandex Disk multipart upload requires non-zero total_size".to_string(),
            ));
        }

        let resolved = self.resolve_path(remote_path);
        let encoded = encode_yd_path(&resolved);
        let url_encoded = urlencoding::encode(&encoded).into_owned();
        let link = self.acquire_upload_target(&url_encoded).await?;

        let meta = YandexMultipartMeta {
            href: link.href,
            encoded_path: url_encoded,
            total: total_size,
            part: yandex_runner_part_size(total_size),
        };
        Ok(MultipartHandle {
            upload_id: meta.encode(),
            remote_path: remote_path.to_string(),
        })
    }

    // DAG-P2-05: Yandex's upload-target PUT is a single send with a known
    // length and no whole-part hashing, so stream the part body one bounded
    // window at a time instead of buffering the whole part in memory.
    fn multipart_streams_part_body(&self) -> bool {
        true
    }

    async fn upload_part(
        &mut self,
        handle: &MultipartHandle,
        part_number: u32,
        data: Vec<u8>,
    ) -> Result<UploadedPart, ProviderError> {
        self.upload_part_body(
            handle,
            part_number,
            crate::transfer_multipart::PartBody::owned(data),
        )
        .await
    }

    async fn upload_part_body(
        &mut self,
        handle: &MultipartHandle,
        part_number: u32,
        body: crate::transfer_multipart::PartBody,
    ) -> Result<UploadedPart, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        if part_number == 0 {
            return Err(ProviderError::Other(
                "Yandex Disk upload_part requires 1-based part_number".to_string(),
            ));
        }
        let part_len = body.len();
        if part_len == 0 {
            return Err(ProviderError::Other(
                "Yandex Disk upload_part received empty data".to_string(),
            ));
        }
        let meta = YandexMultipartMeta::decode(&handle.upload_id)?;
        let offset = (part_number as u64 - 1) * meta.part;
        let end = offset
            .checked_add(part_len)
            .ok_or_else(|| ProviderError::Other("Yandex Disk part offset overflow".to_string()))?;
        if end > meta.total {
            return Err(ProviderError::Other(format!(
                "Yandex Disk part {} exceeds declared total: offset {} + len {} > total {}",
                part_number, offset, part_len, meta.total
            )));
        }
        let content_range = format!("bytes {}-{}/{}", offset, end - 1, meta.total);

        let resp = self
            .client
            .put(&meta.href)
            .header("Content-Type", "application/octet-stream")
            .header("Content-Range", content_range)
            // DAG-P2-05: explicit length so the streamed body is fixed-length,
            // never chunked (the ranged PUT requires it).
            .header("Content-Length", part_len.to_string())
            .body(body.into_reqwest_body())
            .send()
            .await
            .map_err(|e| ProviderError::TransferFailed(e.to_string()))?;

        let status = resp.status();
        let code = status.as_u16();
        // 201 = full upload accepted (final chunk).
        // 202 = partial accept (any intermediate chunk).
        // 410/404 = upload-target expired; surface a typed error so the
        // runner can decide whether to rebuild from scratch.
        if code == 201 || code == 202 {
            Ok(UploadedPart {
                part_number,
                etag: String::new(),
            })
        } else if code == 410 || code == 404 {
            Err(ProviderError::TransferFailed(format!(
                "Yandex Disk upload-target expired (HTTP {}): re-acquire required",
                code
            )))
        } else {
            let text = resp.text().await.unwrap_or_default();
            Err(ProviderError::TransferFailed(format!(
                "Yandex Disk chunk {} failed ({}): {}",
                part_number,
                status,
                sanitize_api_error(&text)
            )))
        }
    }

    async fn complete_multipart_upload(
        &mut self,
        handle: MultipartHandle,
        parts: Vec<UploadedPart>,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let meta = YandexMultipartMeta::decode(&handle.upload_id)?;
        let expected = meta.total.div_ceil(meta.part).max(1) as usize;
        if parts.len() != expected {
            return Err(ProviderError::TransferFailed(format!(
                "Yandex Disk complete: expected {} parts, runner committed {}",
                expected,
                parts.len()
            )));
        }
        Ok(())
    }

    async fn abort_multipart_upload(
        &mut self,
        _handle: MultipartHandle,
    ) -> Result<(), ProviderError> {
        // Yandex Disk has no documented abort for upload-targets; the
        // ephemeral signed URL is GCed automatically when its TTL
        // expires. Returning Ok keeps abort from masking the original
        // transfer error.
        Ok(())
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

    // Row 4 (#347): the body-level `error` string maps to a rich variant
    // independent of the HTTP status (Yandex returns 409/404 etc. with these).
    #[test]
    fn classify_yandex_error_maps_body_error_codes() {
        let unauth = r#"{"error":"UnauthorizedError","description":"token bad"}"#;
        assert!(matches!(
            YandexDiskProvider::classify_yandex_error(401, unauth),
            ProviderError::AuthenticationFailed(_)
        ));

        for code in ["DiskNotFoundError", "DiskPathDoesntExistsError"] {
            let body = format!(r#"{{"error":"{code}","description":"missing"}}"#);
            assert!(
                matches!(
                    YandexDiskProvider::classify_yandex_error(404, &body),
                    ProviderError::NotFound(ref m) if m == "missing"
                ),
                "{code} must map to NotFound"
            );
        }

        for code in [
            "DiskResourceAlreadyExistsError",
            "PlatformResourceAlreadyExists",
        ] {
            let body = format!(r#"{{"error":"{code}","description":"dup"}}"#);
            assert!(
                matches!(
                    YandexDiskProvider::classify_yandex_error(409, &body),
                    ProviderError::AlreadyExists(ref m) if m == "dup"
                ),
                "{code} must map to AlreadyExists"
            );
        }

        let root = r#"{"error":"DiskPathPointsToRootError","description":"is root"}"#;
        assert!(matches!(
            YandexDiskProvider::classify_yandex_error(400, root),
            ProviderError::InvalidPath(_)
        ));

        let quota = r#"{"error":"DiskStorageQuotaExhaustedError","description":"full"}"#;
        assert!(matches!(
            YandexDiskProvider::classify_yandex_error(507, quota),
            ProviderError::TransferFailed(_)
        ));
    }

    // Row 4 (#347): when the body has no recognised `error` code, the HTTP
    // status drives the variant. The catch-all keeps the `StatusCode` Display form.
    #[test]
    fn classify_yandex_error_falls_back_to_http_status() {
        // Empty/garbage body -> status-only mapping.
        assert!(matches!(
            YandexDiskProvider::classify_yandex_error(401, ""),
            ProviderError::AuthenticationFailed(_)
        ));
        assert!(matches!(
            YandexDiskProvider::classify_yandex_error(403, ""),
            ProviderError::PermissionDenied(ref m) if m == "Forbidden"
        ));
        assert!(matches!(
            YandexDiskProvider::classify_yandex_error(404, "not json"),
            ProviderError::NotFound(_)
        ));
        assert!(matches!(
            YandexDiskProvider::classify_yandex_error(409, ""),
            ProviderError::AlreadyExists(_)
        ));
        assert!(matches!(
            YandexDiskProvider::classify_yandex_error(429, ""),
            ProviderError::ServerError(ref m) if m == "Rate limit exceeded"
        ));
        assert!(matches!(
            YandexDiskProvider::classify_yandex_error(507, ""),
            ProviderError::TransferFailed(ref m) if m == "Insufficient storage"
        ));
        // Catch-all preserves "HTTP <code> <reason>: <body>".
        match YandexDiskProvider::classify_yandex_error(500, "boom") {
            ProviderError::ServerError(msg) => {
                assert!(msg.contains("HTTP 500 Internal Server Error"), "got: {msg}");
                assert!(msg.contains("boom"), "got: {msg}");
            }
            e => panic!("expected ServerError, got {e:?}"),
        }
    }

    // Compile-time invariants for the chunked upload path. These are
    // expressed as `const _: () = assert!(..)` rather than runtime tests
    // because the operands are all `const` (clippy correctly flags runtime
    // assertions on constant expressions).
    const _: () = {
        // Files <= 32 MiB stay on the original single-PUT path.
        assert!(1024 * 1024 < YANDEX_UPLOAD_CHUNKED_THRESHOLD_BYTES);
        assert!(10 * 1024 * 1024 < YANDEX_UPLOAD_CHUNKED_THRESHOLD_BYTES);
        // 100 MiB and 1 GiB must trigger the chunked path.
        assert!(100 * 1024 * 1024 > YANDEX_UPLOAD_CHUNKED_THRESHOLD_BYTES);
        assert!(1024 * 1024 * 1024 > YANDEX_UPLOAD_CHUNKED_THRESHOLD_BYTES);
        // The threshold must be at least one chunk wide.
        assert!(YANDEX_UPLOAD_CHUNK_SIZE_BYTES <= YANDEX_UPLOAD_CHUNKED_THRESHOLD_BYTES);
        // 1 GiB / 8 MiB = 128 chunks (sanity check on chunk granularity).
        assert!((1024u64 * 1024 * 1024).div_ceil(YANDEX_UPLOAD_CHUNK_SIZE_BYTES) == 128);
    };

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

    #[test]
    fn upload_retryable_status_accepts_5xx_408_425_429() {
        for code in [500, 501, 502, 503, 504, 408, 425, 429] {
            let status = reqwest::StatusCode::from_u16(code).unwrap();
            assert!(
                is_yandex_upload_retryable_status(status),
                "HTTP {} should be retryable",
                code
            );
        }
    }

    #[test]
    fn upload_retryable_status_rejects_2xx_4xx_terminal() {
        for code in [200, 201, 202, 400, 401, 403, 404, 410, 413] {
            let status = reqwest::StatusCode::from_u16(code).unwrap();
            assert!(
                !is_yandex_upload_retryable_status(status),
                "HTTP {} should NOT be retryable",
                code
            );
        }
    }

    #[test]
    fn upload_retryable_error_accepts_network_and_server() {
        let net = ProviderError::NetworkError("connection reset".into());
        let srv = ProviderError::ServerError("upstream timeout".into());
        assert!(is_yandex_upload_retryable_error(&net));
        assert!(is_yandex_upload_retryable_error(&srv));
    }

    #[test]
    fn upload_retryable_error_rejects_terminal_classes() {
        let auth = ProviderError::AuthenticationFailed("revoked".into());
        let path = ProviderError::InvalidPath("../traversal".into());
        let parse = ProviderError::ParseError("bad json".into());
        let transfer = ProviderError::TransferFailed("Upload failed: HTTP 413".into());
        assert!(!is_yandex_upload_retryable_error(&auth));
        assert!(!is_yandex_upload_retryable_error(&path));
        assert!(!is_yandex_upload_retryable_error(&parse));
        assert!(!is_yandex_upload_retryable_error(&transfer));
    }

    #[test]
    fn upload_backoff_is_exponential_500ms_base() {
        // Backoff schedule: attempt 1 -> 500 ms, attempt 2 -> 1 s, attempt 3 -> 2 s.
        assert_eq!(YANDEX_UPLOAD_BACKOFF_BASE_MS, 500);
        assert_eq!(YANDEX_UPLOAD_BACKOFF_BASE_MS * (1u64 << 1), 1000);
        assert_eq!(YANDEX_UPLOAD_BACKOFF_BASE_MS * (1u64 << 2), 2000);
        assert_eq!(YANDEX_UPLOAD_MAX_ATTEMPTS, 3);
    }

    // ---- S3-T05 multipart trait wiring ----

    #[test]
    fn yandex_multipart_meta_roundtrip_preserves_fields() {
        let meta = YandexMultipartMeta {
            href: "https://uploader321.disk.yandex.net/upload-target/foo?signature=xyz".to_string(),
            encoded_path: "disk%3A%2Ffoo%2Fbar.bin".to_string(),
            total: 1_073_741_824,
            part: 8 * 1024 * 1024,
        };
        let encoded = meta.encode();
        let decoded = YandexMultipartMeta::decode(&encoded).expect("decode roundtrip");
        assert_eq!(meta, decoded);
    }

    #[test]
    fn yandex_multipart_meta_decode_rejects_garbage() {
        let err = YandexMultipartMeta::decode("not-json").unwrap_err();
        assert!(matches!(err, ProviderError::Other(_)));
    }

    #[test]
    fn yandex_runner_part_size_clamps_and_never_returns_zero() {
        assert_eq!(yandex_runner_part_size(1024), 1024);
        assert_eq!(
            yandex_runner_part_size(YANDEX_UPLOAD_CHUNK_SIZE_BYTES),
            YANDEX_UPLOAD_CHUNK_SIZE_BYTES
        );
        assert_eq!(
            yandex_runner_part_size(50 * 1024 * 1024 * 1024),
            YANDEX_UPLOAD_CHUNK_SIZE_BYTES
        );
        assert_eq!(yandex_runner_part_size(0), 1);
    }

    #[test]
    fn yandex_content_range_math_is_inclusive_zero_based() {
        let part = YANDEX_UPLOAD_CHUNK_SIZE_BYTES;
        let total: u64 = 2 * part + 4096;
        let range = |n: u32| -> String {
            let offset = (n as u64 - 1) * part;
            let len = ((total - offset).min(part)) as usize;
            let end = offset + len as u64;
            format!("bytes {}-{}/{}", offset, end - 1, total)
        };
        assert_eq!(range(1), format!("bytes 0-{}/{}", part - 1, total));
        assert_eq!(
            range(2),
            format!("bytes {}-{}/{}", part, 2 * part - 1, total)
        );
        assert_eq!(
            range(3),
            format!("bytes {}-{}/{}", 2 * part, total - 1, total)
        );
    }

    #[test]
    fn yandex_transfer_hints_advertise_multipart_sequential() {
        // Hints are computed from constants, not state.
        let p = YandexDiskProvider::new("test-token".to_string(), None);
        let hints = p.transfer_optimization_hints();
        assert!(hints.supports_multipart);
        assert_eq!(
            hints.multipart_threshold,
            YANDEX_UPLOAD_CHUNKED_THRESHOLD_BYTES
        );
        assert_eq!(hints.multipart_part_size, YANDEX_UPLOAD_CHUNK_SIZE_BYTES);
        // Content-Range monotonic ⇒ strict sequential dispatch.
        assert_eq!(hints.multipart_max_parallel, 1);
        assert!(hints.supports_resume_download);
        assert!(hints.supports_resume_upload);
    }
}
