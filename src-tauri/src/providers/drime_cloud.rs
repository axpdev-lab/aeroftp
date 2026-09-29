//! Drime Cloud Storage Provider (Bedrive/BeDrive platform)
//!
//! Implements StorageProvider for Drime Cloud using the Bedrive REST API.
//! Uses API Token (Bearer) for authentication: no OAuth2 flow needed.
//!
//! API Base: https://app.drime.cloud/api/v1
//! Auth: Authorization: Bearer {token}
//! IDs: Numeric (but stored as String internally)
//! Pagination: page-based (page + perPage)
//! File entries use hash-based download URLs

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use async_trait::async_trait;
use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{info, warn};

use super::{
    sanitize_api_error, DrimeCloudConfig, FileVersion, MultipartHandle, ProviderError,
    ProviderTransferExecutorKind, ProviderType, RemoteEntry, ShareLinkCapabilities, ShareLinkInfo,
    ShareLinkOptions, ShareLinkResult, StorageInfo, StorageProvider, UploadedPart,
};

const API_BASE: &str = "https://app.drime.cloud/api/v1";

/// Drime chunked-upload size threshold (S3-T12).
///
/// Matches the legacy `MULTIPART_THRESHOLD` in `upload()`: 5 MiB is the
/// per-S3-part minimum the upstream protocol expects.
const DRIME_MULTIPART_THRESHOLD: u64 = 5 * 1024 * 1024;

/// Drime chunked-upload preferred part size (S3-T12).
///
/// Matches the `CHUNK_SIZE` used by `upload_multipart`. 5 MiB is the
/// minimum part size Drime's S3-compatible backend accepts (per AWS S3
/// rules; only the final part may be smaller).
const DRIME_MULTIPART_PART_SIZE: u64 = 5 * 1024 * 1024;

fn drime_log(msg: &str) {
    info!("[DRIME] {}", msg);
}

// ─── API Response Types ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct DrimeFile {
    id: Option<serde_json::Value>, // numeric or string
    name: Option<String>,
    #[serde(rename = "type")]
    file_type: Option<String>, // "file" or "folder"
    #[serde(alias = "file_size")]
    size: Option<u64>,
    #[serde(alias = "updated_at", alias = "modified_at")]
    updated_at: Option<String>, // ISO 8601 or timestamp
    #[serde(default)]
    mime_type: Option<String>,
    /// Encrypted hash for download URLs (Bedrive-specific)
    #[serde(default)]
    hash: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    parent_id: Option<serde_json::Value>,
}

impl DrimeFile {
    /// Extract ID as string regardless of JSON type (number or string)
    fn id_str(&self) -> Option<String> {
        self.id.as_ref().map(|v| match v {
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string().trim_matches('"').to_string(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct DrimeListResponse {
    data: Option<Vec<DrimeFile>>,
    #[allow(dead_code)]
    #[serde(default)]
    current_page: Option<u32>,
    #[serde(default)]
    last_page: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct DrimeUser {
    id: Option<u64>,
    #[allow(dead_code)]
    email: Option<String>,
    #[allow(dead_code)]
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DrimeUserResponse {
    user: Option<DrimeUser>,
}

// ─── S3 Multipart Upload Response Types ─────────────────────────────────

#[derive(Debug, Deserialize)]
struct DrimeMultipartCreateResponse {
    key: Option<String>,
    #[serde(rename = "uploadId")]
    upload_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DrimeSignedUrl {
    #[serde(rename = "partNumber")]
    part_number: u32,
    url: String,
}

#[derive(Debug, Deserialize)]
struct DrimeSignPartUrlsResponse {
    urls: Option<Vec<DrimeSignedUrl>>,
}

/// Side-band metadata embedded in `MultipartHandle.upload_id` for the
/// Drime Cloud S3-multipart trait wiring (S3-T12). The signed PUT URLs
/// are all pre-fetched in `begin_multipart_upload` so the upload_part
/// fan-out doesn't need to round-trip through the Drime API per chunk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct DrimeMultipartMeta {
    key: String,
    upload_id: String,
    parent_id: String,
    filename: String,
    mime: String,
    extension: String,
    total: u64,
    part: u64,
    total_parts: u32,
    /// Pre-signed PUT URLs by 1-based part number.
    signed_urls: Vec<(u32, String)>,
}

impl DrimeMultipartMeta {
    fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    fn decode(raw: &str) -> Result<Self, ProviderError> {
        serde_json::from_str(raw).map_err(|e| {
            ProviderError::Other(format!("Drime multipart handle decode failed: {}", e))
        })
    }

    fn url_for(&self, part_number: u32) -> Option<&str> {
        self.signed_urls
            .iter()
            .find(|(n, _)| *n == part_number)
            .map(|(_, u)| u.as_str())
    }
}

/// Compute the per-chunk size the runner should slice `total` bytes into.
fn drime_runner_part_size(total: u64) -> u64 {
    DRIME_MULTIPART_PART_SIZE.min(total.max(1))
}

/// Total chunks formula matching the S3 multipart contract.
fn drime_total_parts(total: u64, part: u64) -> u32 {
    let raw = total.div_ceil(part.max(1)).max(1);
    raw.min(u32::MAX as u64) as u32
}

/// Transient-error classifier for the Drime list/find retry loop. Any 5xx is
/// retryable, plus the transient 4xx set (404 = eventual-consistency gap, 408
/// timeout, 425 too-early, 429 rate-limit). Extracted from the inline match so the
/// decision is unit-testable; behaviour is identical to `status.is_server_error()
/// || matches!(status, 404 | 408 | 425 | 429)`.
fn drime_status_is_retryable(status: u16) -> bool {
    (500..=599).contains(&status) || matches!(status, 404 | 408 | 425 | 429)
}

// ─── Share Link Response ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct DrimeShareLink {
    hash: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DrimeShareLinkResponse {
    link: Option<DrimeShareLink>,
}

// ─── File Backup/Version Response ───────────────────────────────────────

#[derive(Debug, Deserialize)]
struct DrimeBackupEntry {
    id: Option<serde_json::Value>,
    name: Option<String>,
    file_size: Option<u64>,
    created_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DrimeBackupPagination {
    data: Option<Vec<DrimeBackupEntry>>,
}

#[derive(Debug, Deserialize)]
struct DrimeBackupResponse {
    pagination: Option<DrimeBackupPagination>,
}

// ─── Storage Response ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct DrimeStorageResponse {
    /// Storage consumed in bytes
    #[serde(default)]
    used: Option<u64>,
    /// Remaining capacity in bytes (API field: "available")
    #[serde(default)]
    available: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct DrimeFolderResponse {
    id: Option<serde_json::Value>,
    #[allow(dead_code)]
    name: Option<String>,
}

impl DrimeFolderResponse {
    fn id_str(&self) -> Option<String> {
        self.id.as_ref().map(|v| match v {
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string().trim_matches('"').to_string(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct DrimeFileResponse {
    id: Option<serde_json::Value>,
    #[allow(dead_code)]
    name: Option<String>,
}

/// Response from /file-entries/duplicate: { entries: [...], status: "success" }
#[derive(Debug, Deserialize)]
struct DrimeEntriesResponse {
    entries: Option<Vec<DrimeFileResponse>>,
}

impl DrimeFileResponse {
    fn id_str(&self) -> Option<String> {
        self.id.as_ref().map(|v| match v {
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string().trim_matches('"').to_string(),
        })
    }
}

#[cfg(test)]
thread_local! {
    /// The API base of a local double, for the test running on this thread
    /// (`#[tokio::test]` runs its runtime on the test's own thread).
    static TEST_API_BASE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

// ─── Dir Cache ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct DirInfo {
    id: String,
}

// ─── Provider ────────────────────────────────────────────────────────────

pub struct DrimeCloudProvider {
    config: DrimeCloudConfig,
    client: reqwest::Client,
    connected: bool,
    current_path: String,
    current_folder_id: String,
    dir_cache: HashMap<String, DirInfo>,
    /// Authenticated user ID (from /cli/loggedUser)
    user_id: Option<u64>,
}

/// Honest file/session ceiling for clone-backed Drime workers (DAG-P1-05A).
/// Matches `multipart_max_parallel` and must not exceed 4.
const DRIME_TRANSFER_MAX_SESSIONS: u16 = 4;

/// M3: Maximum number of cached directory entries to prevent unbounded memory growth.
const DIR_CACHE_MAX_ENTRIES: usize = 10_000;

impl Clone for DrimeCloudProvider {
    /// Connected transfer worker: reuses the cloneable `reqwest::Client` pool
    /// and immutable credentials without reconnecting. Mutable state
    /// (path/folder/user/cache) is field-copied so workers do not share a
    /// mutex, part cursor, or receipt vector.
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            client: self.client.clone(),
            connected: self.connected,
            current_path: self.current_path.clone(),
            current_folder_id: self.current_folder_id.clone(),
            dir_cache: self.dir_cache.clone(),
            user_id: self.user_id,
        }
    }
}

impl DrimeCloudProvider {
    pub fn new(config: DrimeCloudConfig) -> Self {
        let mut default_headers = reqwest::header::HeaderMap::new();
        default_headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        let client = reqwest::Client::builder()
            .user_agent(crate::providers::AEROFTP_USER_AGENT)
            .connect_timeout(std::time::Duration::from_secs(30))
            .read_timeout(std::time::Duration::from_secs(1800))
            .default_headers(default_headers)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            config,
            client,
            connected: false,
            current_path: "/".to_string(),
            current_folder_id: String::new(), // root has no ID, empty = root
            dir_cache: HashMap::new(),
            user_id: None,
        }
    }

    /// Connected worker for unit tests (no network). Production clones use
    /// [`clone_for_transfer`] after a real `connect`.
    #[cfg(test)]
    fn connected_for_test(config: DrimeCloudConfig) -> Self {
        let mut p = Self::new(config);
        p.connected = true;
        p
    }

    // ─── Helpers ─────────────────────────────────────────────────────────

    /// M3: Insert into dir_cache with eviction when cap is reached.
    /// Clears the entire cache when it exceeds DIR_CACHE_MAX_ENTRIES,
    /// allowing it to repopulate naturally during navigation.
    fn dir_cache_insert(&mut self, key: String, value: DirInfo) {
        if self.dir_cache.len() >= DIR_CACHE_MAX_ENTRIES {
            tracing::debug!(
                "[DRIME] dir_cache reached {} entries, evicting all",
                self.dir_cache.len()
            );
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

    fn api_url(path: &str) -> String {
        #[cfg(test)]
        if let Some(base) = TEST_API_BASE.with(|base| base.borrow().clone()) {
            return format!("{base}{path}");
        }
        format!("{}{}", API_BASE, path)
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

    /// The id of the folder at `path`, for a read: each name matched
    /// exactly first, and in another letter case as the fallback.
    async fn resolve_folder_id(&mut self, path: &str) -> Result<String, ProviderError> {
        self.resolve_folder(path, true).await
    }

    /// The id of the folder at `path` with every name matched exactly, for a
    /// step that changes or destroys what it finds under it: `rm /docs/x`
    /// beside only `Docs` deleted `Docs/x`. The cache is trusted, because
    /// only exact matches are ever written to it.
    async fn resolve_folder_id_exact(&mut self, path: &str) -> Result<String, ProviderError> {
        self.resolve_folder(path, false).await
    }

    /// Whether the folder at `resolved` is cached, that is, was resolved
    /// with every name matched exactly: a folder under it may be cached
    /// under its path. A listing or a mkdir under a path resolved through
    /// the fallback caches nothing, or the path of another folder would
    /// hold its children's ids.
    fn is_cached_exactly(&self, resolved: &str) -> bool {
        resolved == "/" || self.dir_cache.contains_key(resolved)
    }

    /// The walk behind [`Self::resolve_folder_id`] and
    /// [`Self::resolve_folder_id_exact`]. Only exact matches are cached, and
    /// nothing below a name matched in another case: a spelling the fallback
    /// resolved (`cd /docs` beside only `Docs`) kept the id of `Docs` after
    /// another client made a real `docs`, and a later `rm` under `/docs`
    /// acted inside `Docs`.
    async fn resolve_folder(
        &mut self,
        path: &str,
        other_case: bool,
    ) -> Result<String, ProviderError> {
        let normalized = Self::normalize_path(path);

        if normalized == "/" {
            return Ok(String::new()); // root = no parent_id
        }

        // Check cache
        if let Some(info) = self.dir_cache.get(&normalized) {
            return Ok(info.id.clone());
        }

        // Walk path components
        let parts: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();
        let mut current_id = String::new(); // root
        let mut current_path = String::new();
        let mut exact_so_far = true;

        for part in &parts {
            current_path = format!("{}/{}", current_path, part);

            if let Some(info) = self.dir_cache.get(&current_path) {
                current_id = info.id.clone();
                continue;
            }

            // List children to find the folder: the name as spelled first,
            // another case as the fallback. With sibling folders `D` (listed
            // first) and `d`, a first match that ignored the case walked
            // `/d/x` into `D`.
            let mut page = 1u32;
            let mut exact = None;
            let mut fallback = None;

            loop {
                let url = if current_id.is_empty() {
                    format!(
                        "{}?page={}&perPage=100&workspaceId=0&type=folder",
                        Self::api_url("/drive/file-entries"),
                        page
                    )
                } else {
                    format!(
                        "{}?page={}&perPage=100&workspaceId=0&parentIds={}",
                        Self::api_url("/drive/file-entries"),
                        page,
                        current_id
                    )
                };

                let resp = self
                    .client
                    .get(&url)
                    .header(AUTHORIZATION, self.auth_header()?)
                    .send()
                    .await
                    .map_err(|e| ProviderError::ConnectionFailed(format!("List failed: {}", e)))?;

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

                let list_resp: DrimeListResponse = resp.json().await.map_err(|e| {
                    ProviderError::ServerError(format!("Parse list response failed: {}", e))
                })?;

                let files = list_resp.data.unwrap_or_default();
                let last_page = list_resp.last_page.unwrap_or(1);

                for file in &files {
                    let is_folder = file.file_type.as_deref() == Some("folder");
                    if is_folder {
                        if let (Some(ref name), Some(id)) = (&file.name, file.id_str()) {
                            if name == part {
                                exact = Some(id);
                                break;
                            }
                            if other_case && fallback.is_none() && name.eq_ignore_ascii_case(part) {
                                fallback = Some(id);
                            }
                        }
                    }
                }

                if exact.is_some() || page >= last_page {
                    break;
                }
                page += 1;
            }

            exact_so_far &= exact.is_some();
            let Some(id) = exact.or(fallback) else {
                return Err(ProviderError::NotFound(format!(
                    "Folder '{}' not found in {}",
                    part, current_path
                )));
            };
            // Only an exact match, with every name above it exact too.
            if exact_so_far {
                self.dir_cache_insert(current_path.clone(), DirInfo { id: id.clone() });
            }
            current_id = id;
        }

        Ok(current_id)
    }

    /// Move the entry `file_id` into the folder `to_parent_id` (empty for the
    /// root) under its name.
    async fn move_entry(
        &self,
        file_id: &str,
        to_parent_id: &str,
        to: &str,
    ) -> Result<(), ProviderError> {
        let dest_id: serde_json::Value = if to_parent_id.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::json!(to_parent_id.parse::<i64>().unwrap_or(0))
        };
        let move_body = serde_json::json!({
            "entryIds": [file_id.parse::<i64>().unwrap_or(0)],
            "destinationId": dest_id
        });
        let resp = self
            .client
            .post(Self::api_url("/file-entries/move?workspaceId=0"))
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(move_body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Move failed: {}", e)))?;
        Self::rename_outcome(resp, "Move", to).await
    }

    /// Refuse to undo a first step onto `name` in the folder `folder_id`
    /// when an item other than `file_id` took that name since: Drime keeps
    /// two items with one name, and the undo would double it. `path` names
    /// the way back in the error.
    async fn way_back_is_free(
        &self,
        folder_id: &str,
        name: &str,
        file_id: &str,
        path: &str,
    ) -> Result<(), ProviderError> {
        // The exact name, and any item but the one moving: a first match
        // that ignores the case took an `A.txt` beside the way back for its
        // holder, and a lagging listing that still shows the item there hid
        // a real one.
        let holder = self
            .find_entry_in_folder(folder_id, |found, id| {
                (found == name && id != file_id).then_some(true)
            })
            .await?;
        match holder {
            Some(_) => Err(ProviderError::AlreadyExists(format!(
                "{path} was taken by another item, so the first step was not undone"
            ))),
            None => Ok(()),
        }
    }

    /// Rename the entry `file_id` to `to_name` in the folder it is in.
    async fn rename_entry(
        &self,
        file_id: &str,
        to_name: &str,
        to: &str,
    ) -> Result<(), ProviderError> {
        drime_log(&format!("Renaming id={} to {}", file_id, to_name));
        let url = Self::api_url(&format!("/file-entries/{}?workspaceId=0", file_id));
        let resp = self
            .client
            .put(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(serde_json::json!({ "name": to_name }).to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Rename failed: {}", e)))?;
        Self::rename_outcome(resp, "Rename", to).await
    }

    /// The outcome of a move or rename call: a refusal because the name is
    /// taken (a 4xx that says so) is AlreadyExists.
    async fn rename_outcome(
        resp: reqwest::Response,
        what: &str,
        to: &str,
    ) -> Result<(), ProviderError> {
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_default();
        if status.is_client_error() && body.to_ascii_lowercase().contains("already exist") {
            return Err(ProviderError::AlreadyExists(to.to_string()));
        }
        Err(ProviderError::ServerError(format!(
            "{what} failed ({}): {}",
            status,
            sanitize_api_error(&body)
        )))
    }

    /// Find a file by name in a given folder, returns (file_id, is_dir, hash).
    ///
    /// Drime's API is asynchronously indexed: folders freshly created via
    /// `mkdir` may return 4xx on list-by-parentId for a few hundred milliseconds
    /// before they become visible. We retry transient HTTP errors with
    /// exponential backoff so `put_file` following `mkdir` does not fail
    /// spuriously.
    async fn find_file_in_folder(
        &self,
        folder_id: &str,
        filename: &str,
    ) -> Result<Option<(String, bool, Option<String>)>, ProviderError> {
        // The name as spelled first, whatever the order of the listing: with
        // `A.txt` listed before `a.txt`, a first match that ignored the case
        // resolved `a.txt` to `A.txt`. Another case is the fallback.
        self.find_entry_in_folder(folder_id, |name, _| {
            if name == filename {
                Some(true)
            } else {
                name.eq_ignore_ascii_case(filename).then_some(false)
            }
        })
        .await
    }

    /// [`Self::find_file_in_folder`] without the fallback to another letter
    /// case, for a step that destroys or publishes what it finds. Drime keeps
    /// `A.txt` and `a.txt` side by side, so the fallback answers an item
    /// other than the one named: an upload of `a.txt` beside only `A.txt`
    /// deleted `A.txt` first, and a share link asked for `a.txt` published
    /// `A.txt` (or removed its link, whose URL cannot be made again).
    async fn find_exact_in_folder(
        &self,
        folder_id: &str,
        filename: &str,
    ) -> Result<Option<(String, bool, Option<String>)>, ProviderError> {
        self.find_entry_in_folder(folder_id, |name, _| (name == filename).then_some(true))
            .await
    }

    /// The entry of the folder `folder_id` that `wanted` picks, read page by
    /// page (see [`Self::find_file_in_folder`]): the first it answers
    /// `Some(true)` for, else the first it answers `Some(false)` for.
    async fn find_entry_in_folder(
        &self,
        folder_id: &str,
        wanted: impl Fn(&str, &str) -> Option<bool>,
    ) -> Result<Option<(String, bool, Option<String>)>, ProviderError> {
        const MAX_ATTEMPTS: u32 = 4;
        const RETRY_DELAYS_MS: [u64; 3] = [200, 500, 2000];

        let mut page = 1u32;
        let mut fallback = None;

        loop {
            let url = if folder_id.is_empty() {
                format!(
                    "{}?page={}&perPage=100&workspaceId=0",
                    Self::api_url("/drive/file-entries"),
                    page
                )
            } else {
                format!(
                    "{}?page={}&perPage=100&workspaceId=0&parentIds={}",
                    Self::api_url("/drive/file-entries"),
                    page,
                    folder_id
                )
            };

            let mut attempt = 0u32;
            let list_resp: DrimeListResponse = loop {
                let resp = self
                    .client
                    .get(&url)
                    .header(AUTHORIZATION, self.auth_header()?)
                    .send()
                    .await
                    .map_err(|e| {
                        ProviderError::ConnectionFailed(format!("Find file failed: {}", e))
                    })?;

                let status = resp.status();
                if status.is_success() {
                    break resp.json::<DrimeListResponse>().await.map_err(|e| {
                        ProviderError::ServerError(format!("Parse find response failed: {}", e))
                    })?;
                }

                let body = resp.text().await.unwrap_or_default();
                let retryable = drime_status_is_retryable(status.as_u16());
                attempt += 1;
                if retryable && attempt < MAX_ATTEMPTS {
                    let delay = RETRY_DELAYS_MS[(attempt - 1) as usize];
                    drime_log(&format!(
                        "Find file transient {} (attempt {}/{}), retrying in {}ms",
                        status, attempt, MAX_ATTEMPTS, delay
                    ));
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    continue;
                }

                return Err(ProviderError::ServerError(format!(
                    "Drime list folder '{}' failed ({}): {}",
                    folder_id,
                    status,
                    sanitize_api_error(&body)
                )));
            };

            let files = list_resp.data.unwrap_or_default();
            let last_page = list_resp.last_page.unwrap_or(1);

            for file in &files {
                if let (Some(ref name), Some(id)) = (&file.name, file.id_str()) {
                    let Some(exact) = wanted(name, &id) else {
                        continue;
                    };
                    let is_dir = file.file_type.as_deref() == Some("folder");
                    if exact {
                        return Ok(Some((id, is_dir, file.hash.clone())));
                    }
                    if fallback.is_none() {
                        fallback = Some((id, is_dir, file.hash.clone()));
                    }
                }
            }

            if page >= last_page {
                break;
            }
            page += 1;
        }

        Ok(fallback)
    }

    /// Parse a Drime date string into "YYYY-MM-DD HH:MM:SS" format
    fn parse_date(date_str: &str) -> Option<String> {
        // ISO 8601, "2025-01-15T10:30:00.000000Z", or with a space for the T.
        // Converted to UTC before formatting: the format ends in a literal
        // `Z`, and formatting the parsed offset time printed its local hour
        // under that `Z`, off by the offset.
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(date_str)
            .or_else(|_| chrono::DateTime::parse_from_rfc3339(&date_str.replace(' ', "T")))
        {
            return Some(
                dt.with_timezone(&chrono::Utc)
                    .format("%Y-%m-%d %H:%M:%SZ")
                    .to_string(),
            );
        }
        // Return as-is if it looks like a date (safe truncation at char boundary)
        if date_str.len() >= 10 {
            let end = 19.min(date_str.len());
            let safe_end = if date_str.is_char_boundary(end) {
                end
            } else {
                date_str.len().min(end)
            };
            return Some(date_str[..safe_end].to_string());
        }
        None
    }

    /// S3 multipart upload for files >= 5 MB
    /// Flow: create → sign URLs → PUT chunks → complete → register entry
    async fn upload_multipart(
        &self,
        data: Vec<u8>,
        filename: &str,
        parent_id: &str,
        on_progress: Option<Box<dyn Fn(u64, u64) + Send>>,
    ) -> Result<(), ProviderError> {
        const CHUNK_SIZE: usize = 5 * 1024 * 1024; // 5 MB per Drime API spec
        let file_size = data.len() as u64;
        let total_parts = data.len().div_ceil(CHUNK_SIZE) as u32;

        // Infer MIME and extension from filename
        let extension = filename.rsplit('.').next().unwrap_or("bin").to_string();
        let mime = mime_guess::from_ext(&extension)
            .first_or_octet_stream()
            .to_string();

        drime_log(&format!(
            "Multipart upload: {} ({} bytes, {} parts)",
            filename, file_size, total_parts
        ));

        // Step 1: Create multipart upload
        let create_body = serde_json::json!({
            "filename": filename,
            "mime": mime,
            "size": file_size,
            "extension": extension,
            "workspaceId": 0
        });
        if !parent_id.is_empty() {
            // parentId added below via mutable json
        }
        let mut create_json: serde_json::Value = create_body;
        if !parent_id.is_empty() {
            create_json["parentId"] = serde_json::json!(parent_id.parse::<i64>().unwrap_or(0));
        }

        let resp = self
            .client
            .post(Self::api_url("/s3/multipart/create"))
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(create_json.to_string())
            .send()
            .await
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!("Multipart create failed: {}", e))
            })?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Multipart create failed: {}",
                sanitize_api_error(&body)
            )));
        }

        let create_resp: DrimeMultipartCreateResponse = resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Parse multipart create response: {}", e))
        })?;

        let key = create_resp.key.ok_or_else(|| {
            ProviderError::ServerError("Missing 'key' in multipart create response".to_string())
        })?;
        let upload_id = create_resp.upload_id.ok_or_else(|| {
            ProviderError::ServerError(
                "Missing 'uploadId' in multipart create response".to_string(),
            )
        })?;

        drime_log(&format!(
            "Multipart created: uploadId={}",
            &upload_id[..20.min(upload_id.len())]
        ));

        // Step 2: Get signed URLs for all parts
        let part_numbers: Vec<u32> = (1..=total_parts).collect();
        let sign_body = serde_json::json!({
            "key": key,
            "uploadId": upload_id,
            "partNumbers": part_numbers
        });

        let resp = self
            .client
            .post(Self::api_url("/s3/multipart/batch-sign-part-urls"))
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(sign_body.to_string())
            .send()
            .await
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!("Sign part URLs failed: {}", e))
            })?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Sign part URLs failed: {}",
                sanitize_api_error(&body)
            )));
        }

