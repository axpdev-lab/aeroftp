//! Infomaniak kDrive Storage Provider
//!
//! Implements StorageProvider for Infomaniak kDrive using the REST API.
//! Uses API Token (Bearer) for authentication: no OAuth2 flow needed.
//!
//! API Base: https://api.infomaniak.com
//! Rate limit: 60 requests/minute
//! Upload: direct POST up to 1GB
//! Pagination: cursor-based (has_more + cursor)

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use tracing::info;

use super::{
    sanitize_api_error, send_with_retry, FileVersion, HttpRetryConfig, KDriveConfig,
    MultipartHandle, ProviderError, ProviderType, RemoteEntry, ShareLinkCapabilities, ShareLinkInfo,
    ShareLinkOptions, ShareLinkResult, StorageInfo, StorageProvider, UploadedPart,
};

const API_BASE: &str = "https://api.infomaniak.com";

/// Host for kDrive chunked-session upload endpoints (S3-T03).
///
/// kDrive splits its public API across two subdomains:
///   - `api.infomaniak.com` handles the legacy single-shot `/3/drive/<id>/upload`
///     (used by the legacy `upload()`) and all metadata/list/move operations.
///   - `api.kdrive.infomaniak.com` handles the session-based chunked upload
///     (`/upload/session/start`, `/.../chunk`, `/.../finish`). Hitting the
///     session paths on `api.infomaniak.com` returns
///     `404 method_not_found` (verified live 2026-05-26).
///
/// Documented in `docs/dev/guides/kDrive/06-upload-file.md` (sourced from
/// Infomaniak's iOS SDK).
const KDRIVE_SESSION_HOST: &str = "https://api.kdrive.infomaniak.com";

/// kDrive chunked-upload size threshold (S3-T03).
///
/// Below this size the runner keeps the legacy single-shot `/upload` path;
/// at or above, it switches to the upload-session API
/// (`/upload/session` + `/chunk` + `/finish`) so the DAG shaped-graph can
/// fan out chunks and gain per-chunk retry.
const KDRIVE_MULTIPART_THRESHOLD: u64 = 4 * 1024 * 1024;

/// kDrive chunked-upload preferred part size (S3-T03).
///
/// Infomaniak's docs recommend a 4 MiB chunk granularity; the upload session
/// API rejects anything below 1 MiB (except the closing tail) so this is the
/// safe sweet spot.
const KDRIVE_MULTIPART_PART_SIZE: u64 = 4 * 1024 * 1024;

fn kdrive_log(msg: &str) {
    info!("[KDRIVE] {}", msg);
}

// ─── API Response Types ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    #[allow(dead_code)]
    result: Option<String>,
    data: Option<T>,
    #[allow(dead_code)]
    error: Option<ApiError>,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    #[allow(dead_code)]
    code: Option<String>,
    #[allow(dead_code)]
    description: Option<String>,
}

/// The /3/drive/{id}/files/{id}/files returns { result, data: [...] } at top level
/// but sometimes returns { result, data: { data: [...], has_more, cursor } }
/// We handle both shapes.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum FilesPayload {
    Paginated {
        #[serde(default)]
        data: Vec<KDriveFile>,
        has_more: Option<bool>,
        cursor: Option<String>,
    },
    Flat(Vec<KDriveFile>),
}

#[derive(Debug, Deserialize)]
struct KDriveFile {
    id: i64,
    name: Option<String>,
    #[serde(rename = "type")]
    file_type: Option<String>, // "dir" or "file"
    size: Option<i64>,
    #[serde(rename = "last_modified_at")]
    last_modified: Option<i64>, // Unix timestamp
    #[allow(dead_code)]
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DriveInfo {
    #[allow(dead_code)]
    id: Option<i64>,
    #[allow(dead_code)]
    name: Option<String>,
    used_size: Option<i64>,
    size: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct UploadResponse {
    #[allow(dead_code)]
    id: Option<i64>,
    #[allow(dead_code)]
    name: Option<String>,
}

/// `/3/drive/<id>/upload/session` response payload (S3-T03).
#[derive(Debug, Deserialize)]
struct KDriveUploadSession {
    token: String,
}

/// Side-band metadata embedded in `MultipartHandle.upload_id` for the
/// kDrive chunked-session path. Carries everything `upload_part` and
/// `complete_multipart_upload` need to dispatch chunks against the
/// session without re-resolving the destination parent on every call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct KDriveMultipartMeta {
    token: String,
    parent_id: i64,
    name: String,
    total: u64,
    part: u64,
    total_chunks: u32,
}

impl KDriveMultipartMeta {
    fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    fn decode(raw: &str) -> Result<Self, ProviderError> {
        serde_json::from_str(raw).map_err(|e| {
            ProviderError::Other(format!("kDrive multipart handle decode failed: {}", e))
        })
    }
}

/// Compute the per-chunk size the runner should slice `total` bytes into.
///
/// Caps at `KDRIVE_MULTIPART_PART_SIZE` and never returns zero so a
/// `total < part_size` upload always produces exactly one chunk.
fn kdrive_runner_part_size(total: u64) -> u64 {
    KDRIVE_MULTIPART_PART_SIZE.min(total.max(1))
}

/// Derive `total_chunks` from `total / part` matching the runner formula.
fn kdrive_total_chunks(total: u64, part: u64) -> u32 {
    let raw = total.div_ceil(part.max(1)).max(1);
    raw.min(u32::MAX as u64) as u32
}

#[derive(Debug, Deserialize)]
struct ShareLinkData {
    url: Option<String>,
    #[allow(dead_code)]
    uuid: Option<String>,
    #[serde(default)]
    valid_until: Option<serde_json::Value>,
    #[serde(default)]
    right: Option<String>,
    #[serde(default)]
    has_password: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct KDriveVersion {
    id: i64,
    #[allow(dead_code)]
    file_id: Option<i64>,
    size: Option<i64>,
    created_at: Option<i64>,
    #[allow(dead_code)]
    user_id: Option<i64>,
    version_number: Option<i64>,
}

// KD-010: Trash types: ready for use when StorageProvider trait adds trash methods.
// See docs/dev/guides/kDrive/14-trash.md for API details.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct TrashFile {
    id: i64,
    name: Option<String>,
    #[serde(rename = "type")]
    file_type: Option<String>,
    size: Option<i64>,
    #[serde(rename = "last_modified_at")]
    last_modified: Option<i64>,
    deleted_at: Option<i64>,
    path: Option<String>,
}

/// Paginated response shape for trash listings
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum TrashPayload {
    Paginated {
        #[serde(default)]
        data: Vec<TrashFile>,
        has_more: Option<bool>,
        cursor: Option<String>,
    },
    Flat(Vec<TrashFile>),
}

// ─── Dir Cache ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct DirInfo {
    id: i64,
}

// ─── Provider ────────────────────────────────────────────────────────────

/// M3: Maximum number of cached directory entries to prevent unbounded memory growth.
const DIR_CACHE_MAX_ENTRIES: usize = 10_000;

pub struct KDriveProvider {
    config: KDriveConfig,
    client: reqwest::Client,
    connected: bool,
    root_file_id: i64,
    current_path: String,
    current_file_id: i64,
    dir_cache: HashMap<String, DirInfo>,
}