        let sign_resp: DrimeSignPartUrlsResponse = resp
            .json()
            .await
            .map_err(|e| ProviderError::ServerError(format!("Parse sign URLs response: {}", e)))?;

        let signed_urls = sign_resp.urls.ok_or_else(|| {
            ProviderError::ServerError("Missing 'urls' in sign response".to_string())
        })?;

        // Step 3: Upload each chunk to its signed URL
        let mut completed_parts: Vec<serde_json::Value> = Vec::new();
        let mut bytes_uploaded: u64 = 0;

        for signed in &signed_urls {
            let part_num = signed.part_number as usize;
            let start = (part_num - 1) * CHUNK_SIZE;
            let end = (start + CHUNK_SIZE).min(data.len());
            let chunk = &data[start..end];

            let resp = self
                .client
                .put(&signed.url)
                .body(chunk.to_vec())
                .send()
                .await
                .map_err(|e| {
                    ProviderError::ConnectionFailed(format!(
                        "Upload part {} failed: {}",
                        part_num, e
                    ))
                })?;

            if !resp.status().is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!(
                    "Upload part {} failed: {}",
                    part_num,
                    sanitize_api_error(&body)
                )));
            }

            // Extract ETag from response headers (with quotes)
            let etag = resp
                .headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
                .unwrap_or_default();

            completed_parts.push(serde_json::json!({
                "PartNumber": signed.part_number,
                "ETag": etag
            }));

            bytes_uploaded += chunk.len() as u64;
            if let Some(ref cb) = on_progress {
                cb(bytes_uploaded, file_size);
            }
        }

        // Step 4: Complete multipart upload
        let complete_body = serde_json::json!({
            "key": key,
            "uploadId": upload_id,
            "parts": completed_parts
        });

        let resp = self
            .client
            .post(Self::api_url("/s3/multipart/complete"))
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(complete_body.to_string())
            .send()
            .await
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!("Multipart complete failed: {}", e))
            })?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Multipart complete failed: {}",
                sanitize_api_error(&body)
            )));
        }

        // Step 5: Register file entry in Drime
        let s3_filename = key.rsplit('/').next().unwrap_or(&key);
        let mut entry_body = serde_json::json!({
            "filename": s3_filename,
            "size": file_size,
            "clientName": filename,
            "clientMime": mime,
            "clientExtension": extension,
            "workspaceId": 0
        });
        if !parent_id.is_empty() {
            entry_body["parentId"] = serde_json::json!(parent_id.parse::<i64>().unwrap_or(0));
        }

        let resp = self
            .client
            .post(Self::api_url("/s3/entries"))
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(entry_body.to_string())
            .send()
            .await
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!("S3 entry registration failed: {}", e))
            })?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "S3 entry registration failed: {}",
                sanitize_api_error(&body)
            )));
        }

        drime_log(&format!(
            "Multipart upload complete: {} ({} bytes, {} parts)",
            filename, file_size, total_parts
        ));
        Ok(())
    }

    /// Execute a request with exponential backoff on 429 (rate limit)
    async fn request_with_retry(
        &self,
        build_request: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ProviderError> {
        const MAX_RETRIES: u32 = 3;
        let mut delay = std::time::Duration::from_secs(1);

        for attempt in 0..=MAX_RETRIES {
            let resp = build_request()
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(format!("Request failed: {}", e)))?;

            if resp.status().as_u16() == 429 {
                if attempt < MAX_RETRIES {
                    // Use Retry-After header if present, otherwise exponential backoff
                    let wait = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok())
                        .map(std::time::Duration::from_secs)
                        .unwrap_or(delay);

                    warn!(
                        "[DRIME] Rate limited (429), retrying in {:?} (attempt {}/{})",
                        wait,
                        attempt + 1,
                        MAX_RETRIES
                    );
                    tokio::time::sleep(wait).await;
                    delay *= 2; // exponential backoff: 1s, 2s, 4s
                    continue;
                }
                return Err(ProviderError::ServerError(
                    "Rate limited by Drime API (429). Please try again later.".to_string(),
                ));
            }

            return Ok(resp);
        }

        unreachable!()
    }
}

// ─── StorageProvider Implementation ──────────────────────────────────────