impl KDriveProvider {
    pub fn new(config: KDriveConfig) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(crate::providers::AEROFTP_USER_AGENT)
            .connect_timeout(std::time::Duration::from_secs(30))
            .read_timeout(std::time::Duration::from_secs(1800))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            config,
            client,
            connected: false,
            root_file_id: 1,
            current_path: "/".to_string(),
            current_file_id: 1,
            dir_cache: HashMap::new(),
        }
    }

    // ─── Helpers ─────────────────────────────────────────────────────────

    /// M3: Insert into dir_cache with eviction when cap is reached.
    /// Clears the entire cache when it exceeds DIR_CACHE_MAX_ENTRIES,
    /// allowing it to repopulate naturally during navigation.
    fn dir_cache_insert(&mut self, key: String, value: DirInfo) {
        if self.dir_cache.len() >= DIR_CACHE_MAX_ENTRIES {
            kdrive_log(&format!(
                "dir_cache reached {} entries, evicting all",
                self.dir_cache.len()
            ));
            self.dir_cache.clear();
        }
        self.dir_cache.insert(key, value);
    }

    /// M7: Returns Result instead of silently falling back to an empty header on invalid tokens.
    /// An empty Authorization header would cause silent auth failures that are hard to debug.
    fn auth_header(&self) -> Result<HeaderValue, ProviderError> {
        HeaderValue::from_str(&format!("Bearer {}", self.config.api_token.expose_secret())).map_err(
            |e| {
                ProviderError::AuthenticationFailed(format!(
                    "Invalid characters in API token: {}",
                    e
                ))
            },
        )
    }

    /// KD-004: Send a GET request with automatic retry on 429/5xx
    async fn get_with_retry(&self, url: &str) -> Result<reqwest::Response, ProviderError> {
        let request = self
            .client
            .get(url)
            .header(AUTHORIZATION, self.auth_header()?)
            .build()
            .map_err(|e| ProviderError::ConnectionFailed(format!("Build request failed: {}", e)))?;
        send_with_retry(&self.client, request, &HttpRetryConfig::default())
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Request failed: {}", e)))
    }

    /// KD-004: Send a POST request with automatic retry on 429/5xx
    async fn post_with_retry(
        &self,
        url: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> Result<reqwest::Response, ProviderError> {
        let request = self
            .client
            .post(url)
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, content_type)
            .body(body)
            .build()
            .map_err(|e| ProviderError::ConnectionFailed(format!("Build request failed: {}", e)))?;
        send_with_retry(&self.client, request, &HttpRetryConfig::default())
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Request failed: {}", e)))
    }

    /// KD-004: Send a DELETE request with automatic retry on 429/5xx
    async fn delete_with_retry(&self, url: &str) -> Result<reqwest::Response, ProviderError> {
        let request = self
            .client
            .delete(url)
            .header(AUTHORIZATION, self.auth_header()?)
            .build()
            .map_err(|e| ProviderError::ConnectionFailed(format!("Build request failed: {}", e)))?;
        send_with_retry(&self.client, request, &HttpRetryConfig::default())
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Request failed: {}", e)))
    }

    fn api_url_v2(&self, path: &str) -> String {
        format!("{}/2/drive/{}{}", API_BASE, self.config.drive_id, path)
    }

    fn api_url_v3(&self, path: &str) -> String {
        format!("{}/3/drive/{}{}", API_BASE, self.config.drive_id, path)
    }

    fn normalize_path(path: &str) -> String {
        let trimmed = path.trim().replace('\\', "/");
        if trimmed.is_empty() || trimmed == "/" {
            return "/".to_string();
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
        // "." or empty means current directory
        if trimmed.is_empty() || trimmed == "." {
            return self.current_path.clone();
        }
        // Check leading slash on the raw input before normalizing -
        // normalize_path unconditionally prepends "/", which would otherwise
        // make every relative input appear absolute and skip the current_path join.
        if trimmed.starts_with('/') {
            return Self::normalize_path(trimmed);
        }
        let base = self.current_path.trim_end_matches('/');
        Self::normalize_path(&format!("{}/{}", base, trimmed))
    }

    fn split_path(path: &str) -> (&str, &str) {
        let normalized = path.trim_end_matches('/');
        match normalized.rfind('/') {
            Some(0) | None => ("/", normalized.trim_start_matches('/')),
            Some(pos) => (&normalized[..pos], &normalized[pos + 1..]),
        }
    }

    // ─── Folder Resolution ───────────────────────────────────────────────

    async fn resolve_folder_id(&mut self, path: &str) -> Result<i64, ProviderError> {
        let normalized = Self::normalize_path(path);

        if normalized == "/" {
            return Ok(self.root_file_id);
        }

        // Check cache
        if let Some(info) = self.dir_cache.get(&normalized) {
            return Ok(info.id);
        }

        // Walk path components
        let parts: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();
        let mut current_id = self.root_file_id;
        let mut current_path = String::new();

        for part in &parts {
            current_path = format!("{}/{}", current_path, part);

            if let Some(info) = self.dir_cache.get(&current_path) {
                current_id = info.id;
                continue;
            }

            // KD-011: List children with pagination to handle directories with >200 entries
            let base_url = self.api_url_v3(&format!("/files/{}/files", current_id));
            let mut cursor: Option<String> = None;
            let mut found = false;

            'pagination: loop {
                let url = match cursor {
                    Some(ref c) => format!("{}?with=path&cursor={}", base_url, c),
                    None => format!("{}?with=path", base_url),
                };

                let resp = self.get_with_retry(&url).await?;

                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    return Err(ProviderError::ServerError(format!(
                        "List {} failed ({}): {}",
                        current_path,
                        status,
                        sanitize_api_error(&body)
                    )));
                }

                let api_resp: ApiResponse<FilesPayload> = resp.json().await.map_err(|e| {
                    ProviderError::ServerError(format!("Parse list response failed: {}", e))
                })?;

                let (files, has_more, next_cursor) = match api_resp.data {
                    Some(FilesPayload::Paginated {
                        data,
                        has_more,
                        cursor: c,
                    }) => (data, has_more, c),
                    Some(FilesPayload::Flat(data)) => (data, Some(false), None),
                    None => (vec![], Some(false), None),
                };

                // KD-003: Try exact match first, then case-insensitive fallback
                let mut case_insensitive_match: Option<i64> = None;

                for file in &files {
                    if file.file_type.as_deref() == Some("dir") {
                        if let Some(ref name) = file.name {
                            if name == *part {
                                // Exact match: use immediately
                                self.dir_cache_insert(
                                    current_path.clone(),
                                    DirInfo { id: file.id },
                                );
                                current_id = file.id;
                                found = true;
                                break 'pagination;
                            } else if case_insensitive_match.is_none()
                                && name.eq_ignore_ascii_case(part)
                            {
                                case_insensitive_match = Some(file.id);
                            }
                        }
                    }
                }

                if has_more != Some(true) || next_cursor.is_none() {
                    // No more pages: use case-insensitive match if found
                    if let Some(id) = case_insensitive_match {
                        self.dir_cache_insert(current_path.clone(), DirInfo { id });
                        current_id = id;
                        found = true;
                    }
                    break;
                }
                cursor = next_cursor;
            }

            if !found {
                return Err(ProviderError::NotFound(format!(
                    "Folder '{}' not found in {}",
                    part, current_path
                )));
            }
        }

        Ok(current_id)
    }

    /// Find a file by name in a given folder, returns (file_id, is_dir)
    /// KD-003: Exact match first, case-insensitive fallback
    /// KD-004: Uses retry wrapper for transient errors
    async fn find_file_in_folder(
        &self,
        folder_id: i64,
        filename: &str,
    ) -> Result<Option<(i64, bool)>, ProviderError> {
        let base_url = self.api_url_v3(&format!("/files/{}/files", folder_id));
        let mut cursor: Option<String> = None;
        let mut case_insensitive_match: Option<(i64, bool)> = None;

        loop {
            let url = match cursor {
                Some(ref c) => format!("{}?cursor={}", base_url, c),
                None => base_url.clone(),
            };

            let resp = self.get_with_retry(&url).await?;

            if !resp.status().is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!(
                    "Find file failed: {}",
                    sanitize_api_error(&body)
                )));
            }

            let api_resp: ApiResponse<FilesPayload> = resp.json().await.map_err(|e| {
                ProviderError::ServerError(format!("Parse find response failed: {}", e))
            })?;

            let (files, has_more, next_cursor) = match api_resp.data {
                Some(FilesPayload::Paginated {
                    data,
                    has_more,
                    cursor: c,
                }) => (data, has_more, c),
                Some(FilesPayload::Flat(data)) => (data, Some(false), None),
                None => (vec![], Some(false), None),
            };

            for file in &files {
                if let Some(ref name) = file.name {
                    let is_dir = file.file_type.as_deref() == Some("dir");
                    if name == filename {
                        // Exact match: return immediately
                        return Ok(Some((file.id, is_dir)));
                    } else if case_insensitive_match.is_none()
                        && name.eq_ignore_ascii_case(filename)
                    {
                        case_insensitive_match = Some((file.id, is_dir));
                    }
                }
            }

            if has_more != Some(true) || next_cursor.is_none() {
                break;
            }
            cursor = next_cursor;
        }

        // Fall back to case-insensitive match if no exact match found
        Ok(case_insensitive_match)
    }
}

// ─── StorageProvider Implementation ──────────────────────────────────────

#[async_trait]
impl StorageProvider for KDriveProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::KDrive
    }

    fn display_name(&self) -> String {
        format!("kDrive ({})", self.config.drive_id)
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        kdrive_log(&format!(
            "Connecting to kDrive (drive_id={})",
            self.config.drive_id
        ));

        // Validate token + drive_id by fetching drive info
        let url = self.api_url_v2("");
        kdrive_log(&format!("Connect URL: {}", url));
        let resp = self.get_with_retry(&url).await.map_err(|e| {
            kdrive_log(&format!("Connection error: {}", e));
            ProviderError::ConnectionFailed(format!("Connection failed: {}", e))
        })?;

        let status = resp.status();
        if status.as_u16() == 401 {
            return Err(ProviderError::AuthenticationFailed(
                "Invalid API token. Generate one at manager.infomaniak.com > Developer > API Tokens".to_string()
            ));
        }
        if status.as_u16() == 404 {
            return Err(ProviderError::AuthenticationFailed(format!(
                "Drive ID '{}' not found. Check your kDrive ID in the Infomaniak dashboard.",
                self.config.drive_id
            )));
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ConnectionFailed(format!(
                "kDrive connection failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        // Parse drive info to confirm access
        let _drive_info: ApiResponse<DriveInfo> = resp.json().await.map_err(|e| {
            ProviderError::ConnectionFailed(format!("Failed to parse drive info: {}", e))
        })?;

        // Initialize root
        self.root_file_id = 1;
        self.current_file_id = self.root_file_id;
        self.current_path = "/".to_string();
        self.dir_cache_insert(
            "/".to_string(),
            DirInfo {
                id: self.root_file_id,
            },
        );

        // Navigate to initial path if specified
        if let Some(ref initial) = self.config.initial_path {
            let initial = initial.trim().to_string();
            if !initial.is_empty() && initial != "/" {
                let normalized = Self::normalize_path(&initial);
                kdrive_log(&format!("Navigating to initial path: {}", normalized));
                match self.resolve_folder_id(&normalized).await {
                    Ok(id) => {
                        self.current_path = normalized;
                        self.current_file_id = id;
                    }
                    Err(e) => {
                        kdrive_log(&format!("Initial path error (using root): {}", e));
                    }
                }
            }
        }

        self.connected = true;
        kdrive_log("Connected successfully");
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        self.current_path = "/".to_string();
        self.current_file_id = 1;
        self.dir_cache.clear();
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn pwd(&mut self) -> Result<String, ProviderError> {
        Ok(self.current_path.clone())
    }

    async fn cd(&mut self, path: &str) -> Result<(), ProviderError> {
        let new_path = if path.starts_with('/') {
            Self::normalize_path(path)
        } else if path == ".." {
            let mut parts: Vec<&str> = self
                .current_path
                .split('/')
                .filter(|s| !s.is_empty())
                .collect();
            parts.pop();
            if parts.is_empty() {
                "/".to_string()
            } else {
                format!("/{}", parts.join("/"))
            }
        } else {
            let base = self.current_path.trim_end_matches('/');
            format!("{}/{}", base, path)
        };

        let folder_id = self.resolve_folder_id(&new_path).await?;
        self.current_file_id = folder_id;
        self.current_path = new_path;
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.cd("..").await
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let resolved = self.resolve_path(path);
        let folder_id = self.resolve_folder_id(&resolved).await?;

        let mut entries = Vec::new();
        let mut cursor: Option<String> = None;

        let base_url = self.api_url_v3(&format!("/files/{}/files", folder_id));

        loop {
            let url = match cursor {
                Some(ref c) => format!("{}?cursor={}", base_url, c),
                None => base_url.clone(),
            };

            let resp = self.get_with_retry(&url).await?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!(
                    "List {} failed ({}): {}",
                    resolved,
                    status,
                    sanitize_api_error(&body)
                )));
            }

            let api_resp: ApiResponse<FilesPayload> = resp.json().await.map_err(|e| {
                ProviderError::ServerError(format!("Parse list response failed: {}", e))
            })?;

            let (files, has_more, next_cursor) = match api_resp.data {
                Some(FilesPayload::Paginated {
                    data,
                    has_more,
                    cursor: c,
                }) => (data, has_more, c),
                Some(FilesPayload::Flat(data)) => (data, Some(false), None),
                None => (vec![], Some(false), None),
            };

            for file in files {
                let name = file.name.unwrap_or_else(|| format!("unnamed_{}", file.id));
                let is_dir = file.file_type.as_deref() == Some("dir");
                let size = file.size.unwrap_or(0).max(0) as u64;
                let modified = file.last_modified.map(|ts| {
                    chrono::DateTime::from_timestamp(ts, 0)
                        .unwrap_or_default()
                        .format("%Y-%m-%d %H:%M:%SZ")
                        .to_string()
                });

                // Cache directories
                if is_dir {
                    let dir_path = if resolved == "/" {
                        format!("/{}", name)
                    } else {
                        format!("{}/{}", resolved, name)
                    };
                    self.dir_cache_insert(dir_path, DirInfo { id: file.id });
                }

                let entry_path = if resolved == "/" {
                    format!("/{}", name)
                } else {
                    format!("{}/{}", resolved, name)
                };

                entries.push(RemoteEntry {
                    name,
                    path: entry_path,
                    is_dir,
                    size,
                    modified,
                    permissions: None,
                    owner: None,
                    group: None,
                    is_symlink: false,
                    link_target: None,
                    metadata: HashMap::new(),
                    mime_type: None,
                });
            }

            if has_more != Some(true) || next_cursor.is_none() {
                break;
            }
            cursor = next_cursor;
        }

        // Update current position
        self.current_path = resolved;
        self.current_file_id = folder_id;

        Ok(entries)
    }

    async fn download(
        &mut self,
        remote_path: &str,
        local_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(remote_path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        let (file_id, _is_dir) = self
            .find_file_in_folder(parent_id, filename)
            .await?
            .ok_or_else(|| ProviderError::NotFound(format!("File '{}' not found", filename)))?;

        kdrive_log(&format!("Downloading file {} (id={})", filename, file_id));

        let url = self.api_url_v2(&format!("/files/{}/download", file_id));
        let resp = self.get_with_retry(&url).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Download failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        // KD-002: Streaming download: write chunks progressively instead of buffering in RAM
        // KD-005: Call progress callback with bytes_written/total_size
        use futures_util::StreamExt;

        let total_size = resp.content_length().unwrap_or(0);
        let mut stream = resp.bytes_stream();
        let mut atomic = super::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let mut downloaded: u64 = 0;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                ProviderError::TransferFailed(format!("Download stream error: {}", e))
            })?;
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

        kdrive_log(&format!("Downloaded {} ({} bytes)", filename, downloaded));
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
        let resolved = self.resolve_path(remote_path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        let (file_id, _is_dir) = self
            .find_file_in_folder(parent_id, filename)
            .await?
            .ok_or_else(|| ProviderError::NotFound(format!("File '{}' not found", filename)))?;

        let url = self.api_url_v2(&format!("/files/{}/download", file_id));
        let auth = self.auth_header()?;

        super::http_resumable_download(
            local_path,
            |range_header| {
                let mut req = self.client.get(&url).header(AUTHORIZATION, auth.clone());
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
        let resolved = self.resolve_path(remote_path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        let (file_id, _) = self
            .find_file_in_folder(parent_id, filename)
            .await?
            .ok_or_else(|| ProviderError::NotFound(format!("File '{}' not found", filename)))?;

        let url = self.api_url_v2(&format!("/files/{}/download", file_id));
        let resp = self.get_with_retry(&url).await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Download failed: {}",
                sanitize_api_error(&body)
            )));
        }

        // H2: Size-limited download to prevent OOM on large files
        super::response_bytes_with_limit(resp, super::MAX_DOWNLOAD_TO_BYTES).await
    }

    async fn upload(
        &mut self,
        local_path: &str,
        remote_path: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(remote_path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        // KD-001: Stream file instead of reading entire file into RAM
        let file_meta = tokio::fs::metadata(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let file_size = file_meta.len();
        kdrive_log(&format!(
            "Uploading {} ({} bytes) to folder {}",
            filename, file_size, parent_id
        ));

        // KD-008: Removed preemptive delete: use conflict=version to let the API
        // create a new version atomically. This prevents data loss if the upload fails.

        if let Some(ref cb) = on_progress {
            cb(0, file_size);
        }

        let last_modified = std::fs::metadata(local_path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let base_url = self.api_url_v3("/upload");
        let url =
            format!(
            "{}?directory_id={}&file_name={}&total_size={}&last_modified_at={}&conflict=version",
            base_url, parent_id,
            urlencoding::encode(filename),
            file_size, last_modified
        );

        // KD-001: Use ReaderStream for streaming upload body
        let file = tokio::fs::File::open(local_path)
            .await
            .map_err(ProviderError::IoError)?;
        let stream = tokio_util::io::ReaderStream::new(file);
        let body = reqwest::Body::wrap_stream(stream);

        let resp: reqwest::Response = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(body)
            .send()
            .await
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!("Upload request failed: {}", e))
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Upload failed ({}): {}",
                status,
                sanitize_api_error(&body_text)
            )));
        }

        let _upload_resp: ApiResponse<UploadResponse> = resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Parse upload response failed: {}", e))
        })?;

        // KD-005: Report progress after successful upload completion
        if let Some(ref cb) = on_progress {
            cb(file_size, file_size);
        }

        kdrive_log(&format!("Uploaded {} successfully", filename));
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);
        let (parent_path, dir_name) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        kdrive_log(&format!(
            "Creating directory '{}' in folder {}",
            dir_name, parent_id
        ));

        let url = self.api_url_v3(&format!("/files/{}/directory", parent_id));
        let body_json = serde_json::json!({ "name": dir_name })
            .to_string()
            .into_bytes();
        let resp = self
            .post_with_retry(&url, "application/json", body_json)
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Create directory failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        // Cache the new dir
        let api_resp: ApiResponse<KDriveFile> = resp.json().await.unwrap_or(ApiResponse {
            result: None,
            data: None,
            error: None,
        });
        if let Some(file) = api_resp.data {
            self.dir_cache_insert(resolved, DirInfo { id: file.id });
        }

        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        let (file_id, _) = self
            .find_file_in_folder(parent_id, filename)
            .await?
            .ok_or_else(|| ProviderError::NotFound(format!("'{}' not found", filename)))?;

        kdrive_log(&format!("Deleting {} (id={})", filename, file_id));

        let url = self.api_url_v2(&format!("/files/{}", file_id));
        let resp = self.delete_with_retry(&url).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Delete failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        // Remove from cache if directory
        self.dir_cache.remove(&resolved);

        Ok(())
    }

    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let resolved_from = self.resolve_path(from);
        let resolved_to = self.resolve_path(to);
        let (from_parent, from_name) = Self::split_path(&resolved_from);
        let (to_parent, to_name) = Self::split_path(&resolved_to);
        let from_parent_id = self.resolve_folder_id(from_parent).await?;

        let (file_id, _) = self
            .find_file_in_folder(from_parent_id, from_name)
            .await?
            .ok_or_else(|| ProviderError::NotFound(format!("'{}' not found", from_name)))?;

        // kDrive has no dedicated rename endpoint: use move to same/different parent with new name
        let to_parent_id = if from_parent == to_parent {
            from_parent_id
        } else {
            self.resolve_folder_id(to_parent).await?
        };

        let url = self.api_url_v3(&format!("/files/{}/move/{}", file_id, to_parent_id));
        let body_json = serde_json::json!({ "name": to_name })
            .to_string()
            .into_bytes();
        let resp = self
            .post_with_retry(&url, "application/json", body_json)
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Rename failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        // Update cache
        self.dir_cache.remove(&resolved_from);
        Ok(())
    }

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        self.delete(path).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        // kDrive DELETE on a folder deletes it recursively (moves to trash)
        self.delete(path).await
    }

    async fn delete_permanent(&mut self, path: &str) -> Result<bool, ProviderError> {
        // kDrive trash entries carry file_id in metadata. Match by basename
        // and dispatch to the inherent permanently_delete_trash helper.
        let basename = path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(path);
        if basename.is_empty() {
            return Ok(false);
        }
        let trashed = self.list_trash().await?;
        let id = trashed
            .iter()
            .find(|e| e.name == basename)
            .and_then(|e| e.metadata.get("file_id").cloned());
        match id {
            Some(file_id) => {
                self.permanently_delete_trash(&file_id).await?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let resolved = self.resolve_path(path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        let (file_id, _) = self
            .find_file_in_folder(parent_id, filename)
            .await?
            .ok_or_else(|| ProviderError::NotFound(format!("'{}' not found", filename)))?;

        let url = self.api_url_v3(&format!("/files/{}", file_id));
        let resp = self.get_with_retry(&url).await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Stat failed: {}",
                sanitize_api_error(&body)
            )));
        }

        let api_resp: ApiResponse<KDriveFile> = resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Parse stat response failed: {}", e))
        })?;

        let file = api_resp.data.ok_or_else(|| {
            ProviderError::NotFound(format!("File info not found for '{}'", filename))
        })?;

        let is_dir = file.file_type.as_deref() == Some("dir");
        let size = file.size.unwrap_or(0).max(0) as u64;
        let modified = file.last_modified.map(|ts| {
            chrono::DateTime::from_timestamp(ts, 0)
                .unwrap_or_default()
                .format("%Y-%m-%d %H:%M:%SZ")
                .to_string()
        });

        Ok(RemoteEntry {
            name: file.name.unwrap_or_else(|| filename.to_string()),
            path: resolved,
            is_dir,
            size,
            modified,
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            metadata: HashMap::new(),
            mime_type: None,
        })
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
        // REST API doesn't need keep-alive
        Ok(())
    }

    async fn server_info(&mut self) -> Result<String, ProviderError> {
        Ok(format!(
            "Infomaniak kDrive: Drive ID: {}",
            self.config.drive_id
        ))
    }

    fn supports_server_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let resolved_from = self.resolve_path(from);
        let resolved_to = self.resolve_path(to);
        let (from_parent, from_name) = Self::split_path(&resolved_from);
        let (to_parent, to_name) = Self::split_path(&resolved_to);
        let from_parent_id = self.resolve_folder_id(from_parent).await?;
        let to_parent_id = self.resolve_folder_id(to_parent).await?;

        let (file_id, _) = self
            .find_file_in_folder(from_parent_id, from_name)
            .await?
            .ok_or_else(|| ProviderError::NotFound(format!("'{}' not found", from_name)))?;

        let url = self.api_url_v3(&format!("/files/{}/copy/{}", file_id, to_parent_id));
        let body_json = serde_json::json!({ "name": to_name })
            .to_string()
            .into_bytes();
        let resp = self
            .post_with_retry(&url, "application/json", body_json)
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Copy failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        Ok(())
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        let url = self.api_url_v2("");
        let resp = self.get_with_retry(&url).await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Quota failed: {}",
                sanitize_api_error(&body)
            )));
        }

        let api_resp: ApiResponse<DriveInfo> = resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Parse quota response failed: {}", e))
        })?;

        let drive = api_resp.data.unwrap_or(DriveInfo {
            id: None,
            name: None,
            used_size: None,
            size: None,
        });

        let used = drive.used_size.unwrap_or(0).max(0) as u64;
        let total = drive.size.unwrap_or(0).max(0) as u64;

        Ok(StorageInfo {
            used,
            total,
            free: total.saturating_sub(used),
        })
    }

    // ─── KD-006: Search via kDrive search API ─────────────────────────

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(
        &mut self,
        _path: &str,
        pattern: &str,
    ) -> Result<Vec<RemoteEntry>, ProviderError> {
        // kDrive /files/search?query= is substring on filename and ignores
        // glob metacharacters. Strip glob chars for the broad server
        // prefilter, then apply the precise filter client-side via
        // matches_find_pattern.
        let literal: String = pattern
            .chars()
            .filter(|c| !matches!(c, '*' | '?' | '[' | ']'))
            .collect();
        let server_query = if literal.is_empty() {
            ".".to_string()
        } else {
            literal
        };

        let base_url = self.api_url_v3("/files/search");
        let mut entries = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let url = match cursor {
                Some(ref c) => format!(
                    "{}?query={}&cursor={}",
                    base_url,
                    urlencoding::encode(&server_query),
                    c
                ),
                None => format!("{}?query={}", base_url, urlencoding::encode(&server_query)),
            };

            let resp = self.get_with_retry(&url).await?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!(
                    "Search failed ({}): {}",
                    status,
                    sanitize_api_error(&body)
                )));
            }

            let api_resp: ApiResponse<FilesPayload> = resp.json().await.map_err(|e| {
                ProviderError::ServerError(format!("Parse search response failed: {}", e))
            })?;

            let (files, has_more, next_cursor) = match api_resp.data {
                Some(FilesPayload::Paginated {
                    data,
                    has_more,
                    cursor: c,
                }) => (data, has_more, c),
                Some(FilesPayload::Flat(data)) => (data, Some(false), None),
                None => (vec![], Some(false), None),
            };

            for file in files {
                let name = file.name.unwrap_or_else(|| format!("unnamed_{}", file.id));
                if !super::matches_find_pattern(&name, pattern) {
                    continue;
                }
                let is_dir = file.file_type.as_deref() == Some("dir");
                let size = file.size.unwrap_or(0).max(0) as u64;
                let modified = file.last_modified.map(|ts| {
                    chrono::DateTime::from_timestamp(ts, 0)
                        .unwrap_or_default()
                        .format("%Y-%m-%d %H:%M:%SZ")
                        .to_string()
                });
                let file_path = file.path.unwrap_or_else(|| format!("/{}", name));

                entries.push(RemoteEntry {
                    name,
                    path: file_path,
                    is_dir,
                    size,
                    modified,
                    permissions: None,
                    owner: None,
                    group: None,
                    is_symlink: false,
                    link_target: None,
                    metadata: HashMap::new(),
                    mime_type: None,
                });
            }

            if has_more != Some(true) || next_cursor.is_none() {
                break;
            }
            cursor = next_cursor;
        }

        Ok(entries)
    }

    // ─── KD-007: Share links via kDrive link API ──────────────────────

    fn supports_share_links(&self) -> bool {
        true
    }

    fn share_link_capabilities(&self) -> ShareLinkCapabilities {
        ShareLinkCapabilities {
            supports_expiration: true,
            supports_password: true,
            supports_permissions: true,
            available_permissions: vec!["view".into(), "edit".into()],
            supports_list_links: true,
            supports_revoke: true,
        }
    }

    async fn create_share_link(
        &mut self,
        path: &str,
        options: ShareLinkOptions,
    ) -> Result<ShareLinkResult, ProviderError> {
        let resolved = self.resolve_path(path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        let (file_id, _) = self
            .find_file_in_folder(parent_id, filename)
            .await?
            .ok_or_else(|| ProviderError::NotFound(format!("'{}' not found", filename)))?;

        // Build share link request body with optional expiry and password
        let right = if options.password.is_some() {
            "password"
        } else {
            "public"
        };
        let can_edit = options.permissions.as_deref() == Some("edit");
        let mut body_obj = serde_json::json!({
            "right": right,
            "can_download": true,
            "can_edit": can_edit
        });
        if let Some(secs) = options.expires_in_secs {
            let expires_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                + secs;
            body_obj["valid_until"] = serde_json::json!(expires_at);
        }
        if let Some(ref pw) = options.password {
            body_obj["password"] = serde_json::json!(pw);
        }

        let url = self.api_url_v2(&format!("/files/{}/link", file_id));
        let body_bytes = body_obj.to_string().into_bytes();
        let resp = self
            .post_with_retry(&url, "application/json", body_bytes)
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Create share link failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        let api_resp: ApiResponse<ShareLinkData> = resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Parse share link response failed: {}", e))
        })?;

        let link_data = api_resp.data.ok_or_else(|| {
            ProviderError::ServerError("No share link data in response".to_string())
        })?;

        let link_url = link_data.url.ok_or_else(|| {
            ProviderError::ServerError("No URL in share link response".to_string())
        })?;

        Ok(ShareLinkResult {
            url: link_url,
            password: None,
            expires_at: None,
        })
    }

    async fn list_share_links(&mut self, path: &str) -> Result<Vec<ShareLinkInfo>, ProviderError> {
        let resolved = self.resolve_path(path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        let (file_id, _) = self
            .find_file_in_folder(parent_id, filename)
            .await?
            .ok_or_else(|| ProviderError::NotFound(format!("'{}' not found", filename)))?;

        let url = self.api_url_v2(&format!("/files/{}/link", file_id));
        let resp = self.get_with_retry(&url).await?;

        if resp.status().as_u16() == 404 {
            // No share link exists for this file
            return Ok(vec![]);
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "List share links failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        let api_resp: ApiResponse<ShareLinkData> = resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Parse share link response failed: {}", e))
        })?;

        if let Some(link_data) = api_resp.data {
            if let Some(ref link_url) = link_data.url {
                let expires_at = link_data.valid_until.as_ref().and_then(|v| {
                    v.as_str()
                        .map(|s| s.to_string())
                        .or_else(|| v.as_i64().map(|ts| ts.to_string()))
                });

                return Ok(vec![ShareLinkInfo {
                    id: link_data.uuid.unwrap_or_else(|| file_id.to_string()),
                    url: link_url.clone(),
                    created_at: None,
                    expires_at,
                    password_protected: link_data.has_password.unwrap_or(false),
                    permissions: link_data.right,
                }]);
            }
        }

        Ok(vec![])
    }

    async fn remove_share_link(&mut self, path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        let (file_id, _) = self
            .find_file_in_folder(parent_id, filename)
            .await?
            .ok_or_else(|| ProviderError::NotFound(format!("'{}' not found", filename)))?;

        let url = self.api_url_v2(&format!("/files/{}/link", file_id));
        let resp = self.delete_with_retry(&url).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Remove share link failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        Ok(())
    }

    // ─── KD-009: File versioning via kDrive versions API ──────────────

    fn supports_versions(&self) -> bool {
        true
    }

    async fn list_versions(&mut self, path: &str) -> Result<Vec<FileVersion>, ProviderError> {
        let resolved = self.resolve_path(path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        let (file_id, _) = self
            .find_file_in_folder(parent_id, filename)
            .await?
            .ok_or_else(|| ProviderError::NotFound(format!("'{}' not found", filename)))?;

        let url = self.api_url_v3(&format!("/files/{}/versions", file_id));
        let resp = self.get_with_retry(&url).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "List versions failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        let api_resp: ApiResponse<Vec<KDriveVersion>> = resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Parse versions response failed: {}", e))
        })?;

        let versions = api_resp.data.unwrap_or_default();
        Ok(versions
            .iter()
            .map(|v| {
                let modified = v.created_at.map(|ts| {
                    chrono::DateTime::from_timestamp(ts, 0)
                        .unwrap_or_default()
                        .format("%Y-%m-%d %H:%M:%SZ")
                        .to_string()
                });
                FileVersion {
                    id: v.id.to_string(),
                    modified,
                    size: v.size.unwrap_or(0).max(0) as u64,
                    modified_by: v.version_number.map(|n| format!("v{}", n)),
                }
            })
            .collect())
    }

    async fn download_version(
        &mut self,
        path: &str,
        version_id: &str,
        local_path: &str,
    ) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        let (file_id, _) = self
            .find_file_in_folder(parent_id, filename)
            .await?
            .ok_or_else(|| ProviderError::NotFound(format!("'{}' not found", filename)))?;

        let url = self.api_url_v3(&format!(
            "/files/{}/versions/{}/download",
            file_id, version_id
        ));
        let resp = self.get_with_retry(&url).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Download version failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        // Stream version download to file
        use futures_util::StreamExt;

        let mut stream = resp.bytes_stream();
        let mut atomic = super::atomic_write::AtomicFile::new(local_path)
            .await
            .map_err(ProviderError::IoError)?;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                ProviderError::TransferFailed(format!("Version download error: {}", e))
            })?;
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
        let resolved = self.resolve_path(path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        let (file_id, _) = self
            .find_file_in_folder(parent_id, filename)
            .await?
            .ok_or_else(|| ProviderError::NotFound(format!("'{}' not found", filename)))?;

        let url = self.api_url_v3(&format!(
            "/files/{}/versions/{}/restore",
            file_id, version_id
        ));
        let resp = self
            .post_with_retry(&url, "application/json", vec![])
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Restore version failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        Ok(())
    }

    // ─── KD-010: Trash management via kDrive trash API ────────────────

    fn transfer_optimization_hints(&self) -> super::TransferOptimizationHints {
        super::TransferOptimizationHints {
            supports_multipart: true,
            multipart_threshold: KDRIVE_MULTIPART_THRESHOLD,
            multipart_part_size: KDRIVE_MULTIPART_PART_SIZE,
            // Infomaniak documents parallel chunk POSTs against the same
            // session token. 4 matches the AIMD default budget the runner
            // ships with so the fan-out actually parallelises.
            multipart_max_parallel: 4,
            supports_resume_download: true,
            supports_resume_upload: true,
            ..Default::default()
        }
    }

    // Shaped-graph multipart trait wiring (S3-T03).
    //
    // kDrive's session-upload protocol (see `docs/dev/guides/kDrive/
    // 06-upload-file.md`, sourced from Infomaniak's iOS SDK) maps onto
    // the trait as:
    //   1. `begin_multipart_upload` → POST
    //      `https://api.kdrive.infomaniak.com/3/drive/<id>/upload/session/start`
    //      with JSON body `{file_name, directory_id, total_size,
    //      total_chunks, conflict:"version", drive_id}`. The response
    //      carries an opaque `token`; the handle embeds it together
    //      with parent_id, name, total, part, total_chunks so
    //      subsequent calls don't need to re-resolve.
    //   2. `upload_part` → POST `.../upload/session/{token}/chunk`
    //      with the raw chunk bytes and query params
    //      `chunk_number={0-based}&chunk_size={bytes}&chunk_hash=sha256:HEX`.
    //      Chunks parallelise freely against the same token.
    //   3. `complete_multipart_upload` → POST `.../upload/session/{token}/finish`
    //      with an empty body. The server stitches the chunks server-side
    //      and returns the final FileInfo.
    //   4. `abort_multipart_upload` → best-effort DELETE on the session;
    //      kDrive auto-GCs incomplete sessions after a few hours.
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
                "kDrive multipart upload requires non-zero total_size".to_string(),
            ));
        }

        let resolved = self.resolve_path(remote_path);
        let (parent_path, filename) = Self::split_path(&resolved);
        if filename.is_empty() {
            return Err(ProviderError::Other(format!(
                "kDrive multipart: cannot resolve file name from path '{}'",
                remote_path
            )));
        }
        let parent_id = self.resolve_folder_id(parent_path).await?;
        let part = kdrive_runner_part_size(total_size);
        let total_chunks = kdrive_total_chunks(total_size, part);

        // Session-upload endpoints live on api.kdrive.infomaniak.com,
        // NOT api.infomaniak.com (which only serves the legacy
        // single-shot /upload). Hitting `api.infomaniak.com/.../upload/session*`
        // returns `404 method_not_found` (verified live 2026-05-26).
        let url = format!(
            "{}/3/drive/{}/upload/session/start",
            KDRIVE_SESSION_HOST, self.config.drive_id
        );
        let drive_id_num = self.config.drive_id.parse::<i64>().unwrap_or(0);
        let body_json = serde_json::json!({
            "drive_id": drive_id_num,
            "file_name": filename,
            "directory_id": parent_id,
            "total_size": total_size,
            "total_chunks": total_chunks,
            "conflict": "version",
        })
        .to_string()
        .into_bytes();

        let resp = self
            .post_with_retry(&url, "application/json", body_json)
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "kDrive upload/session failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        let api_resp: ApiResponse<KDriveUploadSession> = resp.json().await.map_err(|e| {
            ProviderError::ParseError(format!("Parse upload/session response failed: {}", e))
        })?;
        let session = api_resp.data.ok_or_else(|| {
            ProviderError::Other("kDrive upload/session returned no token".to_string())
        })?;

        let meta = KDriveMultipartMeta {
            token: session.token,
            parent_id,
            name: filename.to_string(),
            total: total_size,
            part,
            total_chunks,
        };
        Ok(MultipartHandle {
            upload_id: meta.encode(),
            remote_path: remote_path.to_string(),
        })
    }

    async fn upload_part(
        &mut self,
        handle: &MultipartHandle,
        part_number: u32,
        data: Vec<u8>,
    ) -> Result<UploadedPart, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        if part_number == 0 {
            return Err(ProviderError::Other(
                "kDrive upload_part requires 1-based part_number".to_string(),
            ));
        }
        if data.is_empty() {
            return Err(ProviderError::Other(
                "kDrive upload_part received empty data".to_string(),
            ));
        }
        let meta = KDriveMultipartMeta::decode(&handle.upload_id)?;
        if part_number > meta.total_chunks {
            return Err(ProviderError::Other(format!(
                "kDrive part {} exceeds declared total_chunks {}",
                part_number, meta.total_chunks
            )));
        }
        let offset = (part_number as u64 - 1) * meta.part;
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or_else(|| ProviderError::Other("kDrive part offset overflow".to_string()))?;
        if end > meta.total {
            return Err(ProviderError::Other(format!(
                "kDrive part {} exceeds declared total: offset {} + len {} > total {}",
                part_number,
                offset,
                data.len(),
                meta.total
            )));
        }

        // kDrive's chunk endpoint requires three query params:
        //   chunk_number = 0-based index (convert from the trait's 1-based
        //                  `part_number`)
        //   chunk_size   = exact byte length of this chunk
        //   chunk_hash   = "sha256:<lowercase hex digest>"
        let mut hasher = Sha256::new();
        hasher.update(&data);
        let chunk_hash = format!("sha256:{:x}", hasher.finalize());
        let chunk_size = data.len();
        let url = format!(
            "{}/3/drive/{}/upload/session/{}/chunk?chunk_number={}&chunk_size={}&chunk_hash={}",
            KDRIVE_SESSION_HOST,
            self.config.drive_id,
            urlencoding::encode(&meta.token),
            part_number - 1,
            chunk_size,
            urlencoding::encode(&chunk_hash)
        );
        let resp = self
            .post_with_retry(&url, "application/octet-stream", data)
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(format!(
                "kDrive chunk {} failed ({}): {}",
                part_number,
                status,
                sanitize_api_error(&body)
            )));
        }
        Ok(UploadedPart {
            part_number,
            etag: chunk_hash,
        })
    }

    async fn complete_multipart_upload(
        &mut self,
        handle: MultipartHandle,
        parts: Vec<UploadedPart>,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let meta = KDriveMultipartMeta::decode(&handle.upload_id)?;
        if parts.len() != meta.total_chunks as usize {
            return Err(ProviderError::TransferFailed(format!(
                "kDrive complete: expected {} parts, runner committed {}",
                meta.total_chunks,
                parts.len()
            )));
        }

        let url = format!(
            "{}/3/drive/{}/upload/session/{}/finish",
            KDRIVE_SESSION_HOST,
            self.config.drive_id,
            urlencoding::encode(&meta.token)
        );
        let resp = self
            .post_with_retry(&url, "application/json", Vec::new())
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "kDrive upload/session/finish failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }
        Ok(())
    }

    async fn abort_multipart_upload(
        &mut self,
        handle: MultipartHandle,
    ) -> Result<(), ProviderError> {
        let meta = KDriveMultipartMeta::decode(&handle.upload_id)?;
        let url = format!(
            "{}/3/drive/{}/upload/session/{}",
            KDRIVE_SESSION_HOST,
            self.config.drive_id,
            urlencoding::encode(&meta.token)
        );
        // Best-effort: kDrive GCs stale sessions a few hours after
        // last activity. Swallow failures so the abort never masks
        // the original transfer error.
        let _ = self.delete_with_retry(&url).await;
        Ok(())
    }
}