#[async_trait]
impl StorageProvider for DrimeCloudProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::DrimeCloud
    }

    fn display_name(&self) -> String {
        "Drime".to_string()
    }

    async fn connect(&mut self) -> Result<(), ProviderError> {
        drime_log("Connecting to Drime");

        // Validate token via /cli/loggedUser (purpose-built for auth check + returns user info)
        let url = Self::api_url("/cli/loggedUser");
        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .send()
            .await
            .map_err(|e| {
                drime_log(&format!(
                    "Connection error: {} (is_timeout={}, is_connect={})",
                    e,
                    e.is_timeout(),
                    e.is_connect()
                ));
                ProviderError::ConnectionFailed(e.to_string())
            })?;

        let status = resp.status();
        let body = resp.text().await.map_err(|e| {
            ProviderError::ConnectionFailed(format!("Failed to read response: {}", e))
        })?;
        drime_log(&format!(
            "Auth check response: status={}, len={}",
            status,
            body.len()
        ));

        // Detect HTML response (SPA catch-all for non-existent routes)
        if body.starts_with("<!") || body.starts_with("<html") {
            return Err(ProviderError::ConnectionFailed(
                "Server returned HTML instead of JSON. The API might not be available.".to_string(),
            ));
        }

        if status.as_u16() == 401 || body.contains("Unauthenticated") {
            return Err(ProviderError::AuthenticationFailed(
                "Invalid API token. Generate one at app.drime.cloud → Account Settings → Developers".to_string()
            ));
        }

        if !status.is_success() {
            return Err(ProviderError::ConnectionFailed(format!(
                "Drime connection failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        // Extract user ID for folder tree operations
        if let Ok(user_resp) = serde_json::from_str::<DrimeUserResponse>(&body) {
            if let Some(user) = user_resp.user {
                self.user_id = user.id;
                drime_log(&format!("Authenticated as user_id={:?}", self.user_id));
            }
        }

        // Initialize root
        self.current_folder_id = String::new();
        self.current_path = "/".to_string();
        self.dir_cache_insert("/".to_string(), DirInfo { id: String::new() });

        // Navigate to initial path if specified
        if let Some(ref initial) = self.config.initial_path {
            let initial = initial.trim().to_string();
            if !initial.is_empty() && initial != "/" {
                let normalized = Self::normalize_path(&initial);
                drime_log(&format!("Navigating to initial path: {}", normalized));
                match self.resolve_folder_id(&normalized).await {
                    Ok(id) => {
                        self.current_path = normalized;
                        self.current_folder_id = id;
                    }
                    Err(e) => {
                        drime_log(&format!("Initial path error (using root): {}", e));
                    }
                }
            }
        }

        self.connected = true;
        drime_log("Connected successfully");
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ProviderError> {
        self.connected = false;
        self.current_path = "/".to_string();
        self.current_folder_id = String::new();
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
        self.current_folder_id = folder_id;
        self.current_path = new_path;
        Ok(())
    }

    async fn cd_up(&mut self) -> Result<(), ProviderError> {
        self.cd("..").await
    }

    async fn list(&mut self, path: &str) -> Result<Vec<RemoteEntry>, ProviderError> {
        let resolved = self.resolve_path(path);
        let folder_id = self.resolve_folder_id(&resolved).await?;
        let cache_children = self.is_cached_exactly(&resolved);

        let mut entries = Vec::new();
        let mut page = 1u32;

        loop {
            let url = if folder_id.is_empty() {
                format!(
                    "{}?page={}&perPage=50&workspaceId=0",
                    Self::api_url("/drive/file-entries"),
                    page
                )
            } else {
                format!(
                    "{}?page={}&perPage=50&workspaceId=0&parentIds={}",
                    Self::api_url("/drive/file-entries"),
                    page,
                    folder_id
                )
            };

            let resp = self
                .client
                .get(&url)
                .header(AUTHORIZATION, self.auth_header()?)
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(format!("List failed: {}", e)))?;

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

            let list_resp: DrimeListResponse = resp.json().await.map_err(|e| {
                ProviderError::ServerError(format!("Parse list response failed: {}", e))
            })?;

            let files = list_resp.data.unwrap_or_default();
            let last_page = list_resp.last_page.unwrap_or(1);

            for file in files {
                let name = file
                    .name
                    .clone()
                    .unwrap_or_else(|| file.id_str().unwrap_or_else(|| "unnamed".to_string()));
                let is_dir = file.file_type.as_deref() == Some("folder");
                let size = file.size.unwrap_or(0);
                let modified = file.updated_at.as_deref().and_then(Self::parse_date);

                // Cache directories
                if is_dir {
                    if let Some(id) = file.id_str() {
                        let dir_path = if resolved == "/" {
                            format!("/{}", name)
                        } else {
                            format!("{}/{}", resolved, name)
                        };
                        if cache_children {
                            self.dir_cache_insert(dir_path, DirInfo { id });
                        }
                    }
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
                    mime_type: file.mime_type,
                });
            }

            if page >= last_page {
                break;
            }
            page += 1;
        }

        // list() is intentionally read-only: it must NOT mutate current_path
        // or current_folder_id. The benchmark CLI (and any caller mixing
        // list() with subsequent relative-path operations) relies on
        // current_path remaining stable. Mutating it here previously caused
        // a path-concatenation bug where stat()/delete() right after list()
        // resolved "aeroftp-bench/<id>/payload" against current_path
        // "/aeroftp-bench/<id>" and produced the doubled path
        // "/aeroftp-bench/<id>/aeroftp-bench/<id>/payload".
        // current_folder_id was write-only here (no readers), so dropping
        // both is safe. Use cd()/cd_up() to move the working directory.
        let _ = folder_id; // kept resolved for clarity, no longer assigned

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

        let (file_id, _is_dir, file_hash) =
            self.find_file_in_folder(&parent_id, filename)
                .await?
                .ok_or_else(|| ProviderError::NotFound(format!("File '{}' not found", filename)))?;

        drime_log(&format!("Downloading file {} (id={})", filename, file_id));

        // Bedrive uses hash-based download, fall back to ID-based
        let url = if let Some(ref hash) = file_hash {
            Self::api_url(&format!("/file-entries/download/{}", hash))
        } else {
            Self::api_url(&format!("/file-entries/{}/download", file_id))
        };
        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .send()
            .await
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!("Download request failed: {}", e))
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Download failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        let total_size = resp.content_length().unwrap_or(0);
        let bytes = resp.bytes().await.map_err(|e| {
            ProviderError::ServerError(format!("Failed to read download body: {}", e))
        })?;

        if let Some(ref cb) = on_progress {
            cb(bytes.len() as u64, total_size);
        }

        tokio::fs::write(local_path, &bytes)
            .await
            .map_err(ProviderError::IoError)?;

        drime_log(&format!("Downloaded {} ({} bytes)", filename, bytes.len()));
        Ok(())
    }

    async fn download_to_bytes(&mut self, remote_path: &str) -> Result<Vec<u8>, ProviderError> {
        let resolved = self.resolve_path(remote_path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        let (file_id, _, file_hash) = self
            .find_file_in_folder(&parent_id, filename)
            .await?
            .ok_or_else(|| ProviderError::NotFound(format!("File '{}' not found", filename)))?;

        let url = if let Some(ref hash) = file_hash {
            Self::api_url(&format!("/file-entries/download/{}", hash))
        } else {
            Self::api_url(&format!("/file-entries/{}/download", file_id))
        };
        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Download failed: {}", e)))?;

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
        const MULTIPART_THRESHOLD: u64 = 5 * 1024 * 1024; // 5 MB

        let resolved = self.resolve_path(remote_path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id_exact(parent_path).await?;

        // M9: Full file read into memory: no streaming upload API available for Drime Cloud.
        // This limits practical upload size to available RAM. For files >500MB, users should
        // consider alternative providers with chunked upload support (S3, OneDrive, Dropbox).
        let data = tokio::fs::read(local_path)
            .await
            .map_err(ProviderError::IoError)?;

        let file_size = data.len() as u64;
        drime_log(&format!(
            "Uploading {} ({} bytes) to folder '{}'",
            filename, file_size, parent_id
        ));

        // Delete existing file before overwrite: the one of this exact
        // name, never one of another letter case, and never a folder: a
        // folder of that name went to the trash with everything in it.
        let existing = match self.find_exact_in_folder(&parent_id, filename).await? {
            Some((_, true, _)) => {
                return Err(ProviderError::InvalidPath(format!(
                    "{resolved} is a folder: an upload replaces a file, never a folder"
                )))
            }
            Some((id, false, _)) => Some(id),
            None => None,
        };
        if let Some(existing_id) = existing {
            drime_log(&format!(
                "File {} exists (id={}), deleting before overwrite",
                filename, existing_id
            ));
            let del_body =
                serde_json::json!({ "entryIds": [existing_id.parse::<i64>().unwrap_or(0)] });
            let _ = self
                .client
                .post(Self::api_url("/file-entries/delete"))
                .header(AUTHORIZATION, self.auth_header()?)
                .header(CONTENT_TYPE, "application/json")
                .body(del_body.to_string())
                .send()
                .await;
        }

        if let Some(ref cb) = on_progress {
            cb(0, file_size);
        }

        // Route by file size: < 5MB direct upload, >= 5MB S3 multipart
        if file_size >= MULTIPART_THRESHOLD {
            self.upload_multipart(data, filename, &parent_id, on_progress)
                .await?;
        } else {
            // Direct upload via multipart POST
            let mut form = reqwest::multipart::Form::new();
            if !parent_id.is_empty() {
                form = form.text("parentId", parent_id.clone());
            }
            form = form.text("workspaceId", "0");

            let part = reqwest::multipart::Part::bytes(data)
                .file_name(filename.to_string())
                .mime_str("application/octet-stream")
                .map_err(|e| ProviderError::ServerError(format!("MIME error: {}", e)))?;
            form = form.part("file", part);

            let resp = self
                .client
                .post(Self::api_url("/uploads"))
                .header(AUTHORIZATION, self.auth_header()?)
                .multipart(form)
                .send()
                .await
                .map_err(|e| {
                    ProviderError::ConnectionFailed(format!("Upload request failed: {}", e))
                })?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!(
                    "Upload failed ({}): {}",
                    status,
                    sanitize_api_error(&body)
                )));
            }

            if let Some(ref cb) = on_progress {
                cb(file_size, file_size);
            }
        }

        drime_log(&format!("Uploaded {} successfully", filename));
        Ok(())
    }

    async fn mkdir(&mut self, path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);
        let (parent_path, dir_name) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;
        let cache_new = self.is_cached_exactly(parent_path);

        drime_log(&format!(
            "Creating directory '{}' in folder '{}'",
            dir_name, parent_id
        ));

        let body = if parent_id.is_empty() {
            serde_json::json!({
                "name": dir_name
            })
        } else {
            serde_json::json!({
                "name": dir_name,
                "parentId": parent_id
            })
        };

        let url = Self::api_url("/folders?workspaceId=0");
        let resp = self
            .client
            .post(&url)
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Mkdir failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            let body_lower = body.to_lowercase();
            if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY
                && body_lower.contains("already exists")
            {
                return Err(ProviderError::AlreadyExists(resolved));
            }
            return Err(ProviderError::ServerError(format!(
                "Create directory failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        // Cache the new dir
        if let Ok(folder_resp) = resp.json::<DrimeFolderResponse>().await {
            if let (true, Some(id)) = (cache_new, folder_resp.id_str()) {
                self.dir_cache_insert(resolved, DirInfo { id });
            }
        }

        Ok(())
    }

    async fn delete(&mut self, path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id_exact(parent_path).await?;

        let (file_id, _, _) = self
            .find_exact_in_folder(&parent_id, filename)
            .await?
            .ok_or_else(|| ProviderError::NotFound(format!("'{}' not found", filename)))?;

        drime_log(&format!("Deleting {} (id={})", filename, file_id));

        // Batch delete endpoint (moves to trash by default)
        let body = serde_json::json!({ "entryIds": [file_id.parse::<i64>().unwrap_or(0)] });
        let sent = self
            .client
            .post(Self::api_url("/file-entries/delete"))
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await;
        // Whatever the answer, even none (a delete Drime applied whose
        // answer was lost or came back as an error), the ids cached for the
        // item and for everything under it may point into the trash: forgot
        // only after a success, they outlived a failed answer. Under every
        // capitalization: a folder lookup falls back to another case, so one
        // folder can be cached under two spellings.
        super::forget_cached_subtree_ignoring_case(&mut self.dir_cache, &resolved);
        let resp =
            sent.map_err(|e| ProviderError::ConnectionFailed(format!("Delete failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Delete failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        Ok(())
    }

    /// A move keeps the source's name and then a rename gives it the new
    /// one. Drime refuses a rename onto a name taken in the same folder
    /// (400, live on 2026-09-26), but only after the move has happened, and
    /// its move is not documented to refuse a name the destination folder
    /// already holds, so the destination is looked up first and a taken one
    /// refused. When the destination folder already holds the old name the
    /// rename goes first, in the source folder.
    async fn rename(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let resolved_from = self.resolve_path(from);
        let resolved_to = self.resolve_path(to);
        if resolved_from == resolved_to {
            return Ok(());
        }
        super::refuse_occupied_destination(self, &resolved_from, &resolved_to).await?;
        let (from_parent, from_name) = Self::split_path(&resolved_from);
        let (to_parent, to_name) = Self::split_path(&resolved_to);
        let from_parent_id = self.resolve_folder_id(from_parent).await?;

        let (file_id, _, _) = self
            .find_file_in_folder(&from_parent_id, from_name)
            .await?
            .ok_or_else(|| ProviderError::NotFound(format!("'{}' not found", from_name)))?;

        let moves = from_parent != to_parent;
        let renames = from_name != to_name;
        let to_parent_id = if moves {
            Some(self.resolve_folder_id(to_parent).await?)
        } else {
            None
        };
        let rename_first = match &to_parent_id {
            Some(id) if renames => self.find_file_in_folder(id, from_name).await?.is_some(),
            _ => false,
        };
        if rename_first
            && self
                .find_file_in_folder(&from_parent_id, to_name)
                .await?
                .is_some()
        {
            return Err(ProviderError::Other(format!(
                "Cannot move {resolved_from} to {resolved_to} in two steps without two items \
                 sharing a name: the destination folder holds {from_name} and the source \
                 folder holds {to_name}"
            )));
        }
        // Two steps when both the folder and the name change. If the second
        // fails, the first is undone, and if that fails too the error says
        // where the item is.
        let outcome = async {
            match &to_parent_id {
                None => self.rename_entry(&file_id, to_name, &resolved_to).await,
                Some(to_parent_id) if rename_first => {
                    self.rename_entry(&file_id, to_name, &resolved_to).await?;
                    if let Err(e) = self.move_entry(&file_id, to_parent_id, &resolved_to).await {
                        let undone = match self
                            .way_back_is_free(&from_parent_id, from_name, &file_id, &resolved_from)
                            .await
                        {
                            Ok(()) => self.rename_entry(&file_id, from_name, &resolved_from).await,
                            Err(e) => Err(e),
                        };
                        let now_at = format!("{}/{to_name}", from_parent.trim_end_matches('/'));
                        return Err(super::second_step_failed(
                            &resolved_from,
                            &resolved_to,
                            &now_at,
                            e,
                            undone,
                        ));
                    }
                    Ok(())
                }
                Some(to_parent_id) => {
                    self.move_entry(&file_id, to_parent_id, &resolved_to)
                        .await?;
                    if !renames {
                        return Ok(());
                    }
                    if let Err(e) = self.rename_entry(&file_id, to_name, &resolved_to).await {
                        let undone = match self
                            .way_back_is_free(&from_parent_id, from_name, &file_id, &resolved_from)
                            .await
                        {
                            Ok(()) => {
                                self.move_entry(&file_id, &from_parent_id, &resolved_from)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let now_at = format!("{}/{from_name}", to_parent.trim_end_matches('/'));
                        return Err(super::second_step_failed(
                            &resolved_from,
                            &resolved_to,
                            &now_at,
                            e,
                            undone,
                        ));
                    }
                    Ok(())
                }
            }
        }
        .await;

        // Whatever happened, the ids cached for either path, and for
        // everything under them, may now point at moved items. Under every
        // capitalization, as in `delete`.
        super::forget_cached_subtree_ignoring_case(&mut self.dir_cache, &resolved_from);
        super::forget_cached_subtree_ignoring_case(&mut self.dir_cache, &resolved_to);
        outcome
    }

    /// Drime's rename and move never overwrite, so a replace sets the item at
    /// `to` aside, renames `from` in, and then deletes the one set aside, which
    /// Drime moves to its trash: see [`super::replace_by_setting_aside`].
    ///
    /// Only an item of exactly that name is set aside and deleted: the
    /// lookups fall back to another letter case, and a replace onto `a.txt`
    /// beside only `A.txt` set `A.txt` aside and deleted it. Without one it
    /// is the rename, whose look before the move still refuses a name taken
    /// in another case.
    async fn replace(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let (from, to) = (self.resolve_path(from), self.resolve_path(to));
        let (to_parent, to_name) = Self::split_path(&to);
        let to_parent_id = self.resolve_folder_id_exact(to_parent).await?;
        if self
            .find_exact_in_folder(&to_parent_id, to_name)
            .await?
            .is_none()
        {
            return self.rename(&from, &to).await;
        }
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

    async fn rmdir(&mut self, path: &str) -> Result<(), ProviderError> {
        // Round 2 of the 4.2.1 review: the API's delete takes a folder's
        // content along, so a folder that lists anything is refused here and
        // only one that listed empty reaches it.
        self.refuse_non_empty_dir(path).await?;
        self.delete(path).await
    }

    async fn rmdir_recursive(&mut self, path: &str) -> Result<(), ProviderError> {
        self.delete(path).await
    }

    async fn stat(&mut self, path: &str) -> Result<RemoteEntry, ProviderError> {
        let resolved = self.resolve_path(path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        // Search for the file in the parent folder: the name as spelled
        // first, whatever the order of the listing, and another case as the
        // fallback (as `find_file_in_folder`). With `A.txt` listed before
        // `a.txt`, a first match that ignored the case answered `A.txt`.
        let entry = |file: &DrimeFile, name: &str| RemoteEntry {
            name: name.to_string(),
            path: resolved.clone(),
            is_dir: file.file_type.as_deref() == Some("folder"),
            size: file.size.unwrap_or(0),
            modified: file.updated_at.as_deref().and_then(Self::parse_date),
            permissions: None,
            owner: None,
            group: None,
            is_symlink: false,
            link_target: None,
            metadata: HashMap::new(),
            mime_type: file.mime_type.clone(),
        };
        let mut fallback = None;
        let mut page = 1u32;

        loop {
            let url = if parent_id.is_empty() {
                format!(
                    "{}?page={}&perPage=100&workspaceId=0",
                    Self::api_url("/drive/file-entries"),
                    page
                )
            } else {
                format!(
                    "{}?page={}&perPage=100&workspaceId=0&parentIds={}",
                    Self::api_url("/drive/file-entries"),
                    page,
                    parent_id
                )
            };

            let resp = self
                .client
                .get(&url)
                .header(AUTHORIZATION, self.auth_header()?)
                .send()
                .await
                .map_err(|e| ProviderError::ConnectionFailed(format!("Stat failed: {}", e)))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                // A 404 on the file-entries listing is an absence, not a server
                // fault: map it to NotFound so `exists()` returns Ok(false) rather
                // than propagating an Err (same class as the pCloud crypt-overlay
                // probe bug). Any other status stays a ServerError.
                if status.as_u16() == 404 {
                    return Err(ProviderError::NotFound(sanitize_api_error(&body)));
                }
                return Err(ProviderError::ServerError(format!(
                    "Stat failed: {}",
                    sanitize_api_error(&body)
                )));
            }

            let list_resp: DrimeListResponse = resp.json().await.map_err(|e| {
                ProviderError::ServerError(format!("Parse stat response failed: {}", e))
            })?;

            let files = list_resp.data.unwrap_or_default();
            let last_page = list_resp.last_page.unwrap_or(1);

            for file in &files {
                if let Some(ref name) = file.name {
                    if name == filename {
                        return Ok(entry(file, name));
                    }
                    if fallback.is_none() && name.eq_ignore_ascii_case(filename) {
                        fallback = Some(entry(file, name));
                    }
                }
            }

            if page >= last_page {
                break;
            }
            page += 1;
        }

        fallback.ok_or_else(|| ProviderError::NotFound(format!("'{}' not found", filename)))
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
        Ok("Drime: 20GB Secure Cloud Storage".to_string())
    }

    fn supports_server_copy(&self) -> bool {
        true
    }

    fn supports_server_side_copy(&self) -> bool {
        true
    }

    async fn server_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        // Legacy alias kept so CLI / MCP / provider_commands callers keep
        // working. The real `/file-entries/duplicate` implementation lives
        // on `server_side_copy` (S3-T10 migration, v4.0.0).
        StorageProvider::server_side_copy(self, from, to).await
    }

    async fn server_side_copy(&mut self, from: &str, to: &str) -> Result<(), ProviderError> {
        let resolved_from = self.resolve_path(from);
        let resolved_to = self.resolve_path(to);
        let (from_parent, from_name) = Self::split_path(&resolved_from);
        let (to_parent, to_name) = Self::split_path(&resolved_to);
        let from_parent_id = self.resolve_folder_id(from_parent).await?;

        let (file_id, _, _) = self
            .find_file_in_folder(&from_parent_id, from_name)
            .await?
            .ok_or_else(|| ProviderError::NotFound(format!("'{}' not found", from_name)))?;

        let file_id_num = file_id.parse::<i64>().unwrap_or(0);

        // Resolve destination folder (may differ from source)
        let to_parent_id = self.resolve_folder_id(to_parent).await?;

        drime_log(&format!(
            "Duplicating {} (id={}) → {}",
            from_name, file_id, resolved_to
        ));

        // POST /file-entries/duplicate with destinationId for direct cross-folder copy
        let mut dup_body = serde_json::json!({ "entryIds": [file_id_num] });
        if from_parent != to_parent {
            let dest_id: serde_json::Value = if to_parent_id.is_empty() {
                serde_json::Value::Null // root
            } else {
                serde_json::json!(to_parent_id.parse::<i64>().unwrap_or(0))
            };
            dup_body["destinationId"] = dest_id;
        }

        let resp = self
            .client
            .post(Self::api_url("/file-entries/duplicate?workspaceId=0"))
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(dup_body.to_string())
            .send()
            .await
            .map_err(|e| ProviderError::ConnectionFailed(format!("Copy failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Copy failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        // Rename if destination name differs from source
        // API returns { entries: [...], status: "success" }
        if from_name != to_name {
            if let Ok(resp_data) = resp.json::<DrimeEntriesResponse>().await {
                if let Some(first) = resp_data.entries.as_ref().and_then(|e| e.first()) {
                    if let Some(dup_id) = first.id_str() {
                        let rename_url =
                            Self::api_url(&format!("/file-entries/{}?workspaceId=0", dup_id));
                        let rename_resp = self
                            .client
                            .put(&rename_url)
                            .header(AUTHORIZATION, self.auth_header()?)
                            .header(CONTENT_TYPE, "application/json")
                            .body(serde_json::json!({ "name": to_name }).to_string())
                            .send()
                            .await
                            .map_err(|e| {
                                ProviderError::ConnectionFailed(format!(
                                    "Rename after copy failed: {}",
                                    e
                                ))
                            })?;

                        if !rename_resp.status().is_success() {
                            drime_log(&format!(
                                "Warning: rename after duplicate failed ({})",
                                rename_resp.status()
                            ));
                        }
                    }
                }
            }
        }

        Ok(())
    }

    async fn storage_info(&mut self) -> Result<StorageInfo, ProviderError> {
        let url = Self::api_url("/user/space-usage?workspaceId=0");
        let auth = self.auth_header()?;
        let resp = self
            .request_with_retry(|| self.client.get(&url).header(AUTHORIZATION, auth.clone()))
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Quota failed: {}",
                sanitize_api_error(&body)
            )));
        }

        let storage: DrimeStorageResponse = resp.json().await.map_err(|e| {
            ProviderError::ServerError(format!("Parse quota response failed: {}", e))
        })?;

        let used = storage.used.unwrap_or(0);
        let available = storage.available.unwrap_or(0);

        Ok(StorageInfo {
            used,
            total: used + available,
            free: available,
            versioning_bytes: None,
        })
    }

    // ─── Search ─────────────────────────────────────────────────────────

    fn supports_find(&self) -> bool {
        true
    }

    async fn find(
        &mut self,
        _path: &str,
        pattern: &str,
    ) -> Result<Vec<RemoteEntry>, ProviderError> {
        drime_log(&format!("Searching for '{}'", pattern));

        // Drime /drive/file-entries?query= is substring on filename and
        // ignores glob metacharacters. Strip glob chars for the broad server
        // prefilter, then apply the precise filter client-side.
        let literal: String = pattern
            .chars()
            .filter(|c| !matches!(c, '*' | '?' | '[' | ']'))
            .collect();
        let server_query = if literal.is_empty() {
            ".".to_string()
        } else {
            literal
        };

        let mut entries = Vec::new();
        let mut page = 1u32;

        loop {
            let url = format!(
                "{}?workspaceId=0&query={}&page={}&perPage=50",
                Self::api_url("/drive/file-entries"),
                urlencoding::encode(&server_query),
                page
            );

            let auth = self.auth_header()?;
            let resp = self
                .request_with_retry(|| self.client.get(&url).header(AUTHORIZATION, auth.clone()))
                .await?;

            if !resp.status().is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(ProviderError::ServerError(format!(
                    "Search failed: {}",
                    sanitize_api_error(&body)
                )));
            }

            let list_resp: DrimeListResponse = resp
                .json()
                .await
                .map_err(|e| ProviderError::ServerError(format!("Parse search response: {}", e)))?;

            let files = list_resp.data.unwrap_or_default();
            let last_page = list_resp.last_page.unwrap_or(1);

            for file in files {
                let name = file.name.clone().unwrap_or_default();
                if !super::matches_find_pattern(&name, pattern) {
                    continue;
                }
                let is_dir = file.file_type.as_deref() == Some("folder");

                entries.push(RemoteEntry {
                    name: name.clone(),
                    path: format!("/{}", name), // search results don't include full path
                    is_dir,
                    size: file.size.unwrap_or(0),
                    modified: file.updated_at.as_deref().and_then(Self::parse_date),
                    permissions: None,
                    owner: None,
                    group: None,
                    is_symlink: false,
                    link_target: None,
                    metadata: HashMap::new(),
                    mime_type: file.mime_type,
                });
            }

            if page >= last_page {
                break;
            }
            page += 1;
        }

        drime_log(&format!(
            "Search '{}' found {} results",
            pattern,
            entries.len()
        ));
        Ok(entries)
    }

    // ─── Share Links ────────────────────────────────────────────────────

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

    async fn list_share_links(&mut self, path: &str) -> Result<Vec<ShareLinkInfo>, ProviderError> {
        let resolved = self.resolve_path(path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        let (file_id, _, _) = self
            .find_file_in_folder(&parent_id, filename)
            .await?
            .ok_or_else(|| ProviderError::NotFound(format!("'{}' not found", filename)))?;

        let url = Self::api_url(&format!("/file-entries/{}/shareable-link", file_id));
        let auth = self.auth_header()?;
        let resp = self
            .request_with_retry(|| self.client.get(&url).header(AUTHORIZATION, auth.clone()))
            .await?;

        if resp.status().as_u16() == 404 {
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

        let link_resp: DrimeShareLinkResponse = resp
            .json()
            .await
            .map_err(|e| ProviderError::ServerError(format!("Parse share link response: {}", e)))?;

        if let Some(link) = link_resp.link {
            if let Some(hash) = link.hash {
                let share_url = format!("https://app.drime.cloud/drive/shares/{}", hash);
                return Ok(vec![ShareLinkInfo {
                    id: hash,
                    url: share_url,
                    created_at: None,
                    expires_at: None,
                    password_protected: false,
                    permissions: None,
                }]);
            }
        }

        Ok(vec![])
    }

    async fn create_share_link(
        &mut self,
        path: &str,
        options: ShareLinkOptions,
    ) -> Result<ShareLinkResult, ProviderError> {
        let resolved = self.resolve_path(path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id_exact(parent_path).await?;

        let (file_id, _, _) = self
            .find_exact_in_folder(&parent_id, filename)
            .await?
            .ok_or_else(|| ProviderError::NotFound(format!("'{}' not found", filename)))?;

        drime_log(&format!(
            "Creating share link for {} (id={})",
            filename, file_id
        ));

        let allow_edit = options.permissions.as_deref() == Some("edit");
        let mut body = serde_json::json!({
            "allow_download": true,
            "allow_edit": allow_edit
        });

        if let Some(secs) = options.expires_in_secs {
            let expires_at = chrono::Utc::now() + chrono::Duration::seconds(secs as i64);
            body["expires_at"] = serde_json::json!(expires_at.to_rfc3339());
        }
        if let Some(ref pw) = options.password {
            body["password"] = serde_json::json!(pw);
        }

        let url = Self::api_url(&format!("/file-entries/{}/shareable-link", file_id));
        let auth = self.auth_header()?;
        let resp = self
            .request_with_retry(|| {
                self.client
                    .post(&url)
                    .header(AUTHORIZATION, auth.clone())
                    .header(CONTENT_TYPE, "application/json")
                    .body(body.to_string())
            })
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let resp_body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Create share link failed ({}): {}",
                status,
                sanitize_api_error(&resp_body)
            )));
        }

        let link_resp: DrimeShareLinkResponse = resp
            .json()
            .await
            .map_err(|e| ProviderError::ServerError(format!("Parse share link response: {}", e)))?;

        let hash = link_resp.link.and_then(|l| l.hash).ok_or_else(|| {
            ProviderError::ServerError("Missing hash in share link response".to_string())
        })?;

        let share_url = format!("https://app.drime.cloud/drive/shares/{}", hash);
        drime_log(&format!("Share link created: {}", share_url));
        Ok(ShareLinkResult {
            url: share_url,
            password: None,
            expires_at: None,
        })
    }

    async fn remove_share_link(&mut self, path: &str) -> Result<(), ProviderError> {
        let resolved = self.resolve_path(path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id_exact(parent_path).await?;

        let (file_id, _, _) = self
            .find_exact_in_folder(&parent_id, filename)
            .await?
            .ok_or_else(|| ProviderError::NotFound(format!("'{}' not found", filename)))?;

        drime_log(&format!(
            "Removing share link for {} (id={})",
            filename, file_id
        ));

        let url = Self::api_url(&format!("/file-entries/{}/shareable-link", file_id));
        let auth = self.auth_header()?;
        let resp = self
            .request_with_retry(|| self.client.delete(&url).header(AUTHORIZATION, auth.clone()))
            .await?;

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

    // ─── File Versions ──────────────────────────────────────────────────

    fn supports_versions(&self) -> bool {
        true
    }

    async fn list_versions(&mut self, path: &str) -> Result<Vec<FileVersion>, ProviderError> {
        let resolved = self.resolve_path(path);
        let (parent_path, filename) = Self::split_path(&resolved);
        let parent_id = self.resolve_folder_id(parent_path).await?;

        let (file_id, _, _) = self
            .find_file_in_folder(&parent_id, filename)
            .await?
            .ok_or_else(|| ProviderError::NotFound(format!("'{}' not found", filename)))?;

        drime_log(&format!(
            "Listing versions for {} (id={})",
            filename, file_id
        ));

        let url = format!(
            "{}?file_id={}&perPage=50",
            Self::api_url("/file-backup"),
            file_id
        );
        let auth = self.auth_header()?;
        let resp = self
            .request_with_retry(|| self.client.get(&url).header(AUTHORIZATION, auth.clone()))
            .await?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "List versions failed: {}",
                sanitize_api_error(&body)
            )));
        }

        let backup_resp: DrimeBackupResponse = resp
            .json()
            .await
            .map_err(|e| ProviderError::ServerError(format!("Parse backup response: {}", e)))?;

        let versions = backup_resp
            .pagination
            .and_then(|p| p.data)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|entry| {
                let id = entry.id.as_ref().map(|v| match v {
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })?;
                Some(FileVersion {
                    id,
                    modified: entry.created_at.as_deref().and_then(Self::parse_date),
                    size: entry.file_size.unwrap_or(0),
                    modified_by: entry.name,
                })
            })
            .collect();

        Ok(versions)
    }

    fn transfer_optimization_hints(&self) -> super::TransferOptimizationHints {
        super::TransferOptimizationHints {
            supports_multipart: true,
            multipart_threshold: DRIME_MULTIPART_THRESHOLD,
            multipart_part_size: DRIME_MULTIPART_PART_SIZE,
            // Drime's S3-compatible backend takes independent PUT
            // requests against the per-part signed URLs in parallel.
            // 4 matches the AIMD default budget the runner ships with.
            multipart_max_parallel: DRIME_TRANSFER_MAX_SESSIONS as u8,
            supports_resume_download: true,
            supports_resume_upload: true,
            supports_server_checksum: true,
            preferred_checksum_algo: Some("etag".to_string()),
            ..Default::default()
        }
    }

    // DAG-P1-05A: presigned part PUTs are addressable from an opaque
    // MultipartHandle, so each part can run on an independent clone that
    // holds only its own `reqwest::Client`. Begin/complete/abort stay on
    // the primary session (authenticated Drime API).
    fn transfer_executor_kind(&self) -> ProviderTransferExecutorKind {
        ProviderTransferExecutorKind::HttpClonePool
    }

    fn transfer_executor_max_sessions(&self) -> u16 {
        DRIME_TRANSFER_MAX_SESSIONS
    }

    fn clone_for_transfer(&self) -> Result<Box<dyn StorageProvider>, ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        Ok(Box::new(self.clone()))
    }

    // Shaped-graph multipart trait wiring (S3-T12).
    //
    // Drime's S3 multipart protocol maps onto the trait as:
    //   1. `begin_multipart_upload` → POST `/s3/multipart/create` returns
    //      `key` + `uploadId`. We then POST `/s3/multipart/batch-sign-part-urls`
    //      for every chunk number so the trait handle can carry pre-signed
    //      PUT URLs and `upload_part` doesn't need to round-trip the Drime
    //      API per chunk.
    //   2. `upload_part` → PUT the matching signed URL with the chunk body;
    //      Drime echoes the S3 ETag in the response header.
    //   3. `complete_multipart_upload` → POST `/s3/multipart/complete` with
    //      `[{PartNumber, ETag}]` then POST `/s3/entries` to register the
    //      file inside Drime's tree. Both calls are required; without the
    //      second the bytes land on S3 but never appear in the UI.
    //   4. `abort_multipart_upload` → POST `/s3/multipart/abort` best-effort.
    //
    // Drime is `dev-only, disabled in release` (see CLAUDE.md "Completato
    // in v2.6.0"), but the trait wiring is built unconditionally so the
    // runner has the right shape once the gate flips.
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
                "Drime multipart upload requires non-zero total_size".to_string(),
            ));
        }

        let resolved = self.resolve_path(remote_path);
        let (parent_path, filename) = Self::split_path(&resolved);
        if filename.is_empty() {
            return Err(ProviderError::InvalidPath("Missing file name".into()));
        }
        let parent_id = self.resolve_folder_id(parent_path).await?;
        let part = drime_runner_part_size(total_size);
        let total_parts = drime_total_parts(total_size, part);

        let extension = filename.rsplit('.').next().unwrap_or("bin").to_string();
        let mime = mime_guess::from_ext(&extension)
            .first_or_octet_stream()
            .to_string();

        // Step 1: create multipart session.
        let mut create_json = serde_json::json!({
            "filename": filename,
            "mime": mime,
            "size": total_size,
            "extension": extension,
            "workspaceId": 0,
        });
        if !parent_id.is_empty() {
            create_json["parentId"] = serde_json::json!(parent_id.parse::<i64>().unwrap_or(0));
        }

        let resp = self
            .client
            .post(Self::api_url("/s3/multipart/create"))
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(create_json.to_string())
            .send()
            .await
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!("Multipart create failed: {}", e))
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Drime multipart create failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        let create_resp: DrimeMultipartCreateResponse = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(format!("Parse multipart create: {}", e)))?;
        let key = create_resp
            .key
            .ok_or_else(|| ProviderError::ServerError("Drime: missing key".into()))?;
        let upload_id = create_resp
            .upload_id
            .ok_or_else(|| ProviderError::ServerError("Drime: missing uploadId".into()))?;

        // Step 2: batch-sign every part URL up front so upload_part
        // doesn't have to round-trip per chunk.
        let part_numbers: Vec<u32> = (1..=total_parts).collect();
        let sign_body = serde_json::json!({
            "key": key,
            "uploadId": upload_id,
            "partNumbers": part_numbers,
        });
        let resp = self
            .client
            .post(Self::api_url("/s3/multipart/batch-sign-part-urls"))
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(sign_body.to_string())
            .send()
            .await
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!("Drime sign part URLs failed: {}", e))
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Drime sign part URLs failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }
        let sign_resp: DrimeSignPartUrlsResponse = resp
            .json()
            .await
            .map_err(|e| ProviderError::ParseError(format!("Parse sign URLs: {}", e)))?;
        let urls = sign_resp.urls.unwrap_or_default();
        if (urls.len() as u32) != total_parts {
            return Err(ProviderError::ServerError(format!(
                "Drime returned {} signed URLs but {} parts were requested",
                urls.len(),
                total_parts
            )));
        }
        let signed_urls: Vec<(u32, String)> =
            urls.into_iter().map(|u| (u.part_number, u.url)).collect();

        let meta = DrimeMultipartMeta {
            key,
            upload_id,
            parent_id,
            filename: filename.to_string(),
            mime,
            extension,
            total: total_size,
            part,
            total_parts,
            signed_urls,
        };
        Ok(MultipartHandle {
            upload_id: meta.encode(),
            remote_path: remote_path.to_string(),
        })
    }

    // DAG-P2-05: Drime's presigned S3 PUT is a single send with a known length
    // and no whole-part hashing, so stream the part body one bounded window at a
    // time instead of buffering the whole part in memory.
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
                "Drime upload_part requires 1-based part_number".to_string(),
            ));
        }
        let part_len = body.len();
        if part_len == 0 {
            return Err(ProviderError::Other(
                "Drime upload_part received empty data".to_string(),
            ));
        }
        let meta = DrimeMultipartMeta::decode(&handle.upload_id)?;
        if part_number > meta.total_parts {
            return Err(ProviderError::Other(format!(
                "Drime part {} exceeds declared total_parts {}",
                part_number, meta.total_parts
            )));
        }
        let url = meta
            .url_for(part_number)
            .ok_or_else(|| {
                ProviderError::Other(format!(
                    "Drime upload_part: no signed URL for part {}",
                    part_number
                ))
            })?
            .to_string();

        let resp = self
            .client
            .put(&url)
            // DAG-P2-05: explicit length so the streamed body is fixed-length,
            // never chunked (the presigned S3 PUT rejects plain chunked).
            .header("Content-Length", part_len.to_string())
            .body(body.into_reqwest_body())
            .send()
            .await
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!(
                    "Drime upload part {} failed: {}",
                    part_number, e
                ))
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::TransferFailed(format!(
                "Drime upload part {} failed ({}): {}",
                part_number,
                status,
                sanitize_api_error(&body)
            )));
        }

        // S3-compatible backend echoes the part ETag in the header; we
        // pass it through verbatim so complete_multipart_upload can fill
        // the `{PartNumber, ETag}` array.
        let etag = resp
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .unwrap_or_default();
        Ok(UploadedPart { part_number, etag })
    }

    async fn complete_multipart_upload(
        &mut self,
        handle: MultipartHandle,
        parts: Vec<UploadedPart>,
    ) -> Result<(), ProviderError> {
        if !self.connected {
            return Err(ProviderError::NotConnected);
        }
        let meta = DrimeMultipartMeta::decode(&handle.upload_id)?;
        if parts.len() != meta.total_parts as usize {
            return Err(ProviderError::TransferFailed(format!(
                "Drime complete: expected {} parts, runner committed {}",
                meta.total_parts,
                parts.len()
            )));
        }

        // S3 multipart complete contract: parts sorted by part_number with
        // their ETag.
        let mut sorted = parts;
        sorted.sort_by_key(|p| p.part_number);
        let parts_json: Vec<_> = sorted
            .iter()
            .map(|p| serde_json::json!({"PartNumber": p.part_number, "ETag": p.etag}))
            .collect();
        let complete_body = serde_json::json!({
            "key": meta.key,
            "uploadId": meta.upload_id,
            "parts": parts_json,
        });

        let resp = self
            .client
            .post(Self::api_url("/s3/multipart/complete"))
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(complete_body.to_string())
            .send()
            .await
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!("Drime multipart complete failed: {}", e))
            })?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Drime multipart complete failed ({}): {}",
                status,
                sanitize_api_error(&body)
            )));
        }

        // Step 5: register the file inside Drime's directory tree.
        // Without this the bytes sit on the S3 backend but never appear
        // in the UI.
        let s3_filename = meta
            .key
            .rsplit('/')
            .next()
            .unwrap_or(meta.key.as_str())
            .to_string();
        let mut entry_body = serde_json::json!({
            "filename": s3_filename,
            "size": meta.total,
            "clientName": meta.filename,
            "clientMime": meta.mime,
            "clientExtension": meta.extension,
            "workspaceId": 0,
        });
        if !meta.parent_id.is_empty() {
            entry_body["parentId"] = serde_json::json!(meta.parent_id.parse::<i64>().unwrap_or(0));
        }
        let resp = self
            .client
            .post(Self::api_url("/s3/entries"))
            .header(AUTHORIZATION, self.auth_header()?)
            .header(CONTENT_TYPE, "application/json")
            .body(entry_body.to_string())
            .send()
            .await
            .map_err(|e| {
                ProviderError::ConnectionFailed(format!("Drime entry registration failed: {}", e))
            })?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::ServerError(format!(
                "Drime entry registration failed ({}): {}",
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
        // Best-effort: Drime's S3 backend GCs orphaned multipart uploads
        // after their TTL; the abort endpoint is a courtesy hint.
        if let Ok(meta) = DrimeMultipartMeta::decode(&handle.upload_id) {
            let body = serde_json::json!({
                "key": meta.key,
                "uploadId": meta.upload_id,
            })
            .to_string();
            if let Ok(auth) = self.auth_header() {
                let _ = self
                    .client
                    .post(Self::api_url("/s3/multipart/abort"))
                    .header(AUTHORIZATION, auth)
                    .header(CONTENT_TYPE, "application/json")
                    .body(body)
                    .send()
                    .await;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_provider() -> DrimeCloudProvider {
        let config = DrimeCloudConfig {
            api_token: secrecy::SecretString::from("test-token".to_string()),
            initial_path: None,
        };
        DrimeCloudProvider::new(config)
    }

    /// A Drime double whose root holds the folders `src` (1) and `dst` (2)
    /// and the file `b.txt` (12); `src` holds `a.txt` (11), and `dst`
    /// holds `a.txt` (21) when `dst_holds_a`. A rename (`PUT
    /// /file-entries/{id}`) answers `rename_status`; a move succeeds. Returns
    /// a provider on it and every change as `METHOD path`.
    async fn provider_on_drime(
        dst_holds_a: bool,
        rename_status: u16,
    ) -> (
        DrimeCloudProvider,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        use axum::response::IntoResponse;
        use std::sync::{Arc, Mutex};
        let changes: Arc<Mutex<Vec<String>>> = Arc::default();
        let seen = Arc::clone(&changes);
        let app = axum::Router::new().fallback(axum::routing::any(
            move |req: axum::extract::Request| {
                let seen = Arc::clone(&seen);
                async move {
                    let method = req.method().as_str().to_string();
                    let path = req.uri().path().to_string();
                    let entry = |id: u64, name: &str, kind: &str| {
                        serde_json::json!({ "id": id, "name": name, "type": kind })
                    };
                    if method != "GET" {
                        seen.lock().unwrap().push(format!("{method} {path}"));
                        if method == "PUT" && rename_status != 200 {
                            return (
                                axum::http::StatusCode::from_u16(rename_status).unwrap(),
                                r#"{"message":"An item with this name already exists."}"#,
                            )
                                .into_response();
                        }
                        return axum::Json(serde_json::json!({ "status": "success" }))
                            .into_response();
                    }
                    let parent = req
                        .uri()
                        .query()
                        .unwrap_or("")
                        .split('&')
                        .find_map(|pair| pair.strip_prefix("parentIds="))
                        .unwrap_or("")
                        .to_string();
                    let data = match parent.as_str() {
                        "" => vec![entry(1, "src", "folder"), entry(2, "dst", "folder"), entry(12, "b.txt", "file")],
                        "1" => vec![entry(11, "a.txt", "file")],
                        "2" if dst_holds_a => vec![entry(21, "a.txt", "file")],
                        _ => vec![],
                    };
                    axum::Json(serde_json::json!({ "data": data, "last_page": 1 })).into_response()
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        TEST_API_BASE.with(|base| *base.borrow_mut() = Some(format!("http://{addr}")));
        let mut provider = test_provider();
        provider.connected = true;
        (provider, changes)
    }

    /// A move onto a taken name moved the source first and was refused only
    /// by the rename after it, leaving the source moved. It is now refused
    /// before any change.
    #[tokio::test]
    async fn a_rename_onto_a_taken_name_is_refused_before_any_change() {
        let (mut provider, changes) = provider_on_drime(false, 200).await;
        let outcome = provider.rename("/src/a.txt", "/b.txt").await;
        assert!(
            matches!(outcome, Err(ProviderError::AlreadyExists(_))),
            "{outcome:?}"
        );
        assert!(
            changes.lock().unwrap().is_empty(),
            "{:?}",
            changes.lock().unwrap()
        );
    }

    /// The move keeps the old name: with `/dst/a.txt` present it put a
    /// second `a.txt` in `/dst` until the rename. The rename goes first.
    #[tokio::test]
    async fn a_move_whose_destination_holds_the_old_name_renames_first() {
        let (mut provider, changes) = provider_on_drime(true, 200).await;
        provider
            .rename("/src/a.txt", "/dst/c.txt")
            .await
            .expect("rename then move");
        assert_eq!(
            *changes.lock().unwrap(),
            ["PUT /file-entries/11", "POST /file-entries/move"]
        );
    }

    /// A name taken between the look and the rename is refused by Drime with
    /// 400 (live, CLI exit 10); that is AlreadyExists (exit 9). The status is
    /// live; the wording is the one Drime gives a folder name already taken.
    #[tokio::test]
    async fn a_rename_drime_refuses_for_a_taken_name_is_already_exists() {
        let (mut provider, _) = provider_on_drime(false, 400).await;
        let outcome = provider.rename("/src/a.txt", "/src/c.txt").await;
        assert!(
            matches!(outcome, Err(ProviderError::AlreadyExists(_))),
            "{outcome:?}"
        );
    }

    /// A Drime double that keeps `entries` (id, name, parent id, kind; the
    /// root is the parent `""`) in memory: listings by `parentIds`, a rename
    /// (`PUT /file-entries/{id}`, 400 onto a name taken in its folder; to a
    /// name starting with `fail` 500, and with `failrace` another `a.txt`
    /// appears in folder 1; to one starting with `ghost` 400 as a name taken
    /// since the look), a move (403 into a folder named `nomove...`), a
    /// delete (500 for an entry whose name starts with `fail`), a small
    /// upload (`POST /uploads`) and the creation or removal of a share link.
    /// Returns a provider on it, the entries, and every change as `rename ID
    /// NAME`, `move ID PARENT`, `delete ID`, `upload NAME PARENT`, `share ID`
    /// or `unshare ID`.
    #[allow(clippy::type_complexity)]
    async fn provider_on_drime_entries(
        entries: &[(u64, &str, &str, &str)],
    ) -> (
        DrimeCloudProvider,
        std::sync::Arc<std::sync::Mutex<Vec<(u64, String, String, String)>>>,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        use axum::response::IntoResponse;
        use std::sync::{Arc, Mutex};
        let store: Arc<Mutex<Vec<(u64, String, String, String)>>> = Arc::new(Mutex::new(
            entries
                .iter()
                .map(|(id, name, parent, kind)| {
                    (*id, name.to_string(), parent.to_string(), kind.to_string())
                })
                .collect(),
        ));
        let changes: Arc<Mutex<Vec<String>>> = Arc::default();
        let (items, seen) = (Arc::clone(&store), Arc::clone(&changes));
        let app =
            axum::Router::new().fallback(axum::routing::any(move |req: axum::extract::Request| {
                let (items, seen) = (Arc::clone(&items), Arc::clone(&seen));
                async move {
                    let method = req.method().as_str().to_string();
                    let path = req.uri().path().to_string();
                    let query = req.uri().query().unwrap_or("").to_string();
                    let body = axum::body::to_bytes(req.into_body(), 1 << 16)
                        .await
                        .unwrap();
                    let args: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
                    let mut items = items.lock().unwrap();
                    let ok =
                        || axum::Json(serde_json::json!({ "status": "success" })).into_response();
                    if method == "GET" {
                        let parent = query
                            .split('&')
                            .find_map(|pair| pair.strip_prefix("parentIds="))
                            .unwrap_or("");
                        let data: Vec<serde_json::Value> = items
                            .iter()
                            .filter(|(_, _, p, _)| p == parent)
                            .map(|(id, name, _, kind)| {
                                serde_json::json!({ "id": id, "name": name, "type": kind })
                            })
                            .collect();
                        return axum::Json(serde_json::json!({ "data": data, "last_page": 1 }))
                            .into_response();
                    }
                    match (method.as_str(), path.as_str()) {
                        ("PUT", p) if p.starts_with("/file-entries/") => {
                            let id: u64 = p.trim_start_matches("/file-entries/").parse().unwrap();
                            let name = args["name"].as_str().unwrap_or("").to_string();
                            if name.starts_with("failrace") {
                                // Refused, and meanwhile another `a.txt` took
                                // the way back into folder 1.
                                items.push((
                                    99,
                                    "a.txt".to_string(),
                                    "1".to_string(),
                                    "file".to_string(),
                                ));
                                return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "{}")
                                    .into_response();
                            }
                            if name.starts_with("fail") {
                                return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "{}")
                                    .into_response();
                            }
                            if name.starts_with("ghost") {
                                // Taken since the look, by an item not listed yet.
                                return (
                                    axum::http::StatusCode::BAD_REQUEST,
                                    r#"{"message":"An item with this name already exists."}"#,
                                )
                                    .into_response();
                            }
                            let parent = items.iter().find(|e| e.0 == id).unwrap().2.clone();
                            if items.iter().any(|e| e.2 == parent && e.1 == name) {
                                return (
                                    axum::http::StatusCode::BAD_REQUEST,
                                    r#"{"message":"An item with this name already exists."}"#,
                                )
                                    .into_response();
                            }
                            items.iter_mut().find(|e| e.0 == id).unwrap().1 = name.clone();
                            seen.lock().unwrap().push(format!("rename {id} {name}"));
                            ok()
                        }
                        ("POST", "/file-entries/move") => {
                            let parent = match &args["destinationId"] {
                                serde_json::Value::Number(n) => n.to_string(),
                                _ => String::new(),
                            };
                            let refuses = items
                                .iter()
                                .any(|e| e.0.to_string() == parent && e.1.starts_with("nomove"));
                            if refuses {
                                return (axum::http::StatusCode::FORBIDDEN, "{}").into_response();
                            }
                            for id in args["entryIds"].as_array().unwrap() {
                                let id = id.as_u64().unwrap();
                                items.iter_mut().find(|e| e.0 == id).unwrap().2 = parent.clone();
                                seen.lock().unwrap().push(format!("move {id} {parent}"));
                            }
                            ok()
                        }
                        ("POST", "/file-entries/delete") => {
                            for id in args["entryIds"].as_array().unwrap() {
                                let id = id.as_u64().unwrap();
                                let refused =
                                    items.iter().any(|e| e.0 == id && e.1.starts_with("fail"));
                                if refused {
                                    return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "{}")
                                        .into_response();
                                }
                                items.retain(|e| e.0 != id);
                                seen.lock().unwrap().push(format!("delete {id}"));
                            }
                            ok()
                        }
                        (method @ ("POST" | "DELETE"), p) if p.ends_with("/shareable-link") => {
                            let id = p
                                .trim_start_matches("/file-entries/")
                                .trim_end_matches("/shareable-link");
                            let what = if method == "POST" { "share" } else { "unshare" };
                            seen.lock().unwrap().push(format!("{what} {id}"));
                            axum::Json(serde_json::json!({ "link": { "hash": "H" } }))
                                .into_response()
                        }
                        ("POST", "/uploads") => {
                            // The form's `parentId` part and the file part's name.
                            let form = String::from_utf8_lossy(&body);
                            let part = |after: &str, until: &str| {
                                form.split(after)
                                    .nth(1)
                                    .and_then(|rest| rest.split(until).next())
                                    .unwrap_or("")
                                    .to_string()
                            };
                            let name = part("filename=\"", "\"");
                            let parent = part("name=\"parentId\"\r\n\r\n", "\r\n");
                            let id = items.iter().map(|e| e.0).max().unwrap_or(0) + 1;
                            seen.lock().unwrap().push(format!("upload {name} {parent}"));
                            items.push((id, name, parent, "file".to_string()));
                            ok()
                        }
                        _ => (axum::http::StatusCode::BAD_REQUEST, "unexpected").into_response(),
                    }
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        TEST_API_BASE.with(|base| *base.borrow_mut() = Some(format!("http://{addr}")));
        let mut provider = test_provider();
        provider.connected = true;
        (provider, store, changes)
    }

    /// Drime's rename and move never overwrite, so the replace behind CLI
    /// `edit` and the served WebDAV MOVE with `Overwrite: T` was the rename,
    /// refused onto the file it is meant to replace (400, live on
    /// 2026-09-26). It now renames the old file aside, renames the new one
    /// in and deletes the one set aside; no name is ever taken twice, so the
    /// 400 Drime gives a taken name is never met.
    #[tokio::test]
    async fn a_replace_sets_the_old_file_aside_renames_the_new_one_in_then_deletes_it() {
        let (mut provider, store, changes) = provider_on_drime_entries(&[
            (1, "d", "", "folder"),
            (11, "a.txt", "1", "file"),
            (12, "b.txt", "1", "file"),
        ])
        .await;
        provider
            .replace("/d/a.txt", "/d/b.txt")
            .await
            .expect("replace");
        let changes = changes.lock().unwrap().clone();
        assert_eq!(changes.len(), 3, "{changes:?}");
        assert!(
            changes[0].starts_with("rename 12 .b.txt.aeroftp-replaced-"),
            "{changes:?}"
        );
        assert_eq!(changes[1], "rename 11 b.txt");
        assert_eq!(changes[2], "delete 12");
        let names: Vec<String> = store.lock().unwrap().iter().map(|e| e.1.clone()).collect();
        assert_eq!(names, ["d", "b.txt"]);
    }

    /// A move to another folder under a new name is two steps. When the
    /// rename after the move failed, the item stayed in the new folder under
    /// its old name while the error said nothing of it. The move is undone.
    #[tokio::test]
    async fn a_move_whose_rename_fails_is_moved_back() {
        let (mut provider, store, changes) = provider_on_drime_entries(&[
            (1, "d", "", "folder"),
            (2, "e", "", "folder"),
            (11, "a.txt", "1", "file"),
        ])
        .await;
        let outcome = provider.rename("/d/a.txt", "/e/fail.txt").await;
        assert!(outcome.is_err(), "{outcome:?}");
        assert_eq!(*changes.lock().unwrap(), ["move 11 2", "move 11 1"]);
        let a = store
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.0 == 11)
            .cloned()
            .unwrap();
        assert_eq!((a.1.as_str(), a.2.as_str()), ("a.txt", "1"));
    }

    /// The folders `d` (1) and `e` (2), `nomove` (4, which refuses moves in)
    /// holding its own `a.txt` (44), and `a.txt` (11) in `d`.
    async fn provider_for_two_step_renames() -> (
        DrimeCloudProvider,
        std::sync::Arc<std::sync::Mutex<Vec<(u64, String, String, String)>>>,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        provider_on_drime_entries(&[
            (1, "d", "", "folder"),
            (2, "e", "", "folder"),
            (4, "nomove", "", "folder"),
            (11, "a.txt", "1", "file"),
            (44, "a.txt", "4", "file"),
        ])
        .await
    }

    /// When the name the undo would take back was taken meanwhile, undoing
    /// would put a second `a.txt` in `d`: the first step stays, and the error
    /// says where the item is, never AlreadyExists, since something changed.
    #[tokio::test]
    async fn an_undo_whose_way_back_is_taken_is_not_made_and_says_where_the_item_is() {
        let (mut provider, _, changes) = provider_for_two_step_renames().await;
        let outcome = provider.rename("/d/a.txt", "/e/failrace.txt").await;
        match outcome {
            Err(ProviderError::Other(message)) => {
                assert!(message.contains("now at /e/a.txt"), "{message}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(*changes.lock().unwrap(), ["move 11 2"], "no move back");
    }

    /// The way back is `/d/a.txt`: an `A.txt` beside it is another name,
    /// and the undo goes on. A first match that ignored the case took it
    /// for the holder and left the item moved.
    #[tokio::test]
    async fn an_item_of_another_letter_case_does_not_block_the_undo() {
        let (mut provider, store, changes) = provider_on_drime_entries(&[
            (1, "d", "", "folder"),
            (2, "e", "", "folder"),
            (11, "a.txt", "1", "file"),
            (10, "A.txt", "1", "file"),
        ])
        .await;
        let outcome = provider.rename("/d/a.txt", "/e/fail.txt").await;
        assert!(outcome.is_err(), "{outcome:?}");
        assert_eq!(*changes.lock().unwrap(), ["move 11 2", "move 11 1"]);
        let a = store
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.0 == 11)
            .cloned()
            .unwrap();
        assert_eq!((a.1.as_str(), a.2.as_str()), ("a.txt", "1"));
    }

    /// With sibling folders `D` (listed first) and `d`, the folder lookup
    /// took the first match ignoring the case: `/d/x.txt` looked into `D`
    /// and was not found, while `ls /d` shows it. The name as spelled comes
    /// first.
    #[tokio::test]
    async fn a_folder_resolves_to_the_name_as_spelled_first() {
        let (mut provider, _, _) = provider_on_drime_entries(&[
            (3, "D", "", "folder"),
            (4, "d", "", "folder"),
            (12, "x.txt", "4", "file"),
        ])
        .await;
        let found = provider.stat("/d/x.txt").await.expect("stat");
        assert_eq!(found.name, "x.txt");
    }

    /// With `A.txt` listed before `a.txt`, a path lookup that took the first
    /// match ignoring the case resolved `/d/a.txt` to `A.txt`, and the move
    /// moved the other file. The name as spelled comes first.
    #[tokio::test]
    async fn a_path_resolves_to_the_name_as_spelled_first() {
        let (mut provider, _, changes) = provider_on_drime_entries(&[
            (1, "d", "", "folder"),
            (2, "e", "", "folder"),
            (10, "A.txt", "1", "file"),
            (11, "a.txt", "1", "file"),
        ])
        .await;
        provider
            .rename("/d/a.txt", "/e/a.txt")
            .await
            .expect("a free name");
        assert_eq!(*changes.lock().unwrap(), ["move 11 2"]);
    }

    /// A second step refused for a name taken since the look, with the first
    /// step undone, is AlreadyExists as it came: nothing changed.
    #[tokio::test]
    async fn a_taken_name_after_an_undone_first_step_is_already_exists() {
        let (mut provider, store, changes) = provider_for_two_step_renames().await;
        let outcome = provider.rename("/d/a.txt", "/e/ghost.txt").await;
        assert!(
            matches!(outcome, Err(ProviderError::AlreadyExists(_))),
            "{outcome:?}"
        );
        assert_eq!(*changes.lock().unwrap(), ["move 11 2", "move 11 1"]);
        let a = store
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.0 == 11)
            .cloned()
            .unwrap();
        assert_eq!((a.1.as_str(), a.2.as_str()), ("a.txt", "1"));
    }

    /// Renamed first (the destination folder holds the old name), then the
    /// move refused: the rename is undone.
    #[tokio::test]
    async fn a_rename_first_whose_move_fails_is_renamed_back() {
        let (mut provider, store, changes) = provider_for_two_step_renames().await;
        let outcome = provider.rename("/d/a.txt", "/nomove/c.txt").await;
        assert!(outcome.is_err(), "{outcome:?}");
        assert_eq!(
            *changes.lock().unwrap(),
            ["rename 11 c.txt", "rename 11 a.txt"]
        );
        let a = store
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.0 == 11)
            .cloned()
            .unwrap();
        assert_eq!((a.1.as_str(), a.2.as_str()), ("a.txt", "1"));
    }

    /// After every outcome the ids cached for both paths and everything
    /// under them are forgotten; a sibling sharing the prefix stays.
    #[tokio::test]
    async fn a_failed_rename_forgets_the_ids_cached_under_both_paths() {
        let (mut provider, _, _) = provider_for_two_step_renames().await;
        for path in ["/d/a.txt/x", "/e/fail.txt/x", "/d/a.txtx"] {
            provider.dir_cache_insert(
                path.to_string(),
                DirInfo {
                    id: "Z".to_string(),
                },
            );
        }
        let outcome = provider.rename("/d/a.txt", "/e/fail.txt").await;
        assert!(outcome.is_err(), "{outcome:?}");
        assert!(!provider.dir_cache.contains_key("/d/a.txt/x"));
        assert!(!provider.dir_cache.contains_key("/e/fail.txt/x"));
        assert!(provider.dir_cache.contains_key("/d/a.txtx"));
    }

    /// A folder lookup falls back to another letter case and caches the
    /// spelling it was given, so `/D` can hold the id of `d`. A rename or
    /// delete forgot only its own spelling: `/D/sub` kept the id of the
    /// renamed folder and `/GONE/deep` that of the trashed one. Every
    /// capitalization goes; a sibling sharing the prefix stays.
    #[tokio::test]
    async fn a_rename_or_delete_forgets_the_ids_cached_under_any_case() {
        let (mut provider, _, _) =
            provider_on_drime_entries(&[(1, "d", "", "folder"), (3, "gone", "", "folder")]).await;
        for path in ["/D", "/D/sub", "/GONE/deep", "/Dx"] {
            provider.dir_cache_insert(
                path.to_string(),
                DirInfo {
                    id: "Z".to_string(),
                },
            );
        }
        provider.rename("/d", "/moved").await.expect("rename");
        provider.delete("/gone").await.expect("delete");
        let mut cached: Vec<&str> = provider.dir_cache.keys().map(String::as_str).collect();
        cached.sort();
        assert_eq!(cached, ["/Dx"], "a sibling sharing the prefix stays");
    }

    /// A small local file to upload.
    fn local_file() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"new").unwrap();
        file
    }

    /// Drime keeps `A.txt` and `a.txt` side by side, and the lookup falls
    /// back to another case: an upload of `a.txt` beside only `A.txt`
    /// deleted `A.txt` before it uploaded. Only a file of the exact name is
    /// deleted first.
    #[tokio::test]
    async fn an_upload_does_not_delete_a_file_of_another_case() {
        let (mut provider, store, changes) =
            provider_on_drime_entries(&[(1, "d", "", "folder"), (10, "A.txt", "1", "file")]).await;
        let file = local_file();
        provider
            .upload(file.path().to_str().unwrap(), "/d/a.txt", None)
            .await
            .expect("upload");
        assert_eq!(*changes.lock().unwrap(), ["upload a.txt 1"]);
        let names: Vec<String> = store.lock().unwrap().iter().map(|e| e.1.clone()).collect();
        assert_eq!(names, ["d", "A.txt", "a.txt"]);
    }

    /// `rm /d/a.txt` beside only `A.txt` trashed `A.txt`. A delete takes
    /// the exact name only.
    #[tokio::test]
    async fn a_delete_does_not_take_a_file_of_another_case() {
        let (mut provider, _, changes) =
            provider_on_drime_entries(&[(1, "d", "", "folder"), (10, "A.txt", "1", "file")]).await;
        let outcome = provider.delete("/d/a.txt").await;
        assert!(
            matches!(outcome, Err(ProviderError::NotFound(_))),
            "{outcome:?}"
        );
        assert!(changes.lock().unwrap().is_empty());
    }

    /// A replace onto `a.txt` beside only `A.txt` took `A.txt` for the file
    /// to replace: it was set aside and deleted. Only an item of the exact
    /// name is replaced; here the replace is the rename, whose look before
    /// the move refuses the name taken in another case, and nothing changes.
    #[tokio::test]
    async fn a_replace_does_not_delete_a_file_of_another_case() {
        let (mut provider, _, changes) = provider_on_drime_entries(&[
            (1, "d", "", "folder"),
            (10, "A.txt", "1", "file"),
            (11, "x.txt", "1", "file"),
        ])
        .await;
        let outcome = provider.replace("/d/x.txt", "/d/a.txt").await;
        assert!(
            matches!(outcome, Err(ProviderError::AlreadyExists(_))),
            "{outcome:?}"
        );
        assert!(
            changes.lock().unwrap().is_empty(),
            "{:?}",
            changes.lock().unwrap()
        );
    }

    /// An upload deletes the file it replaces first, and took a folder of
    /// that name too: `put f /d/x` with `x` a folder sent the folder to the
    /// trash with everything in it. Only a file is replaced; a folder there
    /// is refused before any change.
    #[tokio::test]
    async fn an_upload_onto_a_folder_is_refused_before_any_change() {
        let (mut provider, _, changes) = provider_on_drime_entries(&[
            (1, "d", "", "folder"),
            (5, "x", "1", "folder"),
            (6, "inner.txt", "5", "file"),
        ])
        .await;
        let file = local_file();
        let outcome = provider
            .upload(file.path().to_str().unwrap(), "/d/x", None)
            .await;
        assert!(
            matches!(outcome, Err(ProviderError::InvalidPath(_))),
            "{outcome:?}"
        );
        assert!(
            changes.lock().unwrap().is_empty(),
            "{:?}",
            changes.lock().unwrap()
        );
    }

    /// A share link asked for `a.txt` beside only `A.txt` found `A.txt`
    /// ignoring the case: creating one published `A.txt`, and removing one
    /// removed `A.txt`'s link, whose URL cannot be made again. Both take the
    /// exact name only.
    #[tokio::test]
    async fn share_links_do_not_take_a_file_of_another_case() {
        let (mut provider, _, changes) =
            provider_on_drime_entries(&[(1, "d", "", "folder"), (10, "A.txt", "1", "file")]).await;
        let created = provider
            .create_share_link("/d/a.txt", ShareLinkOptions::default())
            .await;
        assert!(
            matches!(created, Err(ProviderError::NotFound(_))),
            "{created:?}"
        );
        let removed = provider.remove_share_link("/d/a.txt").await;
        assert!(
            matches!(removed, Err(ProviderError::NotFound(_))),
            "{removed:?}"
        );
        assert!(
            changes.lock().unwrap().is_empty(),
            "{:?}",
            changes.lock().unwrap()
        );
    }

    /// A delete forgot the cached ids only after a success, so a delete
    /// whose answer was an error (Drime may still have applied it) left
    /// `/faildir/sub` pointing at what may be in the trash. They are
    /// forgotten whatever the answer.
    #[tokio::test]
    async fn a_failed_delete_forgets_the_ids_cached_under_it() {
        let (mut provider, _, _) = provider_on_drime_entries(&[(2, "faildir", "", "folder")]).await;
        provider.dir_cache_insert(
            "/faildir/sub".to_string(),
            DirInfo {
                id: "Z".to_string(),
            },
        );
        let outcome = provider.delete("/faildir").await;
        assert!(outcome.is_err(), "{outcome:?}");
        assert!(!provider.dir_cache.contains_key("/faildir/sub"));
    }

    /// `cd /docs` beside only `Docs` resolved to `Docs` through the case
    /// fallback and cached it as `/docs` (a listing there cached its
    /// subfolders too). After another client made a real `docs`, `rm
    /// /docs/x.txt` took the parent from the cache and deleted `Docs/x.txt`.
    /// A fallback caches nothing now.
    #[tokio::test]
    async fn a_delete_after_a_fallback_cd_takes_the_folder_named() {
        let (mut provider, store, changes) = provider_on_drime_entries(&[
            (3, "Docs", "", "folder"),
            (5, "sub", "3", "folder"),
            (31, "x.txt", "3", "file"),
        ])
        .await;
        provider.cd("/docs").await.expect("cd through the fallback");
        provider
            .list("/docs")
            .await
            .expect("ls through the fallback");
        let spelled: Vec<&String> = provider
            .dir_cache
            .keys()
            .filter(|k| k.starts_with("/docs"))
            .collect();
        assert!(
            spelled.is_empty(),
            "cached through the fallback: {spelled:?}"
        );
        store.lock().unwrap().extend([
            (4, "docs".to_string(), String::new(), "folder".to_string()),
            (41, "x.txt".to_string(), "4".to_string(), "file".to_string()),
        ]);
        provider.delete("/docs/x.txt").await.expect("rm");
        assert_eq!(*changes.lock().unwrap(), ["delete 41"]);
    }

    /// With only `Docs` there, `rm /docs/x.txt` and `put /docs/x.txt`
    /// resolved `/docs` to `Docs` through the fallback: the delete, and the
    /// delete before the upload, deleted `Docs/x.txt`. A step that deletes
    /// resolves every folder on the path exactly.
    #[tokio::test]
    async fn a_delete_under_a_folder_of_another_case_is_refused() {
        let (mut provider, _, changes) =
            provider_on_drime_entries(&[(3, "Docs", "", "folder"), (31, "x.txt", "3", "file")])
                .await;
        let outcome = provider.delete("/docs/x.txt").await;
        assert!(
            matches!(outcome, Err(ProviderError::NotFound(_))),
            "{outcome:?}"
        );
        let file = local_file();
        let outcome = provider
            .upload(file.path().to_str().unwrap(), "/docs/x.txt", None)
            .await;
        assert!(
            matches!(outcome, Err(ProviderError::NotFound(_))),
            "{outcome:?}"
        );
        assert!(
            changes.lock().unwrap().is_empty(),
            "{:?}",
            changes.lock().unwrap()
        );
    }

    /// With `A.txt` listed before `a.txt`, `stat /d/a.txt` took the first
    /// match ignoring the case and answered `A.txt`. The name as spelled
    /// comes first.
    #[tokio::test]
    async fn stat_answers_the_name_as_spelled_first() {
        let (mut provider, _, _) = provider_on_drime_entries(&[
            (1, "d", "", "folder"),
            (10, "A.txt", "1", "file"),
            (11, "a.txt", "1", "file"),
        ])
        .await;
        let found = provider.stat("/d/a.txt").await.expect("stat");
        assert_eq!(found.name, "a.txt");
    }

    /// The replace sets the old item aside, so the name is empty for a
    /// moment: no atomic replace is claimed, and the callers that need one
    /// refuse before they write.
    #[tokio::test]
    async fn drime_does_not_claim_an_atomic_replace() {
        let mut provider = test_provider();
        assert!(!provider.supports_atomic_replace().await.unwrap());
        assert!(provider.replace_sets_aside());
    }

    #[test]
    fn api_url_prefixes_api_base() {
        assert_eq!(
            DrimeCloudProvider::api_url("/files"),
            format!("{}/files", API_BASE)
        );
        assert_eq!(DrimeCloudProvider::api_url(""), API_BASE.to_string());
    }

    #[test]
    fn normalize_path_preserves_root_and_trims_trailing_slash() {
        assert_eq!(DrimeCloudProvider::normalize_path(""), "/");
        assert_eq!(DrimeCloudProvider::normalize_path("/"), "/");
        assert_eq!(DrimeCloudProvider::normalize_path("   "), "/");
        assert_eq!(DrimeCloudProvider::normalize_path("foo"), "/foo");
        assert_eq!(DrimeCloudProvider::normalize_path("/foo"), "/foo");
        assert_eq!(DrimeCloudProvider::normalize_path("/foo/"), "/foo");
        assert_eq!(DrimeCloudProvider::normalize_path(r"foo\bar"), "/foo/bar");
    }

    #[test]
    fn split_path_handles_root_nested_and_missing_separator() {
        assert_eq!(DrimeCloudProvider::split_path("/foo"), ("/", "foo"));
        assert_eq!(DrimeCloudProvider::split_path("/a/b"), ("/a", "b"));
        assert_eq!(DrimeCloudProvider::split_path("/a/b/c"), ("/a/b", "c"));
        assert_eq!(DrimeCloudProvider::split_path("bare"), ("/", "bare"));
        assert_eq!(DrimeCloudProvider::split_path("/foo/"), ("/", "foo"));
    }

    #[test]
    fn resolve_path_is_relative_to_current_path() {
        let mut p = test_provider();
        p.current_path = "/base".to_string();
        // absolute path bypasses current_path
        assert_eq!(p.resolve_path("/abs"), "/abs");
        // "." and empty map to current_path
        assert_eq!(p.resolve_path("."), "/base");
        assert_eq!(p.resolve_path(""), "/base");
        // relative path is joined against current_path
        assert_eq!(p.resolve_path("child"), "/base/child");
        assert_eq!(p.resolve_path("sub/leaf"), "/base/sub/leaf");

        // when current_path is root, no double slash
        let mut p2 = test_provider();
        p2.current_path = "/".to_string();
        assert_eq!(p2.resolve_path("child"), "/child");
    }

    #[test]
    fn parse_date_accepts_rfc3339_and_falls_back_to_truncation() {
        let iso = DrimeCloudProvider::parse_date("2025-01-15T10:30:00Z").unwrap();
        assert!(iso.starts_with("2025-01-15"));
        let with_frac = DrimeCloudProvider::parse_date("2025-01-15T10:30:00.000000Z").unwrap();
        assert!(with_frac.starts_with("2025-01-15"));
        // unparseable but date-like string gets safely truncated
        let fallback = DrimeCloudProvider::parse_date("2025-01-15 10:30:00 server time").unwrap();
        assert_eq!(fallback.len(), 19);
        assert_eq!(DrimeCloudProvider::parse_date("short"), None);
    }

    /// Addendum to the 4.2.1 closeout: an offset was dropped instead of
    /// applied (local time of the offset with a literal `Z`), so a file time
    /// sent with one was off by that offset. It is converted to UTC first.
    #[test]
    fn parse_date_converts_an_offset_to_utc() {
        assert_eq!(
            DrimeCloudProvider::parse_date("2025-01-15T10:30:00+02:00").as_deref(),
            Some("2025-01-15 08:30:00Z")
        );
        assert_eq!(
            DrimeCloudProvider::parse_date("2025-01-15T22:30:00.000000-05:00").as_deref(),
            Some("2025-01-16 03:30:00Z"),
            "the date moves with the hour"
        );
        assert_eq!(
            DrimeCloudProvider::parse_date("2025-01-15 10:30:00+01:00").as_deref(),
            Some("2025-01-15 09:30:00Z"),
            "the branch with a space instead of T too"
        );
        assert_eq!(
            DrimeCloudProvider::parse_date("2025-01-15T10:30:00.000000Z").as_deref(),
            Some("2025-01-15 10:30:00Z"),
            "a UTC time is unchanged"
        );
    }

    // ---- S3-T12 multipart trait wiring ----

    #[test]
    fn drime_multipart_meta_roundtrip_preserves_fields() {
        let meta = DrimeMultipartMeta {
            key: "users/123/abc.bin".to_string(),
            upload_id: "drime-upload-id-xyz".to_string(),
            parent_id: "555".to_string(),
            filename: "weird name (1).bin".to_string(),
            mime: "application/octet-stream".to_string(),
            extension: "bin".to_string(),
            total: 1_073_741_824,
            part: 5 * 1024 * 1024,
            total_parts: 205,
            signed_urls: vec![
                (1, "https://drime-s3/u/1".to_string()),
                (2, "https://drime-s3/u/2".to_string()),
            ],
        };
        let encoded = meta.encode();
        let decoded = DrimeMultipartMeta::decode(&encoded).expect("decode roundtrip");
        assert_eq!(meta, decoded);
        assert_eq!(decoded.url_for(2), Some("https://drime-s3/u/2"));
        assert_eq!(decoded.url_for(99), None);
    }

    #[test]
    fn drime_multipart_meta_decode_rejects_garbage() {
        let err = DrimeMultipartMeta::decode("not-json").unwrap_err();
        assert!(matches!(err, ProviderError::Other(_)));
    }

    #[test]
    fn drime_runner_part_size_clamps_and_never_returns_zero() {
        assert_eq!(drime_runner_part_size(1024), 1024);
        assert_eq!(
            drime_runner_part_size(DRIME_MULTIPART_PART_SIZE),
            DRIME_MULTIPART_PART_SIZE
        );
        assert_eq!(
            drime_runner_part_size(50 * 1024 * 1024 * 1024),
            DRIME_MULTIPART_PART_SIZE
        );
        assert_eq!(drime_runner_part_size(0), 1);
    }

    #[test]
    fn drime_total_parts_matches_runner_formula() {
        let p = DRIME_MULTIPART_PART_SIZE;
        assert_eq!(drime_total_parts(0, p), 1);
        assert_eq!(drime_total_parts(p, p), 1);
        assert_eq!(drime_total_parts(4 * p, p), 4);
        assert_eq!(drime_total_parts(p + 1, p), 2);
        // part=0 guard treats it as 1 to avoid divide-by-zero
        assert_eq!(drime_total_parts(7, 0), 7);
    }

    #[test]
    fn drime_status_is_retryable_covers_5xx_and_transient_4xx_only() {
        // Whole 5xx range is retryable.
        for code in [500u16, 502, 503, 504, 599] {
            assert!(drime_status_is_retryable(code), "5xx {code} must retry");
        }
        // Transient 4xx allow-list.
        for code in [404u16, 408, 425, 429] {
            assert!(
                drime_status_is_retryable(code),
                "transient {code} must retry"
            );
        }
        // Hard failures and success never retry.
        for code in [200u16, 204, 400, 401, 403, 409, 410, 422, 451] {
            assert!(!drime_status_is_retryable(code), "{code} must not retry");
        }
    }

    #[test]
    fn drime_transfer_hints_advertise_multipart_with_etag() {
        let p = test_provider();
        let hints = p.transfer_optimization_hints();
        assert!(hints.supports_multipart);
        assert_eq!(hints.multipart_threshold, DRIME_MULTIPART_THRESHOLD);
        assert_eq!(hints.multipart_part_size, DRIME_MULTIPART_PART_SIZE);
        assert_eq!(hints.multipart_max_parallel, 4);
        assert!(hints.supports_resume_download);
        assert!(hints.supports_resume_upload);
        assert!(hints.supports_server_checksum);
        assert_eq!(hints.preferred_checksum_algo.as_deref(), Some("etag"));
    }

    // ---- DAG-P1-05A: HttpClonePool worker promotion ----

    #[test]
    fn clone_for_transfer_requires_connection() {
        let p = test_provider();
        assert!(!p.is_connected());
        assert!(matches!(
            p.clone_for_transfer(),
            Err(ProviderError::NotConnected)
        ));
    }

    #[test]
    fn connected_clone_succeeds_and_reports_same_provider_type() {
        let p = DrimeCloudProvider::connected_for_test(DrimeCloudConfig {
            api_token: secrecy::SecretString::from("test-token".to_string()),
            initial_path: None,
        });
        let worker = p.clone_for_transfer().expect("connected clone");
        assert_eq!(worker.provider_type(), ProviderType::DrimeCloud);
        assert!(worker.is_connected());
        assert_eq!(
            p.transfer_executor_kind(),
            ProviderTransferExecutorKind::HttpClonePool
        );
        assert_eq!(p.transfer_executor_max_sessions(), 4);
    }

    #[test]
    fn clone_and_primary_are_distinct_objects_with_independent_cache() {
        let mut p = DrimeCloudProvider::connected_for_test(DrimeCloudConfig {
            api_token: secrecy::SecretString::from("test-token".to_string()),
            initial_path: None,
        });
        p.current_path = "/docs".to_string();
        p.current_folder_id = "42".to_string();
        p.user_id = Some(7);
        p.dir_cache_insert(
            "/docs".to_string(),
            DirInfo {
                id: "42".to_string(),
            },
        );

        let worker = p.clone_for_transfer().expect("clone");
        // Distinct boxed objects (not the same allocation as primary).
        let primary_ptr = &p as *const _ as usize;
        let worker_any = worker.as_ref() as *const dyn StorageProvider as *const () as usize;
        assert_ne!(primary_ptr, worker_any);

        // Mutating the primary cache must not affect the worker's copy.
        p.dir_cache.clear();
        p.current_path = "/other".to_string();
        // Worker remains connected and same type; upload_part only needs
        // connected + client + handle (no shared mutable cache).
        assert!(worker.is_connected());
        assert_eq!(worker.provider_type(), ProviderType::DrimeCloud);
        assert!(p.dir_cache.is_empty());
    }

    #[test]
    fn runtime_composition_yields_http_clone_pool_when_connected() {
        use crate::provider_transfer_executor::{
            compose_runtime_transfer_capabilities, resolve_session_model,
        };
        use crate::transfer_dag::Capability;

        let p = DrimeCloudProvider::connected_for_test(DrimeCloudConfig {
            api_token: secrecy::SecretString::from("test-token".to_string()),
            initial_path: None,
        });
        let advertised = p.transfer_capabilities();
        let can_clone = p.clone_for_transfer().is_ok();
        assert!(can_clone);
        let caps = compose_runtime_transfer_capabilities(
            &advertised,
            p.transfer_executor_kind(),
            can_clone,
        );
        assert_eq!(caps.file_parallel, Capability::Supported);
        assert_eq!(caps.session_pool, Capability::Supported);
        assert_eq!(caps.max_file_slots, Some(4));
        assert_eq!(caps.max_chunk_slots, Some(4));

        let model = resolve_session_model(
            ProviderType::DrimeCloud,
            &caps,
            p.transfer_executor_kind(),
            can_clone,
            p.transfer_executor_max_sessions(),
            8,
        );
        assert!(matches!(
            model,
            crate::provider_transfer_executor::ProviderExecutorSessionModel::HttpClonePool { .. }
        ));
        assert_eq!(model.max_leases(), 4);
    }

    #[test]
    fn forced_clone_failure_demotes_runtime_file_parallelism() {
        use crate::provider_transfer_executor::compose_runtime_transfer_capabilities;
        use crate::transfer_dag::Capability;

        let p = test_provider(); // disconnected
        assert_eq!(
            p.transfer_executor_kind(),
            ProviderTransferExecutorKind::HttpClonePool
        );
        let can_clone = p.clone_for_transfer().is_ok();
        assert!(!can_clone);
        let caps = compose_runtime_transfer_capabilities(
            &p.transfer_capabilities(),
            p.transfer_executor_kind(),
            can_clone,
        );
        assert_eq!(caps.file_parallel, Capability::Unsupported);
        assert_eq!(caps.session_pool, Capability::Unsupported);
        assert_eq!(caps.max_file_slots, Some(1));
        // Multipart protocol truth is preserved for serial fallback.
        assert_eq!(caps.max_chunk_slots, Some(4));
    }

    #[tokio::test]
    async fn concurrent_part_puts_overlap_on_independent_workers() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use std::time::Duration;

        use axum::{extract::Path, http::StatusCode, routing::put, Router};

        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let request_count = Arc::new(AtomicUsize::new(0));
        let in_flight_h = Arc::clone(&in_flight);
        let peak_h = Arc::clone(&peak);
        let count_h = Arc::clone(&request_count);

        let app = Router::new().route(
            "/part/{n}",
            put(move |Path(n): Path<u32>| {
                let in_flight = Arc::clone(&in_flight_h);
                let peak = Arc::clone(&peak_h);
                let count = Arc::clone(&count_h);
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    // Hold the wire long enough for siblings to overlap.
                    tokio::time::sleep(Duration::from_millis(80)).await;
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    (
                        StatusCode::OK,
                        [(axum::http::header::ETAG, format!("\"part-{n}\""))],
                        format!("ok-{n}"),
                    )
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let addr = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        let primary = DrimeCloudProvider::connected_for_test(DrimeCloudConfig {
            api_token: secrecy::SecretString::from("test-token".to_string()),
            initial_path: None,
        });
        let base = format!("http://{addr}");
        let signed_urls: Vec<(u32, String)> = (1u32..=4)
            .map(|n| (n, format!("{base}/part/{n}")))
            .collect();
        let meta = DrimeMultipartMeta {
            key: "users/1/f.bin".to_string(),
            upload_id: "uid-fixture".to_string(),
            parent_id: String::new(),
            filename: "f.bin".to_string(),
            mime: "application/octet-stream".to_string(),
            extension: "bin".to_string(),
            total: 4 * 1024,
            part: 1024,
            total_parts: 4,
            signed_urls,
        };
        let handle = MultipartHandle {
            upload_id: meta.encode(),
            remote_path: "/f.bin".to_string(),
        };

        // N independent worker acquisitions (one per part).
        let mut workers: Vec<Box<dyn StorageProvider>> = (0..4)
            .map(|_| primary.clone_for_transfer().expect("worker clone"))
            .collect();
        let mut set = tokio::task::JoinSet::new();
        for (i, mut worker) in workers.drain(..).enumerate() {
            let handle = handle.clone();
            let part = (i as u32) + 1;
            set.spawn(async move {
                worker
                    .upload_part(&handle, part, vec![b'x'; 64])
                    .await
                    .map_err(|e| e.to_string())
            });
        }
        let mut receipts = Vec::new();
        while let Some(res) = set.join_next().await {
            receipts.push(res.expect("join").expect("upload_part"));
        }
        receipts.sort_by_key(|r| r.part_number);
        assert_eq!(receipts.len(), 4);
        for (i, r) in receipts.iter().enumerate() {
            assert_eq!(r.part_number, (i as u32) + 1);
            assert!(r.etag.contains(&(i + 1).to_string()) || !r.etag.is_empty());
        }

        let observed_peak = peak.load(Ordering::SeqCst);
        let total_reqs = request_count.load(Ordering::SeqCst);
        assert_eq!(total_reqs, 4, "each part must hit the wire once");
        assert!(
            observed_peak > 1,
            "independent workers must overlap on the wire (peak={observed_peak})"
        );
        assert!(
            observed_peak <= 4,
            "peak must stay within the honest ceiling of 4 (peak={observed_peak})"
        );

        server.abort();
    }

    #[tokio::test]
    async fn out_of_order_part_completion_returns_requested_receipts() {
        use axum::{http::StatusCode, routing::put, Router};

        let app = Router::new().route(
            "/part/{n}",
            put(
                |axum::extract::Path(n): axum::extract::Path<u32>| async move {
                    // Deliberately reverse latency so part 4 finishes first.
                    let delay_ms = 20u64 * (5u64.saturating_sub(n as u64));
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    (
                        StatusCode::OK,
                        [(axum::http::header::ETAG, format!("\"etag-{n}\""))],
                        "ok",
                    )
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        let primary = DrimeCloudProvider::connected_for_test(DrimeCloudConfig {
            api_token: secrecy::SecretString::from("test-token".to_string()),
            initial_path: None,
        });
        let base = format!("http://{addr}");
        let meta = DrimeMultipartMeta {
            key: "k".into(),
            upload_id: "u".into(),
            parent_id: String::new(),
            filename: "f.bin".into(),
            mime: "application/octet-stream".into(),
            extension: "bin".into(),
            total: 4096,
            part: 1024,
            total_parts: 4,
            signed_urls: (1u32..=4)
                .map(|n| (n, format!("{base}/part/{n}")))
                .collect(),
        };
        let handle = MultipartHandle {
            upload_id: meta.encode(),
            remote_path: "/f.bin".into(),
        };
        // Launch in reverse order; receipts must still match part numbers.
        let mut set = tokio::task::JoinSet::new();
        for part in [4u32, 1, 3, 2] {
            let mut worker = primary.clone_for_transfer().expect("clone");
            let handle = handle.clone();
            set.spawn(async move {
                worker
                    .upload_part(&handle, part, vec![0u8; 32])
                    .await
                    .map(|r| (part, r))
                    .map_err(|e| e.to_string())
            });
        }
        let mut got = Vec::new();
        while let Some(res) = set.join_next().await {
            let (requested, receipt) = res.unwrap().unwrap();
            assert_eq!(receipt.part_number, requested);
            got.push(receipt.part_number);
        }
        got.sort();
        assert_eq!(got, vec![1, 2, 3, 4]);
        server.abort();
    }

    #[test]
    fn clone_multipart_worker_helper_returns_some_only_when_connected() {
        use crate::transfer_multipart::clone_multipart_worker;

        let disconnected = test_provider();
        assert!(clone_multipart_worker(&disconnected).is_none());

        let connected = DrimeCloudProvider::connected_for_test(DrimeCloudConfig {
            api_token: secrecy::SecretString::from("test-token".to_string()),
            initial_path: None,
        });
        assert!(clone_multipart_worker(&connected).is_some());
    }

    #[tokio::test]
    async fn one_part_failure_does_not_mutate_primary_cache() {
        use axum::{http::StatusCode, routing::put, Router};

        let app = Router::new().route(
            "/part/{n}",
            put(
                |axum::extract::Path(n): axum::extract::Path<u32>| async move {
                    if n == 2 {
                        (StatusCode::INTERNAL_SERVER_ERROR, "boom")
                    } else {
                        (StatusCode::OK, "ok")
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        let mut primary = DrimeCloudProvider::connected_for_test(DrimeCloudConfig {
            api_token: secrecy::SecretString::from("test-token".to_string()),
            initial_path: None,
        });
        primary.dir_cache_insert("/keep".into(), DirInfo { id: "1".into() });
        let cache_before = primary.dir_cache.len();

        let base = format!("http://{addr}");
        let meta = DrimeMultipartMeta {
            key: "k".into(),
            upload_id: "u".into(),
            parent_id: String::new(),
            filename: "f.bin".into(),
            mime: "application/octet-stream".into(),
            extension: "bin".into(),
            total: 2048,
            part: 1024,
            total_parts: 2,
            signed_urls: vec![(1, format!("{base}/part/1")), (2, format!("{base}/part/2"))],
        };
        let handle = MultipartHandle {
            upload_id: meta.encode(),
            remote_path: "/f.bin".into(),
        };
        let mut w1 = primary.clone_for_transfer().unwrap();
        let mut w2 = primary.clone_for_transfer().unwrap();
        let ok = w1.upload_part(&handle, 1, vec![1u8; 8]).await;
        let err = w2.upload_part(&handle, 2, vec![2u8; 8]).await;
        assert!(ok.is_ok());
        assert!(err.is_err());
        // Primary mutable cache untouched by worker PUTs.
        assert_eq!(primary.dir_cache.len(), cache_before);
        assert!(primary.dir_cache.contains_key("/keep"));
        server.abort();
    }
}