// =============================================================================
// kDrive Trash Management (KD-010)
// =============================================================================

impl KDriveProvider {
    /// List items in the kDrive trash.
    pub async fn list_trash(&mut self) -> Result<Vec<RemoteEntry>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let url = self.api_url_v3("/trash");
        let resp = self.get_with_retry(&url).await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "List trash failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        let api_resp: ApiResponse<TrashPayload> = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(sanitize_api_error(&e.to_string())))?;

        let payload = api_resp
            .data
            .ok_or_else(|| ProviderError::ParseError("No trash data in response".to_string()))?;

        let files = match payload {
            TrashPayload::Paginated { data, .. } => data,
            TrashPayload::Flat(data) => data,
        };

        let entries = files
            .iter()
            .map(|f| {
                let name = f.name.clone().unwrap_or_else(|| f.id.to_string());
                let is_dir = f.file_type.as_deref() == Some("dir");
                let size = f.size.unwrap_or(0) as u64;
                let modified = f.last_modified.map(|ts| {
                    chrono::DateTime::from_timestamp(ts, 0)
                        .map(|dt| dt.to_rfc3339())
                        .unwrap_or_default()
                });

                let mut metadata = std::collections::HashMap::new();
                metadata.insert("file_id".to_string(), f.id.to_string());

                RemoteEntry {
                    name,
                    path: f.path.clone().unwrap_or_else(|| format!("/{}", f.id)),
                    is_dir,
                    size,
                    modified,
                    permissions: None,
                    owner: None,
                    group: None,
                    is_symlink: false,
                    link_target: None,
                    mime_type: None,
                    metadata,
                }
            })
            .collect();

        Ok(entries)
    }

    /// Restore a file or folder from the kDrive trash.
    pub async fn restore_from_trash(&mut self, file_id: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let url = format!(
            "{}/2/drive/{}/trash/{}/restore",
            API_BASE, self.config.drive_id, file_id
        );
        let resp = self
            .post_with_retry(&url, "application/json", Vec::new())
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Restore from trash failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        tracing::info!("kDrive: restored item {} from trash", file_id);
        Ok(())
    }

    /// Permanently delete an item from the kDrive trash.
    pub async fn permanently_delete_trash(&mut self, file_id: &str) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let url = format!(
            "{}/2/drive/{}/trash/{}",
            API_BASE, self.config.drive_id, file_id
        );
        let resp = self.delete_with_retry(&url).await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Permanent delete failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        tracing::info!("kDrive: permanently deleted item {} from trash", file_id);
        Ok(())
    }

    /// Empty the entire kDrive trash.
    pub async fn empty_trash(&mut self) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }

        let url = format!("{}/2/drive/{}/trash", API_BASE, self.config.drive_id);
        let resp = self.delete_with_retry(&url).await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Empty trash failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        tracing::info!("kDrive: trash emptied");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_provider() -> KDriveProvider {
        let config = KDriveConfig {
            api_token: secrecy::SecretString::from("test-token".to_string()),
            drive_id: "987654".to_string(),
            initial_path: None,
        };
        KDriveProvider::new(config)
    }

    #[test]
    fn api_url_v2_and_v3_embed_drive_id() {
        let p = test_provider();
        assert_eq!(
            p.api_url_v2("/files"),
            format!("{}/2/drive/987654/files", API_BASE)
        );
        assert_eq!(
            p.api_url_v3("/files"),
            format!("{}/3/drive/987654/files", API_BASE)
        );
        assert_eq!(p.api_url_v2(""), format!("{}/2/drive/987654", API_BASE));
    }

    #[test]
    fn normalize_path_preserves_root_and_trims_trailing() {
        assert_eq!(KDriveProvider::normalize_path(""), "/");
        assert_eq!(KDriveProvider::normalize_path("/"), "/");
        assert_eq!(KDriveProvider::normalize_path("foo"), "/foo");
        assert_eq!(KDriveProvider::normalize_path("/foo/"), "/foo");
        assert_eq!(KDriveProvider::normalize_path(r"a\b"), "/a/b");
    }

    #[test]
    fn split_path_handles_root_nested_and_bare() {
        assert_eq!(KDriveProvider::split_path("/file.txt"), ("/", "file.txt"));
        assert_eq!(KDriveProvider::split_path("/a/b/c"), ("/a/b", "c"));
        assert_eq!(KDriveProvider::split_path("bare"), ("/", "bare"));
        assert_eq!(KDriveProvider::split_path("/a/"), ("/", "a"));
    }

    #[test]
    fn resolve_path_joins_relative_against_current_path() {
        let mut p = test_provider();
        p.current_path = "/docs".to_string();
        assert_eq!(p.resolve_path("/abs"), "/abs");
        assert_eq!(p.resolve_path("."), "/docs");
        assert_eq!(p.resolve_path(""), "/docs");
        assert_eq!(p.resolve_path("child"), "/docs/child");
        assert_eq!(p.resolve_path("sub/leaf"), "/docs/sub/leaf");

        // when current_path is root, no double slash
        let mut p2 = test_provider();
        p2.current_path = "/".to_string();
        assert_eq!(p2.resolve_path("child"), "/child");
    }

    // ---- S3-T03 multipart trait wiring ----

    #[test]
    fn kdrive_multipart_meta_roundtrip_preserves_fields() {
        let meta = KDriveMultipartMeta {
            token: "abc-XYZ_123.tok".to_string(),
            parent_id: 9_999_999,
            name: "weird name (1).bin".to_string(),
            total: 1_073_741_824,
            part: 4 * 1024 * 1024,
            total_chunks: 256,
        };
        let encoded = meta.encode();
        let decoded = KDriveMultipartMeta::decode(&encoded).expect("decode roundtrip");
        assert_eq!(meta, decoded);
    }

    #[test]
    fn kdrive_multipart_meta_decode_rejects_garbage() {
        let err = KDriveMultipartMeta::decode("{not json}").unwrap_err();
        assert!(matches!(err, ProviderError::Other(_)));
    }

    #[test]
    fn kdrive_runner_part_size_clamps_and_never_returns_zero() {
        assert_eq!(kdrive_runner_part_size(1024), 1024);
        assert_eq!(
            kdrive_runner_part_size(KDRIVE_MULTIPART_PART_SIZE),
            KDRIVE_MULTIPART_PART_SIZE
        );
        assert_eq!(
            kdrive_runner_part_size(50 * 1024 * 1024 * 1024),
            KDRIVE_MULTIPART_PART_SIZE
        );
        assert_eq!(kdrive_runner_part_size(0), 1);
    }

    #[test]
    fn kdrive_total_chunks_matches_runner_formula() {
        let p = KDRIVE_MULTIPART_PART_SIZE;
        // total=0 ⇒ at least one chunk so abort/finish never get a zero loop.
        assert_eq!(kdrive_total_chunks(0, p), 1);
        // exact multiple
        assert_eq!(kdrive_total_chunks(p, p), 1);
        assert_eq!(kdrive_total_chunks(4 * p, p), 4);
        // tail chunk rounds up
        assert_eq!(kdrive_total_chunks(p + 1, p), 2);
        // part=0 guard: never divide by zero (treated as part=1)
        assert_eq!(kdrive_total_chunks(7, 0), 7);
    }

    #[test]
    fn kdrive_transfer_hints_advertise_multipart() {
        let hints = test_provider().transfer_optimization_hints();
        assert!(hints.supports_multipart);
        assert_eq!(hints.multipart_threshold, KDRIVE_MULTIPART_THRESHOLD);
        assert_eq!(hints.multipart_part_size, KDRIVE_MULTIPART_PART_SIZE);
        // 4-slot fan-out matches AIMD default budget.
        assert_eq!(hints.multipart_max_parallel, 4);
        assert!(hints.supports_resume_download);
        assert!(hints.supports_resume_upload);
    }
}
